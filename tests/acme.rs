//! **ACME against a CA that answers** (#76) — the release's one genuinely new
//! test seam (spec v2, "Testing").
//!
//! Autocert cannot be honestly tested through the harness alone: the half
//! worth proving is a CA reaching *back* into the Instance — over TLS-ALPN-01
//! on the HTTPS port, or HTTP-01 through the LAN door — and liking what it
//! finds. So these run against **Pebble**, Let's Encrypt's own test CA, with
//! its DNS stub answering every name with this machine. `TEST_ACME_DIRECTORY`
//! is the switch; unset — the everyday loop — every test here skips, saying so.
//! `docs/agents/acme.md` stands one up.
//!
//! Pebble validates on **fixed ports** — TLS-ALPN-01 on 5001, HTTP-01 on 5002
//! — so these tests cannot run beside each other, and `.config/nextest.toml`
//! runs this binary one test at a time. Each test proves one challenge by
//! making the other impossible: the port it would need is left ephemeral,
//! where Pebble cannot find it.

mod common;

use std::net::SocketAddr;
use std::time::Duration;

use common::TestApp;
use common::logs::LogCapture;
use common::tls::{NAME, served_certificate};
use radio_scout::Clock;

/// Where Pebble validates TLS-ALPN-01 (its `tlsPort`)...
const TLS_ALPN_PORT: u16 = 5001;
/// ...and HTTP-01 (its `httpPort`).
const HTTP_PORT: u16 = 5002;

/// The CA this run was handed — or `None`, having said why the test that
/// follows proves nothing.
///
/// The directory decides; the other two are **required** beside it, because a
/// half-configured run that quietly skipped would be a green run that
/// exercised nothing (`tests/common/s3.rs`'s rule).
struct Ca {
    directory: String,
    /// The root that signed Pebble's own HTTPS — what `[tls] directory_root`
    /// is for.
    directory_root: std::path::PathBuf,
    /// The root Pebble issues under, which it generates afresh on every start
    /// and serves from its management API.
    issued_root_pem: String,
}

async fn ca() -> Option<Ca> {
    let Ok(directory) = std::env::var("TEST_ACME_DIRECTORY") else {
        // Test-runner output, not application output: a skipped test has to
        // say so to whoever is reading the run, and no subscriber is installed.
        #[allow(clippy::print_stderr)]
        {
            eprintln!("skipping ACME test: TEST_ACME_DIRECTORY unset (see docs/agents/acme.md)");
        }
        return None;
    };
    let required = |var: &str| {
        std::env::var(var)
            .unwrap_or_else(|_| panic!("{var} must be set beside TEST_ACME_DIRECTORY"))
    };
    let directory_root = std::path::PathBuf::from(required("TEST_ACME_DIRECTORY_ROOT"));
    let roots = required("TEST_ACME_ISSUED_ROOTS");
    let pem = std::fs::read(&directory_root).expect("the directory's root");
    let issued_root_pem = reqwest::Client::builder()
        .tls_certs_only([reqwest::Certificate::from_pem(&pem).expect("a PEM root")])
        .build()
        .expect("a client")
        .get(&roots)
        .send()
        .await
        .expect("Pebble's management API")
        .text()
        .await
        .expect("the issuing root");
    Some(Ca {
        directory,
        directory_root,
        issued_root_pem,
    })
}

impl Ca {
    /// An Instance asking this CA for [`NAME`], on the ports given — `0` for a
    /// port Pebble must not be able to find.
    async fn instance(&self, plain: u16, https: u16, clock: Clock) -> TestApp {
        self.instance_for(NAME, plain, https, clock).await
    }

    /// ...for some other name.
    async fn instance_for(&self, name: &str, plain: u16, https: u16, clock: Clock) -> TestApp {
        let (directory, root) = (self.directory.clone(), self.directory_root.clone());
        let domain: radio_scout::tls::Domain = name.parse().expect("a domain");
        TestApp::builder()
            .clock(clock)
            .config(move |config| {
                config.server.port = plain;
                config.tls.port = https;
                config.tls.domains = vec![domain];
                config.tls.directory = directory;
                config.tls.directory_root = Some(root);
            })
            .spawn()
            .await
    }

    /// A browser that trusts what this CA issues, and nothing else, looking
    /// `name` up as `https`.
    fn client(&self, name: &str, https: SocketAddr) -> reqwest::Client {
        reqwest::Client::builder()
            .tls_certs_only([
                reqwest::Certificate::from_pem(self.issued_root_pem.as_bytes())
                    .expect("the issuing root"),
            ])
            .resolve(name, https)
            .tls_info(true)
            .build()
            .expect("an https client")
    }

    /// `GET /healthz` over HTTPS as [`NAME`], answering with the certificate
    /// that was shown.
    async fn served(&self, app: &TestApp) -> Vec<u8> {
        self.served_as(app, NAME).await
    }

    /// ...as `name`, which the certificate has to be for or a browser refuses.
    async fn served_as(&self, app: &TestApp, name: &str) -> Vec<u8> {
        let https = app.https_addr();
        let response = self
            .client(name, https)
            .get(format!("https://{name}:{}/healthz", https.port()))
            .send()
            .await
            .expect("an HTTPS request a browser would make");
        assert_eq!(response.status(), 200);
        served_certificate(&response)
    }
}

/// **The whole of ACME in one request**: a fresh Instance asks the CA, the CA
/// validates over **TLS-ALPN-01** on the HTTPS port — the plain port is out of
/// its reach — and a browser that trusts the CA is answered.
#[tokio::test]
async fn a_certificate_is_issued_over_tls_alpn_and_served() {
    let Some(ca) = ca().await else { return };
    let capture = LogCapture::start();
    let app = ca.instance(0, TLS_ALPN_PORT, Clock::system()).await;

    app.settle().await;

    ca.served(&app).await;
    // **The Operator is shown what they agreed to** — the terms URL the CA's
    // own directory names (Pebble's is a `data:` URL), logged once, when the
    // account that agreed is made.
    let agreed = capture.lines_containing("agreeing to the certificate authority's terms");
    assert_eq!(agreed.len(), 1, "{agreed:?}");
    assert!(agreed[0].contains("terms=data:text/plain"), "{}", agreed[0]);
    // ...and the status page knows what it got, and when it will renew it —
    // from the moment it was issued, not from the next look an hour on.
    app.login().await;
    let (_, status) = app.admin_get("/api/admin/status").await;
    assert_eq!(status["tls"]["source"], "acme", "{status}");
    assert_eq!(
        status["tls"]["domains"],
        serde_json::json!([NAME]),
        "{status}"
    );
    let (expires, renews) = (
        status["tls"]["notAfterMs"].as_i64().expect("an expiry"),
        status["tls"]["renewAtMs"].as_i64().expect("a renewal"),
    );
    assert!(
        radio_scout::now_ms() < renews && renews < expires,
        "renews at {renews}, expires at {expires}"
    );
}

/// **...and HTTP-01 when TLS-ALPN-01 cannot work** — an Operator who forwarded
/// port 80 and not 443, which rdio's `autocert` never managed. The CA reaches
/// the plain port, and the LAN door hands it the key authorization.
#[tokio::test]
async fn when_tls_alpn_cannot_reach_the_instance_http_01_does() {
    let Some(ca) = ca().await else { return };
    let app = ca.instance(HTTP_PORT, 0, Clock::system()).await;

    app.settle().await;

    ca.served(&app).await;
}

/// **A restart serves what was issued, and orders nothing** — the certificate
/// is on disk beside the database, `0600`, and is answered with the moment
/// the restarted Instance binds.
#[tokio::test]
async fn an_issued_certificate_survives_a_restart() {
    let Some(ca) = ca().await else { return };
    let capture = LogCapture::start();
    let mut app = ca.instance(0, TLS_ALPN_PORT, Clock::system()).await;
    app.settle().await;
    let issued = ca.served(&app).await;

    app.restart().await;

    // Before the restarted Worker has done anything at all...
    assert_eq!(ca.served(&app).await, issued);
    app.settle().await;
    // ...and after it has looked, and found nothing due.
    assert_eq!(ca.served(&app).await, issued);
    assert_eq!(
        capture.lines_containing("issued a certificate").len(),
        1,
        "a restart ordered another certificate"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let stored = app.path().join("tls").join("certificate.json");
        let mode = std::fs::metadata(stored)
            .expect("stored")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600, "the key's file is {:o}", mode & 0o777);
    }
}

/// **Renewal**: once the schedule says so, a new certificate replaces the old
/// one with nothing restarted — proven by moving the Instance's clock most of
/// a certificate's life forward, not by waiting for it.
#[tokio::test]
async fn a_certificate_is_renewed_when_it_falls_due() {
    let Some(ca) = ca().await else { return };
    let app = ca
        .instance(0, TLS_ALPN_PORT, Clock::frozen(radio_scout::now_ms()))
        .await;
    app.settle().await;
    let first = ca.served(&app).await;

    // Pebble issues for ninety days; eighty-nine is past any point the
    // schedule could have chosen, ARI's window or two-thirds of the life.
    app.advance(Duration::from_secs(89 * 24 * 60 * 60)).await;

    let renewed = ca.served(&app).await;
    assert_ne!(renewed, first, "the certificate was not renewed");
}

/// **A CA that cannot reach the Instance is an ERROR naming the domain** —
/// the line an Operator who forgot to forward a port needs — and the plain
/// port keeps serving while it waits to try again. **Not before it has
/// waited**: a CA counts failed validations, so being woken early (the clock
/// moved, anything at all) is not a reason to spend another.
#[tokio::test]
async fn a_failed_issuance_says_so_and_backs_off_before_trying_again() {
    let Some(ca) = ca().await else { return };
    let capture = LogCapture::start();
    // Neither port where Pebble looks: no challenge can succeed.
    let app = ca
        .instance(0, 0, Clock::frozen(radio_scout::now_ms()))
        .await;

    app.settle().await;

    let failed = capture.lines_containing("could not get a certificate");
    assert_eq!(failed.len(), 1, "{failed:?}");
    assert!(failed[0].contains("ERROR"), "{}", failed[0]);
    assert!(failed[0].contains(NAME), "{}", failed[0]);
    assert_eq!(app.get("/healthz").await.status(), 200);

    // A minute on: still inside the first fifteen-minute wait.
    app.advance(Duration::from_secs(60)).await;
    assert_eq!(
        capture
            .lines_containing("could not get a certificate")
            .len(),
        1
    );

    // Past it: one more try, and a longer wait after it.
    app.advance(Duration::from_secs(15 * 60)).await;
    let failed = capture.lines_containing("could not get a certificate");
    assert_eq!(failed.len(), 2, "{failed:?}");
    assert!(failed[1].contains("failures=2"), "{}", failed[1]);
    assert!(failed[1].contains("retry_in_secs=2700"), "{}", failed[1]);
}

/// **Between renewals, looking again orders nothing.** The Worker wakes to ask
/// the CA's renewal window and to re-check it — and a certificate that is not
/// due is left alone, every time.
#[tokio::test]
async fn looking_again_between_renewals_orders_nothing() {
    let Some(ca) = ca().await else { return };
    let capture = LogCapture::start();
    let app = ca
        .instance(0, TLS_ALPN_PORT, Clock::frozen(radio_scout::now_ms()))
        .await;
    app.settle().await;
    let issued = ca.served(&app).await;

    // Past the guard after an issuance, so the CA's window is asked for...
    app.advance(Duration::from_secs(2 * 60 * 60)).await;
    // ...and a minute later, while that answer is still believed.
    app.advance(Duration::from_secs(60)).await;

    assert_eq!(ca.served(&app).await, issued);
    assert_eq!(capture.lines_containing("issued a certificate").len(), 1);
}

/// **A CA that refuses the order is an ERROR in its own words** — here because
/// the name is on Pebble's blocklist, which is the shape of a CA policy refusal
/// an Operator could meet for real.
#[tokio::test]
async fn an_order_the_ca_refuses_says_so() {
    let Some(ca) = ca().await else { return };
    let capture = LogCapture::start();
    let app = ca
        .instance_for("blocked-domain.example", 0, TLS_ALPN_PORT, Clock::system())
        .await;

    app.settle().await;

    let failed = capture.lines_containing("could not get a certificate");
    assert_eq!(failed.len(), 1, "{failed:?}");
    assert!(failed[0].contains("refused the order"), "{}", failed[0]);
}

/// **A new name is a new certificate**, not the stored one for the old names —
/// which a browser would refuse, and which nothing would ever renew. The name
/// already proven is not proven again: the CA holds its authorization as valid
/// (Let's Encrypt keeps one for thirty days), and only the new name is
/// challenged.
#[tokio::test]
async fn a_name_added_gets_a_certificate_covering_it() {
    let Some(ca) = ca().await else { return };
    let mut app = ca.instance(0, TLS_ALPN_PORT, Clock::system()).await;
    app.settle().await;
    let old = ca.served(&app).await;

    app.restart_with(|config| {
        config
            .tls
            .domains
            .push("radio.test".parse().expect("a domain"))
    })
    .await;
    app.settle().await;

    let renamed = ca.served_as(&app, "radio.test").await;
    assert_ne!(renamed, old);
    assert_eq!(
        ca.served(&app).await,
        renamed,
        "one certificate for both names"
    );
}

/// **A renewal the CA will not take as a replacement is still a renewal.**
/// ARI's `replaces` is a courtesy — it spares the CA's rate limits — and a CA
/// that refuses it (one without ARI, or one that never issued the old
/// certificate to this account, which is what a lost account file looks like)
/// is asked again without it.
#[tokio::test]
async fn a_renewal_the_ca_will_not_call_a_replacement_still_renews() {
    let Some(ca) = ca().await else { return };
    let mut app = ca
        .instance(0, TLS_ALPN_PORT, Clock::frozen(radio_scout::now_ms()))
        .await;
    app.settle().await;
    let first = ca.served(&app).await;

    std::fs::remove_file(app.path().join("tls").join("account.json")).expect("lose the account");
    app.restart_after(Duration::from_secs(89 * 24 * 60 * 60))
        .await;
    app.settle().await;

    assert_ne!(
        ca.served(&app).await,
        first,
        "the certificate was not renewed"
    );
}
