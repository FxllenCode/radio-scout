//! **ACME** (#76, RFC 8555): a certificate from Let's Encrypt — or any CA with
//! an ACME directory — issued, kept and renewed by the Instance itself.
//!
//! # What rdio-scanner does, and what is better here
//!
//! rdio hands Go's `autocert` a domain and a cache directory relative to the
//! working directory. Four things follow, all fixed here:
//!
//! - **Only TLS-ALPN-01.** `autocert.Manager.TLSConfig()` is all rdio wires, so
//!   an Operator who forwarded only port 80 never gets a certificate. Here both
//!   challenges are answered: **TLS-ALPN-01 first** — it needs only the port
//!   the site is on anyway — and **HTTP-01** through the LAN door if it fails.
//! - **Renewal on a fixed thirty days before expiry.** Here it follows the CA's
//!   own **ARI** window (RFC 9773) when the CA offers one — which is how Let's
//!   Encrypt asks for early renewal after a mass revocation — and two-thirds of
//!   the lifetime otherwise, which keeps working as certificate lifetimes
//!   shrink towards Let's Encrypt's announced 45 days.
//! - **A cache under whatever the working directory was.** Here it is
//!   `<base_dir>/tls/`, beside the database it belongs with, the key `0600`.
//! - **Failures are silent.** Here every failed attempt is an ERROR line naming
//!   what the CA said, and the status page carries the expiry and the last
//!   error, so a renewal that has been failing for a month is visible before
//!   the certificate it was renewing expires.
//!
//! # The shape
//!
//! The decisions are pure — [`renew_at`], [`retry_in`], [`StoredCertificate::answers`]
//! — and [`Acme::pass`] performs them, called by the TLS Worker on the
//! Instance's [`crate::Clock`]. A pass that issues takes as long as the CA
//! does; nothing else waits on it, because the plain port and every Worker
//! carry on.
//!
//! **Nothing here ever logs a key.** The account's key and the certificate's
//! live in files written `0600` and in memory, and an error from the CA is
//! about a name or a challenge, never a credential (ADR-0011 rule 2).

use std::path::Path;
use std::time::Duration;

use instant_acme::{
    Account, AccountCredentials, AuthorizationStatus, CertificateIdentifier, ChallengeType,
    Identifier, NewAccount, NewOrder, OrderStatus, RetryPolicy,
};
use serde::{Deserialize, Serialize};
use tracing::{error, info, warn};

use super::Tls;
use super::certificate::Certificate;

/// The fewest milliseconds between two issuances, whatever the schedule says.
///
/// A guard against a loop rather than a policy: a CA that hands back a
/// certificate already inside its own renewal window, or a clock that jumped,
/// would otherwise have the Worker issue back to back — and Let's Encrypt allows
/// five duplicate certificates a week, after which it refuses for days.
pub const MIN_BETWEEN_ISSUANCES: Duration = Duration::from_secs(60 * 60);

/// The longest one issuance may take, start to finish.
///
/// A CA normally answers in seconds. The bound is for one that does not answer
/// at all — `instant-acme`'s requests carry no timeout of their own — so a
/// silent CA costs one failed attempt and a backoff rather than a Worker that
/// never comes back to renew anything.
pub const ISSUANCE_DEADLINE: Duration = Duration::from_secs(5 * 60);

/// How long an ARI answer is trusted when the CA names no `Retry-After` worth
/// believing — RFC 9773 §4.3.2 suggests polling about twice a day.
const ARI_RECHECK: Duration = Duration::from_secs(6 * 60 * 60);

/// **When to renew** a certificate valid from `not_before_ms` to
/// `not_after_ms`.
///
/// Inside the CA's ARI `window` when it offered one, at `fraction` of the way
/// through it — RFC 9773 §4.2 asks for a random point, so a CA's whole fleet
/// does not renew in the same second. Otherwise two-thirds of the way through
/// the certificate's life, which is Let's Encrypt's own advice and scales with
/// whatever lifetime the CA issues.
pub fn renew_at(
    not_before_ms: i64,
    not_after_ms: i64,
    window: Option<(i64, i64)>,
    fraction: f64,
) -> i64 {
    match window {
        Some((start, end)) if end >= start => {
            start + ((end - start) as f64 * fraction.clamp(0.0, 1.0)) as i64
        }
        _ => not_before_ms + (not_after_ms - not_before_ms) * 2 / 3,
    }
}

/// How long to wait after the `failures`th failed attempt in a row.
///
/// Fifteen minutes, then three times longer each time, to a ceiling of six
/// hours. A CA counts failed validations — Let's Encrypt allows five an hour
/// per name — and **one attempt can spend two**, TLS-ALPN-01 and then HTTP-01,
/// so attempts at 0, 15 and 60 minutes stay inside the limit where a
/// five-minute start would not. A name whose DNS is wrong must not be retried
/// every minute; an Operator who has just fixed it should not wait a day
/// either, and a restart tries again at once.
pub fn retry_in(failures: u32) -> Duration {
    const FIRST: u64 = 15 * 60;
    const CEILING: u64 = 6 * 60 * 60;
    let exponent = failures.saturating_sub(1).min(8);
    Duration::from_secs((FIRST * 3u64.pow(exponent)).min(CEILING))
}

/// An issued certificate as it is kept on disk: the chain and key, and what it
/// was issued *for* — so a configuration that has since named another domain or
/// another CA is not answered with it.
///
/// One file rather than a PEM pair, so a renewal replaces the chain and its key
/// in a single rename and a crash can never leave one without the other.
#[derive(Serialize, Deserialize)]
pub struct StoredCertificate {
    pub directory: String,
    pub domains: Vec<String>,
    pub chain_pem: String,
    pub key_pem: String,
}

impl StoredCertificate {
    /// Whether this certificate is the one `directory` and `domains` ask for:
    /// the same CA, and exactly the same names in any order.
    pub fn answers(&self, directory: &str, domains: &[String]) -> bool {
        let mut stored = self.domains.clone();
        let mut asked = domains.to_vec();
        stored.sort();
        asked.sort();
        self.directory == directory && stored == asked
    }
}

/// The ACME account, as kept on disk — tied to the directory it was made on.
#[derive(Serialize, Deserialize)]
struct StoredAccount {
    directory: String,
    credentials: AccountCredentials,
}

const CERTIFICATE_FILE: &str = "certificate.json";
const ACCOUNT_FILE: &str = "account.json";

/// The certificate this Instance issued last time, if it still answers the
/// configuration — what a restart serves before the Worker has done anything,
/// so a restart is not an issuance.
pub fn stored(dir: &Path, directory: &str, domains: &[String]) -> Option<Certificate> {
    let text = std::fs::read_to_string(dir.join(CERTIFICATE_FILE)).ok()?;
    let stored: StoredCertificate = serde_json::from_str(&text).ok()?;
    stored
        .answers(directory, domains)
        .then(|| Certificate::from_pem(stored.chain_pem.as_bytes(), stored.key_pem.as_bytes()))?
        .ok()
}

/// What the ACME half of the TLS Worker remembers between passes.
pub struct Acme {
    /// The names this Instance's certificate is for.
    domains: Vec<String>,
    account: Option<Account>,
    /// Failed attempts in a row, and when the next may be made.
    failures: u32,
    retry_at_ms: Option<i64>,
    /// When this Instance last issued, so it never does twice inside
    /// [`MIN_BETWEEN_ISSUANCES`].
    issued_at_ms: Option<i64>,
    /// The CA's renewal window for the certificate being served, and until
    /// when to believe it.
    ari: Option<Ari>,
}

struct Ari {
    leaf: Vec<u8>,
    window: Option<(i64, i64)>,
    fraction: f64,
    until_ms: i64,
}

impl Acme {
    /// The keeper of a certificate for `domains`, with nothing done yet.
    pub fn new(domains: Vec<String>) -> Self {
        Acme {
            domains,
            account: None,
            failures: 0,
            retry_at_ms: None,
            issued_at_ms: None,
            ari: None,
        }
    }

    /// Issue if there is nothing worth serving, renew if the schedule says so,
    /// and answer when to look again.
    pub async fn pass(&mut self, tls: &Tls, now_ms: i64) -> i64 {
        // Woken early — the clock moved, or something changed — but still
        // backing off from the last failure: nothing to do until it is over.
        if let Some(retry_at) = self.retry_at_ms.filter(|at| now_ms < *at) {
            return retry_at;
        }
        let served = tls.served();
        let due_at = match &served {
            None => now_ms,
            Some(certificate) => self.renewal_due_at(tls, certificate, now_ms).await,
        };
        // Never twice inside the guard, whatever the schedule says.
        let due_at = self
            .issued_at_ms
            .map_or(due_at, |issued| due_at.max(issued + guard_ms()));
        tls.health_mut(|health| health.renew_at_ms = served.as_ref().map(|_| due_at));
        if now_ms < due_at {
            return self
                .ari
                .as_ref()
                .map_or(due_at, |ari| due_at.min(ari.until_ms));
        }

        let domains = self.domains.join(",");
        let issued =
            tokio::time::timeout(ISSUANCE_DEADLINE, self.issue(tls, served.as_ref())).await;
        match issued.unwrap_or_else(|_| Err("the CA did not finish within five minutes".into())) {
            Ok(certificate) => {
                let not_after_ms = certificate.not_after_ms;
                // What the status page shows until the next pass asks the CA's
                // window: the lifetime rule, never sooner than the guard.
                let renews_ms = renew_at(certificate.not_before_ms, not_after_ms, None, 0.0)
                    .max(now_ms + guard_ms());
                tls.serve(certificate);
                tls.health_mut(|health| {
                    health.last_error = None;
                    health.renew_at_ms = Some(renews_ms);
                });
                info!(%domains, not_after_ms, "issued a certificate");
                self.failures = 0;
                self.retry_at_ms = None;
                self.issued_at_ms = Some(now_ms);
                self.ari = None;
                now_ms + guard_ms()
            }
            Err(why) => {
                self.failures += 1;
                let failures = self.failures;
                let retry_in_secs = retry_in(failures).as_secs();
                let retry_at = now_ms + retry_in(failures).as_millis() as i64;
                self.retry_at_ms = Some(retry_at);
                // ERROR rather than WARN: a certificate that cannot be renewed
                // takes the site down when it expires, and that is an Operator
                // acting or nobody (ADR-0011 rule 7). Once per attempt, and
                // attempts back off, so this is never a flood.
                error!(%domains, failures, retry_in_secs, error = %why, "could not get a certificate");
                tls.health_mut(|health| {
                    health.last_error = Some(super::LastError {
                        at_ms: now_ms,
                        message: why,
                    })
                });
                retry_at
            }
        }
    }

    /// When the certificate being served falls due — asking the CA's ARI
    /// endpoint when the last answer has gone stale, and falling back to the
    /// certificate's own lifetime when the CA offers none or cannot be reached.
    async fn renewal_due_at(&mut self, tls: &Tls, certificate: &Certificate, now_ms: i64) -> i64 {
        let leaf = certificate.leaf().to_vec();
        let stale = !self
            .ari
            .as_ref()
            .is_some_and(|ari| ari.leaf == leaf && now_ms < ari.until_ms);
        if stale {
            let window = self.ask_ari(tls, certificate).await;
            let believe_for = window.as_ref().map_or(ARI_RECHECK, |(_, retry)| {
                (*retry).clamp(Duration::from_secs(60), ARI_RECHECK)
            });
            self.ari = Some(Ari {
                leaf,
                window: window.map(|(window, _)| window),
                fraction: random_fraction(),
                until_ms: now_ms + believe_for.as_millis() as i64,
            });
        }
        let ari = self.ari.as_ref().expect("just set");
        renew_at(
            certificate.not_before_ms,
            certificate.not_after_ms,
            ari.window,
            ari.fraction,
        )
    }

    /// The CA's renewal window for `certificate`, and how long to believe it —
    /// or nothing, quietly, when the CA has no ARI or cannot be reached. Asking
    /// is an optimisation of *when*; failing to ask must never stop a renewal.
    async fn ask_ari(
        &mut self,
        tls: &Tls,
        certificate: &Certificate,
    ) -> Option<((i64, i64), Duration)> {
        let id = CertificateIdentifier::try_from(certificate.leaf()).ok()?;
        let account = self.account(tls).await.ok()?;
        let (info, retry_after) = account.renewal_info(&id).await.ok()?;
        let window = &info.suggested_window;
        let to_ms = |at: time::OffsetDateTime| (at.unix_timestamp_nanos() / 1_000_000) as i64;
        Some(((to_ms(window.start), to_ms(window.end)), retry_after))
    }

    /// The account to order with: the one already open, the one on disk for
    /// this directory, or a new one.
    async fn account(&mut self, tls: &Tls) -> Result<Account, String> {
        if let Some(account) = &self.account {
            return Ok(account.clone());
        }
        let config = tls.config();
        let builder = client(config)?;
        let path = tls.dir().join(ACCOUNT_FILE);
        let stored = std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| serde_json::from_str::<StoredAccount>(&text).ok())
            .filter(|stored| stored.directory == config.directory);
        let account = match stored {
            Some(stored) => builder
                .from_credentials(stored.credentials)
                .await
                .map_err(|error| format!("could not reopen the ACME account: {error}"))?,
            None => {
                let contact = config.email.as_ref().map(|email| format!("mailto:{email}"));
                let contacts: Vec<&str> = contact.iter().map(String::as_str).collect();
                let (account, credentials) = builder
                    .create(
                        &NewAccount {
                            contact: &contacts,
                            // Naming a domain in `[tls]` is the Operator's
                            // agreement — what rdio's `autocert.AcceptTOS` and
                            // Caddy both do — and the terms are logged below.
                            terms_of_service_agreed: true,
                            only_return_existing: false,
                        },
                        config.directory.clone(),
                        None,
                    )
                    .await
                    .map_err(|error| format!("could not register an ACME account: {error}"))?;
                write_private(
                    &path,
                    &serde_json::to_vec(&StoredAccount {
                        directory: config.directory.clone(),
                        credentials,
                    })
                    .expect("account credentials serialize"),
                )
                .map_err(|error| format!("could not save the ACME account: {error}"))?;
                let terms = terms_of_service(config)
                    .await
                    .unwrap_or_else(|| "(the CA did not say)".into());
                let directory = &config.directory;
                info!(%directory, %terms, "registered an ACME account, agreeing to the certificate authority's terms");
                account
            }
        };
        self.account = Some(account.clone());
        Ok(account)
    }

    /// One issuance: TLS-ALPN-01, then HTTP-01 if that failed.
    async fn issue(
        &mut self,
        tls: &Tls,
        replacing: Option<&Certificate>,
    ) -> Result<Certificate, String> {
        let account = self.account(tls).await?;
        let mut why = Vec::new();
        for challenge in [ChallengeType::TlsAlpn01, ChallengeType::Http01] {
            let attempt = attempt(tls, &account, &self.domains, challenge.clone(), replacing).await;
            tls.withdraw_challenges();
            match attempt {
                Ok(issued) => return Ok(keep(tls, issued)),
                Err(error) => {
                    warn!(challenge = ?challenge, %error, "an ACME challenge did not succeed");
                    why.push(format!("{challenge:?}: {error}"));
                }
            }
        }
        Err(why.join("; "))
    }
}

/// [`MIN_BETWEEN_ISSUANCES`], in the clock's units.
fn guard_ms() -> i64 {
    MIN_BETWEEN_ISSUANCES.as_millis() as i64
}

/// The ACME client for `config`'s directory: trusting the system's roots, or
/// only `directory_root` when one is named — a private CA's.
fn client(config: &super::TlsConfig) -> Result<instant_acme::AccountBuilder, String> {
    // `instant-acme` builds its client on rustls' process-wide provider, and
    // this tree enables two, so rustls will not guess. Idempotent: an
    // already-installed provider is left exactly as it is.
    let _ = super::provider().as_ref().clone().install_default();
    match &config.directory_root {
        Some(root) => Account::builder_with_root(root),
        None => Account::builder(),
    }
    .map_err(|error| format!("could not build an ACME client: {error}"))
}

/// Whether polling left the order ready to finalize. A CA that marks an order
/// invalid normally says why, which `poll_ready` hands back as an error; this
/// is the order it left some other way.
fn ready(status: OrderStatus) -> Result<(), String> {
    match status {
        OrderStatus::Ready => Ok(()),
        status => Err(format!("the CA left the order {status:?}")),
    }
}

/// An issued certificate and the PEM it was parsed from, which is what is
/// written to disk.
struct Issued(Certificate, StoredCertificate);

/// One order, answered with one kind of challenge.
async fn attempt(
    tls: &Tls,
    account: &Account,
    domains: &[String],
    challenge_type: ChallengeType,
    replacing: Option<&Certificate>,
) -> Result<Issued, String> {
    let identifiers: Vec<Identifier> = domains.iter().cloned().map(Identifier::Dns).collect();
    let replaces = replacing.and_then(|certificate| {
        CertificateIdentifier::try_from(certificate.leaf())
            .ok()
            .map(CertificateIdentifier::into_owned)
    });
    let mut new_order = NewOrder::new(&identifiers);
    if let Some(replaces) = replaces.clone() {
        new_order = new_order.replaces(replaces);
    }
    let mut order = match account.new_order(&new_order).await {
        Ok(order) => Ok(order),
        // A CA that will not take this as a replacement — one without ARI, or
        // one that never issued the certificate to this account — is asked for
        // the same certificate without the courtesy.
        Err(_) if replaces.is_some() => account.new_order(&NewOrder::new(&identifiers)).await,
        Err(error) => Err(error),
    }
    .map_err(|error| format!("the CA refused the order: {error}"))?;

    let mut authorizations = order.authorizations();
    while let Some(authorization) = authorizations.next().await {
        let mut authorization =
            authorization.map_err(|error| format!("could not read an authorization: {error}"))?;
        // Already proven for this account; anything other than pending fails
        // below, in the CA's own words.
        if authorization.status == AuthorizationStatus::Valid {
            continue;
        }
        let mut challenge = authorization
            .challenge(challenge_type.clone())
            .ok_or_else(|| format!("the CA offered no {challenge_type:?} challenge"))?;
        let name = challenge.identifier().to_string();
        let key_authorization = challenge.key_authorization();
        match challenge_type {
            ChallengeType::TlsAlpn01 => tls.offer_alpn_challenge(
                &name,
                alpn_certificate(&name, key_authorization.digest().as_ref())?,
            ),
            _ => tls.offer_http_challenge(&challenge.token, key_authorization.as_str().to_owned()),
        }
        challenge
            .set_ready()
            .await
            .map_err(|error| format!("the CA would not start checking: {error}"))?;
    }

    order
        .poll_ready(&RetryPolicy::default())
        .await
        .map_err(|error| format!("the CA did not finish checking: {error}"))
        .and_then(ready)?;
    let key_pem = order
        .finalize()
        .await
        .map_err(|error| format!("the CA would not finalize the order: {error}"))?;
    let chain_pem = order
        .poll_certificate(&RetryPolicy::default())
        .await
        .map_err(|error| format!("the CA did not hand over the certificate: {error}"))?;
    let certificate = Certificate::from_pem(chain_pem.as_bytes(), key_pem.as_bytes())
        .map_err(|why| format!("the CA's certificate cannot be served: {why}"))?;
    Ok(Issued(
        certificate,
        StoredCertificate {
            directory: tls.config().directory.clone(),
            domains: domains.to_vec(),
            chain_pem,
            key_pem,
        },
    ))
}

/// The certificate a CA validating TLS-ALPN-01 is shown (RFC 8737 §3): for
/// `name`, self-signed, carrying the key authorization's digest in the
/// `acmeIdentifier` extension.
fn alpn_certificate(
    name: &str,
    digest: &[u8],
) -> Result<std::sync::Arc<rustls::sign::CertifiedKey>, String> {
    let fail = |error: rcgen::Error| format!("could not make the challenge certificate: {error}");
    let key = rcgen::KeyPair::generate().map_err(fail)?;
    let mut params = rcgen::CertificateParams::new(vec![name.to_string()]).map_err(fail)?;
    params.custom_extensions = vec![rcgen::CustomExtension::new_acme_identifier(digest)];
    let certificate = params.self_signed(&key).map_err(fail)?;
    let key_der = rustls_pki_types::PrivateKeyDer::Pkcs8(key.serialize_der().into());
    // `CertifiedKey::new`, not `from_der`: the latter checks the key against the
    // certificate by *parsing* it, and webpki refuses the critical
    // `acmeIdentifier` extension this certificate exists to carry. The pair is
    // ours, made a line above, so there is nothing to check.
    let signing = super::provider()
        .key_provider
        .load_private_key(key_der)
        .map_err(|error| format!("could not use the challenge key: {error}"))?;
    Ok(std::sync::Arc::new(rustls::sign::CertifiedKey::new(
        vec![certificate.der().clone()],
        signing,
    )))
}

/// Keep what was issued: written to disk so a restart serves it, and handed
/// back to be served now.
///
/// **Served even if it cannot be saved.** Throwing away a certificate the CA
/// has already issued would mean ordering it again, against Let's Encrypt's
/// limit of five duplicate certificates a week; serving it and saying so costs
/// only a re-issue at the next restart.
fn keep(tls: &Tls, issued: Issued) -> Certificate {
    if let Err(error) = persist(tls, &issued) {
        error!(%error, "could not save the issued certificate; it is being served, and a restart will ask for another");
    }
    issued.0
}

/// Write what was issued, so a restart serves it rather than ordering another.
fn persist(tls: &Tls, issued: &Issued) -> std::io::Result<()> {
    write_private(
        &tls.dir().join(CERTIFICATE_FILE),
        &serde_json::to_vec(&issued.1).expect("a certificate serializes"),
    )
}

/// Write `bytes` to `path` owner-readable only, replacing it in one rename so a
/// crash leaves the old file or the new one and never half of either.
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;

    let dir = path.parent().expect("a file in a directory");
    std::fs::create_dir_all(dir)?;
    let staging = dir.join(format!(
        ".{}.{}",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("tls"),
        uuid::Uuid::new_v4().simple()
    ));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let written = options
        .open(&staging)
        .and_then(|mut file| file.write_all(bytes).and_then(|()| file.sync_all()))
        .and_then(|()| std::fs::rename(&staging, path));
    if written.is_err() {
        let _ = std::fs::remove_file(&staging);
    }
    written
}

/// The CA's terms of service, read from its directory — `instant-acme` keeps
/// the directory's `meta` to itself, and the Operator agreeing to the terms is
/// owed a link to them.
async fn terms_of_service(config: &super::TlsConfig) -> Option<String> {
    // A `User-Agent` because RFC 8555 §6.1 requires one on every request, and a
    // CA is entitled to refuse without it — Pebble does.
    let mut client = reqwest::Client::builder().user_agent(USER_AGENT);
    if let Some(root) = &config.directory_root {
        let pem = std::fs::read(root).ok()?;
        client = client.tls_certs_only([reqwest::Certificate::from_pem(&pem).ok()?]);
    }
    let response = client
        .build()
        .ok()?
        .get(&config.directory)
        .send()
        .await
        .ok()?;
    // Text and then JSON, rather than `Response::json`: that is behind reqwest's
    // `json` feature, which only the test build enables.
    let directory: serde_json::Value = serde_json::from_str(&response.text().await.ok()?).ok()?;
    directory["meta"]["termsOfService"]
        .as_str()
        .map(str::to_owned)
}

/// What this Instance calls itself to a CA.
const USER_AGENT: &str = concat!("radio-scout/", env!("CARGO_PKG_VERSION"));

/// A uniform point in `[0, 1)` — where in the CA's window to renew.
fn random_fraction() -> f64 {
    use argon2::password_hash::rand_core::{OsRng, RngCore};
    OsRng.next_u32() as f64 / (u32::MAX as f64 + 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use rstest::rstest;

    const DAY: i64 = 24 * 60 * 60 * 1000;

    /// With no window from the CA, two-thirds of the way through the
    /// certificate's life — 60 days into a 90-day certificate, 30 into a
    /// 45-day one.
    #[rstest]
    #[case::ninety_days(0, 90 * DAY, 60 * DAY)]
    #[case::forty_five_days(0, 45 * DAY, 30 * DAY)]
    #[case::six_days(10 * DAY, 16 * DAY, 14 * DAY)]
    fn without_ari_renew_two_thirds_of_the_way_through(
        #[case] not_before: i64,
        #[case] not_after: i64,
        #[case] expected: i64,
    ) {
        assert_eq!(renew_at(not_before, not_after, None, 0.5), expected);
    }

    /// With one, at the given point inside it — and never outside it, whatever
    /// the point.
    #[rstest]
    #[case::the_start(0.0, 50 * DAY)]
    #[case::the_middle(0.5, 51 * DAY)]
    #[case::the_end(1.0, 52 * DAY)]
    #[case::clamped_below(-3.0, 50 * DAY)]
    #[case::clamped_above(7.0, 52 * DAY)]
    fn with_ari_renew_inside_the_cas_window(#[case] fraction: f64, #[case] expected: i64) {
        assert_eq!(
            renew_at(0, 90 * DAY, Some((50 * DAY, 52 * DAY)), fraction),
            expected
        );
    }

    /// A window that ends before it starts is no window at all.
    #[test]
    fn a_backwards_window_is_ignored() {
        assert_eq!(
            renew_at(0, 90 * DAY, Some((52 * DAY, 50 * DAY)), 0.5),
            60 * DAY
        );
    }

    /// Fifteen minutes, forty-five, two and a quarter hours... and never more
    /// than six.
    #[rstest]
    #[case(0, 15 * 60)]
    #[case(1, 15 * 60)]
    #[case(2, 45 * 60)]
    #[case(3, 135 * 60)]
    #[case(4, 6 * 60 * 60)]
    #[case(50, 6 * 60 * 60)]
    #[case(u32::MAX, 6 * 60 * 60)]
    fn failures_back_off_to_a_ceiling(#[case] failures: u32, #[case] secs: u64) {
        assert_eq!(retry_in(failures), Duration::from_secs(secs));
    }

    fn stored(directory: &str, domains: &[&str]) -> StoredCertificate {
        StoredCertificate {
            directory: directory.into(),
            domains: domains.iter().map(|d| d.to_string()).collect(),
            chain_pem: String::new(),
            key_pem: String::new(),
        }
    }

    /// A stored certificate is served only to the configuration it was issued
    /// for: a new name or a new CA (staging to production, most often) is a new
    /// certificate, not a browser warning.
    #[rstest]
    #[case::the_same(crate::tls::LETS_ENCRYPT, &["a.example", "b.example"], true)]
    #[case::reordered(crate::tls::LETS_ENCRYPT, &["b.example", "a.example"], true)]
    #[case::a_name_added(crate::tls::LETS_ENCRYPT, &["a.example", "b.example", "c.example"], false)]
    #[case::a_name_removed(crate::tls::LETS_ENCRYPT, &["a.example"], false)]
    #[case::another_ca("https://acme-staging-v02.api.letsencrypt.org/directory", &["a.example", "b.example"], false)]
    fn a_stored_certificate_answers_only_what_it_was_issued_for(
        #[case] directory: &str,
        #[case] domains: &[&str],
        #[case] answers: bool,
    ) {
        let domains: Vec<String> = domains.iter().map(|d| d.to_string()).collect();

        assert_eq!(
            stored(crate::tls::LETS_ENCRYPT, &["a.example", "b.example"])
                .answers(directory, &domains),
            answers
        );
    }

    /// The key never leaves its file readable by anyone else, and a replace
    /// leaves no staging file behind.
    #[cfg(unix)]
    #[test]
    fn a_private_file_is_written_0600_and_replaced_whole() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("tls").join("certificate.json");

        write_private(&path, b"first").expect("write");
        write_private(&path, b"second").expect("replace");

        assert_eq!(std::fs::read(&path).expect("read"), b"second");
        let mode = std::fs::metadata(&path).expect("stat").permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "mode was {:o}", mode & 0o777);
        assert_eq!(
            std::fs::read_dir(path.parent().expect("dir"))
                .expect("ls")
                .count(),
            1,
            "a staging file was left behind"
        );
    }

    /// A certificate the CA has issued is never thrown away for want of a disk
    /// to write it to — it is served, and the log says a restart will cost a
    /// re-issue.
    #[test]
    fn an_issued_certificate_that_cannot_be_saved_is_still_kept() {
        let logs = crate::testing::LogCapture::start();
        let dir = tempfile::tempdir().expect("tempdir");
        // A *file* where the directory should be, so nothing can be written
        // under it.
        let blocked = dir.path().join("tls");
        std::fs::write(&blocked, b"not a directory").expect("block it");
        let tls = Tls::new(Default::default(), blocked, None);
        let key = rcgen::KeyPair::generate().expect("a key");
        let leaf = rcgen::CertificateParams::new(vec!["scanner.example".to_string()])
            .expect("params")
            .self_signed(&key)
            .expect("a certificate");
        let (chain_pem, key_pem) = (leaf.pem(), key.serialize_pem());
        let certificate =
            Certificate::from_pem(chain_pem.as_bytes(), key_pem.as_bytes()).expect("parses");

        let kept = keep(
            &tls,
            Issued(
                certificate,
                StoredCertificate {
                    directory: crate::tls::LETS_ENCRYPT.into(),
                    domains: vec!["scanner.example".into()],
                    chain_pem,
                    key_pem,
                },
            ),
        );

        assert_eq!(kept.leaf().as_ref(), leaf.der().as_ref());
        let text = logs.text();
        assert!(
            text.contains("could not save the issued certificate"),
            "{text}"
        );
    }

    /// Polling that leaves an order anything but ready is a failure in its own
    /// words — the CA normally says why as an error, and this is the order it
    /// left without one.
    #[rstest]
    #[case(OrderStatus::Ready, true)]
    #[case(OrderStatus::Invalid, false)]
    #[case(OrderStatus::Pending, false)]
    fn only_a_ready_order_is_finalized(#[case] status: OrderStatus, #[case] ready_to_go: bool) {
        assert_eq!(ready(status).is_ok(), ready_to_go);
    }

    /// The system's roots unless the Operator named a private CA's — and a root
    /// that cannot be read is said so, rather than a client built trusting
    /// nothing.
    #[test]
    fn the_client_trusts_the_system_or_the_named_root() {
        let system = super::super::TlsConfig::default();
        let missing = super::super::TlsConfig {
            directory_root: Some("/nowhere/root.pem".into()),
            ..Default::default()
        };

        assert!(client(&system).is_ok());
        assert!(
            client(&missing)
                .err()
                .is_some_and(|why| why.contains("could not build an ACME client")),
        );
    }

    /// A replace that fails leaves the old file and no staging file — the
    /// rename is the only step that touches what is there.
    #[test]
    fn a_failed_replace_leaves_no_staging_file_behind() {
        let dir = tempfile::tempdir().expect("tempdir");
        // A directory with something in it, where the file should go: the
        // staging file is written and the rename onto it fails.
        let target = dir.path().join("certificate.json");
        std::fs::create_dir(&target).expect("a directory in the way");
        std::fs::write(target.join("inside"), b"x").expect("not empty");

        assert!(write_private(&target, b"new").is_err());

        let left: Vec<_> = std::fs::read_dir(dir.path())
            .expect("ls")
            .map(|entry| entry.expect("entry").file_name())
            .collect();
        assert_eq!(left, ["certificate.json"], "{left:?}");
    }

    /// A directory, served the way a CA serves one — over plain HTTP here,
    /// since nothing about reading `meta` depends on the transport — and the
    /// `User-Agent` it was asked with, which RFC 8555 §6.1 requires and Pebble
    /// enforces.
    async fn directory_answering(
        body: &'static str,
    ) -> (String, std::sync::Arc<std::sync::Mutex<Option<String>>>) {
        let asked_as = std::sync::Arc::new(std::sync::Mutex::new(None));
        let seen = asked_as.clone();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        let app = axum::Router::new().route(
            "/dir",
            axum::routing::get(move |headers: axum::http::HeaderMap| async move {
                *seen.lock().expect("seen") = headers
                    .get(axum::http::header::USER_AGENT)
                    .and_then(|agent| agent.to_str().ok())
                    .map(str::to_owned);
                body
            }),
        );
        tokio::spawn(async move { axum::serve(listener, app).await });
        (format!("http://{addr}/dir"), asked_as)
    }

    /// The Operator agreeing to the terms is shown them: the URL the CA's own
    /// directory names, read with the `User-Agent` RFC 8555 asks for — and
    /// nothing invented when the CA names none.
    #[tokio::test]
    async fn the_terms_are_the_ones_the_directory_names() {
        let (named, asked_as) =
            directory_answering(r#"{"meta":{"termsOfService":"https://ca.example/terms"}}"#).await;
        let (silent, _) = directory_answering(r#"{"newOrder":"x"}"#).await;
        let config = |directory| super::super::TlsConfig {
            directory,
            ..Default::default()
        };

        assert_eq!(
            terms_of_service(&config(named)).await.as_deref(),
            Some("https://ca.example/terms")
        );
        assert_eq!(asked_as.lock().expect("asked").as_deref(), Some(USER_AGENT));
        assert_eq!(terms_of_service(&config(silent)).await, None);
    }

    proptest! {
        /// Whatever the certificate and the window, renewal is never after the
        /// certificate has expired — unless the CA's own window says so.
        #[test]
        fn renewal_without_a_window_is_inside_the_lifetime(
            not_before in 0i64..1_000_000_000_000,
            life in 1i64..(400 * DAY),
            fraction in 0.0f64..1.0,
        ) {
            let at = renew_at(not_before, not_before + life, None, fraction);
            prop_assert!(at > not_before && at < not_before + life);
        }

        /// A random point is always a point inside the window.
        #[test]
        fn a_random_point_is_inside_the_window(_ in 0..64u8) {
            let fraction = random_fraction();
            prop_assert!((0.0..1.0).contains(&fraction));
        }
    }
}
