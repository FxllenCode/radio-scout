//! Live-feed WebSocket loop (`src/live.rs` `handle_socket`) behaviors that the
//! matcher unit tests can't reach (ticket #26 hardening): malformed-message
//! tolerance, subscription replacement, global `all`, and per-connection
//! filtering across concurrent clients.

mod common;
use common::{
    CallUpload, Drained, FILTER_BUDGET, TestApp, Ws, drain_until, expect_ping,
    expect_server_closed, frame_within, subscribe,
};

use std::time::Duration;

use futures_util::SinkExt;
use tokio_tungstenite::tungstenite::Message as WsMessage;

/// Is a frame delivered within the window? `None` = correctly filtered.
async fn received(ws: &mut Ws) -> Option<serde_json::Value> {
    frame_within(ws, FILTER_BUDGET).await
}

/// An app with the live-feed test key registered.
async fn feed_app() -> TestApp {
    TestApp::with_key("test-key").await
}

/// The same, with a heartbeat short enough that reaping is observable in
/// milliseconds rather than the production 30 s (#9).
async fn feed_app_with_heartbeat(heartbeat: Duration) -> TestApp {
    let app = TestApp::builder().heartbeat(heartbeat).spawn().await;
    app.create_api_key("test-key").await;
    app
}

/// Post a Call for `system`/`talkgroup`, timestamped so each talkgroup is its
/// own Call rather than a duplicate of the last.
async fn post_call(app: &TestApp, system: i64, talkgroup: i64) {
    app.upload_ok(live_call(system, talkgroup)).await;
}

/// Post a Call carrying a `patches` array (rdio `patches[]`).
async fn post_call_with_patches(app: &TestApp, system: i64, talkgroup: i64, patches: &str) {
    app.upload_ok(live_call(system, talkgroup).set("patches", patches))
        .await;
}

/// The shape every Call above shares: the live-feed key, a small WAV, and a
/// timestamp derived from the Talkgroup so consecutive posts are distinct Calls
/// rather than duplicates of each other (#5's dedup window).
fn live_call(system: i64, talkgroup: i64) -> CallUpload {
    CallUpload::new()
        .key("test-key")
        .system(system)
        .talkgroup(talkgroup)
        .at(1000 + talkgroup)
        .audio(b"audio")
}

/// A malformed frame (and a valid-JSON-but-unknown-shape frame) must be silently
/// ignored — never acked, never fatal — and the loop keeps serving afterward.
#[tokio::test]
async fn malformed_messages_are_ignored_and_the_loop_survives() {
    let app = feed_app().await;
    let mut ws = app.connect_ws().await;

    // Garbage + a well-formed-but-unknown message: neither should ack.
    ws.send(WsMessage::Text("not json {".into())).await.unwrap();
    ws.send(WsMessage::Text(r#"{"t":"bogus"}"#.into()))
        .await
        .unwrap();

    // The connection is still alive: a real subscribe still acks (and it is the
    // FIRST frame we see — the junk produced no frames).
    subscribe(&mut ws, r#"{"t":"sub","sel":{"11":{"54241":true}}}"#).await;
    post_call(&app, 11, 54241).await;

    let call = received(&mut ws).await.expect("subscribed call delivered");
    assert_eq!(call["t"], "call");
    assert_eq!(call["call"]["talkgroupRef"], 54241);
}

/// Re-subscribing REPLACES the matrix: the old talkgroup stops matching, the new
/// one starts.
#[tokio::test]
async fn resubscribing_replaces_the_previous_selection() {
    let app = feed_app().await;
    let mut ws = app.connect_ws().await;

    subscribe(&mut ws, r#"{"t":"sub","sel":{"11":{"100":true}}}"#).await;
    subscribe(&mut ws, r#"{"t":"sub","sel":{"11":{"200":true}}}"#).await; // replaces

    // Old talkgroup 100 is no longer subscribed; new talkgroup 200 is. Posting
    // both, the only frame that arrives is 200 (100 would arrive first if the
    // old selection had leaked through).
    post_call(&app, 11, 100).await;
    post_call(&app, 11, 200).await;

    let call = received(&mut ws).await.expect("new selection delivered");
    assert_eq!(call["call"]["talkgroupRef"], 200, "replaced, not merged");
}

/// `all:true` is the global monitor-everything subscription (spec story 21).
#[tokio::test]
async fn all_true_receives_any_call() {
    let app = feed_app().await;
    let mut ws = app.connect_ws().await;

    subscribe(&mut ws, r#"{"t":"sub","all":true}"#).await;
    post_call(&app, 77, 4242).await; // never explicitly selected

    let call = received(&mut ws)
        .await
        .expect("all:true delivers everything");
    assert_eq!(call["call"]["systemRef"], 77);
    assert_eq!(call["call"]["talkgroupRef"], 4242);
}

/// **Hold System** over the wire (#11, spec US 11): the client can't enumerate a
/// System's Talkgroups, so it holds one with a `"*"` key and the server keeps
/// filtering out everything else — the point of server-side filtering is that a
/// phone stops *receiving* what it isn't listening to.
#[tokio::test]
async fn a_system_wildcard_holds_the_whole_system() {
    let app = feed_app().await;
    let mut ws = app.connect_ws().await;

    subscribe(&mut ws, r#"{"t":"sub","sel":{"11":{"*":true}}}"#).await;

    post_call(&app, 22, 4242).await; // another system: filtered
    post_call(&app, 11, 909).await; // never named individually: delivered
    let call = received(&mut ws).await.expect("held system delivers");
    assert_eq!(call["call"]["systemRef"], 11);
    assert_eq!(call["call"]["talkgroupRef"], 909);
    assert!(received(&mut ws).await.is_none(), "nothing else follows");
}

/// **Avoid** over the wire (#11, spec US 14): a listener starts all-on, so an
/// avoided Talkgroup is an explicit exception to `all` — and the Calls it would
/// have carried never reach the device at all.
#[tokio::test]
async fn an_explicit_exception_avoids_one_talkgroup_of_an_all_on_selection() {
    let app = feed_app().await;
    let mut ws = app.connect_ws().await;

    subscribe(
        &mut ws,
        r#"{"t":"sub","all":true,"sel":{"11":{"54241":false}}}"#,
    )
    .await;

    post_call(&app, 11, 54241).await; // avoided
    post_call(&app, 11, 999).await; // everything else still plays
    let call = received(&mut ws).await.expect("unavoided call delivers");
    assert_eq!(call["call"]["talkgroupRef"], 999);
    assert!(
        received(&mut ws).await.is_none(),
        "the avoided one never came"
    );
}

/// Two clients with different filters are served independently: a call reaches
/// only the client that subscribed to it.
#[tokio::test]
async fn concurrent_clients_are_filtered_independently() {
    let app = feed_app().await;
    let mut a = app.connect_ws().await;
    let mut b = app.connect_ws().await;

    subscribe(&mut a, r#"{"t":"sub","sel":{"11":{"100":true}}}"#).await;
    subscribe(&mut b, r#"{"t":"sub","sel":{"11":{"200":true}}}"#).await;

    post_call(&app, 11, 100).await;

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

/// A client that sends a clean Close is dropped by the server, which
/// reciprocates the close handshake rather than leaking the connection.
#[tokio::test]
async fn client_close_ends_the_connection() {
    let app = feed_app().await;
    let mut ws = app.connect_ws().await;

    ws.send(WsMessage::Close(None)).await.expect("send close");

    expect_server_closed(&mut ws).await;
}

/// On connect the server greets the client with its protocol version and
/// heartbeat cadence (#9) — rdio has no such handshake.
#[tokio::test]
async fn hello_greeting_announces_protocol_and_heartbeat() {
    let app = feed_app().await;

    let (_ws, hello) = app.connect_ws_with_hello().await;
    assert_eq!(hello["t"], "hello");
    assert_eq!(hello["protocol"], 1);
    assert_eq!(hello["heartbeatMs"], 30_000, "default heartbeat period");
}

/// The server pings on the heartbeat interval, and a client that answers (via
/// tokio-tungstenite's auto-pong) stays fully functional across heartbeats.
#[tokio::test]
async fn heartbeat_pings_and_keeps_a_responsive_client_alive() {
    let app = feed_app_with_heartbeat(Duration::from_millis(150)).await;
    let mut ws = app.connect_ws().await;

    expect_ping(&mut ws).await; // heartbeat fired; reading it auto-ponged

    // We answered the ping, so the connection is alive and still delivers.
    subscribe(&mut ws, r#"{"t":"sub","all":true}"#).await;
    post_call(&app, 11, 54241).await;
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
    let app = feed_app_with_heartbeat(Duration::from_millis(400)).await;
    let mut ws = app.connect_ws().await; // consumes the hello at ~t=0

    // A tokio timer only fires late, never early, so within the first fraction of
    // a period no ping can have arrived — deterministic, not a flaky race.
    assert_eq!(
        drain_until(&mut ws, Duration::from_millis(150), |msg| matches!(
            msg,
            WsMessage::Ping(_)
        ))
        .await,
        Drained::TimedOut,
        "no heartbeat ping should arrive before the first full period",
    );
}

/// A client that goes silent (never answers the heartbeat) is reaped after two
/// unanswered pings — rdio leaves such half-open connections lingering.
#[tokio::test]
async fn silent_connection_is_reaped_by_the_heartbeat() {
    let heartbeat = Duration::from_millis(100);
    let app = feed_app_with_heartbeat(heartbeat).await;
    let (mut ws, _hello) = app.connect_ws_with_hello().await;

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
    let app = feed_app().await;

    // Three calls land while the client is (pretend) disconnected.
    post_call(&app, 11, 100).await; // id 1
    post_call(&app, 11, 200).await; // id 2
    post_call(&app, 11, 300).await; // id 3

    let mut ws = app.connect_ws().await;
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
    let app = feed_app().await;
    post_call(&app, 11, 100).await; // exists before the client subscribes

    let mut ws = app.connect_ws().await;
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
    let app = feed_app().await;
    let mut ws = app.connect_ws().await;

    // Subscribed to 300 only — NOT the call's own talkgroup 100.
    subscribe(&mut ws, r#"{"t":"sub","sel":{"11":{"300":true}}}"#).await;

    post_call_with_patches(&app, 11, 100, "[300]").await;

    let call = received(&mut ws).await.expect("patched call delivered");
    assert_eq!(call["call"]["talkgroupRef"], 100);
    assert_eq!(call["call"]["patches"], serde_json::json!([300]));
}
