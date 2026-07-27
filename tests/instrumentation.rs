//! Ingest + subsystem instrumentation (#29), asserted over the real HTTP/WS
//! boundary.
//!
//! Two halves of the 2026-07-25 incident, both structural (ADR-0011 rules 3 and
//! 4): a Call that does not become a row must leave a line saying *why*, and an
//! internal failure must be recorded on the server rather than posted to the
//! recorder's log. Every test here reads back what the server actually wrote.

mod common;
use common::logs::LogCapture;
use common::{CallUpload, TestApp, next_json, next_text, request_id_of};

use std::time::Duration;

use futures_util::SinkExt;
use radio_scout::IngestConfig;
use radio_scout::db::entities::call;
use radio_scout::db::repo::NewCall;
use rstest::rstest;
use tokio_tungstenite::tungstenite::Message as WsMessage;

/// The recorder every test here authenticates as.
const RECORDER_KEY: &str = "recorder-key";

/// An app with the recorder's key registered.
async fn recorder_app() -> TestApp {
    TestApp::with_key(RECORDER_KEY).await
}

/// A recorder-shaped upload for `system`/`talkgroup`.
fn form(key: &str, system: i64, talkgroup: i64, timestamp_ms: i64) -> CallUpload {
    upload_parts(key, timestamp_ms)
        .system(system)
        .talkgroup(talkgroup)
}

/// The parts every upload carries, whether or not it names a System or a
/// Talkgroup.
fn upload_parts(key: &str, timestamp_ms: i64) -> CallUpload {
    CallUpload::new()
        .key(key)
        .at(timestamp_ms)
        .remove("system")
        .remove("talkgroup")
}

/// A Trunk-Recorder-dialect upload, either half of which a dying recorder can
/// omit.
fn tr_form(meta: Option<&str>, audio: Option<&[u8]>) -> CallUpload {
    let upload = CallUpload::tr(meta.unwrap_or_default()).key(RECORDER_KEY);
    let upload = match meta {
        Some(_) => upload,
        None => upload.remove("meta"),
    };
    match audio {
        Some(audio) => upload.audio_named(audio, "a.wav", "audio/x-wav"),
        None => upload.no_audio(),
    }
}

// ---------------------------------------------------------------------------
// Rule 3: every rejection says why
// ---------------------------------------------------------------------------

/// The five ways an upload declines to become a row (ADR-0011 rule 3). Each
/// needs its own fixture, so the case names it and [`provoke`] builds it.
#[derive(Debug, Clone, Copy)]
enum Rejected {
    InvalidApiKey,
    Duplicate,
    Blacklisted,
    NotPopulated,
    NoTalkgroup,
}

/// Bring up a fresh app, provoke `case`, and hand back what the recorder got.
async fn provoke(case: Rejected) -> (u16, String) {
    let app = TestApp::builder()
        .ingest(IngestConfig {
            auto_populate: !matches!(case, Rejected::NotPopulated),
            ..Default::default()
        })
        .spawn()
        .await;
    app.create_api_key(RECORDER_KEY).await;
    let upload = || form(RECORDER_KEY, 11, 54241, 1000);

    match case {
        // A key that was never registered: unknown, and so out of scope for
        // every System (ADR-0008).
        Rejected::InvalidApiKey => app.upload(form("nope", 11, 54241, 1000)).await,
        Rejected::Duplicate => {
            let (status, body) = app.upload(upload()).await;
            assert_eq!(status, 200, "the first upload stores: {body:?}");
            app.upload(upload()).await
        }
        Rejected::Blacklisted => {
            app.seed_system(11, false, Some("54241")).await;
            app.upload(upload()).await
        }
        // Auto-populate off + an unknown System: nothing to attach the Call to.
        Rejected::NotPopulated => app.upload(upload()).await,
        Rejected::NoTalkgroup => {
            app.upload(upload_parts(RECORDER_KEY, 1000).system(11))
                .await
        }
    }
}

/// Rule 3, the rule the 2026-07-25 incident bought: a Call that does not become a
/// row leaves a line saying why, in a field an operator can grep — whatever the
/// recorder was told.
///
/// The `expected_body` column is why this matters: two of these five answer HTTP
/// 200 `Call imported successfully.` so the recorder never retries, and one
/// answers 200 `duplicate call rejected`. From the recorder's side a dropped Call
/// is indistinguishable from a stored one, which makes the server's own log the
/// only place the truth exists. Those bodies are also the ADR-0001 wire contract,
/// so they are pinned here byte-for-byte.
#[rstest]
#[case::invalid_api_key(
    Rejected::InvalidApiKey,
    "invalid-api-key",
    401,
    "Invalid API key for system 11 talkgroup 54241.\n"
)]
#[case::duplicate(Rejected::Duplicate, "duplicate", 200, "duplicate call rejected\n")]
#[case::blacklisted(
    Rejected::Blacklisted,
    "blacklisted",
    200,
    "Call imported successfully.\n"
)]
#[case::not_populated(
    Rejected::NotPopulated,
    "not-populated",
    200,
    "Call imported successfully.\n"
)]
#[case::no_talkgroup(
    Rejected::NoTalkgroup,
    "no-talkgroup",
    417,
    "Incomplete call data: no talkgroup\n"
)]
#[tokio::test]
async fn every_rejected_upload_logs_at_warn_with_a_machine_readable_reason(
    #[case] case: Rejected,
    #[case] reason: &str,
    #[case] expected_status: u16,
    #[case] expected_body: &str,
) {
    let capture = LogCapture::start();

    let (status, body) = provoke(case).await;

    assert_eq!(status, expected_status, "{body:?}");
    assert_eq!(body, expected_body, "the recorder's wire contract");
    let line = capture.only_line_containing("ingest rejected");
    assert!(line.contains(" WARN "), "{line}");
    assert!(line.contains(&format!("reason={reason}")), "{line}");
}

/// Rule 2, on the path that handles a secret by definition: neither the key that
/// worked nor the one that didn't may appear in any line, at any level, in any
/// form. An invalid key has no row to name it by, so the line carries the reason
/// alone.
#[tokio::test]
async fn no_line_ever_carries_the_key_it_was_sent() {
    const GOOD: &str = "correct-horse-battery-staple";
    const BAD: &str = "hunter2-is-not-the-key";

    let capture = LogCapture::start();
    let app = TestApp::with_key(GOOD).await;

    assert_eq!(app.upload(form(GOOD, 11, 54241, 1000)).await.0, 200);
    assert_eq!(app.upload(form(BAD, 11, 54241, 2000)).await.0, 401);

    let line = capture.only_line_containing("reason=invalid-api-key");
    assert!(line.contains(" WARN "), "{line}");

    capture.assert_never_logged(GOOD);
    capture.assert_never_logged(BAD);
    // Not even a fragment: rule 2 forbids a truncated or prefixed secret too.
    capture.assert_never_logged("hunter2");
    capture.assert_never_logged("correct-horse");
}

/// A recorder dying mid-upload — the connection drops between the headers and
/// the closing boundary — is garbage arriving, not nothing arriving, and the two
/// must not look the same. Each place our reader can choke names itself, in both
/// dialects, and each 417 body is pinned: the reason slug *is* that string with
/// its dashes spelled as spaces, so renaming one rewrites the other.
#[rstest]
// The stream ends inside a part's headers: nothing is readable at all.
#[case::no_part(
    "/api/call-upload",
    b"--BOUNDARY\r\nContent-Disposition: form-data; name=\"key\"\r\n",
    "malformed-multipart-body",
    "Incomplete call data: malformed multipart body\n"
)]
// Headers complete, body cut off: the part exists, its value doesn't.
#[case::cut_field(
    "/api/call-upload",
    b"--BOUNDARY\r\nContent-Disposition: form-data; name=\"key\"\r\n\r\nabc",
    "could-not-read-field",
    "Incomplete call data: could not read field\n"
)]
#[case::cut_audio(
    "/api/call-upload",
    b"--BOUNDARY\r\nContent-Disposition: form-data; name=\"audio\"; filename=\"a.wav\"\r\n\r\nRIFF",
    "could-not-read-audio",
    "Incomplete call data: could not read audio\n"
)]
// The Trunk Recorder dialect reads its own body and must say so too.
#[case::tr_no_part(
    "/api/trunk-recorder-call-upload",
    b"--BOUNDARY\r\nContent-Disposition: form-data; name=\"key\"\r\n",
    "malformed-multipart-body",
    "Incomplete call data: malformed multipart body\n"
)]
#[case::tr_cut_audio(
    "/api/trunk-recorder-call-upload",
    b"--BOUNDARY\r\nContent-Disposition: form-data; name=\"audio\"; filename=\"a.wav\"\r\n\r\nRIFF",
    "could-not-read-audio",
    "Incomplete call data: could not read audio\n"
)]
#[tokio::test]
async fn a_body_our_reader_cannot_parse_says_which_part_it_choked_on(
    #[case] path: &str,
    #[case] raw_body: &[u8],
    #[case] reason: &str,
    #[case] expected_body: &str,
) {
    let capture = LogCapture::start();
    let app = TestApp::spawn().await;

    let (status, body) = app
        .post_bytes(
            path,
            "multipart/form-data; boundary=BOUNDARY",
            raw_body.to_vec(),
        )
        .await;

    assert_eq!(status, 417, "{body:?}");
    assert_eq!(body, expected_body, "the recorder's wire contract");
    let line = capture.only_line_containing("ingest rejected");
    assert!(line.contains(" WARN "), "{line}");
    assert!(line.contains(&format!("reason={reason}")), "{line}");
}

/// Trunk Recorder's native dialect carries its metadata as one JSON part, so it
/// has rejections of its own — each with a reason, and with TR's own wire string
/// where it differs from the generic one.
#[rstest]
#[case::no_meta(None, Some(b"audio-bytes".as_slice()), "no-meta", "Incomplete call data: no meta\n")]
#[case::invalid_meta(
    Some("{not json"),
    Some(b"audio-bytes".as_slice()),
    "invalid-meta",
    "Invalid call data\n"
)]
#[case::no_talkgroup(
    Some(r#"{"short_name":"butco"}"#),
    Some(b"audio-bytes".as_slice()),
    "no-talkgroup",
    "Incomplete call data: no talkgroup\n"
)]
// Talkgroup 0 is TR's "I could not decode one", not a Talkgroup — rdio's parser
// rejects it and so must we, or every undecoded call in a system piles up under
// a phantom Talkgroup 0.
#[case::talkgroup_zero(
    Some(r#"{"short_name":"butco","talkgroup":0}"#),
    Some(b"audio-bytes".as_slice()),
    "no-talkgroup",
    "Incomplete call data: no talkgroup\n"
)]
#[case::no_audio(
    Some(r#"{"short_name":"butco","talkgroup":54241}"#),
    None,
    "no-audio",
    "Incomplete call data: no audio\n"
)]
// A part that is present and empty: a recorder that produced no samples. A
// zero-byte Call would play as silence forever.
#[case::empty_audio(
    Some(r#"{"short_name":"butco","talkgroup":54241}"#),
    Some(b"".as_slice()),
    "no-audio",
    "Incomplete call data: no audio\n"
)]
#[tokio::test]
async fn the_trunk_recorder_dialect_reports_its_own_rejections(
    #[case] meta: Option<&str>,
    #[case] audio: Option<&[u8]>,
    #[case] reason: &str,
    #[case] expected_body: &str,
) {
    let capture = LogCapture::start();
    let app = TestApp::spawn().await;

    let (status, body) = app.upload_tr(tr_form(meta, audio)).await;

    assert_eq!(status, 417, "{body:?}");
    assert_eq!(body, expected_body, "the recorder's wire contract");
    let line = capture.only_line_containing("ingest rejected");
    assert!(line.contains(&format!("reason={reason}")), "{line}");
}

// ---------------------------------------------------------------------------
// One upload, one span
// ---------------------------------------------------------------------------

/// Every line about one upload is attributable without repeating context: the
/// System and Talkgroup ride the span, the Call id joins it once there is one,
/// and the whole thing nests under the request that carried it (#28).
#[tokio::test]
async fn one_upload_is_one_span_carrying_system_talkgroup_and_call_id() {
    let capture = LogCapture::start();
    let app = recorder_app().await;

    assert_eq!(app.upload(form(RECORDER_KEY, 11, 54241, 1000)).await.0, 200);

    let stored = capture.only_line_containing("call stored");
    assert!(stored.contains(" INFO "), "{stored}");
    assert!(stored.contains("system_ref=11"), "{stored}");
    assert!(stored.contains("talkgroup_ref=54241"), "{stored}");
    assert!(stored.contains("call_id=1"), "{stored}");
    assert!(
        stored.contains("request_id="),
        "nested in the request: {stored}"
    );

    // A Call that never became a row has no id — and still names its System and
    // Talkgroup, which is what makes a rejection actionable.
    assert_eq!(app.upload(form(RECORDER_KEY, 11, 54241, 1000)).await.0, 200);
    let rejected = capture.only_line_containing("ingest rejected");
    assert!(rejected.contains("system_ref=11"), "{rejected}");
    assert!(rejected.contains("talkgroup_ref=54241"), "{rejected}");
    assert!(!rejected.contains("call_id"), "no row, no id: {rejected}");
}

/// A body too malformed to yield a System and a Talkgroup is rejected before the
/// upload has an identity, so its line carries the request id and nothing else —
/// which is the whole of what is known about it. Naming that here keeps it a
/// decision rather than an oversight: the request line supplies the path and the
/// recorder's address under the same id.
#[tokio::test]
async fn a_rejection_before_an_upload_has_an_identity_is_attributable_by_request_id() {
    let capture = LogCapture::start();
    let app = TestApp::spawn().await;

    let (status, _) = app
        .post_bytes(
            "/api/call-upload",
            "multipart/form-data; boundary=BOUNDARY",
            b"--BOUNDARY\r\nContent-Disposition: form-data; name=\"key\"\r\n".to_vec(),
        )
        .await;
    assert_eq!(status, 417);

    let rejected = capture.only_line_containing("ingest rejected");
    assert!(rejected.contains("request_id="), "{rejected}");
    assert!(
        !rejected.contains("system_ref"),
        "nothing to name: {rejected}"
    );

    // ...and the request line under that same id says who sent it and where.
    let id = rejected
        .split("request_id=")
        .nth(1)
        .and_then(|rest| rest.split('}').next())
        .expect("a request id on the rejection")
        .to_owned();
    let request = capture.only_line_containing("path=/api/call-upload ");
    assert!(request.contains(&format!("request_id={id}")), "{request}");
    assert!(request.contains("client_addr=127.0.0.1"), "{request}");
}

// ---------------------------------------------------------------------------
// Rule 4: a 5xx tells the client a ref and the operator the cause
// ---------------------------------------------------------------------------

/// The other half of the 2026-07-25 incident: the raw SQL error travelled to
/// Trunk Recorder and the server recorded nothing. Now the cause stays here,
/// against the ref the client is given — and carries the upload's own context,
/// because "which System was that" is the next question after "what broke".
#[tokio::test]
async fn a_5xx_gives_the_client_a_ref_and_the_operator_the_cause() {
    let capture = LogCapture::start();
    let app = recorder_app().await;

    // Break the schema under the handler's feet, the way a missing column did.
    app.break_table("calls").await;

    let resp = app
        .upload_response(form(RECORDER_KEY, 11, 54241, 1000))
        .await;
    assert_eq!(resp.status(), 500);
    let request_id = request_id_of(&resp);
    let body = resp.text().await.expect("body");

    // The client gets the ref and nothing else.
    let missing_table = app.missing_table_cause("calls");
    assert_eq!(body, format!("internal error (ref: {request_id})\n"));
    assert!(
        !body.contains(&missing_table),
        "the cause must not travel: {body:?}"
    );

    // The operator gets the cause, against the same ref, in the upload's span.
    let line = capture.only_line_containing("server error");
    assert!(line.contains(" ERROR "), "{line}");
    assert!(line.contains(&format!("request_id={request_id}")), "{line}");
    assert!(line.contains("stage=dedup"), "what it was doing: {line}");
    assert!(line.contains(&missing_table), "why it failed: {line}");
    assert!(line.contains("system_ref=11"), "what it was about: {line}");
    assert!(line.contains("talkgroup_ref=54241"), "{line}");
}

/// Every stage of the pipeline names itself, so the ERROR line answers "which
/// query" without a backtrace — each of these is a real table the 2026-07-25
/// class of bug (a schema the code expects and the database doesn't have) could
/// take out from under a running server.
#[rstest]
#[case::auth("api_keys", true, "auth")]
#[case::policy("systems", true, "auto-populate-policy")]
// No `system` field, so the Ref is assigned before anything else runs.
#[case::assign_system_ref("systems", false, "assign-system-ref")]
// Dedup and the row insert both survive; writing the Call's frequency roster
// does not.
#[case::store_call("call_frequencies", true, "store-call")]
// The Call is stored; building the view to push to listeners is not. (An upload
// with no patches never writes this table, so the insert is untroubled by its
// absence — only the read-back trips.)
#[case::build_call_view("call_patches", true, "build-call-view")]
#[tokio::test]
async fn a_failure_names_the_stage_of_the_pipeline_it_happened_in(
    #[case] drop_table: &str,
    #[case] with_system: bool,
    #[case] stage: &str,
) {
    let capture = LogCapture::start();
    let app = recorder_app().await;
    app.break_table(drop_table).await;

    let mut upload = upload_parts(RECORDER_KEY, 1000)
        .talkgroup(54241)
        .set("frequencies", r#"[{"freq":774031250,"pos":0.0,"len":1.5}]"#);
    if with_system {
        upload = upload.system(11);
    }
    let (status, body) = app.upload(upload).await;

    assert_eq!(status, 500, "{body:?}");
    let line = capture.only_line_containing("server error");
    assert!(line.contains(" ERROR "), "{line}");
    assert!(line.contains(&format!("stage={stage}")), "{line}");
    // Rule 2 holds even when the failing query is the one that binds the key:
    // whatever the driver puts in its error, the key is not in the log.
    capture.assert_never_logged(RECORDER_KEY);
}

/// The TR dialect resolves its System by name before anything else can run, so a
/// failure there is its own stage.
#[tokio::test]
async fn the_trunk_recorder_dialect_names_its_own_failing_stage() {
    let capture = LogCapture::start();
    let app = TestApp::spawn().await;
    app.break_table("systems").await;

    let (status, _) = app
        .upload_tr(tr_form(
            Some(r#"{"short_name":"butco","talkgroup":54241}"#),
            Some(b"audio-bytes"),
        ))
        .await;

    assert_eq!(status, 500);
    let line = capture.only_line_containing("server error");
    assert!(line.contains("stage=resolve-system"), "{line}");
}

/// An audio store that cannot be written is a 500 like any other — and the Call
/// is not left as a row pointing at audio that was never stored (ADR-0001 writes
/// the object first for exactly this reason).
#[tokio::test]
async fn an_unwritable_audio_store_is_a_server_error_not_a_row() {
    let capture = LogCapture::start();
    let app = recorder_app().await;
    let audio_root = app.path().join("audio");

    // Replace the store's root with a regular file: every write beneath it now
    // fails, deterministically and without waiting for a network timeout.
    std::fs::remove_dir_all(&audio_root).expect("remove audio root");
    std::fs::write(&audio_root, b"not a directory").expect("write audio root");

    let (status, body) = app.upload(form(RECORDER_KEY, 11, 54241, 1000)).await;

    assert_eq!(status, 500, "{body:?}");
    assert!(body.starts_with("internal error (ref: "), "{body:?}");
    let line = capture.only_line_containing("server error");
    assert!(line.contains("stage=store-audio"), "{line}");
    assert_eq!(
        app.count::<call::Entity>().await,
        0,
        "no row for audio that was never stored"
    );
}

/// The listening side of the same store failure: a Call whose audio cannot even
/// be stat-ed is a 500 with a ref, not a 404 — "gone" and "broken" are different
/// problems and only the log can tell them apart.
#[tokio::test]
async fn an_unreadable_audio_store_is_a_server_error_not_a_missing_call() {
    let capture = LogCapture::start();
    let app = TestApp::spawn().await;
    let audio_root = app.path().join("audio");
    app.put_object("ab/clip.wav", b"0123456789").await;
    let id = app
        .seed_call(NewCall {
            system_ref: 11,
            talkgroup_ref: 54241,
            call_at_ms: 1000,
            object_key: "ab/clip.wav".into(),
            audio_mime: Some("audio/x-wav".into()),
            ..Default::default()
        })
        .await;

    std::fs::remove_dir_all(&audio_root).expect("remove audio root");
    std::fs::write(&audio_root, b"not a directory").expect("write audio root");

    let resp = app.get(&format!("/api/call/{id}/audio")).await;
    assert_eq!(resp.status(), 500);
    assert!(
        resp.text()
            .await
            .expect("body")
            .starts_with("internal error (ref: "),
        "the store's own words stay here"
    );
    let line = capture.only_line_containing("server error");
    assert!(line.contains("stage=stat-audio"), "{line}");
}

/// The same rule outside ingest: nothing anywhere posts its internals to a
/// client. A read path 500s with a ref too, and says what it was doing.
#[tokio::test]
async fn a_read_path_5xx_carries_only_a_ref_as_well() {
    let capture = LogCapture::start();
    let app = TestApp::spawn().await;
    app.db.clone().close().await.expect("close the pool");

    let resp = app.get("/api/calls").await;
    assert_eq!(resp.status(), 500);
    let request_id = request_id_of(&resp);
    assert_eq!(
        resp.text().await.expect("body"),
        format!("internal error (ref: {request_id})\n")
    );

    let line = capture.only_line_containing("server error");
    assert!(line.contains(" ERROR "), "{line}");
    assert!(line.contains("stage=search-calls"), "{line}");
}

// ---------------------------------------------------------------------------
// The live feed
// ---------------------------------------------------------------------------

/// A reconnecting client's catch-up is one line, not one per Call (rule 8), and
/// it carries the size — the number that says whether the client's history has a
/// gap it must fill from the archive (#13).
#[tokio::test]
async fn a_subscription_and_its_catch_up_each_log_once() {
    let capture = LogCapture::start();
    let app = recorder_app().await;
    app.upload_ok(form(RECORDER_KEY, 11, 100, 1000)).await;
    app.upload_ok(form(RECORDER_KEY, 11, 300, 2000)).await;

    let mut ws = app.connect_ws().await;
    ws.send(WsMessage::Text(
        r#"{"t":"sub","sel":{"11":{"100":true,"300":true}},"since":0}"#.into(),
    ))
    .await
    .expect("send sub");
    // The ack, then both backfilled Calls.
    for _ in 0..3 {
        next_text(&mut ws).await;
    }

    let subscribed = capture.wait_for("live-feed subscription").await;
    assert!(
        subscribed.contains(" DEBUG "),
        "protocol detail: {subscribed}"
    );
    assert!(subscribed.contains("systems=1"), "{subscribed}");

    let catch_up = capture.wait_for("live-feed catch-up").await;
    assert!(catch_up.contains(" DEBUG "), "{catch_up}");
    assert!(catch_up.contains("sent=2"), "the backfill size: {catch_up}");
    assert!(catch_up.contains("truncated=false"), "{catch_up}");
    assert_eq!(
        capture.lines_containing("live-feed catch-up").len(),
        1,
        "one line per reconnect, never one per Call"
    );
}

/// Catch-up is best-effort for the *connection's* sake — a transient DB failure
/// must not kill a live socket — but never for the operator's: a backfill that
/// quietly returns nothing looks exactly like a client that missed nothing.
#[tokio::test]
async fn a_catch_up_that_cannot_read_the_archive_says_so_and_keeps_the_socket() {
    let capture = LogCapture::start();
    let app = TestApp::spawn().await;
    app.break_table("calls").await;

    let mut ws = app.connect_ws().await;
    ws.send(WsMessage::Text(
        r#"{"t":"sub","sel":{"11":{"100":true}},"since":0}"#.into(),
    ))
    .await
    .expect("send sub");
    // The ack still arrives: the subscription is live even though the backfill
    // failed, which is the whole point of swallowing it.
    assert_eq!(next_json(&mut ws).await["t"], "subscribed");

    let line = capture.wait_for("live-feed catch-up query failed").await;
    assert!(line.contains(" WARN "), "{line}");
    assert!(line.contains("since=0"), "{line}");
    assert!(
        line.contains(&app.missing_table_cause("calls")),
        "why it failed: {line}"
    );
}

/// A half-open connection being reaped is a listener that silently stopped
/// hearing anything — WARN, not silence (rdio leaves such connections lingering
/// and says nothing at all).
#[tokio::test]
async fn a_reaped_connection_says_so() {
    let capture = LogCapture::start();
    let heartbeat = Duration::from_millis(100);
    let app = TestApp::builder().heartbeat(heartbeat).spawn().await;
    let (_ws, _hello) = app.connect_ws_with_hello().await;

    // Go silent: never read, so no auto-pong is ever sent.
    tokio::time::sleep(heartbeat * 4).await;

    let line = capture.wait_for("live-feed listener reaped").await;
    assert!(line.contains(" WARN "), "{line}");
}

/// Rule 5 on the Web Push path (#16). A push endpoint is a stable per-device
/// identifier — worse than an IP address, because it survives a lease — so it
/// never appears in a log line, at any level, however the subscription is
/// refused. The push *service* may be named on a failure, because which service
/// is broken is the operator's problem; the device may not.
#[tokio::test]
async fn a_refused_push_subscription_says_why_without_naming_the_device() {
    let capture = LogCapture::start();
    let app = TestApp::spawn().await;
    let endpoint = "https://push.example.net/p/a-device-nobody-else-should-see";

    let response = app
        .post_json(
            "/api/push/subscribe",
            serde_json::json!({
                "endpoint": endpoint,
                "keys": { "p256dh": "not-a-key", "auth": "BTBZMqHH6r4Tts7J_aSIgg" },
            }),
        )
        .await;

    assert_eq!(response.status(), 400);
    let line = capture.only_line_containing("push subscription rejected");
    assert!(line.contains(" WARN "), "{line}");
    assert!(line.contains(r#"reason="bad-key""#), "{line}");
    capture.assert_never_logged(endpoint);
}

/// A push service that says the device is gone is worth one line — the
/// subscription's **Id**, never its endpoint — because a subscription
/// disappearing on its own is otherwise invisible.
#[tokio::test]
async fn a_gone_device_is_logged_by_its_id_and_not_its_endpoint() {
    let capture = LogCapture::start();
    let service = common::PushService::start().await;
    let app = TestApp::with_key("k").await;
    let endpoint = service.endpoint();
    app.post_json(
        "/api/push/subscribe",
        serde_json::json!({
            "endpoint": endpoint,
            "keys": { "p256dh": common::SUBSCRIBER_PUBLIC, "auth": common::SUBSCRIBER_AUTH },
            "selection": { "all": true, "sel": {} },
        }),
    )
    .await;
    service.answer_with(410);

    app.upload_ok(CallUpload::new()).await;

    let line = capture.wait_for("push subscription gone").await;
    assert!(line.contains(" INFO "), "{line}");
    assert!(line.contains("status=410"), "{line}");
    assert!(line.contains("subscription="), "{line}");
    capture.assert_never_logged(&endpoint);
}
