//! Walking-skeleton integration test (ticket #1).
//!
//! The first proof that the whole path works end to end: the harness (#21)
//! brings up the real Axum app against a temp filesystem store, then drives it
//! over its actual HTTP + WebSocket boundary — POSTing a synthetic Call,
//! connecting a WS client, and asserting on the response strings, the
//! stored/served audio, and the live-feed push. Ingest -> store -> fanout.

use std::time::Duration;

mod common;
use common::{CallUpload, TestApp, next_json, no_frame_within, subscribe};

/// The Call every test here posts: rdio's full generic dialect, as SDRTrunk
/// sends it.
fn call(system: i64, talkgroup: i64, audio: &[u8]) -> CallUpload {
    CallUpload::new()
        .key("test-key")
        .system(system)
        .talkgroup(talkgroup)
        .audio_named(audio, "audio.wav", "audio/x-wav")
        .set("systemLabel", "RSP25MTL")
        .set("talkgroupLabel", "TDB A1")
        .set("talkgroupGroup", "Fire")
        .set("talkgroupTag", "Fire dispatch")
        .set("frequency", "774031250")
        .set("source", "4424000")
        .set("dateTime", "2022-11-29T18:05:38.000Z")
}

/// The load-bearing rdio-scanner success contract: a valid call POSTed to
/// `/api/call-upload` returns HTTP 200 + `Call imported successfully.`, is
/// stored to the object store, and is served back over the audio endpoint.
#[tokio::test]
async fn ingest_stores_call_and_serves_audio() {
    let app = TestApp::with_key("test-key").await;
    let audio_bytes = b"RIFF\x00\x00\x00\x00WAVEfake-pcm-audio";

    app.upload_ok(call(11, 54241, audio_bytes)).await;

    // The bytes really reached the store, not just the row (ADR-0002)...
    let stored = app.the_call().await;
    assert_eq!(
        app.object_bytes(&stored.object_key).await.as_deref(),
        Some(audio_bytes.as_slice())
    );

    // ...and come back byte-for-byte over the audio endpoint.
    let served = app.get(&format!("/api/call/{}/audio", stored.id)).await;
    assert_eq!(served.status(), 200);
    assert_eq!(
        served.bytes().await.expect("audio bytes").as_ref(),
        audio_bytes.as_slice(),
        "audio round-trips"
    );
}

/// Audio is served with HTTP range support (ADR-0002 / #4) — iOS `<audio>` needs
/// it. Full GET is 200 + `Accept-Ranges`; a `Range` request is 206 + the partial
/// bytes + `Content-Range`; an out-of-bounds range is 416.
#[tokio::test]
async fn serves_audio_with_http_range() {
    let app = TestApp::with_key("test-key").await;
    let audio_bytes = b"0123456789ABCDEFGHIJ";
    app.upload_ok(call(11, 54241, audio_bytes)).await;
    let path = format!("/api/call/{}/audio", app.the_call().await.id);

    // Full request.
    let full = app.get(&path).await;
    assert_eq!(full.status(), 200);
    assert_eq!(full.headers()["accept-ranges"], "bytes");
    assert_eq!(
        full.bytes().await.expect("body").as_ref(),
        audio_bytes.as_slice()
    );

    // Range request bytes=4-9 -> 206 with bytes [4, 9].
    let part = app.get_range(&path, "bytes=4-9").await;
    assert_eq!(part.status(), 206);
    assert_eq!(
        part.headers()["content-range"],
        format!("bytes 4-9/{}", audio_bytes.len()).as_str()
    );
    assert_eq!(
        part.bytes().await.expect("body").as_ref(),
        &audio_bytes[4..=9]
    );

    // Open-ended suffix and an out-of-bounds range.
    let suffix = app.get_range(&path, "bytes=-4").await;
    assert_eq!(suffix.status(), 206);
    assert_eq!(
        suffix.bytes().await.expect("body").as_ref(),
        &audio_bytes[16..]
    );

    let bad = app
        .get_range(&path, &format!("bytes={}-", audio_bytes.len() + 5))
        .await;
    assert_eq!(bad.status(), 416);
}

/// The other load-bearing rdio string: a call with no talkgroup is rejected as
/// incomplete. SDRTrunk health-checks on `incomplete call data: no talkgroup`.
#[tokio::test]
async fn ingest_without_talkgroup_is_incomplete() {
    let app = TestApp::with_key("test-key").await;

    let (status, body) = app
        .upload(
            CallUpload::new()
                .key("test-key")
                .remove("talkgroup")
                .audio(b"x"),
        )
        .await;

    assert_eq!(status, 417, "incomplete data is HTTP 417");
    assert!(
        body.to_lowercase()
            .starts_with("incomplete call data: no talkgroup"),
        "got {body:?}"
    );
}

/// An ingested call is pushed over the live feed to a client subscribed to its
/// system+talkgroup, as a compact `{"t":"call", ...}` message.
#[tokio::test]
async fn ingested_call_is_pushed_to_subscribed_ws_client() {
    let app = TestApp::with_key("test-key").await;
    let (mut ws, hello) = app.connect_ws_with_hello().await;
    assert_eq!(hello["t"], "hello", "the greeting comes first (#9)");

    // Subscribe to system 11, talkgroup 54241, and wait for the ack so the POST
    // below can't race ahead of the subscription being applied server-side.
    subscribe(&mut ws, r#"{"t":"sub","sel":{"11":{"54241":true}}}"#).await;

    app.upload_ok(call(11, 54241, b"audio-bytes")).await;

    let frame = next_json(&mut ws).await;
    assert_eq!(frame["t"], "call");
    assert_eq!(frame["call"]["systemRef"], 11);
    assert_eq!(frame["call"]["talkgroupRef"], 54241);
    assert_eq!(frame["call"]["talkgroupTag"], "Fire dispatch");
    // Spelled out rather than derived from the row: the URL the client will
    // fetch, *and* that the first stored Call gets internal id 1.
    assert_eq!(frame["call"]["audioUrl"], "/api/call/1/audio");
}

/// Server-side filtering (ADR-0004): a client subscribed to a *different*
/// talkgroup must NOT receive the call — bandwidth/battery aren't wasted.
#[tokio::test]
async fn call_is_not_pushed_to_non_matching_subscriber() {
    let app = TestApp::with_key("test-key").await;
    let mut ws = app.connect_ws().await;
    subscribe(&mut ws, r#"{"t":"sub","sel":{"11":{"99999":true}}}"#).await;

    app.upload_ok(call(11, 54241, b"audio-bytes")).await;

    no_frame_within(&mut ws, Duration::from_millis(400)).await;
}
