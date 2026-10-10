//! **Built-in TLS** (#76, spec US 61): HTTPS from the binary itself, for an
//! Operator who wants no third party in front of their scanner.
//!
//! # It is the third of three ways public, on purpose
//!
//! The recommended way to put an Instance on the internet is a **Cloudflare
//! Tunnel**: `cloudflared` beside the scanner dials *out*, so no port is
//! forwarded, no certificate lives on the Pi, and it works behind the CGNAT a
//! home connection increasingly sits behind. A reverse proxy is the second way
//! and stays first-class. Both need nothing from this module — what they needed
//! was `[server] trusted_proxies` believing loopback by default, so a tunnel on
//! the same machine is not every visitor at once (see [`crate::config::Server`]).
//!
//! This module is for the rest: a VPS with nothing else on it, an Operator who
//! will not route audio through Cloudflare (whose CDN terms name audio files
//! outright), and everyone migrating from rdio-scanner's `ssl_auto_cert` and
//! `ssl_cert_file`, which parity says must keep working. `docs/deploy.md` puts
//! the three in that order and says why.
//!
//! # The section
//!
//! `[tls]` is off until it names a certificate [`Source`] — **`domains`** for
//! ACME (Let's Encrypt unless `directory` says otherwise), or **`cert_file` +
//! `key_file`** for an Operator's own. Both at once refuses to boot, as does
//! every value that could never work: a wildcard (it needs a DNS challenge,
//! which this does not do), a bare IP address, a name that is not one, an ACME
//! directory over plain HTTP, half a pair of files, or a TLS port that is the
//! plain one. Each is a field's own refusal where it can be — [`Domain`] in its
//! `Deserialize`, the `ProxyNet` precedent — so the message carries the line
//! and column the Operator has to edit.
//!
//! rdio-scanner refuses none of this. A typo'd `ssl_auto_cert` is handed to Go's
//! `autocert` as written, which asks the CA, is refused, and logs nothing an
//! Operator would see until a browser shows them an error page.

pub mod acme;
pub mod certificate;
pub mod door;
pub mod listener;
pub mod worker;

use std::collections::HashMap;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::atomic::{AtomicU16, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use rustls::server::{ClientHello, ResolvesServerCert};
use rustls::sign::CertifiedKey;
use serde::{Deserialize, Serialize};

use crate::worker::WakeUp;
use certificate::Certificate;

/// How often an Operator's own certificate files are read again (#76).
///
/// A minute, because the reason to look is a renewal that has just landed and
/// the cost is reading two files of a few kilobytes — nothing, even on a Pi.
/// rdio-scanner reads them once, at boot, so a renewed certificate is served
/// only after somebody remembers to restart it.
pub const FILES_RECHECK: Duration = Duration::from_secs(60);

/// The ALPN protocol a TLS-ALPN-01 validation asks for (RFC 8737 §6.2).
pub const ACME_TLS_ALPN: &[u8] = b"acme-tls/1";

/// The one crypto provider every TLS configuration here is built on — the
/// aws-lc-rs that reqwest and object_store already link, named explicitly
/// because the tree enables `ring` too, and rustls refuses to guess between
/// two.
pub fn provider() -> Arc<rustls::crypto::CryptoProvider> {
    Arc::new(rustls::crypto::aws_lc_rs::default_provider())
}

/// A request that arrived on the HTTPS listener — a request extension that
/// listener's router stamps on everything it serves.
///
/// What makes the admin session cookie `Secure` on built-in TLS (ADR-0008). It
/// is the **listener's** word, not a header: nothing a client sends can put it
/// on a request, where `X-Forwarded-Proto` is believed only from a trusted
/// proxy because anybody can write one.
#[derive(Debug, Clone, Copy)]
pub struct OverTls;

/// Built-in TLS as the running Instance holds it: what is being served, what
/// the CA is being shown, and the Worker's wake-up.
///
/// Shared by everything that has a part in it — the HTTPS listener asks it for
/// a certificate on every handshake, the LAN door asks it for a challenge, the
/// Worker installs what it issued or re-read, and the status page reads how it
/// is doing — so it is a cheap-to-clone handle on `AppState`, the
/// [`crate::delay::Delays`] shape.
#[derive(Clone)]
pub struct Tls(Arc<Inner>);

struct Inner {
    config: TlsConfig,
    source: Source,
    /// Where an issued certificate and the CA account are kept.
    dir: PathBuf,
    /// Where a redirect from the LAN door sends a stranger.
    public_url: Option<String>,
    /// What a handshake is answered with. `None` until there is one — an ACME
    /// Instance's first issuance is still under way, and a handshake then
    /// fails rather than being answered with something a browser would refuse.
    served: RwLock<Option<Certificate>>,
    /// HTTP-01: token to key authorization, while the CA is checking.
    http01: Mutex<HashMap<String, String>>,
    /// TLS-ALPN-01: name to the challenge certificate, while the CA is checking.
    alpn01: Mutex<HashMap<String, Arc<CertifiedKey>>>,
    /// What the status page reports.
    health: Mutex<Health>,
    /// The port HTTPS actually bound — `[tls] port = 0` asks the OS, and a
    /// redirect has to name the one it chose.
    https_port: AtomicU16,
    /// What the Worker owes, and its wake-up.
    wake_up: WakeUp,
}

/// How the certificate is doing, as the Worker last found it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Health {
    /// When the certificate being served expires.
    pub not_after_ms: Option<i64>,
    /// When the Worker will next try to renew it (ACME only).
    pub renew_at_ms: Option<i64>,
    /// The last thing that went wrong — cleared by a success.
    pub last_error: Option<LastError>,
}

/// Something that went wrong keeping the certificate current, and when.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LastError {
    pub at_ms: i64,
    /// In the words of whatever refused: the CA's, or the file's.
    pub message: String,
}

impl Default for Tls {
    /// Off — what `AppState::new` holds until an Instance with `[tls]` on
    /// replaces it.
    fn default() -> Self {
        Tls::new(TlsConfig::default(), PathBuf::new(), None)
    }
}

impl std::fmt::Debug for Tls {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tls")
            .field("source", &self.0.source)
            .finish_non_exhaustive()
    }
}

impl Tls {
    /// Built-in TLS as `config` describes it, keeping what it issues in `dir`.
    pub fn new(config: TlsConfig, dir: PathBuf, public_url: Option<String>) -> Self {
        Tls(Arc::new(Inner {
            source: config.source(),
            config,
            dir,
            public_url,
            served: RwLock::new(None),
            http01: Mutex::new(HashMap::new()),
            alpn01: Mutex::new(HashMap::new()),
            health: Mutex::new(Health::default()),
            https_port: AtomicU16::new(0),
            wake_up: WakeUp::default(),
        }))
    }

    /// Whether this Instance serves HTTPS itself.
    pub fn is_on(&self) -> bool {
        self.0.source != Source::Off
    }

    /// Where the certificate comes from.
    pub fn source(&self) -> &Source {
        &self.0.source
    }

    pub fn config(&self) -> &TlsConfig {
        &self.0.config
    }

    /// Where an issued certificate and the CA account are kept.
    pub fn dir(&self) -> &std::path::Path {
        &self.0.dir
    }

    /// Tell the Worker to look again — time has passed, or something changed.
    pub fn wake(&self) {
        self.0.wake_up.owes(1);
    }

    pub(crate) fn wake_up(&self) -> &WakeUp {
        &self.0.wake_up
    }

    /// Answer every handshake from here on with `certificate`.
    pub fn serve(&self, certificate: Certificate) {
        self.health_mut(|health| health.not_after_ms = Some(certificate.not_after_ms));
        *self.0.served.write().expect("served") = Some(certificate);
    }

    /// What handshakes are being answered with.
    pub fn served(&self) -> Option<Certificate> {
        self.0.served.read().expect("served").clone()
    }

    /// The key authorization the CA is owed for an HTTP-01 `token`, while one
    /// is being checked.
    pub fn http_challenge(&self, token: &str) -> Option<String> {
        self.0.http01.lock().expect("http01").get(token).cloned()
    }

    /// Hold `key_authorization` out for the CA at `token`, until withdrawn.
    pub(crate) fn offer_http_challenge(&self, token: &str, key_authorization: String) {
        self.0
            .http01
            .lock()
            .expect("http01")
            .insert(token.to_string(), key_authorization);
    }

    /// Hold a TLS-ALPN-01 certificate out for the CA at `name`.
    pub(crate) fn offer_alpn_challenge(&self, name: &str, certificate: Arc<CertifiedKey>) {
        self.0
            .alpn01
            .lock()
            .expect("alpn01")
            .insert(name.to_string(), certificate);
    }

    /// Stop offering every challenge — the CA has finished looking.
    pub(crate) fn withdraw_challenges(&self) {
        self.0.http01.lock().expect("http01").clear();
        self.0.alpn01.lock().expect("alpn01").clear();
    }

    /// The port HTTPS bound.
    pub fn https_port(&self) -> u16 {
        self.0.https_port.load(Ordering::Relaxed)
    }

    pub(crate) fn bound(&self, port: u16) {
        self.0.https_port.store(port, Ordering::Relaxed);
    }

    /// Where a stranger is redirected to.
    pub fn public_url(&self) -> Option<&str> {
        self.0.public_url.as_deref()
    }

    /// How the certificate is doing.
    pub fn health(&self) -> Health {
        self.0.health.lock().expect("health").clone()
    }

    pub(crate) fn health_mut(&self, change: impl FnOnce(&mut Health)) {
        change(&mut self.0.health.lock().expect("health"));
    }

    /// The rustls configuration the HTTPS listener hands every handshake.
    ///
    /// The resolver is this handle, so a certificate installed by
    /// [`Tls::serve`] is used from the very next handshake with nothing
    /// restarted. HTTP/2 is offered beside HTTP/1.1 — `axum::serve` enables the
    /// extended CONNECT a WebSocket needs over it — and `acme-tls/1`, which
    /// only a CA validating TLS-ALPN-01 ever asks for.
    pub fn server_config(&self) -> Arc<rustls::ServerConfig> {
        let mut config = rustls::ServerConfig::builder_with_provider(provider())
            .with_safe_default_protocol_versions()
            .expect("the provider supports the default protocol versions")
            .with_no_client_auth()
            .with_cert_resolver(Arc::new(self.clone()));
        config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec(), ACME_TLS_ALPN.to_vec()];
        Arc::new(config)
    }
}

impl ResolvesServerCert for Tls {
    /// A CA validating TLS-ALPN-01 asks for `acme-tls/1` and is shown the
    /// challenge certificate for the name it asked about; everyone else is
    /// shown the certificate being served — whatever name they asked for, so
    /// a browser pointed at the bare IP gets a name mismatch it can explain
    /// rather than a handshake that simply fails.
    fn resolve(&self, hello: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
        let validating = hello
            .alpn()
            .is_some_and(|mut offered| offered.any(|protocol| protocol == ACME_TLS_ALPN));
        if validating {
            let name = hello.server_name()?.to_ascii_lowercase();
            return self.0.alpn01.lock().expect("alpn01").get(&name).cloned();
        }
        self.served().map(|certificate| certificate.key)
    }
}

/// Let's Encrypt's production ACME directory — where a certificate comes from
/// unless `[tls] directory` names another.
pub const LETS_ENCRYPT: &str = "https://acme-v02.api.letsencrypt.org/directory";

/// `[tls]` — built-in HTTPS, and where its certificate comes from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TlsConfig {
    /// Where HTTPS listens, once a [`Source`] is named. 443, because the port
    /// in a browser's address bar is the one a stranger types.
    pub port: u16,
    /// The names to ask an ACME CA for. **Non-empty switches ACME on.**
    pub domains: Vec<Domain>,
    /// The ACME directory to ask — Let's Encrypt's production one by default.
    /// Its staging directory is the same URL with `-staging`, for an Operator
    /// trying this out without spending the real rate limit.
    #[serde(deserialize_with = "https_directory")]
    pub directory: String,
    /// A root certificate (PEM) to trust for the directory's **own** HTTPS —
    /// a private CA such as `step-ca`, or Let's Encrypt's test CA Pebble.
    /// Unset, the directory is checked against the system's roots.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub directory_root: Option<PathBuf>,
    /// A contact address for the CA account. Optional: Let's Encrypt stopped
    /// sending expiry mail in 2025, and this Instance renews by itself.
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "email_address"
    )]
    pub email: Option<String>,
    /// An Operator's own certificate chain (PEM) — rdio's `ssl_cert_file`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cert_file: Option<PathBuf>,
    /// ...and its private key (PEM) — rdio's `ssl_key_file`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key_file: Option<PathBuf>,
}

impl Default for TlsConfig {
    fn default() -> Self {
        TlsConfig {
            port: 443,
            domains: Vec::new(),
            directory: LETS_ENCRYPT.to_string(),
            directory_root: None,
            email: None,
            cert_file: None,
            key_file: None,
        }
    }
}

/// Where this Instance's certificate comes from — the whole of what the
/// section decides.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// No built-in TLS: plain HTTP on `[server] port`, as ever.
    Off,
    /// Issued and renewed by an ACME CA, for these names.
    Acme(Vec<String>),
    /// An Operator's own chain and key, re-read when they change.
    Files { cert: PathBuf, key: PathBuf },
}

impl TlsConfig {
    /// What this section asks for. Assumes a section [`crate::config`] has
    /// already validated: half a pair of files reads as [`Source::Off`] here,
    /// because it never boots.
    pub fn source(&self) -> Source {
        if !self.domains.is_empty() {
            return Source::Acme(self.domains.iter().map(|d| d.0.clone()).collect());
        }
        match (&self.cert_file, &self.key_file) {
            (Some(cert), Some(key)) => Source::Files {
                cert: cert.clone(),
                key: key.clone(),
            },
            _ => Source::Off,
        }
    }
}

/// One name a certificate is asked for: lowercased, and refused here if no CA
/// could ever issue it over the challenges this module answers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Domain(String);

/// What a usable domain looks like. One string per way of being wrong, so the
/// Operator is told *which* rule they broke.
const EXPECTED_DOMAIN: &str = "a domain name such as scanner.example";
const EXPECTED_NO_WILDCARD: &str = "a domain name without a wildcard — a wildcard certificate needs a DNS challenge, which built-in TLS does not answer";
const EXPECTED_NOT_AN_ADDRESS: &str =
    "a domain name, not an IP address — point a name at this machine and use that";

impl FromStr for Domain {
    /// The expectation the value failed.
    type Err = &'static str;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let name = text.trim().to_ascii_lowercase();
        if name.contains('*') {
            return Err(EXPECTED_NO_WILDCARD);
        }
        if name.parse::<std::net::IpAddr>().is_ok() {
            return Err(EXPECTED_NOT_AN_ADDRESS);
        }
        // RFC 1035's preferred syntax, which is what a public CA will issue
        // for: dot-separated labels of letters, digits and inner hyphens, each
        // 1-63 long, 253 in all.
        let label_ok = |label: &str| {
            (1..=63).contains(&label.len())
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        };
        match name.len() <= 253 && name.split('.').all(label_ok) {
            true => Ok(Domain(name)),
            false => Err(EXPECTED_DOMAIN),
        }
    }
}

impl Serialize for Domain {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for Domain {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        checked(deserializer, "tls.domains", str::parse)
    }
}

impl std::fmt::Display for Domain {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// A string from the file, put through `check` — and refused **there**, with
/// the line and column the Operator has to edit, naming `key` and saying what
/// was expected. The `ProxyNet` precedent, in the one shape all three of this
/// section's checked fields share.
fn checked<'de, D, T>(
    deserializer: D,
    key: &str,
    check: impl FnOnce(&str) -> Result<T, &'static str>,
) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let text = String::deserialize(deserializer)?;
    check(&text).map_err(|expected| {
        serde::de::Error::custom(crate::config::rejected(key, format!("{text:?}"), expected))
    })
}

/// What `[tls] directory` has to be.
pub const EXPECTED_DIRECTORY: &str = "an https:// URL of an ACME directory";

/// An ACME directory URL, or why this one is not. RFC 8555 §6.1: the protocol
/// runs over HTTPS and nothing else, so a plain `http://` directory is a typo
/// rather than a choice.
pub fn checked_directory(text: &str) -> Result<String, &'static str> {
    let text = text.trim();
    match text.parse::<http::Uri>() {
        Ok(uri) if uri.scheme_str() == Some("https") && uri.host().is_some() => Ok(text.into()),
        _ => Err(EXPECTED_DIRECTORY),
    }
}

fn https_directory<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<String, D::Error> {
    checked(deserializer, "tls.directory", checked_directory)
}

/// What `[tls] email` has to be.
pub const EXPECTED_EMAIL: &str = "an email address such as you@example.com";

/// A contact address, or why this is not one. Deliberately loose — the CA is
/// the authority on what it will accept — but a value with no `@`, or with a
/// space in it, is a typo every CA refuses, and refusing it here saves a failed
/// account registration the Operator would only meet in the log.
pub fn checked_email(text: &str) -> Result<String, &'static str> {
    let text = text.trim();
    let usable = match text.split_once('@') {
        Some((user, host)) => {
            !user.is_empty()
                && host.contains('.')
                && !host.starts_with('.')
                && !host.ends_with('.')
                && !text.contains(char::is_whitespace)
                && !host.contains('@')
        }
        None => false,
    };
    match usable {
        true => Ok(text.into()),
        false => Err(EXPECTED_EMAIL),
    }
}

/// `String` rather than `Option<String>`, for `metrics::written_token`'s
/// reason: an absent key never reaches here.
fn email_address<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<String>, D::Error> {
    checked(deserializer, "tls.email", checked_email).map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `Debug` says which source is in force — the one thing worth knowing in
    /// a `{:?}` — and nothing about what is being served, which is a key.
    #[test]
    fn debugging_the_handle_names_its_source_and_nothing_else() {
        let files = Tls::new(
            TlsConfig {
                cert_file: Some("/c.pem".into()),
                key_file: Some("/k.pem".into()),
                ..TlsConfig::default()
            },
            PathBuf::new(),
            None,
        );

        let rendered = format!("{files:?}");

        assert!(rendered.contains("Files"), "{rendered}");
        assert!(rendered.contains("/c.pem"), "{rendered}");
        assert!(!rendered.contains("served"), "{rendered}");
        assert!(format!("{:?}", Tls::default()).contains("Off"));
    }
}
