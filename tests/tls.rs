//! **Built-in TLS** from an Operator's own certificate files (#76) — rdio's
//! `ssl_cert_file`/`ssl_key_file`, re-read when they change.
//!
//! Driven over real sockets: a client that trusts only the test's own CA
//! ([`common::tls::TestCa`]), so a handshake that succeeds is one the Instance
//! answered with the certificate on disk. ACME, which needs a CA that answers,
//! is `tests/acme.rs`'s; what is proven here is everything the two sources
//! share — the listener, the LAN door and the `Secure` cookie — plus the reload
//! that is the files' own.

mod common;

use std::time::Duration;

use common::tls::{NAME, TestCa, served_certificate};
use common::{ADMIN_PASSWORD, TestApp};
use radio_scout::Clock;

/// How often an Operator's files are looked at again — read from the module
/// rather than restated, so the test advances exactly as far as it must.
const RECHECK: Duration = radio_scout::tls::FILES_RECHECK;

/// The scrape token every app here is started with, so `/metrics` is served.
const METRICS_TOKEN: &str = "a-scrape-token";

/// An Instance serving `issued` from files in its own directory — the app's
/// own temp directory, so they go when it does.
async fn serving(ca: &TestCa, clock: Clock) -> (TestApp, common::tls::Issued) {
    let issued = ca.issue(&[NAME]);
    let (chain_pem, key_pem) = (issued.chain_pem.clone(), issued.key_pem.clone());
    let app = TestApp::builder()
        .clock(clock)
        .config(move |config| {
            let on_disk = common::tls::Issued {
                chain_pem,
                key_pem,
                leaf_der: Vec::new(),
            };
            let (cert, key) = on_disk.write_to(&config.server.base_dir);
            config.tls.cert_file = Some(cert);
            config.tls.key_file = Some(key);
            config.metrics.token = Some(METRICS_TOKEN.to_owned());
        })
        .spawn()
        .await;
    (app, issued)
}

/// `/metrics`, scraped.
async fn scrape(app: &TestApp) -> String {
    app.client()
        .get(app.url("/metrics"))
        .bearer_auth(METRICS_TOKEN)
        .send()
        .await
        .expect("a scrape")
        .text()
        .await
        .expect("an exposition")
}

/// The whole of the files source in one request: HTTPS answers, with the
/// certificate the Operator put on disk.
#[tokio::test]
async fn an_operators_own_certificate_is_served_over_https() {
    let ca = TestCa::new();
    let (app, issued) = serving(&ca, Clock::system()).await;

    let response = ca
        .client(app.https_addr())
        .get(app.https_url("/healthz"))
        .send()
        .await
        .expect("an HTTPS request");

    assert_eq!(response.status(), 200);
    assert_eq!(served_certificate(&response), issued.leaf_der);
    assert_eq!(response.text().await.expect("a body"), "ok");
}

/// **A renewed certificate is served without a restart** — which is what
/// certbot and `tailscale cert` do every few weeks, and what rdio needs a
/// restart to notice.
#[tokio::test]
async fn replaced_files_are_served_once_they_have_been_looked_at_again() {
    let ca = TestCa::new();
    let clock = Clock::frozen(1_700_000_000_000);
    let (app, first) = serving(&ca, clock).await;
    let client = ca.client(app.https_addr());

    let renewed = ca.issue(&[NAME]);
    let dir = app.config().tls.cert_file.clone().expect("a cert file");
    renewed.write_to(dir.parent().expect("its directory"));
    // Not yet: nothing has looked.
    let before = client
        .get(app.https_url("/healthz"))
        .send()
        .await
        .expect("before");
    assert_eq!(served_certificate(&before), first.leaf_der);

    app.advance(RECHECK).await;

    // A fresh connection, because a kept-alive one is still on the old
    // handshake — which is exactly what a browser mid-session would see.
    let after = ca
        .client(app.https_addr())
        .get(app.https_url("/healthz"))
        .send()
        .await
        .expect("after");
    assert_eq!(served_certificate(&after), renewed.leaf_der);
}

/// ...and a file caught half-written — certbot replacing it, an Operator
/// mid-`cp` — is not a reason to stop serving the certificate that was working.
#[tokio::test]
async fn a_broken_replacement_keeps_the_working_certificate() {
    let ca = TestCa::new();
    let clock = Clock::frozen(1_700_000_000_000);
    let (app, first) = serving(&ca, clock).await;
    let cert = app.config().tls.cert_file.clone().expect("a cert file");
    std::fs::write(&cert, "-----BEGIN CERTIFICATE-----\ntruncat").expect("break it");

    app.advance(RECHECK).await;

    let response = ca
        .client(app.https_addr())
        .get(app.https_url("/healthz"))
        .send()
        .await
        .expect("still serving");
    assert_eq!(served_certificate(&response), first.leaf_der);
}

/// **The admin cookie is `Secure` because the request arrived over TLS** —
/// the listener's own word, not a header anybody could send. Over the plain
/// port the same login gets a cookie without it, because a browser would
/// otherwise throw it away (ADR-0008).
#[tokio::test]
async fn a_session_opened_over_https_is_marked_secure() {
    let ca = TestCa::new();
    let (app, _) = serving(&ca, Clock::system()).await;

    let over_tls = ca
        .client(app.https_addr())
        .post(app.https_url("/api/admin/login"))
        .json(&serde_json::json!({ "password": ADMIN_PASSWORD }))
        .send()
        .await
        .expect("login over https");
    let over_plain = app.login_as(ADMIN_PASSWORD).await;

    let cookie = |response: &reqwest::Response| {
        response
            .headers()
            .get("set-cookie")
            .expect("a session cookie")
            .to_str()
            .expect("ascii")
            .to_owned()
    };
    assert_eq!(over_tls.status(), 200);
    assert!(
        cookie(&over_tls).contains("Secure"),
        "{}",
        cookie(&over_tls)
    );
    assert!(
        !cookie(&over_plain).contains("Secure"),
        "{}",
        cookie(&over_plain)
    );
}

/// **The LAN door**: with TLS on, a peer on this machine still gets the whole
/// app over plain HTTP — a Trunk Recorder on the same Pi posts exactly where
/// it always did.
#[tokio::test]
async fn the_plain_port_still_serves_this_machine() {
    let ca = TestCa::new();
    let (app, _) = serving(&ca, Clock::system()).await;

    let response = app.get("/healthz").await;

    assert_eq!(response.status(), 200);
    assert_eq!(response.text().await.expect("a body"), "ok");
}

/// ...and a challenge token this Instance never handed out is not there, to
/// anyone who asks.
#[tokio::test]
async fn an_unknown_challenge_is_not_found() {
    let ca = TestCa::new();
    let (app, _) = serving(&ca, Clock::system()).await;

    let response = app.get("/.well-known/acme-challenge/nobody-asked").await;

    assert_eq!(response.status(), 404);
}

/// **The live feed works over TLS** — the upgrade travels the same listener,
/// so a Listener on `https://` hears Calls exactly as one on `http://` does.
#[tokio::test]
async fn the_live_feed_upgrades_over_tls() {
    let ca = TestCa::new();
    let (app, _) = serving(&ca, Clock::system()).await;

    let greeting = common::tls::wss_greeting(&ca, app.https_addr(), "/api/live").await;

    assert_eq!(greeting["t"], "hello", "{greeting}");
}

/// A certificate source that cannot be read **refuses to boot, naming the
/// file** — an HTTPS port that answers no handshake is the worst way to find
/// out.
#[tokio::test]
async fn files_that_cannot_be_read_refuse_to_start_naming_the_path() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let mut config = radio_scout::config::Config::default();
    config.server.port = 0;
    config.tls.port = 0;
    config.server.base_dir = tmp.path().to_path_buf();
    config.tls.cert_file = Some(tmp.path().join("missing-fullchain.pem"));
    config.tls.key_file = Some(tmp.path().join("missing-privkey.pem"));
    if let Some(server) = common::postgres_server() {
        config.database.url = Some(common::create_test_database(&server).await);
    }

    let Err(error) =
        radio_scout::instance::start(config, radio_scout::instance::Wiring::default()).await
    else {
        panic!("a certificate that is not there must not boot");
    };

    assert!(
        error.to_string().contains("missing-fullchain.pem"),
        "{error}"
    );
}

/// **The status page says when the certificate expires, and what last went
/// wrong** (#76) — the month of warning a failing renewal gives is worth
/// nothing if nobody is shown it. The same reading goes to Prometheus, so an
/// alert can fire on it without anybody looking.
#[tokio::test]
async fn certificate_health_is_on_the_status_page_and_in_metrics() {
    let ca = TestCa::new();
    let clock = Clock::frozen(1_700_000_000_000);
    let (app, _) = serving(&ca, clock).await;
    app.login().await;

    let (_, healthy) = app.admin_get("/api/admin/status").await;
    assert_eq!(healthy["tls"]["source"], "files", "{healthy}");
    assert!(healthy["tls"]["notAfterMs"].is_i64(), "{healthy}");
    assert!(healthy["tls"].get("lastError").is_none(), "{healthy}");
    let text = scrape(&app).await;
    assert!(
        text.contains("radio_scout_tls_certificate_expiry_timestamp_seconds "),
        "{text}"
    );
    assert!(
        text.contains("radio_scout_tls_certificate_failing 0"),
        "{text}"
    );

    let cert = app.config().tls.cert_file.clone().expect("a cert file");
    std::fs::write(&cert, "not a certificate").expect("break it");
    app.advance(RECHECK).await;

    let (_, failing) = app.admin_get("/api/admin/status").await;
    let error = failing["tls"]["lastError"].as_str().expect("a last error");
    assert!(error.contains("fullchain.pem"), "names the file: {error}");
    assert!(failing["tls"]["lastErrorAtMs"].is_i64(), "{failing}");
    assert!(
        scrape(&app)
            .await
            .contains("radio_scout_tls_certificate_failing 1"),
        "the failure reaches Prometheus too"
    );
}

/// ...and an Instance with no built-in TLS has nothing to report — which is
/// every Instance behind a tunnel, and must not read as one whose certificate
/// expired in 1970.
#[tokio::test]
async fn an_instance_without_tls_reports_no_certificate() {
    let app = TestApp::spawn().await;
    app.login().await;

    let (_, status) = app.admin_get("/api/admin/status").await;

    assert!(status.get("tls").is_none(), "{status}");
}

/// **A client that does not speak TLS costs nobody else anything** — the
/// internet's port-443 scanners, mostly. Handshakes run beside the accept loop
/// rather than in it, so a failed one is that client's problem alone.
#[tokio::test]
async fn a_client_that_does_not_speak_tls_does_not_stop_the_next_one() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let ca = TestCa::new();
    let (app, _) = serving(&ca, Clock::system()).await;

    let mut stranger = tokio::net::TcpStream::connect(app.https_addr())
        .await
        .expect("connect");
    stranger
        .write_all(b"GET / HTTP/1.1\r\nHost: scanner.test\r\n\r\n")
        .await
        .expect("write");
    let mut answer = Vec::new();
    let _ = stranger.read_to_end(&mut answer).await;

    let response = ca
        .client(app.https_addr())
        .get(app.https_url("/healthz"))
        .send()
        .await
        .expect("the next client is served");
    assert_eq!(response.status(), 200);
}

/// ...and a certificate file that has **vanished** — or that certbot left
/// readable by root alone — is the same: the certificate already being served
/// stays. But it is **said**, once, on the page and in the log: a file that is
/// gone for good would otherwise be found out only when the certificate it was
/// renewing expired.
#[tokio::test]
async fn a_vanished_file_keeps_the_working_certificate_and_says_so_once() {
    let ca = TestCa::new();
    let clock = Clock::frozen(1_700_000_000_000);
    let (app, first) = serving(&ca, clock).await;
    app.login().await;
    let logs = common::logs::LogCapture::start();
    let key = app.config().tls.key_file.clone().expect("a key file");
    std::fs::remove_file(&key).expect("gone");

    app.advance(RECHECK).await;
    app.advance(RECHECK).await;

    let response = ca
        .client(app.https_addr())
        .get(app.https_url("/healthz"))
        .send()
        .await
        .expect("still serving");
    assert_eq!(served_certificate(&response), first.leaf_der);
    let said = logs.lines_containing("the certificate files cannot be served");
    assert_eq!(said.len(), 1, "once, not every minute: {said:?}");
    assert!(said[0].contains("privkey.pem"), "{}", said[0]);
    let (_, status) = app.admin_get("/api/admin/status").await;
    let error = status["tls"]["lastError"].as_str().expect("a last error");
    assert!(error.contains("privkey.pem"), "{error}");
}

/// **An Instance stops even while its CA is silent** — a SIGTERM, a restart or
/// an upgrade must not wait out an issuance, and a CA that accepts a connection
/// and never answers would otherwise hold the Worker, and so the stop, for as
/// long as it liked.
///
/// Against `instance::start` rather than `TestApp`, which settles every
/// Worker's first pass before it hands an app over — the very pass this CA
/// never lets finish.
#[tokio::test]
async fn an_instance_stops_while_its_ca_is_not_answering() {
    let ca = TestCa::new();
    let silent = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("a CA that never answers");
    let port = silent.local_addr().expect("its port").port();
    let (asked, was_asked) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let (held, _) = silent.accept().await.expect("the Instance calls");
        let _ = asked.send(());
        // Hold the connection open and say nothing, for as long as the test
        // lasts.
        std::future::pending::<()>().await;
        drop(held);
    });
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().join("root.pem");
    std::fs::write(&root, ca.root_pem()).expect("the CA's root");
    let mut config = radio_scout::config::Config::default();
    config.server.port = 0;
    config.tls.port = 0;
    config.server.base_dir = tmp.path().to_path_buf();
    config.tls.domains = vec![NAME.parse().expect("a domain")];
    config.tls.directory = format!("https://127.0.0.1:{port}/dir");
    config.tls.directory_root = Some(root);
    if let Some(server) = common::postgres_server() {
        config.database.url = Some(common::create_test_database(&server).await);
    }
    let mut instance = radio_scout::instance::start(
        config,
        radio_scout::instance::Wiring::default().bind(std::net::IpAddr::from([127, 0, 0, 1])),
    )
    .await
    .expect("start");
    was_asked.await.expect("the Worker is mid-issuance");

    let stopped = tokio::time::timeout(Duration::from_secs(10), instance.stop()).await;

    assert!(stopped.is_ok(), "the stop waited on the CA");
}

/// A private CA's root that cannot be read **refuses the boot, naming it** —
/// rather than a boot that looks fine and an issuance that fails minutes later.
#[tokio::test]
async fn a_ca_root_that_cannot_be_read_refuses_to_start_naming_it() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let mut config = radio_scout::config::Config::default();
    config.server.port = 0;
    config.tls.port = 0;
    config.server.base_dir = tmp.path().to_path_buf();
    config.tls.domains = vec![NAME.parse().expect("a domain")];
    config.tls.directory_root = Some(tmp.path().join("missing-root.pem"));
    if let Some(server) = common::postgres_server() {
        config.database.url = Some(common::create_test_database(&server).await);
    }

    let Err(error) =
        radio_scout::instance::start(config, radio_scout::instance::Wiring::default()).await
    else {
        panic!("a root that is not there must not boot");
    };

    assert!(error.to_string().contains("missing-root.pem"), "{error}");
}
