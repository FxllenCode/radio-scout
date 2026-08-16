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
    let line = capture.only_line_containing("request refused");
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
    let line = capture.only_line_containing("request refused");
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
    let line = capture.only_line_containing("request refused");
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
    let rejected = capture.only_line_containing("request refused");
    assert!(rejected.contains("system_ref=11"), "{rejected}");
    assert!(rejected.contains("talkgroup_ref=54241"), "{rejected}");
    assert!(!rejected.contains("call_id"), "no row, no id: {rejected}");
}

/// Dropping a patch ref leaves a line saying how many went (#81), so "why isn't
/// my patch fanning out?" is answerable from the log rather than by reading the
/// `call_patches` table.
///
/// It is DEBUG, not the WARN rule 7 gives a drop: every SDRTrunk patch upload
/// drops its trailing radio ids, so this is protocol detail on the normal path,
/// and a WARN on every patched Call would be crying wolf. One aggregated line
/// per Call, never one per ref (rule 8).
#[tokio::test]
async fn dropped_patch_refs_leave_one_line_saying_how_many() {
    let capture = LogCapture::start();
    let app = recorder_app().await;
    app.seed_talkgroup(11, 300).await;

    // Two of the four refs are Talkgroups this System has: 54241 (the Call's
    // own, auto-populated on the way past) and the seeded 300.
    assert_eq!(
        app.upload(
            form(RECORDER_KEY, 11, 54241, 1000).set("patches", "[54241,300,1610051,1610092]")
        )
        .await
        .0,
        200
    );

    let line = capture.only_line_containing("patch refs resolved");
    assert!(line.contains(" DEBUG "), "{line}");
    assert!(line.contains("dropped=2"), "{line}");
    assert!(line.contains("collapsed=0"), "{line}");
    assert!(line.contains("kept=2"), "{line}");
    assert!(line.contains("system_ref=11"), "{line}");
}

/// ...and **a patch array that resolves cleanly says nothing at all**.
///
/// The line exists to answer "why is my patch not fanning out?", so it is
/// guarded on something having actually gone unrecognised. Without that guard
/// every patched Call on a healthy System writes a DEBUG line reporting that
/// nothing happened — per Call, forever, on a Pi (ADR-0011 rules 7 and 8: DEBUG
/// is protocol detail, and detail nobody can act on is noise).
///
/// The guard is two comparisons against zero on unsigned counts, so relaxing
/// either one to `>=` makes it constantly true — and the test above cannot see
/// that, because it only ever asks what happens when something *was* dropped.
/// Both mutations survived the whole suite until this existed.
#[tokio::test]
async fn a_patch_array_that_resolves_cleanly_leaves_no_line() {
    let capture = LogCapture::start();
    let app = recorder_app().await;
    app.seed_talkgroup(11, 300).await;

    // Every ref is a Talkgroup this System has — the Call's own and the seeded
    // one — so nothing is dropped and nothing collapses.
    assert_eq!(
        app.upload(form(RECORDER_KEY, 11, 54241, 1000).set("patches", "[54241,300]"))
            .await
            .0,
        200
    );

    capture.assert_never_logged("patch refs resolved");
}

/// The same line's other half, since #45: two patch refs that name **one**
/// channel are reported as collapsed rather than dropped.
///
/// The distinction is the whole reason the counts are separate. `dropped` says
/// the System has never heard of a ref, which may be a misconfiguration;
/// `collapsed` says channel merge did its job, which is the opposite of a
/// problem — and an operator reading one number for both would go looking for a
/// fault that isn't there.
#[tokio::test]
async fn patch_refs_naming_one_merged_channel_are_reported_as_collapsed() {
    let capture = LogCapture::start();
    let app = recorder_app().await;
    app.seed_talkgroup(11, 300).await;
    app.seed_member_ref(11, 300, 8123).await;

    assert_eq!(
        app.upload(form(RECORDER_KEY, 11, 54241, 1000).set("patches", "[300,8123]"))
            .await
            .0,
        200
    );

    let line = capture.only_line_containing("patch refs resolved");
    assert!(line.contains(" DEBUG "), "{line}");
    assert!(line.contains("dropped=0"), "{line}");
    assert!(line.contains("collapsed=1"), "{line}");
    assert!(line.contains("kept=1"), "{line}");
}

/// The other side of it: a Call whose patch refs all resolve says nothing at
/// all. A line on every patched Call would drown the one that means something.
#[tokio::test]
async fn a_patch_that_loses_nothing_logs_nothing() {
    let capture = LogCapture::start();
    let app = recorder_app().await;
    app.seed_talkgroup(11, 300).await;

    assert_eq!(
        app.upload(form(RECORDER_KEY, 11, 54241, 1000).set("patches", "[300]"))
            .await
            .0,
        200
    );
    // ...and neither does a Call with no patches at all.
    assert_eq!(app.upload(form(RECORDER_KEY, 11, 54241, 9000)).await.0, 200);

    capture.assert_never_logged("patch refs with no Talkgroup");
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

    let rejected = capture.only_line_containing("request refused");
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

    // The Talkgroup has to already exist for dedup to be the stage that breaks:
    // since #45 it is keyed on the resolved channel, and a Ref no channel owns
    // cannot have a duplicate, so the query is skipped rather than spent. On a
    // System that has never heard this Ref the first statement to touch `calls`
    // is the insert, and this test would be asserting about `store-call`
    // instead — which the stage table below already covers.
    app.seed_talkgroup(11, 54241).await;

    // Break the schema under the handler's feet, the way a missing column did.
    app.refuse_statements_on("calls");

    let resp = app
        .upload_response(form(RECORDER_KEY, 11, 54241, 1000))
        .await;
    assert_eq!(resp.status(), 500);
    let request_id = request_id_of(&resp);
    let body = resp.text().await.expect("body");

    // The client gets the ref and nothing else.
    let refusal = common::REFUSED;
    assert_eq!(body, format!("internal error (request id: {request_id})\n"));
    assert!(
        !body.contains(refusal),
        "the cause must not travel: {body:?}"
    );

    // The operator gets the cause, against the same ref, in the upload's span.
    let line = capture.only_line_containing("server error");
    assert!(line.contains(" ERROR "), "{line}");
    assert!(line.contains(&format!("request_id={request_id}")), "{line}");
    assert!(line.contains("stage=dedup"), "what it was doing: {line}");
    assert!(line.contains(refusal), "why it failed: {line}");
    assert!(line.contains("system_ref=11"), "what it was about: {line}");
    assert!(line.contains("talkgroup_ref=54241"), "{line}");
}

/// Every stage of the pipeline names itself, so the ERROR line answers "which
/// query" without a backtrace — each of these is a real table the 2026-07-25
/// class of bug (a schema the code expects and the database doesn't have) could
/// take out from under a running server.
#[rstest]
#[case::auth("api_keys", true, "auth")]
#[case::resolve_refs("systems", true, "resolve-refs")]
// No `system` field, so the Ref is assigned before anything else runs.
#[case::assign_system_ref("systems", false, "assign-system-ref")]
// Reading the dedup window and the row insert both survive; writing the Call's
// frequency roster does not.
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
    app.refuse_statements_on(drop_table);

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
    app.refuse_statements_on("systems");

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
    assert!(body.starts_with("internal error (request id: "), "{body:?}");
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
        .seed_call(
            NewCall {
                audio_mime: Some("audio/x-wav".into()),
                ..NewCall::new(11, 54241, 1000)
            },
            common::audio_at("ab/clip.wav"),
        )
        .await;

    std::fs::remove_dir_all(&audio_root).expect("remove audio root");
    std::fs::write(&audio_root, b"not a directory").expect("write audio root");

    let resp = app.get(&format!("/api/call/{id}/audio")).await;
    assert_eq!(resp.status(), 500);
    assert!(
        resp.text()
            .await
            .expect("body")
            .starts_with("internal error (request id: "),
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
        format!("internal error (request id: {request_id})\n")
    );

    let line = capture.only_line_containing("server error");
    assert!(line.contains(" ERROR "), "{line}");
    assert!(line.contains("stage=search-calls"), "{line}");
}

// ---------------------------------------------------------------------------
// The live feed
// ---------------------------------------------------------------------------

/// A reconnecting client's **Backfill** is one line, not one per Call (rule 8), and
/// it carries the size — the number that says whether the client's history has a
/// gap it must fill from the archive (#13).
#[tokio::test]
async fn a_subscription_and_its_backfill_each_log_once() {
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

    let backfill = capture.wait_for("live-feed Backfill").await;
    assert!(backfill.contains(" DEBUG "), "{backfill}");
    assert!(backfill.contains("sent=2"), "the Backfill size: {backfill}");
    assert!(backfill.contains("truncated=false"), "{backfill}");
    assert_eq!(
        capture.lines_containing("live-feed Backfill").len(),
        1,
        "one line per reconnect, never one per Call"
    );
}

/// A **Backfill** is best-effort for the *connection's* sake — a transient DB failure
/// must not kill a live socket — but never for the operator's: a backfill that
/// quietly returns nothing looks exactly like a client that missed nothing.
#[tokio::test]
async fn a_backfill_that_cannot_read_the_archive_says_so_and_keeps_the_socket() {
    let capture = LogCapture::start();
    let app = TestApp::spawn().await;
    app.refuse_statements_on("calls");

    let mut ws = app.connect_ws().await;
    ws.send(WsMessage::Text(
        r#"{"t":"sub","sel":{"11":{"100":true}},"since":0}"#.into(),
    ))
    .await
    .expect("send sub");
    // The ack still arrives: the subscription is live even though the backfill
    // failed, which is the whole point of swallowing it.
    assert_eq!(next_json(&mut ws).await["t"], "subscribed");

    let line = capture.wait_for("live-feed Backfill failed").await;
    assert!(line.contains(" WARN "), "{line}");
    assert!(line.contains("since=0"), "{line}");
    assert!(line.contains(common::REFUSED), "why it failed: {line}");
}

/// The other half of the same swallow (#86). A Backfill reads a page and then
/// the batch that denormalizes it, so the second failure needs the line the
/// first has. What it replaced said nothing at all: the loop dropped any Call
/// whose view failed to build and carried on, which is the shape of bug an
/// operator can only ever report as "some calls go missing sometimes".
///
/// **Since #98 both arms write the same message**, because both are one read of
/// the Archive and the operator's question is the same either way — what is
/// gone, and why. `error=` names the statement that went down; a second
/// *message* for it would be the per-callsite drift ADR-0011 rule 6 forbids.
#[tokio::test]
async fn a_backfill_that_cannot_build_its_view_says_so_and_keeps_the_socket() {
    let capture = LogCapture::start();
    let app = recorder_app().await;
    // Arranged through the front door first, then refused: the denormalizer
    // reads `systems`, and so does the ingest that puts a Call there to
    // denormalize. The page read itself only names `calls`, so it still
    // succeeds — which is what puts the failure on the second query.
    app.upload_ok(form(RECORDER_KEY, 11, 100, 1000)).await;
    app.refuse_statements_on("systems");

    let mut ws = app.connect_ws().await;
    ws.send(WsMessage::Text(
        r#"{"t":"sub","sel":{"11":{"100":true}},"since":0}"#.into(),
    ))
    .await
    .expect("send sub");
    assert_eq!(next_json(&mut ws).await["t"], "subscribed");

    let line = capture.wait_for("live-feed Backfill failed").await;
    assert!(line.contains(" WARN "), "{line}");
    assert!(line.contains("since=0"), "{line}");
    assert!(line.contains(common::REFUSED), "why it failed: {line}");
}

// A half-open connection being reaped is a listener that silently stopped
// hearing anything, and it says so at WARN — rdio leaves such connections
// lingering and says nothing at all. That line is asserted in `src/live.rs`'s
// own table since #94 (`an_unanswered_ping_reaps_on_the_next_tick`), because
// reaching it here meant shortening the shipped heartbeat from outside and then
// sleeping through it.

// Rule 5's sharpest case was the Web Push endpoint — a stable per-device
// identifier, worse than an IP because it survives a lease — and the two tests
// that pinned it went with #107 (ADR-0014). The rule is unchanged and its live
// subjects are the listener IP tests above and the **Downstream** peer key,
// which is stored recoverably and asserted on in `tests/downstream.rs`.

// ---------------------------------------------------------------------------
// Merge curation leaves a line (#50)
// ---------------------------------------------------------------------------

/// **Every merge action logs what moved — counts *and* ids.**
///
/// A fold is the one curation act that rewrites the archive rather than the
/// configuration, and the report it returns reaches only whoever clicked. An
/// Operator reading journald a week later, wondering where a channel went, has
/// this line and nothing else — so it has to carry the Refs that moved and the
/// Talkgroup ids behind them, not merely a total.
///
/// INFO rather than WARN (ADR-0011 rule 7): nothing was rejected and no Call was
/// destroyed. A Talkgroup row went, and unfolding brings it back.
#[tokio::test]
async fn a_fold_from_the_browser_says_what_it_moved() {
    let capture = LogCapture::start();
    let app = TestApp::with_key(RECORDER_KEY).await;
    app.login().await;
    app.upload_ok(CallUpload::new().key(RECORDER_KEY).talkgroup(100).at(1000))
        .await;
    app.upload_ok(CallUpload::new().key(RECORDER_KEY).talkgroup(8123).at(2000))
        .await;
    let owner = app.talkgroup_by_ref(11, 100).await.expect("the owner").id;
    let absorbed = app
        .talkgroup_by_ref(11, 8123)
        .await
        .expect("the churn row")
        .id;

    let (status, report) = app
        .admin_post(
            &format!("/api/admin/talkgroups/{owner}/members"),
            serde_json::json!({"fold": [8123]}),
        )
        .await;
    assert_eq!(status, 200, "{report}");

    let line = capture.only_line_containing("talkgroup member Refs changed");
    assert!(line.contains(" INFO "), "{line}");
    assert!(line.contains(&format!("talkgroup_id={owner}")), "{line}");
    assert!(line.contains("folded=1"), "{line}");
    assert!(line.contains("calls_repointed=1"), "{line}");
    assert!(line.contains("refs=8123"), "the Refs that moved: {line}");
    assert!(
        line.contains(&format!("talkgroup_ids={absorbed}")),
        "the id of the channel that went: {line}"
    );
    assert!(line.contains("dry_run=false"), "{line}");
}

/// A preview leaves a line too, and it says it was one — so a log full of
/// merges cannot be misread as a log full of merges that happened.
#[tokio::test]
async fn a_previewed_fold_says_it_wrote_nothing() {
    let capture = LogCapture::start();
    let app = TestApp::spawn().await;
    app.login().await;
    app.seed_talkgroup(11, 100).await;
    app.seed_talkgroup(11, 8123).await;
    let owner = app.talkgroup_by_ref(11, 100).await.expect("the owner").id;

    app.admin_post(
        &format!("/api/admin/talkgroups/{owner}/members?dryRun"),
        serde_json::json!({"fold": [8123]}),
    )
    .await;

    let line = capture.only_line_containing("talkgroup member Refs changed");
    assert!(line.contains("dry_run=true"), "{line}");
}

/// A Range edit is a merge action too, and leaves its own line — no Call moved,
/// so there is no `calls_repointed` to report and the counts are what changed.
#[tokio::test]
async fn a_range_edit_says_what_it_changed() {
    let capture = LogCapture::start();
    let app = TestApp::spawn().await;
    app.login().await;
    app.seed_unit(11, 1200, "Engine 1").await;
    let unit = app.unit_by_ref(11, 1200).await.expect("the Unit").id;

    app.admin_post(
        &format!("/api/admin/units/{unit}/ranges"),
        serde_json::json!({"add": [{"from": 1201, "to": 1299}]}),
    )
    .await;

    let line = capture.only_line_containing("unit Ranges changed");
    assert!(line.contains(" INFO "), "{line}");
    assert!(line.contains(&format!("unit_id={unit}")), "{line}");
    assert!(line.contains("unit_ref=1200"), "{line}");
    assert!(line.contains("added=1"), "{line}");
    assert!(line.contains("removed=0"), "{line}");
    assert!(
        line.contains("spans=1201-1299"),
        "which block, not merely how many: {line}"
    );
}

/// **A Ref that named no channel is a hole in the line, not a missing column.**
///
/// The two lists are positional — `refs` and `talkgroup_ids` line up entry by
/// entry — so a Ref that absorbed nothing has to occupy its place. Dropping it
/// would silently shift every id after it onto the wrong Ref, which is the one
/// way a log line about a merge could mislead rather than merely omit.
#[tokio::test]
async fn a_recorded_ref_leaves_a_gap_in_the_ids_rather_than_a_shorter_list() {
    let capture = LogCapture::start();
    let app = TestApp::spawn().await;
    app.login().await;
    app.seed_talkgroup(11, 100).await;
    app.seed_talkgroup(11, 8123).await;
    let owner = app.talkgroup_by_ref(11, 100).await.expect("the owner").id;
    let absorbed = app
        .talkgroup_by_ref(11, 8123)
        .await
        .expect("the churn row")
        .id;

    // 9000 names no channel at all; 8123 names one.
    app.admin_post(
        &format!("/api/admin/talkgroups/{owner}/members"),
        serde_json::json!({"fold": [9000, 8123]}),
    )
    .await;

    let line = capture.only_line_containing("talkgroup member Refs changed");
    assert!(line.contains("refs=9000,8123"), "{line}");
    assert!(
        line.contains(&format!("talkgroup_ids=-,{absorbed}")),
        "the ref that absorbed nothing holds its place: {line}"
    );
}

/// **A merge that moved nothing is not a merge action, and leaves no line.**
///
/// A form submitted with nothing changed, or a re-imported file whose merges all
/// already applied, must not write a line saying a merge happened — a log an
/// Operator greps for "where did that channel go" is useless if it is full of
/// merges that were not.
#[tokio::test]
async fn a_fold_that_moves_nothing_writes_no_line() {
    let capture = LogCapture::start();
    let app = TestApp::spawn().await;
    app.login().await;
    app.seed_talkgroup(11, 100).await;
    let owner = app.talkgroup_by_ref(11, 100).await.expect("the owner").id;

    app.admin_post(
        &format!("/api/admin/talkgroups/{owner}/members"),
        serde_json::json!({}),
    )
    .await;

    capture.assert_never_logged("talkgroup member Refs changed");
}

/// The same for Ranges, and the same reason. Both halves of the guard are
/// asserted: a delta that changes nothing is silent...
#[tokio::test]
async fn a_range_edit_that_changes_nothing_writes_no_line() {
    let capture = LogCapture::start();
    let app = TestApp::spawn().await;
    app.login().await;
    app.seed_unit(11, 1200, "Engine 1").await;
    let unit = app.unit_by_ref(11, 1200).await.expect("the Unit").id;

    app.admin_post(
        &format!("/api/admin/units/{unit}/ranges"),
        serde_json::json!({}),
    )
    .await;

    capture.assert_never_logged("unit Ranges changed");
}

/// ...and a delta that only *removes* still speaks, which is the half a guard
/// written against additions alone would silently drop.
#[tokio::test]
async fn removing_a_range_says_so() {
    let capture = LogCapture::start();
    let app = TestApp::spawn().await;
    app.login().await;
    app.seed_unit(11, 1200, "Engine 1").await;
    app.seed_unit_range(11, 1200, 1201, 1299).await;
    let unit = app.unit_by_ref(11, 1200).await.expect("the Unit").id;

    app.admin_post(
        &format!("/api/admin/units/{unit}/ranges"),
        serde_json::json!({"remove": [{"from": 1201, "to": 1299}]}),
    )
    .await;

    let line = capture.only_line_containing("unit Ranges changed");
    assert!(line.contains("added=0"), "{line}");
    assert!(line.contains("removed=1"), "{line}");
    assert!(line.contains("spans="), "{line}");
}

// ---------------------------------------------------------------------------
// A configuration document leaves a line (#51)
// ---------------------------------------------------------------------------

/// **A restore says what it did**, in one line an Operator finds afterwards.
///
/// The report reaches only whoever clicked, and a configuration import is the
/// single most consequential thing this surface does — every entity at once. So
/// the counts are asserted with a *created and an updated row of the same kind*,
/// which is the only shape that tells "created + updated" from either of them
/// alone.
#[tokio::test]
async fn importing_a_configuration_says_what_it_moved() {
    let capture = LogCapture::start();
    let app = TestApp::spawn().await;
    app.login().await;
    let first = serde_json::json!({
        "version": 1,
        "systems": [{
            "ref": 11,
            "label": "Fulton",
            "talkgroups": [{"ref": 100, "label": "Fire Dispatch"}],
            "units": [{"ref": 1200, "label": "Engine 1"}],
        }],
    });
    app.admin_post("/api/admin/config/import", first).await;

    // One System updated, one Talkgroup updated and one created, one Unit
    // updated, one key issued, and one entry refused.
    app.admin_post(
        "/api/admin/config/import",
        serde_json::json!({
            "version": 1,
            "systems": [{
                "ref": 11,
                "label": "Fulton County",
                "talkgroups": [
                    {"ref": 100, "label": "Fire Dispatch 1"},
                    {"ref": 200, "label": "EMS"},
                    {"ref": 300, "led": "puce"},
                ],
                "units": [{"ref": 1200, "label": "Engine One"}],
            }],
            "apiKeys": [{"label": "the pi"}],
        }),
    )
    .await;

    // The seeding import left a line of its own; this is the one under test.
    let lines = capture.lines_containing("configuration document imported");
    let line = lines.last().expect("a line").clone();
    assert!(line.contains(" INFO "), "{line}");
    assert!(line.contains("systems=1"), "{line}");
    assert!(
        line.contains("talkgroups=2"),
        "one updated and one created — the refused one is neither: {line}"
    );
    assert!(line.contains("units=1"), "{line}");
    assert!(line.contains("keys_issued=1"), "{line}");
    assert!(line.contains("rejected=1"), "{line}");
    assert!(line.contains("dry_run=false"), "{line}");
}

/// A preview leaves a line too, and it says it was one — so a log full of
/// restores cannot be misread as a log full of restores that happened.
#[tokio::test]
async fn a_previewed_configuration_import_says_it_wrote_nothing() {
    let capture = LogCapture::start();
    let app = TestApp::spawn().await;
    app.login().await;

    app.admin_post(
        "/api/admin/config/import?dryRun",
        serde_json::json!({"version": 1, "systems": [{"ref": 11}]}),
    )
    .await;

    let line = capture.only_line_containing("configuration document imported");
    assert!(line.contains("dry_run=true"), "{line}");
    assert!(line.contains("systems=1"), "{line}");
}
