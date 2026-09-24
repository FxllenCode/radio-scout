//! **Where a Recorder dials in** (#71, spec US 50) — the socket adapter, and
//! nothing that decides anything.
//!
//! Trunk Recorder's `statusServer` is a **client**: the recorder opens the
//! connection and talks, and the far end never has to say a word. So this is the
//! mirror image of [`crate::live`] — one direction, no protocol to negotiate,
//! and the only frames we send are the pings that keep a proxy from timing the
//! connection out.
//!
//! # The credential is in the query string, and that is deliberate
//!
//! `statusServer` is a bare URL. It carries no headers a recorder can set, so
//! the only two credentials expressible are a query parameter and TR's own
//! `instanceKey`, which rides in the body of every frame. The query parameter
//! wins for three reasons: the connection is authorized **before** it is
//! upgraded, so a refusal is an ordinary `401` that the request middleware logs
//! and counts (#70) rather than a frame sent to a socket already open; it is the
//! **same API key** the recorder already holds for uploading, so there is one
//! roster and one revoke; and `http_log` writes a request's path and **never**
//! its query (ADR-0011 rule 2), so "a recorder's key is never logged" is true by
//! construction rather than by a redaction pass that can be wrong. It is the
//! **Share link**'s argument (#64), one credential along.
//!
//! The key is **not** scoped to a System here, though an [`crate::db::entities::api_key`]
//! row can be: a status connection is about no particular channel — it carries
//! every System the recorder watches — so a scope would have to be either
//! ignored or used to filter somebody's own dashboard into uselessness. What the
//! key answers is *is this a recorder I issued a credential to*, which is the
//! question being asked.
//!
//! # Silence is the only liveness signal worth having
//!
//! A recorder sends a `rates` frame **every three seconds** whatever is
//! happening (`monitor_systems.cc:981`), so thirty seconds of quiet is
//! conclusive. What counts as quiet is deliberately narrow: only a **text**
//! frame resets the counter, never a pong. `websocketpp` answers a ping from
//! inside its own transport, so a recorder whose plugin thread had wedged would
//! go on answering them perfectly — and a liveness check that a dead recorder
//! passes is worse than none, because the dashboard would say it was fine.

use std::time::Duration;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Query, State};
use axum::response::Response;
use serde::Deserialize;
use tokio::time::{Instant, MissedTickBehavior, interval_at};
use tracing::{Instrument, Span, debug, info, warn};

use crate::AppState;
use crate::db::repo;
use crate::failure::{Failure, Reason, Stage};
use crate::recorder::Recorders;
use crate::recorder::dialect::Message as Status;

/// How often the socket is pinged, and how long one unit of silence is.
///
/// Five times the recorder's own three-second cadence, so a single dropped
/// frame is never mistaken for a recorder that has stopped.
const HEARTBEAT: Duration = Duration::from_secs(15);

/// How many quiet heartbeats end the connection.
///
/// Two, so a recorder is reaped after thirty to forty-five seconds of saying
/// nothing — ten to fifteen missed `rates` frames, which is not a hiccup.
const QUIET_PERIODS: u32 = 2;

/// What a dialing recorder presents.
#[derive(Debug, Deserialize)]
pub struct Dial {
    /// The **API key**, in the query string — see the module docs.
    #[serde(default)]
    key: String,
}

/// `GET /api/recorder-status` — a **Recorder** dialing in.
///
/// Authorized **before** the upgrade, so a bad key is an ordinary `401` that the
/// request middleware turns into a line and a counter like every other refusal
/// (#92, #70). rdio-scanner has no equivalent surface at all.
pub async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    Query(dial): Query<Dial>,
) -> Result<Response, Failure> {
    if !repo::authorize_recorder(&state.db, &dial.key)
        .await
        .map_err(Stage::Auth.failed())?
    {
        return Err(Reason::InvalidRecorderKey.into());
    }
    // Carry the request span into the connection task, so everything this socket
    // says stays attributable to the request that opened it (#28).
    let span = Span::current();
    Ok(ws.on_upgrade(move |socket| handle_socket(socket, state).instrument(span)))
}

/// A connection's lifetime, bracketed by two lines and never one per frame
/// (ADR-0011 rule 8) — a recorder speaks twenty times a minute while idle.
async fn handle_socket(socket: WebSocket, state: AppState) {
    info!("recorder connected");
    let connected_at = Instant::now();
    let ended = run(socket, &state.recorders, state.clock).await;
    let connected_ms = connected_at.elapsed().as_millis() as u64;
    // `malformed` is reported **once, when the connection ends** rather than per
    // frame — the Mining sweep's rule (#48), and the same reason: a recorder
    // whose dialect we cannot read at all would otherwise write a line every
    // three seconds for as long as it is plugged in.
    match ended {
        Ended::HungUp { malformed } => {
            info!(connected_ms, malformed, "recorder disconnected");
        }
        // **WARN**, and this is the one thing here an Operator acts on: their
        // recorder is still holding a socket open and has stopped saying
        // anything, which no other signal on the instance would show.
        Ended::WentQuiet { malformed } => {
            warn!(connected_ms, malformed, "recorder stopped reporting");
        }
    }
}

/// How a connection finished.
#[derive(Debug, PartialEq, Eq)]
enum Ended {
    /// The recorder closed, or the socket did.
    HungUp { malformed: u64 },
    /// It held the socket open and stopped talking.
    WentQuiet { malformed: u64 },
}

/// One frame in.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Frame {
    Text(String),
    /// A frame with no meaning to this protocol — a pong, a binary blob.
    /// Deliberately **not** proof of life; see the module docs.
    Quiet,
}

/// Signals that a send failed: the peer is gone.
#[derive(Debug, PartialEq, Eq)]
struct Gone;

/// The transport a recorder connection runs over.
///
/// Axum's `WebSocket` is the only implementation that ships; `mod tests`
/// substitutes its own, which is what lets the reap be proven without a
/// wall-clock sleep — [`crate::live`]'s `Socket` seam, and the #37/#97 rule that
/// a dependency is named at the interface rather than damaged underneath.
trait Frames: Send {
    fn recv(&mut self) -> impl Future<Output = Option<Frame>> + Send;
    fn ping(&mut self) -> impl Future<Output = Result<(), Gone>> + Send;
}

/// What one frame from axum's `WebSocket` means here.
fn frame_of(received: Option<Result<Message, axum::Error>>) -> Option<Frame> {
    match received {
        Some(Ok(Message::Text(text))) => Some(Frame::Text(text.as_str().to_owned())),
        // A close frame, a socket error and a stream that ended are one outcome:
        // the recorder is gone.
        Some(Ok(Message::Close(_))) | Some(Err(_)) | None => None,
        Some(Ok(_)) => Some(Frame::Quiet),
    }
}

impl Frames for WebSocket {
    async fn recv(&mut self) -> Option<Frame> {
        frame_of(WebSocket::recv(self).await)
    }

    async fn ping(&mut self) -> Result<(), Gone> {
        WebSocket::send(self, Message::Ping(bytes::Bytes::new()))
            .await
            .map_err(|_| Gone)
    }
}

/// Pump frames into the fold until the recorder stops.
///
/// The whole of the loop. Everything that decides what a frame *means* is
/// [`crate::recorder::Folded::apply`], which awaits nothing; everything here
/// either waits or does as it is told.
async fn run(mut frames: impl Frames, recorders: &Recorders, clock: crate::Clock) -> Ended {
    let dialed = recorders.arrive(clock.now_ms());
    let mut malformed = 0_u64;
    let mut quiet = 0_u32;

    let mut ticker = interval_at(Instant::now() + HEARTBEAT, HEARTBEAT);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            incoming = frames.recv() => match incoming {
                Some(Frame::Text(text)) => {
                    quiet = 0;
                    match Status::read(&text) {
                        Ok(message) => recorders.apply(dialed.id(), message, clock.now_ms()),
                        // Counted, not logged: see `handle_socket`.
                        Err(_) => malformed += 1,
                    }
                }
                Some(Frame::Quiet) => {}
                None => return Ended::HungUp { malformed },
            },
            _ = ticker.tick() => {
                if quiet >= QUIET_PERIODS {
                    return Ended::WentQuiet { malformed };
                }
                quiet += 1;
                if frames.ping().await.is_err() {
                    return Ended::HungUp { malformed };
                }
            }
        }
    }
}

/// `GET /api/admin/recorders` — the dashboard.
///
/// A **poll**, not a push, and that is the whole of the transport decision: the
/// answer is a value this process already holds, so serving it is a serialize
/// and nothing else. A second live protocol would owe a version, a heartbeat, a
/// reaping rule and a Backfill-shaped question for one admin screen that is read
/// in bursts (#94's machinery, rebuilt for nothing).
///
/// Behind the admin session for [`crate::listeners::history`]'s reason: what an
/// Operator's receivers are doing is the Operator's own business.
pub async fn dashboard(State(state): State<AppState>) -> super::Dashboard {
    let dashboard = state.recorders.dashboard(state.clock.now_ms());
    let recorders = dashboard.recorders.len();
    debug!(recorders, "recorder dashboard read");
    dashboard
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    /// A recorder that says what it was told to and then hangs up.
    struct Scripted {
        frames: VecDeque<Frame>,
        /// `false` once the far end is gone, so a ping fails the way a real
        /// socket's would.
        reachable: bool,
        /// `true` for a recorder that closes the socket once it has said
        /// everything, rather than holding it open.
        closes: bool,
        pings: u32,
    }

    impl Scripted {
        fn saying(frames: Vec<Frame>) -> Self {
            Scripted {
                frames: frames.into(),
                reachable: true,
                closes: false,
                pings: 0,
            }
        }
    }

    impl Frames for Scripted {
        async fn recv(&mut self) -> Option<Frame> {
            match self.frames.pop_front() {
                Some(frame) => Some(frame),
                None if self.closes => None,
                // Nothing left to say and never closing: the recorder that
                // holds a socket open and goes quiet, which is what the reap
                // exists for. `pending` rather than `None`, or the loop would
                // end before a tick could ever fire.
                None => std::future::pending().await,
            }
        }

        async fn ping(&mut self) -> Result<(), Gone> {
            self.pings += 1;
            if self.reachable { Ok(()) } else { Err(Gone) }
        }
    }

    fn text(json: &str) -> Frame {
        Frame::Text(json.to_owned())
    }

    /// A recorder that closes the socket is simply gone, and its row moves to
    /// the departed list.
    #[tokio::test]
    async fn a_recorder_that_hangs_up_ends_the_connection() {
        let recorders = Recorders::default();
        let mut scripted = Scripted::saying(vec![text(
            r#"{"type":"config","instanceId":"butco-pi","sources":[],"systems":[]}"#,
        )]);
        scripted.frames.push_back(Frame::Quiet);
        scripted.closes = true;

        let ended = run(scripted, &recorders, crate::Clock::frozen(1_000)).await;

        assert_eq!(ended, Ended::HungUp { malformed: 0 });
        let dashboard = recorders.dashboard(2_000);
        assert_eq!(dashboard.recorders.len(), 1);
        assert!(!dashboard.recorders[0].connected);
        assert_eq!(dashboard.recorders[0].name.as_deref(), Some("butco-pi"));
    }

    /// **The reap**, proven without a sleep: the shipped fifteen-second period
    /// costs microseconds under `tokio::time::pause`, which is exactly what
    /// naming the transport at an interface buys (#94's argument).
    #[tokio::test(start_paused = true)]
    async fn a_recorder_that_stops_talking_is_reaped() {
        let recorders = Recorders::default();

        let ended = run(
            Scripted::saying(vec![text(r#"{"type":"rates","rates":[]}"#)]),
            &recorders,
            crate::Clock::frozen(1_000),
        )
        .await;

        assert_eq!(ended, Ended::WentQuiet { malformed: 0 });
        assert_eq!(recorders.connected(), 0);
    }

    /// **A pong is not proof of life.** `websocketpp` answers a ping from inside
    /// its own transport, so a recorder whose plugin thread had wedged would go
    /// on answering them perfectly — and a liveness check a dead recorder passes
    /// is worse than none, because the dashboard would say it was fine.
    #[tokio::test(start_paused = true)]
    async fn answering_pings_does_not_keep_a_silent_recorder_alive() {
        let recorders = Recorders::default();
        let pongs = std::iter::repeat_n(Frame::Quiet, 200).collect();

        let ended = run(
            Scripted::saying(pongs),
            &recorders,
            crate::Clock::frozen(1_000),
        )
        .await;

        assert_eq!(ended, Ended::WentQuiet { malformed: 0 });
    }

    /// And a recorder that *is* talking is never reaped, however long it runs.
    #[tokio::test(start_paused = true)]
    async fn a_talking_recorder_is_never_reaped() {
        let recorders = Recorders::default();
        // Far more frames than the reap could ever survive, arriving with no
        // wait between them — the loop takes them all before a tick can fire,
        // which is the point: what matters is that each one resets the counter.
        let chatter = std::iter::repeat_n(text(r#"{"type":"rates","rates":[]}"#), 500).collect();

        let ended = run(
            Scripted::saying(chatter),
            &recorders,
            crate::Clock::frozen(1_000),
        )
        .await;

        assert_eq!(ended, Ended::WentQuiet { malformed: 0 });
        // It only ended because the script ran out; every frame in it was read.
        assert_eq!(recorders.dashboard(2_000).recorders.len(), 1);
    }

    /// A frame we cannot read is **counted**, never logged one at a time: a
    /// recorder speaking a dialect this release does not know would otherwise
    /// write a line every three seconds for as long as it is plugged in
    /// (ADR-0011 rule 8).
    #[tokio::test(start_paused = true)]
    async fn unreadable_frames_are_counted_and_the_connection_survives_them() {
        let recorders = Recorders::default();

        let ended = run(
            Scripted::saying(vec![
                text("not json at all"),
                text(r#"{"no":"type"}"#),
                text(r#"{"type":"rates","rates":[{"id":"0","decoderate":"12"}]}"#),
            ]),
            &recorders,
            crate::Clock::frozen(1_000),
        )
        .await;

        assert_eq!(ended, Ended::WentQuiet { malformed: 2 });
    }

    /// A ping that cannot be sent ends the connection then and there, rather
    /// than waiting out the reap on a socket already known to be gone.
    #[tokio::test(start_paused = true)]
    async fn a_failed_ping_ends_it() {
        let recorders = Recorders::default();
        let mut scripted = Scripted::saying(vec![]);
        scripted.reachable = false;

        let ended = run(scripted, &recorders, crate::Clock::frozen(1_000)).await;

        assert_eq!(ended, Ended::HungUp { malformed: 0 });
    }

    /// The translation from axum's frames, as a table — every message kind is a
    /// case stated here rather than one a real client would have to be made to
    /// send.
    #[test]
    fn axums_frames_translate() {
        assert_eq!(
            frame_of(Some(Ok(Message::Text("hi".into())))),
            Some(Frame::Text("hi".into()))
        );
        assert_eq!(frame_of(Some(Ok(Message::Close(None)))), None);
        assert_eq!(frame_of(None), None);
        assert_eq!(
            frame_of(Some(Ok(Message::Pong(bytes::Bytes::new())))),
            Some(Frame::Quiet)
        );
        assert_eq!(
            frame_of(Some(Ok(Message::Binary(bytes::Bytes::new())))),
            Some(Frame::Quiet)
        );
        assert!(frame_of(Some(Err(axum::Error::new(std::io::Error::other("x"))))).is_none());
    }
}
