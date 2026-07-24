//! Live-feed WebSocket loop (`src/live.rs` `handle_socket`) behaviors that the
//! matcher unit tests can't reach (ticket #26 hardening): malformed-message
//! tolerance, subscription replacement, global `all`, and per-connection
//! filtering across concurrent clients.

mod common;
use common::{Ws, connect, next_text, spawn};

use std::time::Duration;

use futures_util::SinkExt;
use radio_scout::db::repo;
use sea_orm::DatabaseConnection;
use tokio_tungstenite::tungstenite::Message as WsMessage;

/// Send a `sub` message and wait for the server's `subscribed` ack.
async fn subscribe(ws: &mut Ws, body: &str) {
    ws.send(WsMessage::Text(body.into())).await.expect("send");
    let ack = next_text(ws).await;
    assert!(ack.contains("subscribed"), "expected ack, got {ack:?}");
}

/// Is a frame delivered within the window? `false` = correctly filtered.
async fn received(ws: &mut Ws) -> Option<serde_json::Value> {
    match tokio::time::timeout(Duration::from_millis(400), next_text(ws)).await {
        Ok(text) => Some(serde_json::from_str(&text).expect("json")),
        Err(_) => None,
    }
}

async fn post_call(addr: &str, system: i64, talkgroup: i64) {
    let audio = reqwest::multipart::Part::bytes(b"audio".to_vec())
        .file_name("a.wav")
        .mime_str("audio/x-wav")
        .unwrap();
    let form = reqwest::multipart::Form::new()
        .text("key", "test-key")
        .text("system", system.to_string())
        .text("talkgroup", talkgroup.to_string())
        .text("timestamp", (1000 + talkgroup).to_string())
        .part("audio", audio);
    let resp = reqwest::Client::new()
        .post(format!("http://{addr}/api/call-upload"))
        .multipart(form)
        .send()
        .await
        .expect("upload");
    assert_eq!(resp.status(), 200);
}

async fn seed(db: &DatabaseConnection) {
    repo::create_api_key(db, "test-key", None, None, 0)
        .await
        .unwrap();
}

/// A malformed frame (and a valid-JSON-but-unknown-shape frame) must be silently
/// ignored — never acked, never fatal — and the loop keeps serving afterward.
#[tokio::test]
async fn malformed_messages_are_ignored_and_the_loop_survives() {
    let (addr, db, _tmp) = spawn().await;
    seed(&db).await;
    let mut ws = connect(&addr).await;

    // Garbage + a well-formed-but-unknown message: neither should ack.
    ws.send(WsMessage::Text("not json {".into())).await.unwrap();
    ws.send(WsMessage::Text(r#"{"t":"bogus"}"#.into()))
        .await
        .unwrap();

    // The connection is still alive: a real subscribe still acks (and it is the
    // FIRST frame we see — the junk produced no frames).
    subscribe(&mut ws, r#"{"t":"sub","sel":{"11":{"54241":true}}}"#).await;
    post_call(&addr, 11, 54241).await;

    let call = received(&mut ws).await.expect("subscribed call delivered");
    assert_eq!(call["t"], "call");
    assert_eq!(call["call"]["talkgroupRef"], 54241);
}

/// Re-subscribing REPLACES the matrix: the old talkgroup stops matching, the new
/// one starts.
#[tokio::test]
async fn resubscribing_replaces_the_previous_selection() {
    let (addr, db, _tmp) = spawn().await;
    seed(&db).await;
    let mut ws = connect(&addr).await;

    subscribe(&mut ws, r#"{"t":"sub","sel":{"11":{"100":true}}}"#).await;
    subscribe(&mut ws, r#"{"t":"sub","sel":{"11":{"200":true}}}"#).await; // replaces

    // Old talkgroup 100 is no longer subscribed; new talkgroup 200 is. Posting
    // both, the only frame that arrives is 200 (100 would arrive first if the
    // old selection had leaked through).
    post_call(&addr, 11, 100).await;
    post_call(&addr, 11, 200).await;

    let call = received(&mut ws).await.expect("new selection delivered");
    assert_eq!(call["call"]["talkgroupRef"], 200, "replaced, not merged");
}

/// `all:true` is the global monitor-everything subscription (spec story 21).
#[tokio::test]
async fn all_true_receives_any_call() {
    let (addr, db, _tmp) = spawn().await;
    seed(&db).await;
    let mut ws = connect(&addr).await;

    subscribe(&mut ws, r#"{"t":"sub","all":true}"#).await;
    post_call(&addr, 77, 4242).await; // never explicitly selected

    let call = received(&mut ws)
        .await
        .expect("all:true delivers everything");
    assert_eq!(call["call"]["systemRef"], 77);
    assert_eq!(call["call"]["talkgroupRef"], 4242);
}

/// Two clients with different filters are served independently: a call reaches
/// only the client that subscribed to it.
#[tokio::test]
async fn concurrent_clients_are_filtered_independently() {
    let (addr, db, _tmp) = spawn().await;
    seed(&db).await;
    let mut a = connect(&addr).await;
    let mut b = connect(&addr).await;

    subscribe(&mut a, r#"{"t":"sub","sel":{"11":{"100":true}}}"#).await;
    subscribe(&mut b, r#"{"t":"sub","sel":{"11":{"200":true}}}"#).await;

    post_call(&addr, 11, 100).await;

    let to_a = received(&mut a).await.expect("client A subscribed to 100");
    assert_eq!(to_a["call"]["talkgroupRef"], 100);
    assert!(
        received(&mut b).await.is_none(),
        "client B (talkgroup 200) must not receive talkgroup 100"
    );
}
