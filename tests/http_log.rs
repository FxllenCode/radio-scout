//! The HTTP request log (#28), asserted over the real HTTP boundary.
//!
//! The question this answers is the one that cost an hour on 2026-07-25: *is
//! anything reaching me?* "Nothing arrived" and "something arrived wrong" must
//! not look identical from the console, so every request leaves a line — and the
//! fields on that line are the contract, not the fact that something was
//! written. Every test below reads back what the server actually wrote.

mod common;
use common::logs::LogCapture;
use common::{connect, get, next_text, spawn, spawn_with_blob};

use std::time::Duration;

use bytes::Bytes;
use futures_util::SinkExt;
use radio_scout::BlobStore;
use radio_scout::db::repo::{self, NewCall};
use radio_scout::http_log::REQUEST_ID_HEADER;
use sea_orm::{ConnectionTrait, DatabaseConnection};
use tokio_tungstenite::tungstenite::Message as WsMessage;

/// A Call whose audio really is in the store, so `/api/call/{id}/audio` answers
/// 200 (and 206 for a range) rather than 404.
async fn seed_playable_call(db: &DatabaseConnection, blob: &BlobStore) -> i64 {
    blob.put("ab/clip.wav", Bytes::from_static(b"0123456789"))
        .await
        .expect("put audio");
    repo::insert_call(
        db,
        &NewCall {
            system_ref: 11,
            talkgroup_ref: 54241,
            call_at_ms: 1000,
            object_key: "ab/clip.wav".into(),
            audio_mime: Some("audio/x-wav".into()),
            ..Default::default()
        },
        false,
        0,
    )
    .await
    .expect("insert call")
    .id
}

/// The request line for `path`, of which there must be exactly one.
fn line_for(capture: &LogCapture, path: &str) -> String {
    capture.only_line_containing(&format!("path={path} "))
}

/// Wait for `needle` to appear in the capture. Lines a *connection* emits — the
/// live feed's connect/disconnect — land when the server's task next runs, not
/// when the client's call returns.
async fn wait_for(capture: &LogCapture, needle: &str) -> String {
    for _ in 0..2_000 {
        if let Some(line) = capture.lines_containing(needle).into_iter().next() {
            return line;
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    panic!("{needle:?} never appeared in:\n{}", capture.text());
}

/// Every request leaves one line naming what was asked for and how it went.
#[tokio::test]
async fn a_request_logs_one_line_with_method_path_status_and_duration() {
    let capture = LogCapture::start();
    let (addr, _db, _tmp) = spawn().await;

    let resp = get(&addr, "/api/calls").await;
    assert_eq!(resp.status(), 200);

    let line = line_for(&capture, "/api/calls");
    assert!(line.contains(" INFO "), "{line}");
    assert!(line.contains("method=GET"), "{line}");
    assert!(line.contains("status=200"), "{line}");
    assert!(line.contains("duration_us="), "{line}");
}

/// The chatty classes sit at DEBUG: a media element issues several range
/// requests per Call, a page load pulls a dozen assets, and a packaged
/// deployment probes `/healthz` every few seconds. A Pi must not write an INFO
/// line for each.
#[tokio::test]
async fn asset_audio_and_probe_requests_log_at_debug() {
    let capture = LogCapture::start();
    let tmp = tempfile::tempdir().expect("tempdir");
    let blob = BlobStore::filesystem(tmp.path().join("audio")).expect("blob");
    let (addr, db) = spawn_with_blob(blob.clone(), tmp.path()).await;
    let id = seed_playable_call(&db, &blob).await;
    let audio = format!("/api/call/{id}/audio");

    assert_eq!(get(&addr, "/healthz").await.status(), 200);
    assert_eq!(get(&addr, "/favicon.svg").await.status(), 200);
    assert_eq!(get(&addr, &audio).await.status(), 200);
    let ranged = reqwest::Client::new()
        .get(format!("http://{addr}{audio}"))
        .header("Range", "bytes=2-5")
        .send()
        .await
        .expect("range request");
    assert_eq!(ranged.status(), 206);

    for path in ["/healthz", "/favicon.svg"] {
        let line = line_for(&capture, path);
        assert!(line.contains(" DEBUG "), "{path} should be DEBUG: {line}");
    }
    // Both the whole-object 200 and the ranged 206 — one media element makes
    // several of these per Call, which is the entire reason for the demotion.
    let audio_lines = capture.lines_containing(&format!("path={audio} "));
    assert_eq!(audio_lines.len(), 2, "{audio_lines:#?}");
    assert!(audio_lines[0].contains("status=200"), "{audio_lines:#?}");
    assert!(audio_lines[1].contains("status=206"), "{audio_lines:#?}");
    for line in &audio_lines {
        assert!(line.contains(" DEBUG "), "audio should be DEBUG: {line}");
    }
}

/// The line names the **path**, never the query string. Access codes are a query
/// parameter in rdio-scanner's world and ADR-0008 keeps that shape, so logging
/// the URI whole would make rule 2 — a secret is never logged, at any level, in
/// any form — conditional on which routes happen to exist today.
#[tokio::test]
async fn a_query_string_never_reaches_the_log() {
    let capture = LogCapture::start();
    let (addr, _db, _tmp) = spawn().await;

    let resp = get(&addr, "/api/calls?limit=5&pretend_access_code=hunter2").await;
    assert_eq!(resp.status(), 200);

    let line = line_for(&capture, "/api/calls");
    assert!(line.contains("path=/api/calls "), "the path alone: {line}");
    capture.assert_never_logged("hunter2");
    capture.assert_never_logged("pretend_access_code");
}

/// A page navigation is not an asset: `/archive` has no extension, so the SPA
/// shell stays a notable event while the fetches it triggers do not.
#[tokio::test]
async fn spa_navigation_stays_at_info() {
    let capture = LogCapture::start();
    let (addr, _db, _tmp) = spawn().await;

    assert_eq!(get(&addr, "/archive").await.status(), 200);

    let line = line_for(&capture, "/archive");
    assert!(line.contains(" INFO "), "{line}");
}

/// A rejected request is exactly what "nothing is arriving" needs to be
/// distinguishable from, so a 4xx is never demoted to DEBUG by its class and
/// never silent at INFO.
#[tokio::test]
async fn a_4xx_logs_at_warn_whatever_the_route_class() {
    let capture = LogCapture::start();
    let (addr, _db, _tmp) = spawn().await;

    // An unrouted API path (404 from the SPA fallback) and a chatty-class audio
    // request for a Call that does not exist.
    assert_eq!(get(&addr, "/api/nope").await.status(), 404);
    assert_eq!(get(&addr, "/api/call/999999/audio").await.status(), 404);

    for path in ["/api/nope", "/api/call/999999/audio"] {
        let line = line_for(&capture, path);
        assert!(line.contains(" WARN "), "{path} should be WARN: {line}");
        assert!(line.contains("status=404"), "{line}");
    }
}

/// The 2026-07-25 incident in miniature: every upload 500s and the server must
/// not be the last to know. The class demotion does not apply, and the
/// listener's address is dropped — rule 5 allows it only on a DEBUG line.
#[tokio::test]
async fn a_5xx_logs_at_error_without_a_listener_address() {
    let capture = LogCapture::start();
    let tmp = tempfile::tempdir().expect("tempdir");
    let blob = BlobStore::filesystem(tmp.path().join("audio")).expect("blob");
    let (addr, db) = spawn_with_blob(blob.clone(), tmp.path()).await;
    let id = seed_playable_call(&db, &blob).await;

    // Break the lookup under the handler's feet, the way a missing column did.
    db.execute_unprepared("DROP TABLE calls")
        .await
        .expect("drop calls");

    let audio = format!("/api/call/{id}/audio");
    assert_eq!(get(&addr, &audio).await.status(), 500);

    let line = line_for(&capture, &audio);
    assert!(line.contains(" ERROR "), "{line}");
    assert!(line.contains("status=500"), "{line}");
    assert!(
        !line.contains("client_addr"),
        "a listener's address may not ride a line above DEBUG: {line}"
    );
}

/// Which host sent this is the diagnostic that matters for a recorder, so an
/// ingest line carries its address at INFO (ADR-0011 rule 5).
#[tokio::test]
async fn an_ingest_line_carries_the_recorder_address_at_info() {
    let capture = LogCapture::start();
    let (addr, db, _tmp) = spawn().await;
    repo::create_api_key(&db, "recorder-key", None, None, 0)
        .await
        .expect("api key");

    let form = reqwest::multipart::Form::new()
        .text("key", "recorder-key")
        .text("system", "11")
        .text("talkgroup", "54241")
        .text("timestamp", "1000")
        .part(
            "audio",
            reqwest::multipart::Part::bytes(b"audio-bytes".to_vec())
                .file_name("a.wav")
                .mime_str("audio/x-wav")
                .expect("mime"),
        );
    let resp = reqwest::Client::new()
        .post(format!("http://{addr}/api/call-upload"))
        .multipart(form)
        .send()
        .await
        .expect("upload");
    assert_eq!(resp.status(), 200);

    let line = line_for(&capture, "/api/call-upload");
    assert!(line.contains(" INFO "), "{line}");
    assert!(line.contains("method=POST"), "{line}");
    assert!(line.contains("client_addr=127.0.0.1"), "{line}");
}

/// An upload that never reaches the handler — the body is not multipart, so the
/// extractor rejects it — still leaves a line naming the host that sent it.
/// "Nothing is arriving" and "your recorder is sending garbage" are different
/// problems, and on 2026-07-25 they looked identical from the server.
#[tokio::test]
async fn a_malformed_upload_still_names_the_recorder_that_sent_it() {
    let capture = LogCapture::start();
    let (addr, _db, _tmp) = spawn().await;

    let resp = reqwest::Client::new()
        .post(format!("http://{addr}/api/call-upload"))
        .header("content-type", "text/plain")
        .body("not a multipart body")
        .send()
        .await
        .expect("upload");
    assert_eq!(resp.status(), 400);

    let line = line_for(&capture, "/api/call-upload");
    assert!(line.contains(" WARN "), "{line}");
    assert!(line.contains("status=400"), "{line}");
    assert!(line.contains("client_addr=127.0.0.1"), "{line}");
}

/// The other half of rule 5: a listener's address appears only on a line that is
/// already DEBUG, so a public instance does not accumulate a record of who
/// listened and when at its default verbosity.
#[tokio::test]
async fn a_listener_address_rides_only_a_debug_line() {
    let capture = LogCapture::start();
    let tmp = tempfile::tempdir().expect("tempdir");
    let blob = BlobStore::filesystem(tmp.path().join("audio")).expect("blob");
    let (addr, db) = spawn_with_blob(blob.clone(), tmp.path()).await;
    let id = seed_playable_call(&db, &blob).await;

    let audio = format!("/api/call/{id}/audio");
    assert_eq!(get(&addr, &audio).await.status(), 200);
    assert_eq!(get(&addr, "/api/calls").await.status(), 200);
    assert_eq!(get(&addr, "/archive").await.status(), 200);
    let ws = connect(&addr).await;
    let upgrade = wait_for(&capture, "path=/api/live ").await;

    // Present, on the DEBUG line.
    let audio_line = line_for(&capture, &audio);
    assert!(audio_line.contains(" DEBUG "), "{audio_line}");
    assert!(audio_line.contains("client_addr=127.0.0.1"), "{audio_line}");

    // Absent everywhere a listener is served at INFO.
    for line in [
        line_for(&capture, "/api/calls"),
        line_for(&capture, "/archive"),
        upgrade,
    ] {
        assert!(line.contains(" INFO "), "{line}");
        assert!(
            !line.contains("client_addr"),
            "a listener's address may not ride a line above DEBUG: {line}"
        );
    }
    drop(ws);
}

/// The id is the thread later tickets pull on: #29 hangs a 5xx's correlation ref
/// off it, and an operator holding a client's `x-request-id` can grep the server
/// for everything that happened while serving it.
#[tokio::test]
async fn every_request_carries_a_correlation_id_the_client_can_see() {
    let capture = LogCapture::start();
    let (addr, _db, _tmp) = spawn().await;

    let first = get(&addr, "/api/calls").await;
    let id = first
        .headers()
        .get(REQUEST_ID_HEADER)
        .and_then(|v| v.to_str().ok())
        .expect("x-request-id header")
        .to_owned();
    assert_eq!(id.len(), 16, "16 hex characters: {id}");
    assert!(id.chars().all(|c| c.is_ascii_hexdigit()), "{id}");

    let line = line_for(&capture, "/api/calls");
    assert!(
        line.contains(&format!("request_id={id}")),
        "the line and the header name the same request: {line}"
    );

    let second = get(&addr, "/healthz").await;
    let other = second
        .headers()
        .get(REQUEST_ID_HEADER)
        .and_then(|v| v.to_str().ok())
        .expect("x-request-id header");
    assert_ne!(id, other, "each request gets its own id");
}

/// The live feed is a socket, not a request: it logs its upgrade, then one line
/// when the listener arrives and one when it leaves — and nothing per frame,
/// however much traffic crosses it.
#[tokio::test]
async fn the_live_feed_logs_upgrade_connect_and_disconnect_but_never_a_frame() {
    let capture = LogCapture::start();
    let (addr, _db, _tmp) = spawn().await;

    let mut ws = connect(&addr).await;
    let upgrade = wait_for(&capture, "path=/api/live ").await;
    assert!(upgrade.contains(" INFO "), "{upgrade}");
    assert!(upgrade.contains("status=101"), "{upgrade}");
    let connected = wait_for(&capture, "live-feed listener connected").await;

    // The upgrade and the connection it became share one id, so everything the
    // socket says is attributable to the request that opened it.
    let id = connected
        .split("request_id=")
        .nth(1)
        .and_then(|rest| rest.split('}').next())
        .expect("request_id on the connect line")
        .to_owned();
    assert!(upgrade.contains(&format!("request_id={id}")), "{upgrade}");

    // Traffic in both directions: a subscribe and its ack.
    ws.send(WsMessage::Text(r#"{"t":"sub","all":true}"#.into()))
        .await
        .expect("send sub");
    let ack: serde_json::Value = serde_json::from_str(&next_text(&mut ws).await).expect("ack");
    assert_eq!(ack["t"], "subscribed");

    ws.close(None).await.expect("close");
    let disconnected = wait_for(&capture, "live-feed listener disconnected").await;
    assert!(disconnected.contains(" INFO "), "{disconnected}");
    assert!(disconnected.contains("connected_ms="), "{disconnected}");

    // Three lines for the whole connection, whatever crossed it.
    let lines = capture.lines_containing(&format!("request_id={id}"));
    assert_eq!(
        lines.len(),
        3,
        "one line per frame is a hot loop: {lines:#?}"
    );
}
