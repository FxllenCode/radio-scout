//! Walking-skeleton integration test (ticket #1).
//!
//! This is the project's primary test seam (ADR-0009): it brings up the real
//! Axum app in-process against a temp filesystem store, then drives it over its
//! actual HTTP + WebSocket boundary — POSTing a synthetic Call, connecting a WS
//! client, and asserting on the response strings, the stored/served audio, and
//! the live-feed push. It exercises ingest -> store -> live-feed fanout end to end.

use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use radio_scout::{AppState, BlobStore, InMemoryCallRepository, build_app};
use tokio_tungstenite::MaybeTlsStream;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message as WsMessage;

type Ws = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

/// Bring up the real app on an ephemeral port with a temp filesystem store and
/// an in-memory call repository. Returns the base `host:port` and the TempDir
/// (kept alive by the caller so the store isn't deleted mid-test).
async fn spawn_app() -> (String, tempfile::TempDir) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let audio = Arc::new(BlobStore::filesystem(tmp.path().join("audio")).expect("blob store"));
    let calls = Arc::new(InMemoryCallRepository::new());
    let state = AppState::new(audio, calls);
    let app = build_app(state);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("local_addr");
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serve");
    });

    (format!("127.0.0.1:{}", addr.port()), tmp)
}

/// A synthetic rdio-compatible `/api/call-upload` multipart form for the given
/// system/talkgroup, carrying `audio_bytes` as the audio part.
fn call_form(system: i64, talkgroup: i64, audio_bytes: Vec<u8>) -> reqwest::multipart::Form {
    let audio = reqwest::multipart::Part::bytes(audio_bytes)
        .file_name("audio.wav")
        .mime_str("audio/x-wav")
        .expect("mime");
    reqwest::multipart::Form::new()
        .text("key", "test-key")
        .text("system", system.to_string())
        .text("systemLabel", "RSP25MTL")
        .text("talkgroup", talkgroup.to_string())
        .text("talkgroupLabel", "TDB A1")
        .text("talkgroupGroup", "Fire")
        .text("talkgroupTag", "Fire dispatch")
        .text("frequency", "774031250")
        .text("source", "4424000")
        .text("dateTime", "2022-11-29T18:05:38.000Z")
        .part("audio", audio)
}

/// Read the next text frame from the socket, skipping control frames.
async fn next_text(ws: &mut Ws) -> String {
    loop {
        match ws.next().await {
            Some(Ok(WsMessage::Text(t))) => return t.as_str().to_owned(),
            Some(Ok(WsMessage::Ping(_) | WsMessage::Pong(_))) => continue,
            Some(Ok(WsMessage::Close(_))) | None => panic!("ws closed before a text frame"),
            Some(Ok(_)) => continue,
            Some(Err(e)) => panic!("ws error: {e}"),
        }
    }
}

/// The load-bearing rdio-scanner success contract: a valid call POSTed to
/// `/api/call-upload` returns HTTP 200 + `Call imported successfully.`, is
/// stored to the object store, and is served back over the audio endpoint.
#[tokio::test]
async fn ingest_stores_call_and_serves_audio() {
    let (addr, _tmp) = spawn_app().await;
    let audio_bytes = b"RIFF\x00\x00\x00\x00WAVEfake-pcm-audio".to_vec();

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("http://{addr}/api/call-upload"))
        .multipart(call_form(11, 54241, audio_bytes.clone()))
        .send()
        .await
        .expect("upload");

    assert_eq!(resp.status().as_u16(), 200, "success is HTTP 200");
    let body = resp.text().await.expect("body");
    assert!(
        body.contains("Call imported successfully."),
        "recorders match on this exact string; got {body:?}"
    );

    // The first stored call gets internal Id 1; its audio round-trips byte-for-byte.
    let audio_resp = client
        .get(format!("http://{addr}/api/call/1/audio"))
        .send()
        .await
        .expect("audio get");
    assert_eq!(audio_resp.status().as_u16(), 200);
    let served = audio_resp.bytes().await.expect("audio bytes");
    assert_eq!(served.as_ref(), audio_bytes.as_slice(), "audio round-trips");
}

/// Audio is served with HTTP range support (ADR-0002 / #4) — iOS `<audio>` needs
/// it. Full GET is 200 + `Accept-Ranges`; a `Range` request is 206 + the partial
/// bytes + `Content-Range`; an out-of-bounds range is 416.
#[tokio::test]
async fn serves_audio_with_http_range() {
    let (addr, _tmp) = spawn_app().await;
    let audio_bytes = b"0123456789ABCDEFGHIJ".to_vec();
    let client = reqwest::Client::new();

    client
        .post(format!("http://{addr}/api/call-upload"))
        .multipart(call_form(11, 54241, audio_bytes.clone()))
        .send()
        .await
        .expect("upload");
    let url = format!("http://{addr}/api/call/1/audio");

    // Full request.
    let full = client.get(&url).send().await.expect("full get");
    assert_eq!(full.status().as_u16(), 200);
    assert_eq!(full.headers()["accept-ranges"], "bytes");
    assert_eq!(full.bytes().await.unwrap().as_ref(), audio_bytes.as_slice());

    // Range request bytes=4-9 -> 206 with bytes [4, 9].
    let part = client
        .get(&url)
        .header("Range", "bytes=4-9")
        .send()
        .await
        .expect("range get");
    assert_eq!(part.status().as_u16(), 206);
    assert_eq!(
        part.headers()["content-range"],
        format!("bytes 4-9/{}", audio_bytes.len()).as_str()
    );
    assert_eq!(part.bytes().await.unwrap().as_ref(), &audio_bytes[4..=9]);

    // Open-ended suffix and an out-of-bounds range.
    let suffix = client
        .get(&url)
        .header("Range", "bytes=-4")
        .send()
        .await
        .expect("suffix get");
    assert_eq!(suffix.status().as_u16(), 206);
    assert_eq!(suffix.bytes().await.unwrap().as_ref(), &audio_bytes[16..]);

    let bad = client
        .get(&url)
        .header("Range", format!("bytes={}-", audio_bytes.len() + 5))
        .send()
        .await
        .expect("bad range get");
    assert_eq!(bad.status().as_u16(), 416);
}

/// The other load-bearing rdio string: a call with no talkgroup is rejected as
/// incomplete. SDRTrunk health-checks on `incomplete call data: no talkgroup`.
#[tokio::test]
async fn ingest_without_talkgroup_is_incomplete() {
    let (addr, _tmp) = spawn_app().await;

    let form = reqwest::multipart::Form::new()
        .text("key", "test-key")
        .text("system", "11")
        .part(
            "audio",
            reqwest::multipart::Part::bytes(b"x".to_vec())
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

    assert_eq!(resp.status().as_u16(), 417, "incomplete data is HTTP 417");
    let body = resp.text().await.expect("body");
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
    let (addr, _tmp) = spawn_app().await;

    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/api/live"))
        .await
        .expect("ws connect");

    // Subscribe to system 11, talkgroup 54241, and wait for the ack so the POST
    // below can't race ahead of the subscription being applied server-side.
    ws.send(WsMessage::Text(
        r#"{"t":"sub","sel":{"11":{"54241":true}}}"#.into(),
    ))
    .await
    .expect("send sub");
    let ack = next_text(&mut ws).await;
    assert!(ack.contains("subscribed"), "expected sub ack, got {ack:?}");

    reqwest::Client::new()
        .post(format!("http://{addr}/api/call-upload"))
        .multipart(call_form(11, 54241, b"audio-bytes".to_vec()))
        .send()
        .await
        .expect("upload");

    let msg = next_text(&mut ws).await;
    let v: serde_json::Value = serde_json::from_str(&msg).expect("json");
    assert_eq!(v["t"], "call");
    assert_eq!(v["call"]["systemRef"], 11);
    assert_eq!(v["call"]["talkgroupRef"], 54241);
    assert_eq!(v["call"]["talkgroupTag"], "Fire dispatch");
    assert_eq!(v["call"]["audioUrl"], "/api/call/1/audio");
}

/// Server-side filtering (ADR-0004): a client subscribed to a *different*
/// talkgroup must NOT receive the call — bandwidth/battery aren't wasted.
#[tokio::test]
async fn call_is_not_pushed_to_non_matching_subscriber() {
    let (addr, _tmp) = spawn_app().await;

    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/api/live"))
        .await
        .expect("ws connect");

    ws.send(WsMessage::Text(
        r#"{"t":"sub","sel":{"11":{"99999":true}}}"#.into(),
    ))
    .await
    .expect("send sub");
    let ack = next_text(&mut ws).await;
    assert!(ack.contains("subscribed"));

    reqwest::Client::new()
        .post(format!("http://{addr}/api/call-upload"))
        .multipart(call_form(11, 54241, b"audio-bytes".to_vec()))
        .send()
        .await
        .expect("upload");

    // No matching subscription -> no frame should arrive.
    let got = tokio::time::timeout(Duration::from_millis(400), next_text(&mut ws)).await;
    assert!(
        got.is_err(),
        "non-matching subscriber must not receive the call, got {got:?}"
    );
}
