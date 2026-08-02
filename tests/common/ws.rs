//! The live-feed WebSocket side of the harness (ADR-0004).
//!
//! The feed is a real socket, so a test reads frames off it the way a client
//! would. Two shapes of assertion matter and they need different waits: a frame
//! that *should* arrive gets a generous budget (a slow machine must not fail a
//! correct server), and a frame that should *not* arrive gets a short one (that
//! wait is the whole test, so it is paid on every run).

use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

/// A connected live-feed WebSocket client.
pub type Ws = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

/// How long to wait before concluding a frame is *not* coming. Short, because
/// this wait is paid in full on every "must not arrive" assertion — and it only
/// has to outlast an in-process fanout, which is a channel send.
pub const FILTER_BUDGET: Duration = Duration::from_millis(400);

/// How long to wait for a frame the test expects to arrive. Nothing here waits
/// on wall-clock time, so this is pure slack: a frame that hasn't arrived in
/// five seconds isn't coming, and a server that never answers is a failure to
/// report rather than a suite to hang on.
const FRAME_BUDGET: Duration = Duration::from_secs(5);

/// Read the next text frame, skipping control frames; panics if the socket
/// closes, errors, or goes quiet first.
pub async fn next_text(ws: &mut Ws) -> String {
    tokio::time::timeout(FRAME_BUDGET, next_text_unbounded(ws))
        .await
        .expect("no frame arrived within the budget")
}

/// [`next_text`], parsed — every frame on this protocol is JSON.
pub async fn next_json(ws: &mut Ws) -> serde_json::Value {
    let text = next_text(ws).await;
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("frame is not json ({e}): {text:?}"))
}

async fn next_text_unbounded(ws: &mut Ws) -> String {
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

/// Send a `sub` message and wait for the server's `subscribed` ack.
///
/// Waiting for the ack is what keeps a subscribe/upload pair from racing: an
/// upload posted before the server applied the selection would be filtered out,
/// and the test would fail for a reason that has nothing to do with filtering.
pub async fn subscribe(ws: &mut Ws, body: &str) {
    ws.send(WsMessage::Text(body.into()))
        .await
        .expect("send sub");
    let ack = next_text(ws).await;
    assert!(ack.contains("subscribed"), "expected an ack, got {ack:?}");
}

/// The next frame if one arrives within `budget`, else `None` — "was this
/// filtered out?" answered without hanging on the answer.
pub async fn frame_within(ws: &mut Ws, budget: Duration) -> Option<serde_json::Value> {
    match tokio::time::timeout(budget, next_text_unbounded(ws)).await {
        Ok(text) => {
            Some(serde_json::from_str(&text).unwrap_or_else(|e| panic!("not json ({e}): {text:?}")))
        }
        Err(_) => None,
    }
}

/// Assert nothing arrives within `budget` — server-side filtering working
/// (ADR-0004: a phone stops *receiving* what it isn't listening to). The frame
/// that did arrive is in the failure message, since "which one leaked" is the
/// first thing you want to know.
pub async fn no_frame_within(ws: &mut Ws, budget: Duration) {
    if let Some(frame) = frame_within(ws, budget).await {
        panic!("expected no frame within {budget:?}, got {frame}");
    }
}

// ---------------------------------------------------------------------------
// Transport-level assertions: the control frames, not the protocol on top.
// ---------------------------------------------------------------------------

/// The result of draining frames until a predicate matched (or time ran out).
#[derive(Debug, PartialEq, Eq)]
pub enum Drained {
    /// A frame matching the predicate arrived.
    Matched,
    /// The stream ended (server closed / socket error) before a match.
    Ended,
    /// Neither happened within the budget.
    TimedOut,
}

/// Drain frames — skipping non-matching ones, which also drives
/// tokio-tungstenite's automatic pong — until `want` matches, the stream ends,
/// or `budget` elapses.
pub async fn drain_until(
    ws: &mut Ws,
    budget: Duration,
    want: impl Fn(&WsMessage) -> bool,
) -> Drained {
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

/// How long a control-frame assertion waits. Generous, because it only bounds a
/// failure: nothing here waits on wall-clock time.
///
/// There used to be an `expect_ping` beside this, and #94 took it: the heartbeat
/// is a row in the live connection's own table and a run of its loop under a
/// paused clock, so a real socket is never the thing waiting for a ping.
const CONTROL_BUDGET: Duration = Duration::from_secs(2);

/// Assert the server closed the connection (a Close frame or a stream end)
/// rather than leaving it hanging.
pub async fn expect_server_closed(ws: &mut Ws) {
    assert_ne!(
        drain_until(ws, CONTROL_BUDGET, |msg| matches!(msg, WsMessage::Close(_))).await,
        Drained::TimedOut,
        "server should have closed the connection",
    );
}
