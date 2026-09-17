//! **Is this Instance healthy** (#70, spec US 48–49) — the status document an
//! Operator reads and the Prometheus text a scraper reads.
//!
//! Driven over the real HTTP boundary (ADR-0009), because both halves of this
//! ticket are about what a *stranger* gets: one surface must refuse everybody
//! without a session, the other must refuse everybody without the token, and
//! neither may say anything that names a person.

mod common;
use common::{CallUpload, TestApp};

use serde_json::Value;

/// The token a scraper on these tests presents. Long enough to look like one an
/// Operator would generate, and never asserted against a log line except to
/// prove it is *absent* from one.
const TOKEN: &str = "0f3c8a91d2b74e6f8c1a5d2e7b9043af";

/// An Instance publishing metrics, which is an Instance with a token and
/// nothing else — the token is the switch (#70).
async fn publishing() -> TestApp {
    TestApp::builder()
        .config(|config| config.metrics.token = Some(TOKEN.to_owned()))
        .spawn()
        .await
}

/// `GET /metrics` with a bearer token.
async fn scrape(app: &TestApp, token: &str) -> (u16, String) {
    let response = app
        .client()
        .get(app.url("/metrics"))
        .header(reqwest::header::AUTHORIZATION, format!("Bearer {token}"))
        .send()
        .await
        .expect("scrape");
    let status = response.status().as_u16();
    (status, response.text().await.expect("body"))
}

/// The status document, as the Operator's browser reads it.
async fn status(app: &TestApp) -> Value {
    app.login().await;
    app.get_json("/api/admin/status").await
}

/// **The token is the switch, so an Instance without one has no endpoint.**
///
/// A `404` rather than a `401`, because there is nothing to authenticate to —
/// and the route is *registered* either way, which is the half that matters:
/// leaving it out would let the SPA fallback answer `/metrics` with the app's
/// own HTML and a `200`, and a scraper would ingest that as a parse failure
/// forever rather than as a thing an Operator can find.
#[tokio::test]
async fn an_instance_with_no_token_publishes_nothing() {
    let app = TestApp::spawn().await;

    let response = app.get("/metrics").await;

    assert_eq!(response.status().as_u16(), 404);
    let body = response.text().await.expect("body");
    assert!(body.contains("metrics are not enabled"), "{body:?}");
    assert!(!body.contains("<!doctype"), "the SPA answered: {body:?}");
}

/// A scrape has to present the token, and presenting the wrong one is a **WARN**
/// an Operator can find — because a scraper that has stopped seeing this
/// Instance is exactly the silence they would otherwise have to guess at.
///
/// And the token itself never reaches the log, presented or stored (ADR-0011
/// rule 2): what the line names is the address that knocked.
#[tokio::test]
async fn a_scrape_must_present_the_token_and_the_token_is_never_written_down() {
    let app = publishing().await;
    let _logs = app.store_logs();

    assert_eq!(app.get("/metrics").await.status().as_u16(), 401);
    let (status, body) = scrape(&app, "not-the-token").await;
    assert_eq!(status, 401);
    assert!(body.contains("invalid metrics token"), "{body:?}");

    let page = app.await_logged("request refused").await;
    let events = page["results"].as_array().expect("results");
    let refusals: Vec<&Value> = events
        .iter()
        .filter(|event| event["fields"]["reason"] == "invalid-metrics-token")
        .collect();
    assert_eq!(refusals.len(), 2, "both attempts: {page:#}");
    assert!(refusals.iter().all(|event| event["level"] == "WARN"));
    let whole = page.to_string();
    assert!(!whole.contains(TOKEN), "the configured token was logged");
    assert!(!whole.contains("not-the-token"), "the presented one was");
}

/// The happy path: the exposition, in the content type a `curl` renders as text
/// and Prometheus accepts.
#[tokio::test]
async fn a_scrape_with_the_token_reads_the_exposition() {
    let app = publishing().await;

    let response = app
        .client()
        .get(app.url("/metrics"))
        .header(reqwest::header::AUTHORIZATION, format!("Bearer {TOKEN}"))
        .send()
        .await
        .expect("scrape");

    assert_eq!(response.status().as_u16(), 200);
    assert_eq!(
        common::header_of(&response, "content-type"),
        Some("text/plain; version=0.0.4; charset=utf-8")
    );
    let body = response.text().await.expect("body");
    for family in [
        "radio_scout_build_info",
        "radio_scout_uptime_seconds",
        "radio_scout_ingest_total",
        "radio_scout_worker_in_hand",
        "radio_scout_listeners",
        "radio_scout_audio_bytes",
    ] {
        assert!(body.contains(family), "no {family}:\n{body}");
    }
}

/// **The status page is the Operator's**, like the listener chart and for its
/// reason: how an Instance is doing is not a fact a Listener gets to publish on
/// their behalf.
#[tokio::test]
async fn the_status_page_is_behind_the_admin_session() {
    let app = TestApp::spawn().await;

    assert_eq!(app.get("/api/admin/status").await.status().as_u16(), 401);
}

/// **What arrives is counted by what Ingest decided about it**, in the same
/// closed vocabulary the refusal is logged under — so an Operator's grep and a
/// dashboard's label are the same word.
///
/// Every outcome is present from boot, which is the seeding: a fresh Instance
/// reads `0` for the ones that have not happened rather than "no data", and a
/// dashboard drawn against it is honest on day one.
#[tokio::test]
async fn what_arrives_is_counted_by_what_ingest_decided() {
    let app = TestApp::with_key("k").await;

    let before = status(&app).await;
    for outcome in ["stored", "duplicate", "blacklisted", "invalid-api-key"] {
        assert_eq!(before["ingest"][outcome], 0, "{before:#}");
    }

    app.upload_ok(CallUpload::new().key("k")).await;
    // The identical transmission again, inside the dedup window.
    app.upload(CallUpload::new().key("k")).await;
    app.upload(CallUpload::new().key("wrong")).await;

    let after = status(&app).await;
    assert_eq!(after["ingest"]["stored"], 1, "{after:#}");
    assert_eq!(after["ingest"]["duplicate"], 1, "{after:#}");
    assert_eq!(after["ingest"]["invalid-api-key"], 1, "{after:#}");
    // ...and the refusals are counted again under the *same* slugs on the
    // surface that counts every refusal there is, wherever it happened.
    assert_eq!(after["refused"]["duplicate"], 1, "{after:#}");
    assert_eq!(after["refused"]["invalid-api-key"], 1, "{after:#}");
}

/// **A refusal anywhere is counted**, not only an upload's — which is what the
/// request middleware buys: the counting sees every response, so a route added
/// later is covered without anybody remembering. A `GET` for a Call that is not
/// there never touches ingest and still lands under its own reason.
#[tokio::test]
async fn a_refusal_on_any_surface_is_counted_under_its_reason() {
    let app = TestApp::spawn().await;

    assert_eq!(app.get("/api/call/999999").await.status().as_u16(), 404);
    assert_eq!(
        app.get("/api/calls?after=nope").await.status().as_u16(),
        400
    );

    let status = status(&app).await;
    assert_eq!(status["refused"]["call-not-found"], 1, "{status:#}");
    assert_eq!(status["refused"]["bad-query"], 1, "{status:#}");
}

/// **A 500 is counted by the stage the server was at**, which is the other
/// vocabulary #92 closed — and the one an Operator acts on. `stage=search-calls`
/// here is the same word the ERROR line beside it carries, so a number on the
/// page and a grep in the log find the same thing.
#[tokio::test]
async fn a_server_error_is_counted_by_the_stage_it_happened_at() {
    let app = TestApp::spawn().await;
    app.login().await;
    // The operator log, and deliberately not the Archive: this document reads
    // `calls` itself, so breaking that table would break the page the assertion
    // is made from as well as the request it is about.
    app.refuse_statements_on("logs");

    assert_eq!(app.get("/api/admin/logs").await.status().as_u16(), 500);

    let status = status(&app).await;
    assert_eq!(status["errors"]["search-logs"], 1, "{status:#}");
}

/// **One aggregation, two renderings.** A second read behind `/metrics` would be
/// a second implementation of the thing it claims to describe, and the two would
/// agree right up until one of them was changed.
#[tokio::test]
async fn the_exposition_and_the_document_say_the_same_thing() {
    let app = publishing().await;
    app.create_api_key("k").await;
    app.upload_ok(CallUpload::new()).await;

    let document = status(&app).await;
    let (_, text) = scrape(&app, TOKEN).await;

    assert!(
        text.lines()
            .any(|line| line == r#"radio_scout_ingest_total{outcome="stored"} 1"#),
        "{text}"
    );
    assert_eq!(document["ingest"]["stored"], 1, "{document:#}");
    let calls = document["archive"]["calls"].as_i64().expect("a call count");
    assert!(
        text.lines()
            .any(|line| line == format!("radio_scout_calls {calls}")),
        "the exposition says a different number of calls:\n{text}"
    );
}

/// **Nothing here can name anybody** (ADR-0011 rule 5), which is the criterion
/// this ticket states in so many words.
///
/// Asserted as the absence of the two shapes that would do it: an address, and a
/// per-**Talkgroup** breakdown. The Talkgroup half is the one that looks
/// harmless — on a quiet channel "PD Tac took 3 calls at 03:00" is a record of
/// who was on it, which is exactly why #62 gave `listener_samples` three
/// columns.
#[tokio::test]
async fn neither_surface_says_anything_that_names_anybody() {
    let app = publishing().await;
    app.create_api_key("k").await;
    app.upload_ok(CallUpload::new()).await;
    // A refused unlock is the one thing here that *does* record an address —
    // into the log, under rule 5's authentication exemption. It must not reach
    // either of these surfaces.
    app.post_bytes(
        "/api/unlock",
        "application/json",
        br#"{"code":"nope"}"#.to_vec(),
    )
    .await;

    let document = status(&app).await.to_string();
    let (_, text) = scrape(&app, TOKEN).await;

    for surface in [&document, &text] {
        assert!(!surface.contains("127.0.0.1"), "an address: {surface}");
        assert!(!surface.contains("talkgroup"), "a channel: {surface}");
        assert!(!surface.contains("client_addr"), "{surface}");
    }
}

/// **Every Worker is on the page, by name and by health** — and the health is
/// the half a depth cannot give: a Worker that panicked settles everything it
/// was holding on the way out, so it reads as perfectly idle.
#[tokio::test]
async fn every_worker_is_on_the_page_with_its_depth_and_its_liveness() {
    let app = TestApp::spawn().await;

    let status = status(&app).await;

    let workers = status["workers"].as_array().expect("workers");
    let names: Vec<&str> = workers
        .iter()
        .map(|worker| worker["name"].as_str().expect("a name"))
        .collect();
    assert!(names.contains(&"retention"), "{names:?}");
    assert!(names.contains(&"listeners"), "{names:?}");
    assert!(
        workers.iter().all(|worker| worker["running"] == true),
        "a freshly booted Instance has all of its Workers: {status:#}"
    );
}

/// **Storage headroom is the disk's, not only the policy's.** `[retention]
/// max_size_gb` is absent on nearly every Instance, so a policy-only answer says
/// "no cap" to an Operator whose SD card is filling — which is true and is not
/// the question they came to ask.
#[tokio::test]
async fn a_filesystem_instance_reports_the_room_it_has_left() {
    let app = TestApp::spawn().await;

    let status = status(&app).await;

    let free = status["storage"]["freeBytes"]
        .as_u64()
        .unwrap_or_else(|| panic!("no free-space reading: {status:#}"));
    let total = status["storage"]["totalBytes"].as_u64().expect("a volume");
    assert!(free > 0 && total >= free, "{status:#}");
    // And the policy beside it, so "bounded by a cap" and "bounded by the disk"
    // are both answerable from one document.
    assert!(status["retention"]["days"].is_number(), "{status:#}");
    assert_eq!(
        status["retention"]["maxSizeBytes"],
        Value::Null,
        "an instance with no cap must not claim one: {status:#}"
    );
}

/// **A System is on the page whether or not it has keyed**, with the rate in the
/// window and the last Call over the whole Archive.
///
/// The unbounded last-Call is the point: a receiver that stopped two days ago is
/// precisely what a status page exists to show, and a windowed answer would draw
/// it as silence indistinguishable from a System that has never keyed.
#[tokio::test]
async fn every_system_says_when_it_was_last_heard_from() {
    let app = TestApp::with_key("k").await;
    // Stamped *now*, because the rate window is measured on the transmission
    // instant the recorder supplied — the same column `crate::catalog`'s
    // activity counts on — and `CallUpload`'s default timestamp is 1970.
    app.upload_ok(
        CallUpload::new()
            .key("k")
            .system(11)
            .at(radio_scout::now_ms()),
    )
    .await;

    let status = status(&app).await;

    let systems = status["systems"].as_array().expect("systems");
    let system = systems
        .iter()
        .find(|system| system["ref"] == 11)
        .unwrap_or_else(|| panic!("no system 11: {status:#}"));
    assert_eq!(system["calls"], 1, "{status:#}");
    assert!(system["lastCallAtMs"].as_i64().expect("an instant") > 0);
    assert!(status["rateWindowMs"].as_i64().expect("a window") > 0);
}

/// **The database is read once however often the page is asked.** The expensive
/// half of this document is a `SUM` over every Call there is, and an Operator
/// holding the page open beside a Grafana scraping every second must not run
/// that continuously on a Pi.
///
/// Asserted as a *difference* between the first ask and the second rather than
/// as a pinned number, so it survives every statement this document gains later
/// — `tests/ingest.rs`'s own rule.
#[tokio::test]
async fn the_database_is_read_once_however_often_the_page_is_asked() {
    let app = publishing().await;
    app.login().await;
    // Every Worker first, so the boot sweep's own statements are behind us: the
    // counter is the whole Instance's, and a sweeper waking between the two
    // samples below would be read as a second gauge read.
    app.settle().await;

    let before = app.statements_issued();
    app.get_json("/api/admin/status").await;
    let first = app.statements_issued() - before;

    let between = app.statements_issued();
    app.get_json("/api/admin/status").await;
    scrape(&app, TOKEN).await;
    let again = app.statements_issued() - between;

    assert!(first > 0, "the first ask read nothing at all");
    assert_eq!(
        again, 0,
        "two more asks inside the window cost {again} statements"
    );
}
