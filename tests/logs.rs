//! The operator log surface (#30, ADR-0011): the `logs` table, the admin
//! endpoint that reads it, and the rules about what may be in it.
//!
//! Asserted over the real HTTP boundary like everything else — an operator's
//! view of this feature is a page behind a login, so that is where it is
//! judged.

mod common;

use common::logs::LogCapture;
use common::{ADMIN_PASSWORD, CallUpload, TestApp, request_id_of};
use radio_scout::db::repo::{NewCall, NewLogEvent};
use rstest::rstest;

/// A stored event, as the sink would have written one.
fn event(at_ms: i64, level: &str, message: &str) -> NewLogEvent {
    NewLogEvent {
        at_ms,
        level: level.to_owned(),
        target: "radio_scout::ingest".to_owned(),
        message: message.to_owned(),
        ..Default::default()
    }
}

/// A round instant to date the fixtures from.
const NOW: i64 = 1_800_000_000_000;

/// One minute, in milliseconds.
const MINUTE: i64 = 60_000;

/// The messages a page carries, in the order it carries them.
fn messages(page: &serde_json::Value) -> Vec<String> {
    page["results"]
        .as_array()
        .expect("a results array")
        .iter()
        .map(|event| event["message"].as_str().expect("a message").to_owned())
        .collect()
}

/// Seed three events a minute apart, oldest first.
async fn app_with_three_events() -> TestApp {
    let app = TestApp::spawn().await;
    app.seed_log(event(NOW - 2 * MINUTE, "INFO", "oldest"))
        .await;
    app.seed_log(event(NOW - MINUTE, "WARN", "middle")).await;
    app.seed_log(event(NOW, "ERROR", "newest")).await;
    app
}

/// The Logs view is configuration, so it is behind the admin session like
/// everything else under `/api/admin/` (#19) — a public instance's log is not
/// public. The guard is the prefix layer, so this route is protected by
/// construction rather than by remembering to check.
#[tokio::test]
async fn reading_the_log_needs_an_admin_session() {
    let app = app_with_three_events().await;

    let response = app.get("/api/admin/logs").await;

    assert_eq!(response.status(), 401);
}

/// Newest first, because the question an operator opens this page with is
/// "what just happened?".
#[tokio::test]
async fn events_are_listed_newest_first() {
    let app = app_with_three_events().await;
    app.login().await;

    let page = app.get_json("/api/admin/logs").await;

    assert_eq!(messages(&page), ["newest", "middle", "oldest"]);
    assert_eq!(page["count"], 3);
    assert_eq!(page["hasMore"], false);
}

/// An event arrives as its parts, not as a rendered line: the client formats,
/// filters and searches, and a sentence can do none of those.
#[tokio::test]
async fn an_event_carries_its_parts() {
    let app = TestApp::spawn().await;
    app.seed_log(NewLogEvent {
        at_ms: NOW,
        level: "WARN".into(),
        target: "radio_scout::ingest".into(),
        message: "request refused".into(),
        fields: Some(r#"{"reason":"duplicate"}"#.into()),
        request_id: Some("0123456789abcdef".into()),
    })
    .await;
    app.login().await;

    let page = app.get_json("/api/admin/logs").await;
    let event = &page["results"][0];

    assert_eq!(event["atMs"], NOW);
    assert_eq!(event["level"], "WARN");
    assert_eq!(event["target"], "radio_scout::ingest");
    assert_eq!(event["message"], "request refused");
    // The fields arrive as an object, not as the text they are stored as — the
    // client shows `reason=duplicate`, and a string would make it parse JSON to
    // do it.
    assert_eq!(event["fields"]["reason"], "duplicate");
    // #29's correlation id: the `internal error (request id: …)` a listener reads out
    // is findable here, which is what the ref is *for* when there is no shell.
    assert_eq!(event["requestId"], "0123456789abcdef");
}

/// `level` is a **floor**, not an exact match: an operator chasing a problem
/// asks for "warnings and worse", never for "warnings but not errors".
#[tokio::test]
async fn the_level_filter_is_a_severity_floor() {
    let app = app_with_three_events().await;
    app.login().await;

    let page = app.get_json("/api/admin/logs?level=warn").await;

    assert_eq!(messages(&page), ["newest", "middle"]);
    assert_eq!(page["count"], 2, "the count is of matches, not of rows");
}

/// The other half of finding anything in a month of logs: when.
#[tokio::test]
async fn events_can_be_narrowed_to_a_date_range() {
    let app = app_with_three_events().await;
    app.login().await;

    let after = NOW - MINUTE - 1;
    let before = NOW - 1;
    let page = app
        .get_json(&format!("/api/admin/logs?after={after}&before={before}"))
        .await;

    assert_eq!(messages(&page), ["middle"]);
}

/// An RFC3339 boundary works too, so an operator can hand-write a query
/// without converting a date to milliseconds first — the archive search's own
/// contract (#13).
#[tokio::test]
async fn a_date_range_may_be_written_as_a_timestamp() {
    let app = TestApp::spawn().await;
    // 2020-01-01T00:00:00Z and a day later.
    app.seed_log(event(1_577_836_800_000, "INFO", "on new year"))
        .await;
    app.seed_log(event(1_577_923_200_000, "INFO", "the day after"))
        .await;
    app.login().await;

    let page = app
        .get_json("/api/admin/logs?before=2020-01-01T12:00:00Z")
        .await;

    assert_eq!(messages(&page), ["on new year"]);
}

/// A month of logs is far more rows than a page, so the view pages — and says
/// whether there is more, rather than making the client work it out.
#[tokio::test]
async fn a_long_log_is_paginated() {
    let app = app_with_three_events().await;
    app.login().await;

    let first = app.get_json("/api/admin/logs?limit=2").await;
    assert_eq!(messages(&first), ["newest", "middle"]);
    assert_eq!(first["hasMore"], true);
    assert_eq!(first["count"], 3);
    assert_eq!(first["limit"], 2);

    let second = app.get_json("/api/admin/logs?limit=2&offset=2").await;
    assert_eq!(messages(&second), ["oldest"]);
    assert_eq!(second["hasMore"], false);
    assert_eq!(second["offset"], 2);
}

/// Bad input is named, not silently coerced — the archive search's rule (#13),
/// and the opposite of rdio-scanner, which takes what it can parse and ignores
/// the rest so a typo quietly returns the wrong thing.
#[rstest]
#[case::level("level=chatty", "level")]
#[case::after("after=yesterday", "after")]
#[case::before("before=soon", "before")]
#[case::limit("limit=lots", "limit")]
#[case::offset("offset=-1", "offset")]
#[tokio::test]
async fn a_malformed_filter_says_which_parameter_was_wrong(
    #[case] query: &str,
    #[case] parameter: &str,
) {
    let app = TestApp::spawn().await;
    app.login().await;

    let response = app.get(&format!("/api/admin/logs?{query}")).await;

    assert_eq!(response.status(), 400);
    let body = response.text().await.expect("a body");
    assert!(body.contains(parameter), "{query} -> {body}");
}

/// A session that has been logged out cannot read the log either — the guard
/// is the session's, not the cookie's shape.
#[tokio::test]
async fn a_logged_out_session_can_no_longer_read_the_log() {
    let app = app_with_three_events().await;
    app.login().await;
    app.post_admin("/api/admin/logout").await;

    let response = app
        .client()
        .get(app.url("/api/admin/logs"))
        .header("cookie", app.session_cookie())
        .send()
        .await
        .expect("GET");

    assert_eq!(response.status(), 401);
}

/// The whole loop, end to end: a real upload over real HTTP leaves an event an
/// operator can read back through the browser. This is the feature — the rest
/// of this file is its parts.
#[tokio::test]
async fn a_real_request_becomes_a_readable_event() {
    let app = TestApp::with_key("k").await;
    let _sink = app.store_logs();

    app.upload_ok(CallUpload::new().key("k")).await;
    let page = app.await_logged("request").await;

    // By path, not by message: reading the page is itself an admin request, so
    // "the newest line called `request`" is whichever one this test just made.
    let request_line = page["results"]
        .as_array()
        .expect("results")
        .iter()
        .find(|event| event["fields"]["path"] == "/api/call-upload")
        .unwrap_or_else(|| panic!("the upload's request line should be readable in: {page:#}"));

    assert_eq!(request_line["message"], "request");
    assert_eq!(request_line["level"], "INFO");
    assert_eq!(request_line["fields"]["status"], 200);
    assert!(
        request_line["requestId"].is_string(),
        "a stored request line carries its correlation id: {request_line}"
    );
    // ...and ingest's own line, the one an operator is actually looking for.
    let stored = page["results"]
        .as_array()
        .expect("results")
        .iter()
        .any(|event| event["message"] == "call stored");
    assert!(stored, "ingest's own line should be stored too: {page:#}");
}

/// **ADR-0011 rule 5, on the sink.** A listener's address rides only lines that
/// are already DEBUG, and the sink cannot be configured that low — so an
/// instance that accumulates months of logs never accumulates a record of who
/// listened. rdio-scanner logs every listener's IP and access-code ident at
/// INFO, and stores it.
#[tokio::test]
async fn a_listener_leaves_no_address_in_the_stored_log() {
    let app = TestApp::builder()
        .trusted_proxies("127.0.0.1")
        .spawn()
        .await;
    let console = app.store_logs();
    let listener = "203.0.113.42";

    // A Call whose audio really plays, so the request that fetches it is a
    // *successful* chatty one — which is the only shape that logs a listener's
    // address at all (a 404 escalates to WARN, where rule 5 already drops it).
    let id = app
        .seed_call(NewCall {
            system_ref: 11,
            talkgroup_ref: 54241,
            call_at_ms: 1000,
            object_key: "aa/1.wav".into(),
            ..Default::default()
        })
        .await;
    app.put_object("aa/1.wav", b"audio-bytes").await;

    for path in [
        "/",
        "/api/catalog",
        "/api/calls",
        &format!("/api/call/{id}/audio"),
    ] {
        let response = app.get_via_proxy(path, listener).await;
        assert!(response.status().is_success(), "{path}");
    }
    app.await_logged("request").await;

    // The console *did* record the address — otherwise this test would pass by
    // proving nothing, which is exactly how a rule-5 test rots.
    assert!(
        console.text().contains(listener),
        "the DEBUG request line should carry the address (rule 5 permits it there):\n{}",
        console.text()
    );

    let page = app.get_json("/api/admin/logs?limit=500").await;
    assert!(
        !page.to_string().contains(listener),
        "...but a listener's address must never be *stored*: {page}"
    );
}

/// **The other side of rule 5, and the reason the floor is a floor rather than
/// a ban.** The two addresses ADR-0011 *permits* are stored, deliberately:
/// "which host stopped uploading" and "somebody is guessing my password, from
/// here" are the two questions an operator with no shell most needs answered,
/// and neither is a record of who listened. A ban would have taken both.
#[rstest]
// A recorder, on an ingest line at INFO (rule 5's standing exemption).
#[case::a_recorder("198.51.100.7", true)]
// ...and a refused admin login's source at WARN (#19's amendment to it).
#[case::a_guesser("198.51.100.8", false)]
#[tokio::test]
async fn the_addresses_rule_5_permits_are_the_ones_stored(
    #[case] address: &str,
    #[case] uploads: bool,
) {
    let app = TestApp::builder()
        .trusted_proxies("127.0.0.1")
        .spawn()
        .await;
    app.create_api_key("k").await;
    let _sink = app.store_logs();

    if uploads {
        let (status, _) = app
            .upload_via_proxy(CallUpload::new().key("k"), address)
            .await;
        assert_eq!(status, 200);
    } else {
        let refused = app
            .login_request("not-the-password")
            .header("x-forwarded-for", address)
            .send()
            .await
            .expect("login");
        assert_eq!(refused.status(), 401);
    }
    app.await_logged(if uploads {
        "call stored"
    } else {
        "request refused"
    })
    .await;

    let page = app.get_json("/api/admin/logs?limit=500").await;
    assert!(
        page.to_string().contains(address),
        "an operator cannot firewall an address they cannot see: {page}"
    );
}

/// **The property a request depends on.** A sink that cannot write must not be
/// noticeable from outside: the upload still succeeds, the recorder still gets
/// its wire-contract string, and the console still has every line.
#[tokio::test]
async fn a_broken_log_table_never_reaches_the_request() {
    let app = TestApp::with_key("k").await;
    let _sink = app.store_logs();
    app.refuse_statements_on("logs");

    app.upload_ok(CallUpload::new().key("k")).await;

    assert_eq!(
        app.count::<radio_scout::db::entities::call::Entity>().await,
        1
    );
}

/// A Logs view that cannot read the table fails like every other 5xx (#29):
/// the cause to the operator's log against a correlation ref, and only the ref
/// to the client. The irony is deliberate — the surface that exists to show
/// what went wrong is not exempt from the rule about not leaking internals.
#[tokio::test]
async fn a_log_that_cannot_be_read_keeps_its_cause_on_the_server() {
    let app = TestApp::spawn().await;
    app.login().await;
    let capture = LogCapture::start();
    app.refuse_statements_on("logs");

    let response = app.get("/api/admin/logs").await;

    assert_eq!(response.status(), 500);
    let request_id = request_id_of(&response);
    let body = response.text().await.expect("a body");
    assert_eq!(body, format!("internal error (request id: {request_id})\n"));
    assert!(
        !body.contains("logs"),
        "the driver's own words never travel to the client: {body:?}"
    );

    let logged = capture.wait_for("stage=search-logs").await;
    assert!(logged.contains(common::REFUSED), "{logged}");
    assert!(logged.contains(&request_id), "{logged}");
}

/// The admin password is still the only way in, whatever the Logs view is
/// showing — a wrong one is refused before any of this is reachable.
#[tokio::test]
async fn a_wrong_password_reads_nothing() {
    let app = app_with_three_events().await;

    let refused = app.login_as(&format!("not-{ADMIN_PASSWORD}")).await;
    assert_eq!(refused.status(), 401);

    assert_eq!(app.get("/api/admin/logs").await.status(), 401);
}
