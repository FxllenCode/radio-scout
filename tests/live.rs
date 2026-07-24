//! Live-feed WebSocket loop (`src/live.rs` `handle_socket`) behaviors that the
//! matcher unit tests can't reach (ticket #26 hardening): malformed-message
//! tolerance, subscription replacement, global `all`, and per-connection
//! filtering across concurrent clients.

mod common;
use common::{Ws, connect, connect_and_hello, next_text, spawn, spawn_with_heartbeat};

use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
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

// ---------------------------------------------------------------------------
// #9 improvements over rdio: hello greeting, heartbeat, reconnect catch-up,
// patch fanout.
// ---------------------------------------------------------------------------

/// Post a call carrying a `patches` array (rdio `patches[]`).
async fn post_call_with_patches(addr: &str, system: i64, talkgroup: i64, patches: &str) {
    let audio = reqwest::multipart::Part::bytes(b"audio".to_vec())
        .file_name("a.wav")
        .mime_str("audio/x-wav")
        .unwrap();
    let form = reqwest::multipart::Form::new()
        .text("key", "test-key")
        .text("system", system.to_string())
        .text("talkgroup", talkgroup.to_string())
        .text("timestamp", (1000 + talkgroup).to_string())
        .text("patches", patches.to_string())
        .part("audio", audio);
    let resp = reqwest::Client::new()
        .post(format!("http://{addr}/api/call-upload"))
        .multipart(form)
        .send()
        .await
        .expect("upload");
    assert_eq!(resp.status(), 200);
}

/// The result of draining WS frames until a predicate matched (or time ran out).
#[derive(Debug, PartialEq, Eq)]
enum Drained {
    /// A frame matching the predicate arrived.
    Matched,
    /// The stream ended (server closed / socket error) before a match.
    Ended,
    /// Neither happened within the budget.
    TimedOut,
}

/// Drain frames — skipping non-matching ones, which also drives tokio-tungstenite's
/// automatic pong — until `want` matches, the stream ends, or `budget` elapses.
async fn drain_until(ws: &mut Ws, budget: Duration, want: impl Fn(&WsMessage) -> bool) -> Drained {
    match tokio::time::timeout(budget, async {
        loop {
            match ws.next().await {
                Some(Ok(msg)) if want(&msg) => return true,
                Some(Ok(_)) => continue,
                Some(Err(_)) | None => return false,
            }
        }
    })
    .await
    {
        Ok(true) => Drained::Matched,
        Ok(false) => Drained::Ended,
        Err(_) => Drained::TimedOut,
    }
}

fn is_ping(msg: &WsMessage) -> bool {
    matches!(msg, WsMessage::Ping(_))
}

fn is_close(msg: &WsMessage) -> bool {
    matches!(msg, WsMessage::Close(_))
}

/// Assert the server heartbeat fired (a Ping arrived). Reading it also keeps the
/// connection alive via the automatic pong.
async fn expect_ping(ws: &mut Ws) {
    assert_eq!(
        drain_until(ws, Duration::from_secs(2), is_ping).await,
        Drained::Matched,
        "expected a heartbeat ping",
    );
}

/// Assert the server closed the connection (a Close frame or a stream end) rather
/// than leaving it hanging.
async fn expect_server_closed(ws: &mut Ws) {
    assert_ne!(
        drain_until(ws, Duration::from_secs(2), is_close).await,
        Drained::TimedOut,
        "server should have closed the connection",
    );
}

/// A client that sends a clean Close is dropped by the server, which
/// reciprocates the close handshake rather than leaking the connection.
#[tokio::test]
async fn client_close_ends_the_connection() {
    let (addr, db, _tmp) = spawn().await;
    seed(&db).await;
    let mut ws = connect(&addr).await;

    ws.send(WsMessage::Close(None)).await.expect("send close");

    expect_server_closed(&mut ws).await;
}

/// On connect the server greets the client with its protocol version and
/// heartbeat cadence (#9) — rdio has no such handshake.
#[tokio::test]
async fn hello_greeting_announces_protocol_and_heartbeat() {
    let (addr, db, _tmp) = spawn().await;
    seed(&db).await;

    let (_ws, hello) = connect_and_hello(&addr).await;
    assert_eq!(hello["t"], "hello");
    assert_eq!(hello["protocol"], 1);
    assert_eq!(hello["heartbeatMs"], 30_000, "default heartbeat period");
}

/// The server pings on the heartbeat interval, and a client that answers (via
/// tokio-tungstenite's auto-pong) stays fully functional across heartbeats.
#[tokio::test]
async fn heartbeat_pings_and_keeps_a_responsive_client_alive() {
    let (addr, db, _tmp) = spawn_with_heartbeat(Duration::from_millis(150)).await;
    seed(&db).await;
    let mut ws = connect(&addr).await;

    expect_ping(&mut ws).await; // heartbeat fired; reading it auto-ponged

    // We answered the ping, so the connection is alive and still delivers.
    subscribe(&mut ws, r#"{"t":"sub","all":true}"#).await;
    post_call(&addr, 11, 54241).await;
    let call = received(&mut ws)
        .await
        .expect("still delivering after a heartbeat");
    assert_eq!(call["call"]["talkgroupRef"], 54241);
}

/// The first heartbeat waits a full period — the server does not ping on connect.
/// A tokio timer can only fire late, never early, so "no ping within the first
/// fraction of a period" is deterministic, not a flaky race.
#[tokio::test]
async fn no_heartbeat_ping_on_connect() {
    let (addr, db, _tmp) = spawn_with_heartbeat(Duration::from_millis(400)).await;
    seed(&db).await;
    let mut ws = connect(&addr).await; // consumes the hello at ~t=0

    // A tokio timer only fires late, never early, so within the first fraction of
    // a period no ping can have arrived — deterministic, not a flaky race.
    assert_eq!(
        drain_until(&mut ws, Duration::from_millis(150), is_ping).await,
        Drained::TimedOut,
        "no heartbeat ping should arrive before the first full period",
    );
}

/// A client that goes silent (never answers the heartbeat) is reaped after two
/// unanswered pings — rdio leaves such half-open connections lingering.
#[tokio::test]
async fn silent_connection_is_reaped_by_the_heartbeat() {
    let heartbeat = Duration::from_millis(100);
    let (addr, db, _tmp) = spawn_with_heartbeat(heartbeat).await;
    seed(&db).await;
    let (mut ws, _hello) = connect_and_hello(&addr).await;

    // Go silent: never read, so no auto-pong is ever sent. Two missed heartbeats
    // later the server must have closed us.
    tokio::time::sleep(heartbeat * 4).await;

    expect_server_closed(&mut ws).await;
}

/// Reconnect catch-up (#9): a client that reconnects with a `since` cursor is
/// backfilled the matching Calls it missed, oldest-first and flagged `catchup`,
/// filtered to its selection — where rdio would have dropped them entirely.
#[tokio::test]
async fn reconnect_catchup_backfills_missed_calls() {
    let (addr, db, _tmp) = spawn().await;
    seed(&db).await;

    // Three calls land while the client is (pretend) disconnected.
    post_call(&addr, 11, 100).await; // id 1
    post_call(&addr, 11, 200).await; // id 2
    post_call(&addr, 11, 300).await; // id 3

    let mut ws = connect(&addr).await;
    // Reconnect: select 100 & 300 with a catch-up cursor from the beginning.
    subscribe(
        &mut ws,
        r#"{"t":"sub","sel":{"11":{"100":true,"300":true}},"since":0}"#,
    )
    .await;

    let first = received(&mut ws).await.expect("catch-up for 100");
    assert_eq!(first["call"]["talkgroupRef"], 100);
    assert_eq!(first["catchup"], true);

    let second = received(&mut ws).await.expect("catch-up for 300");
    assert_eq!(second["call"]["talkgroupRef"], 300);
    assert_eq!(second["catchup"], true);

    // Talkgroup 200 was not selected, so it is not replayed.
    assert!(
        received(&mut ws).await.is_none(),
        "only selected talkgroups are backfilled"
    );
}

/// A fresh subscription with no `since` cursor does not replay history — only
/// live Calls arriving after it flow (the common first-connect case).
#[tokio::test]
async fn fresh_subscription_without_cursor_does_not_replay() {
    let (addr, db, _tmp) = spawn().await;
    seed(&db).await;
    post_call(&addr, 11, 100).await; // exists before the client subscribes

    let mut ws = connect(&addr).await;
    subscribe(&mut ws, r#"{"t":"sub","sel":{"11":{"100":true}}}"#).await;

    // No cursor -> the pre-existing call is not backfilled.
    assert!(
        received(&mut ws).await.is_none(),
        "no `since` cursor means no catch-up"
    );
}

/// Patch fanout (#9, spec story 18): a Call on a Talkgroup the listener didn't
/// select still reaches them when they subscribe to one it's patched to, and the
/// patch list rides the wire for display.
#[tokio::test]
async fn patched_call_reaches_a_subscriber_of_the_patched_talkgroup() {
    let (addr, db, _tmp) = spawn().await;
    seed(&db).await;
    let mut ws = connect(&addr).await;

    // Subscribed to 300 only — NOT the call's own talkgroup 100.
    subscribe(&mut ws, r#"{"t":"sub","sel":{"11":{"300":true}}}"#).await;

    post_call_with_patches(&addr, 11, 100, "[300]").await;

    let call = received(&mut ws).await.expect("patched call delivered");
    assert_eq!(call["call"]["talkgroupRef"], 100);
    assert_eq!(call["call"]["patches"], serde_json::json!([300]));
}
