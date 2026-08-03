//! The live feed: a raw WebSocket (ADR-0004) that pushes Call metadata to
//! subscribed Listeners. Audio never rides the socket — only compact JSON.
//!
//! Ticket #9 turned the skeleton's broadcast+filter into the full protocol, and
//! deliberately *improves* on rdio-scanner rather than cloning it:
//!
//! - **Patch fanout** — a Call reaches a subscriber of any Talkgroup it's patched
//!   to, not just its own (rdio's `IsEnabled`, plus we carry `patches[]` on the
//!   wire so the client can display cross-patched traffic).
//! - **Access scope** — every delivery is gated by both the Selection and an
//!   access scope (ADR-0008). v1 listening is open ([`AccessScope::All`]); the
//!   restricted variant is the v2 access-code seam.
//! - **Heartbeat + dead-connection reaping** — rdio has no heartbeat of its own.
//!   The server pings on an interval and reaps half-open connections, keeping
//!   proxies warm and freeing resources promptly.
//! - **Backfill** — rdio silently drops any Call that arrives while a Listener is
//!   briefly disconnected (the core mobile pain). A reconnecting client sends the
//!   last emission it saw as `since`; the server backfills what it missed.
//! - **`hello` greeting, `lagged` and `gap` notices** — the server announces its
//!   protocol version and heartbeat cadence on connect, tells a lagging client
//!   how many Calls it skipped, and says so when a Backfill could not reach back
//!   as far as the Listener asked (archive search, #13, is where the rest lives).
//!
//! ## The connection is a state machine; the socket is an adapter (#94)
//!
//! [`Connection`] is the whole of the protocol: `on(`[`Event`]`) -> Vec<`[`Action`]`>`,
//! with the Selection, the access scope and the heartbeat as its state. Nothing
//! in it awaits, opens a socket or reads a database — so every row of the table
//! (subscribe, delivery, Backfill, heartbeat, reaping, the push claim, hanging
//! up) is a value a test constructs and a value it asserts on.
//!
//! [`run_connection`] is the adapter: it pumps events in and carries actions out
//! over a [`Socket`], which axum's `WebSocket` implements and a test substitutes.
//! That is what makes reaping provable without a wall-clock sleep — a tick is an
//! event, and with `tokio::time::pause` a thirty-second period costs nothing.
//! Before this, the ninety-line loop around three tested decisions had no test of
//! its own, and the only way to watch a connection be reaped was to shorten the
//! shipped heartbeat from outside and then sleep through it.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;

use axum::extract::State;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::response::Response;
use serde::Deserialize;
use tokio::sync::broadcast;
use tokio::time::{Instant, MissedTickBehavior, interval_at};
use tracing::{Instrument, Span, debug, info, warn};

use crate::AppState;
use crate::call::{Emission, StoredCall};
use crate::db::repo;
use crate::selection::Selection;

/// Live-feed protocol version, announced in the `hello` frame. Bumped on a
/// breaking wire change so clients can negotiate.
///
/// **2** since #94: `since` is an **emission**, not a Call id, and every `call`
/// frame carries the `seq` a client is meant to hand back. The two numberings
/// coincide on an instance that has never held a Call back, so an unupgraded
/// client keeps working today — and stops being correct the moment a **Delay**
/// (#73) lands, which is what a version is for.
const PROTOCOL_VERSION: u32 = 2;

/// Channel capacity for the fanout broadcast. Ample for the low-hundreds of
/// listeners this targets; a slow client that lags is told and skipped forward.
const LIVE_FEED_CAPACITY: usize = 1024;

/// Server heartbeat period. Long enough to be negligible on a Pi, short enough
/// to reap a dead connection within ~1 minute — one ping goes out, and if it's
/// still unanswered a period later (two missed intervals) the peer is reaped.
///
/// A constant rather than a knob since #94: the only thing that ever varied it
/// was a test that needed a reap to happen inside a sleep it could afford, and
/// reaping is now a row in [`Connection`]'s table.
const HEARTBEAT: Duration = Duration::from_secs(30);

/// Upper bound on Calls replayed to a reconnecting client (**Backfill**, #9). A
/// client returning after a long gap gets a recent slice, is told its history has
/// a gap, and falls back to archive search (#13) for the rest — the live socket
/// never replays the world.
const BACKFILL_MAX_CALLS: u64 = 100;

/// A Call as it went out on the live feed: the Call itself, and the **emission**
/// it was sent as (#94).
///
/// The pair travels together because a Listener's cursor is the emission and the
/// frame carrying it is built from the Call — separating them would mean every
/// follower of the fanout holding two things that have to stay in step.
#[derive(Clone, Debug)]
pub struct Emitted {
    /// Where this went in the emission sequence.
    pub seq: Emission,
    pub call: Arc<StoredCall>,
}

/// A clonable handle to the live-feed fanout. Cloning shares one channel.
#[derive(Clone)]
pub struct LiveFeed {
    tx: broadcast::Sender<Emitted>,
    /// The next emission to hand out. Shared by every clone, because there is one
    /// sequence per archive and every clone speaks for the same one.
    next: Arc<AtomicI64>,
}

impl LiveFeed {
    /// A fanout that has emitted nothing.
    pub fn new() -> Self {
        let (tx, _rx) = broadcast::channel(LIVE_FEED_CAPACITY);
        LiveFeed {
            tx,
            next: Arc::new(AtomicI64::new(1)),
        }
    }

    /// Continue the emission sequence after `latest`, which is what the archive
    /// already holds ([`repo::latest_emission`]).
    ///
    /// Called once, at boot. A sequence that started over at `1` on every restart
    /// would hand new Calls numbers a Listener's cursor is already past, so their
    /// next reconnect would backfill nothing until the counter had climbed back
    /// over the archive — a hole that opens on restart and closes on its own,
    /// which is the hardest kind to be told about.
    pub fn resume_from(&self, latest: Emission) {
        self.next.store(latest + 1, Ordering::Relaxed);
    }

    /// Take the next emission in the sequence.
    pub(crate) fn next_emission(&self) -> Emission {
        self.next.fetch_add(1, Ordering::Relaxed)
    }

    /// A receiver on the fanout. Every live-feed connection takes one, and so
    /// does the Web Push sender (#16) — a notification is decided from the same
    /// broadcast a socket is served from.
    pub fn subscribe(&self) -> broadcast::Receiver<Emitted> {
        self.tx.subscribe()
    }

    /// Publish an emitted Call to all connected listeners. Sending with no
    /// receivers is not an error — it just means nobody is connected.
    ///
    /// Deliberately **not public**: [`crate::AppState::publish`] is the way in,
    /// because the Web Push sender follows this fanout and has to take on the
    /// Call as work owed *before* it is handed out (#93). Reaching the fanout
    /// directly would publish a Call the sender is never recorded as owing, and
    /// the only symptom would be a test that asserts "nothing was notified"
    /// passing whether or not that was true.
    pub(crate) fn publish(&self, emitted: Emitted) {
        let _ = self.tx.send(emitted);
    }
}

impl Default for LiveFeed {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Access scope (ADR-0008)
// ---------------------------------------------------------------------------

/// A Listener's Talkgroup access within one System (the v2 access-code shape).
#[derive(Debug, Clone)]
pub enum TalkgroupScope {
    /// Every Talkgroup in the System.
    All,
    /// Only these Talkgroup Refs.
    Only(HashSet<i64>),
}

/// A connection's access scope (ADR-0008). A Call is delivered only when
/// **both** the Selection and the access scope admit it — live and in a
/// **Backfill** alike, because a restriction that held on only one of the two
/// paths would hand the archive to anyone who reconnected with a cursor.
///
/// **An input to the connection, not a constant** (#94): [`Connection::new`]
/// takes one, so a test can open a restricted connection and assert what it does
/// and does not receive. Production hands it [`AccessScope::All`], because
/// nothing grants a scope until #68 resolves an access code to one — so "an
/// instance with no codes behaves exactly as today" holds by construction rather
/// than by inspection.
#[derive(Debug, Clone)]
pub enum AccessScope {
    /// Full access (v1 default; scope `"*"`).
    All,
    /// Restricted to specific Systems, each optionally down to specific
    /// Talkgroups (v2; scope `[{id, talkgroups}]`).
    Systems(HashMap<i64, TalkgroupScope>),
}

impl AccessScope {
    /// May this scope hear `(system_ref, talkgroup_ref)`?
    fn permits(&self, system_ref: i64, talkgroup_ref: i64) -> bool {
        match self {
            AccessScope::All => true,
            AccessScope::Systems(systems) => match systems.get(&system_ref) {
                None => false,
                Some(TalkgroupScope::All) => true,
                Some(TalkgroupScope::Only(talkgroups)) => talkgroups.contains(&talkgroup_ref),
            },
        }
    }
}

// ---------------------------------------------------------------------------
// The connection: a table of state and event to actions (#94)
// ---------------------------------------------------------------------------

/// Everything that can reach a live-feed connection.
///
/// Four sources, one enum: the client, the fanout, the clock, and the answer to
/// what the connection itself asked for ([`Event::Backfilled`]). Naming them is
/// what turns the loop into a dispatch rather than a place behaviour hides.
pub(crate) enum Event<'a> {
    /// The connection has just been accepted.
    Opened,
    /// A text frame from the client — the only frame that carries protocol.
    Text(&'a str),
    /// An inbound frame with nothing to say: a pong, a ping, binary. It still
    /// proves the peer is there, which is the whole of its meaning.
    Quiet,
    /// A Call went out on the fanout — or this connection fell behind it.
    Broadcast(Result<Emitted, broadcast::error::RecvError>),
    /// The heartbeat period elapsed.
    Tick,
    /// The **Backfill** this connection asked for, read and handed back.
    Backfilled(Backfill),
    /// The peer is gone: a close frame, a socket error, a stream that ended.
    Gone,
}

/// What the adapter is to do about an [`Event`].
///
/// Deliberately small, and deliberately *ordered*: a list is a sequence, so
/// "release the push claim before taking the next one" is a property of the
/// value this returns rather than a comment above two statements that could be
/// swapped without a test noticing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Action {
    /// Send this text frame to the client.
    Send(String),
    /// Send a heartbeat ping.
    Ping,
    /// Read the **Backfill** from this cursor and feed it back as
    /// [`Event::Backfilled`].
    Backfill(Emission),
    /// Let go of the push subscription this connection was standing in for.
    Release,
    /// Stand in for the push subscription this token names (#16) — while the
    /// socket is open its Listener is demonstrably listening, and is not
    /// notified.
    Claim(String),
    /// End the connection.
    Close,
}

/// A page of the archive read for a reconnecting Listener.
#[derive(Debug, Clone)]
pub(crate) struct Backfill {
    /// The cursor it was read from — carried back so that what is logged and
    /// what is sent both name the gap they are about.
    since: Emission,
    /// The Calls, oldest emission first, each with the emission it went out as.
    calls: Vec<Emitted>,
    /// The page hit [`BACKFILL_MAX_CALLS`], so there are Calls the Listener
    /// missed that it could not reach back far enough to carry.
    truncated: bool,
}

impl Backfill {
    /// A Backfill that could not be read. Empty and untruncated — which is a
    /// claim about *this page*, not about the Listener's history, and the
    /// adapter has already said out loud why it is empty.
    fn unreadable(since: Emission) -> Self {
        Backfill {
            since,
            calls: Vec::new(),
            truncated: false,
        }
    }
}

/// One live-feed connection, as a state machine.
///
/// Its state is the Listener's **Selection**, their access scope, and where the
/// heartbeat has got to. Everything else about a connection — the socket, the
/// database, the push claim it holds — belongs to the adapter, so that this can
/// be driven event by event with nothing running underneath it.
pub(crate) struct Connection {
    sub: Selection,
    scope: AccessScope,
    heartbeat: Heartbeat,
}

impl Connection {
    /// A fresh connection: nothing selected, and this access scope.
    ///
    /// The heartbeat period is [`HEARTBEAT`] and is not a parameter. It was one
    /// for about an hour, and every call site passed the same constant — which
    /// is the shape of knob #94 has just finished deleting from `Wiring`, one
    /// layer down and not even varying.
    pub(crate) fn new(scope: AccessScope) -> Self {
        Connection {
            sub: Selection::default(),
            scope,
            heartbeat: Heartbeat::default(),
        }
    }

    /// The table: what this connection does about one event.
    pub(crate) fn on(&mut self, event: Event<'_>) -> Vec<Action> {
        match event {
            Event::Opened => vec![Action::Send(hello_frame(HEARTBEAT))],
            Event::Text(text) => {
                // Any inbound frame proves the peer is there.
                self.heartbeat.on_activity();
                self.on_text(text)
            }
            Event::Quiet => {
                self.heartbeat.on_activity();
                Vec::new()
            }
            Event::Broadcast(result) => self.on_broadcast(result),
            Event::Tick => self.on_tick(),
            Event::Backfilled(backfill) => self.on_backfilled(backfill),
            Event::Gone => vec![Action::Close],
        }
    }

    /// Does this connection receive `call`? The shared Selection rule
    /// ([`crate::selection`]), gated further by the connection's access scope —
    /// so a Talkgroup the Listener selected but may not hear delivers nothing.
    fn wants(&self, call: &StoredCall) -> bool {
        self.sub.reaches(call, |system_ref, talkgroup_ref| {
            self.scope.permits(system_ref, talkgroup_ref)
        })
    }

    /// Apply a client text message. Malformed and unknown frames are ignored —
    /// the protocol is versioned and expected to grow, and a frame nobody
    /// understands must never end a connection.
    fn on_text(&mut self, text: &str) -> Vec<Action> {
        let Ok(ClientMessage::Sub {
            sel,
            all,
            since,
            push,
        }) = serde_json::from_str::<ClientMessage>(text)
        else {
            return Vec::new();
        };
        self.sub = Selection { sel, all };

        let mut actions = Vec::with_capacity(4);
        // **Released before the next claim is taken.** Re-subscribing with the
        // subscription this socket already holds would otherwise take it and
        // then immediately hand it back as the old claim dropped — leaving a
        // Listener with the feed open being notified about Calls they are
        // watching arrive. The order is a property of this value, so it is
        // asserted rather than reviewed.
        actions.push(Action::Release);
        if let Some(token) = push {
            actions.push(Action::Claim(token));
        }
        // Protocol detail, so DEBUG (ADR-0011 rule 7): a Listener re-subscribes
        // every time they toggle a Talkgroup. The shape of the Selection, never
        // its contents — what someone listens to is theirs (rule 5's spirit).
        debug!(
            systems = self.sub.sel.len(),
            all,
            backfill = since.is_some(),
            "live-feed subscription updated"
        );
        // Acked before the Backfill, so a client is never sent history for a
        // Selection it has not yet been told is live.
        actions.push(Action::Send(subscribed_frame().to_string()));
        if let Some(since) = since {
            actions.push(Action::Backfill(since));
        }
        actions
    }

    /// What this connection does with a fanout result.
    fn on_broadcast(&self, result: Result<Emitted, broadcast::error::RecvError>) -> Vec<Action> {
        match result {
            Ok(emitted) if self.wants(&emitted.call) => {
                vec![Action::Send(call_frame(&emitted))]
            }
            Ok(_) => Vec::new(),
            // A slow client fell behind the fanout: tell it how many Calls it
            // missed so it can refetch from the archive (#13) rather than
            // silently losing them (rdio just drops them). The operator is told
            // too — a Listener that cannot keep up is a real symptom, and the
            // count is the measure of it (#29). Rare by construction: the
            // channel holds 1024 Calls.
            Err(broadcast::error::RecvError::Lagged(skipped)) => {
                warn!(skipped, "live-feed listener lagged behind the fanout");
                vec![Action::Send(lagged_frame(skipped))]
            }
            Err(broadcast::error::RecvError::Closed) => vec![Action::Close],
        }
    }

    /// A heartbeat period elapsed: ping, or reap what never answered the last
    /// one.
    fn on_tick(&mut self) -> Vec<Action> {
        match self.heartbeat.on_tick() {
            Beat::Ping => vec![Action::Ping],
            // A half-open connection is a Listener who stopped hearing anything
            // and doesn't know it — worth saying out loud, and the thing rdio
            // leaves lingering in silence. Bounded by connections, not frames
            // (ADR-0011 rule 8).
            Beat::Dead => {
                warn!(
                    heartbeat_ms = HEARTBEAT.as_millis() as u64,
                    "live-feed listener reaped after an unanswered heartbeat"
                );
                vec![Action::Close]
            }
        }
    }

    /// Deliver a **Backfill**: the Calls this Listener missed and may hear,
    /// oldest first, each flagged so the client files it as history rather than
    /// as fresh live activity.
    fn on_backfilled(&self, backfill: Backfill) -> Vec<Action> {
        let mut actions = Vec::with_capacity(backfill.calls.len() + 1);
        // The gap comes **first**, because the bound keeps the *newest* page:
        // whatever was dropped is older than everything about to arrive. Told at
        // all because a silent truncation is indistinguishable from having
        // missed nothing, which is the one thing a Listener must not have to
        // guess about their own history (CONTEXT.md, **Backfill**).
        if backfill.truncated {
            actions.push(Action::Send(gap_frame(backfill.since)));
        }
        actions.extend(
            backfill
                .calls
                .iter()
                .filter(|emitted| self.wants(&emitted.call))
                .map(|emitted| Action::Send(catchup_frame(emitted))),
        );
        // One line per reconnect, never one per Call (ADR-0011 rule 8).
        debug!(
            since = backfill.since,
            read = backfill.calls.len(),
            sent = actions.len() - usize::from(backfill.truncated),
            truncated = backfill.truncated,
            "live-feed Backfill sent"
        );
        actions
    }
}

/// The server-side heartbeat state machine. Each tick, if the previous ping went
/// unanswered the connection is declared dead; otherwise a fresh ping is sent.
/// Any inbound frame (pong, message, …) counts as liveness.
#[derive(Debug, Default)]
struct Heartbeat {
    awaiting_pong: bool,
}

/// What a heartbeat tick decides.
#[derive(Debug, PartialEq, Eq)]
enum Beat {
    /// Send a ping and expect a pong before the next tick.
    Ping,
    /// The previous ping went unanswered — reap the connection.
    Dead,
}

impl Heartbeat {
    fn on_tick(&mut self) -> Beat {
        if self.awaiting_pong {
            Beat::Dead
        } else {
            self.awaiting_pong = true;
            Beat::Ping
        }
    }

    fn on_activity(&mut self) {
        self.awaiting_pong = false;
    }
}

// ---------------------------------------------------------------------------
// The wire
// ---------------------------------------------------------------------------

/// Messages a client sends to the server.
#[derive(Debug, Deserialize)]
#[serde(tag = "t")]
enum ClientMessage {
    /// Replace the Selection, optionally with a **Backfill** cursor.
    #[serde(rename = "sub")]
    Sub {
        #[serde(default)]
        sel: HashMap<String, HashMap<String, bool>>,
        #[serde(default)]
        all: bool,
        /// The last **emission** the client received (#94). When present, the
        /// server backfills the Calls emitted after it before resuming live.
        ///
        /// An emission rather than a Call id, because they are two orderings of
        /// the same Calls and a **Delay** (#73) pulls them apart: a Call held
        /// back is stored under a low id and emitted after Calls already sent,
        /// so a cursor over ids would step past exactly the Call a policy
        /// delayed.
        #[serde(default)]
        since: Option<Emission>,
        /// The token of the Listener's push subscription (#16), if they have
        /// one. While this socket is open they are demonstrably listening, so
        /// the push sender leaves them alone; when it closes — a shut tab, or
        /// the heartbeat reaping a phone iOS suspended — notifications take
        /// over. A **token**, not an Id: an Id is sequential, and a client that
        /// could name any subscription could silence any Listener.
        #[serde(default)]
        push: Option<String>,
    },
}

/// The greeting sent on connect: protocol version + heartbeat cadence, so the
/// client can (re)subscribe and time its own reconnect logic.
fn hello_frame(heartbeat: Duration) -> String {
    serde_json::json!({
        "t": "hello",
        "protocol": PROTOCOL_VERSION,
        "heartbeatMs": heartbeat.as_millis() as u64,
    })
    .to_string()
}

/// The ack confirming a subscription is live.
fn subscribed_frame() -> &'static str {
    r#"{"t":"subscribed"}"#
}

/// A live Call push, carrying the **emission** it went out as (#94) — the cursor
/// the Listener hands back as `since` on their next reconnect.
///
/// Beside the Call rather than inside it: an emission is a property of *this
/// Call going out*, not of the Call, and the archive's own view of a Call
/// (`GET /api/calls`) has no emission to speak of.
fn call_frame(emitted: &Emitted) -> String {
    serde_json::json!({ "t": "call", "call": emitted.call, "seq": emitted.seq }).to_string()
}

/// A **Backfill** Call push — the same shape as a live one, flagged so the
/// client can enqueue it as history.
fn catchup_frame(emitted: &Emitted) -> String {
    serde_json::json!({
        "t": "call",
        "call": emitted.call,
        "seq": emitted.seq,
        "catchup": true,
    })
    .to_string()
}

/// A notice that the client lagged the fanout and `skipped` Calls were dropped.
fn lagged_frame(skipped: u64) -> String {
    serde_json::json!({ "t": "lagged", "skipped": skipped }).to_string()
}

/// A notice that a **Backfill** could not reach back as far as the Listener
/// asked: there are Calls after `since` this page could not carry, and only
/// archive search (#13) can fill them in.
fn gap_frame(since: Emission) -> String {
    serde_json::json!({ "t": "gap", "since": since }).to_string()
}

// ---------------------------------------------------------------------------
// The adapter
// ---------------------------------------------------------------------------

/// One frame arriving from the peer, as the table cares about it.
#[derive(Debug, PartialEq, Eq)]
enum Inbound {
    Text(String),
    /// A frame with no protocol meaning — ping, pong, binary.
    Quiet,
}

/// One frame going to the peer.
enum Outbound {
    Text(String),
    Ping,
}

/// Signals that a socket send failed — the peer is gone, so the caller ends the
/// connection loop. A named marker beats a bare `Err(())` for intent.
#[derive(Debug, PartialEq, Eq)]
struct Disconnected;

/// The transport a connection runs over.
///
/// Axum's `WebSocket` is the only implementation that ships; a test substitutes
/// its own, which is what lets the loop — the ticker, the ordering of the
/// greeting, a reap — be driven in-process with no port, no network and no
/// wall-clock sleep. Named at the interface rather than faked underneath it,
/// which is the #37/#97 rule.
trait Socket: Send {
    /// The next frame, or `None` once the peer is gone — a close frame, a socket
    /// error and a stream that ended are one outcome as far as the table is
    /// concerned.
    fn recv(&mut self) -> impl Future<Output = Option<Inbound>> + Send;

    /// Send one frame.
    fn send(&mut self, message: Outbound) -> impl Future<Output = Result<(), Disconnected>> + Send;
}

/// What one frame from axum's `WebSocket` means to the table.
///
/// A pure translation rather than a `match` inside the `impl` below, so every
/// message kind is a case a test states instead of a frame it would have to
/// arrange for a real client to send.
fn inbound_of(received: Option<Result<Message, axum::Error>>) -> Option<Inbound> {
    match received {
        Some(Ok(Message::Text(text))) => Some(Inbound::Text(text.as_str().to_owned())),
        // A close frame, a socket error and a stream that ended are one outcome
        // as far as the table is concerned: the peer is gone.
        Some(Ok(Message::Close(_))) | Some(Err(_)) | None => None,
        // Ping, pong, binary: liveness, and nothing else.
        Some(Ok(_)) => Some(Inbound::Quiet),
    }
}

impl From<Outbound> for Message {
    fn from(message: Outbound) -> Self {
        match message {
            Outbound::Text(text) => Message::Text(text.into()),
            Outbound::Ping => Message::Ping(bytes::Bytes::new()),
        }
    }
}

impl Socket for WebSocket {
    async fn recv(&mut self) -> Option<Inbound> {
        inbound_of(WebSocket::recv(self).await)
    }

    async fn send(&mut self, message: Outbound) -> Result<(), Disconnected> {
        WebSocket::send(self, message.into())
            .await
            .map_err(|_| Disconnected)
    }
}

/// `GET /api/live` — upgrade to a WebSocket and run the per-connection loop.
pub async fn ws_handler(ws: WebSocketUpgrade, State(state): State<AppState>) -> Response {
    // v1 listening is open (ADR-0008), so every connection is unrestricted. This
    // is the one line #68 replaces: resolve an access code to a scope here, and
    // everything below already honours it, live and in a Backfill alike.
    let scope = AccessScope::All;
    // `on_upgrade` runs the connection in a task of its own, which would
    // otherwise lose the request span the upgrade was logged under (#28).
    // Carrying it means everything this socket says stays attributable to the
    // request that opened it.
    let span = Span::current();
    ws.on_upgrade(move |socket| handle_socket(socket, state, scope).instrument(span))
}

/// A connection's lifetime, bracketed by the two lines that are the socket's
/// answer to the request log (#28): one when a Listener arrives and one when it
/// leaves — never one per frame, however much crosses it (ADR-0011 rule 8).
///
/// No address on either line: a Listener's IP never appears above DEBUG (rule
/// 5), and rdio-scanner's habit of logging every listener's IP and access-code
/// ident at info (`client.go:152`) is the thing we are deliberately not doing.
async fn handle_socket(socket: impl Socket, state: AppState, scope: AccessScope) {
    info!("live-feed listener connected");
    let connected_at = Instant::now();
    run_connection(socket, state, scope).await;
    // `connected_ms`, not `duration_ms`: a Call already has a `duration_ms` (its
    // audio length) and the request line has a `duration_us`. One grep, one
    // meaning.
    let connected_ms = connected_at.elapsed().as_millis() as u64;
    info!(connected_ms, "live-feed listener disconnected");
}

/// Which source woke the loop. One step short of an [`Event`], because the
/// inbound arm owns a `String` the borrowed event has to point at.
enum Wake {
    Inbound(Option<Inbound>),
    Broadcast(Result<Emitted, broadcast::error::RecvError>),
    Tick,
}

/// Whether the connection carries on after a round of actions.
#[derive(Debug, PartialEq, Eq)]
enum Flow {
    Go,
    Stop,
}

/// Pump events into the table and carry its actions out over the socket.
///
/// The whole of the loop: everything that decides anything is in
/// [`Connection::on`], and everything here either waits or does as it is told.
async fn run_connection(mut socket: impl Socket, state: AppState, scope: AccessScope) {
    let mut receiver = state.live.subscribe();
    let mut conn = Connection::new(scope);
    // The Listener's claim on their push subscription, held for as long as this
    // connection is. Dropping it (any way this function returns) is what hands
    // them back to the push sender (#16).
    let mut attached: Option<crate::push::Attached> = None;

    let opening = conn.on(Event::Opened);
    if !carried_on(perform(&mut socket, &state, &mut conn, &mut attached, opening).await) {
        return;
    }

    // The first heartbeat fires one full period from now — no ping on connect.
    let mut ticker = interval_at(Instant::now() + HEARTBEAT, HEARTBEAT);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);

    loop {
        let wake = tokio::select! {
            incoming = socket.recv() => Wake::Inbound(incoming),
            broadcasted = receiver.recv() => Wake::Broadcast(broadcasted),
            _ = ticker.tick() => Wake::Tick,
        };
        let actions = match wake {
            Wake::Inbound(Some(Inbound::Text(text))) => conn.on(Event::Text(&text)),
            Wake::Inbound(Some(Inbound::Quiet)) => conn.on(Event::Quiet),
            Wake::Inbound(None) => conn.on(Event::Gone),
            Wake::Broadcast(result) => conn.on(Event::Broadcast(result)),
            Wake::Tick => conn.on(Event::Tick),
        };
        if !carried_on(perform(&mut socket, &state, &mut conn, &mut attached, actions).await) {
            break;
        }
    }
}

/// Did a round of actions leave the connection running? A gone peer and a table
/// that asked to close are the same answer here — the loop ends either way.
fn carried_on(outcome: Result<Flow, Disconnected>) -> bool {
    matches!(outcome, Ok(Flow::Go))
}

/// Carry out a round of actions, feeding whatever they produce back into the
/// table before returning.
///
/// A queue rather than recursion, because one action ([`Action::Backfill`])
/// answers with more actions, and an `async fn` that called itself would have to
/// be boxed to do it.
async fn perform(
    socket: &mut impl Socket,
    state: &AppState,
    conn: &mut Connection,
    attached: &mut Option<crate::push::Attached>,
    actions: Vec<Action>,
) -> Result<Flow, Disconnected> {
    let mut queue = VecDeque::from(actions);
    while let Some(action) = queue.pop_front() {
        match action {
            Action::Send(text) => socket.send(Outbound::Text(text)).await?,
            Action::Ping => socket.send(Outbound::Ping).await?,
            Action::Release => {
                attached.take();
            }
            Action::Claim(token) => {
                // A token nobody holds attaches nothing — silently, because a
                // stale token is what a client that unsubscribed on another
                // device has.
                *attached = repo::push_subscription_id(&state.db, &token)
                    .await
                    .unwrap_or_default()
                    .map(|subscription| state.push.attach(subscription));
            }
            Action::Backfill(since) => {
                let page = read_backfill(&state.db, since).await;
                queue.extend(conn.on(Event::Backfilled(page)));
            }
            Action::Close => return Ok(Flow::Stop),
        }
    }
    Ok(Flow::Go)
}

/// Read the Calls a reconnecting Listener missed: everything emitted after
/// `since`, bounded by [`BACKFILL_MAX_CALLS`], oldest emission first.
///
/// **The reading is [`crate::archive::emitted_since`]**, which is the one module
/// that answers for a window of the Archive (#98) — so ordering, the pairing of
/// each Call with the emission it went out as, and the fact that denormalizing a
/// whole page costs the same handful of queries as denormalizing one (#86) are
/// not things this path has to know a second time. Asking per Call instead cost
/// a Pi seven round-trips for every Call on every network blip and every phone
/// unlock, which is exactly rdio-scanner's archive N+1 moved onto the live
/// socket.
///
/// What stays here is the *protocol*: turning that window into frames, and what
/// to do when it cannot be read.
///
/// **Best-effort, and never silently so**: a database error gives back an empty
/// page rather than ending the connection, and says why. A Backfill that quietly
/// returns nothing is indistinguishable from a Listener who missed nothing
/// (#29) — the shape of bug an operator can only report as "some calls go
/// missing sometimes".
///
/// The page is a **snapshot**: a Call pruned between the read and the send is
/// still sent, where a re-fetch would have skipped it. That race was never
/// closed, only narrowed — retention can prune a Call a microsecond after the
/// frame goes out either way — and ADR-0004 already makes this delivery
/// best-effort.
///
/// Delivery is **at-least-once**: a Call emitted in the window between the
/// client's connect and this query arrives both here and live. Call ids are
/// unique, so the client dedups by id — which it does anyway to drive
/// replay/history — rather than the server tracking a high-water mark.
async fn read_backfill(db: &crate::db::Db, since: Emission) -> Backfill {
    let window = match crate::archive::emitted_since(db, since, BACKFILL_MAX_CALLS).await {
        Ok(window) => window,
        Err(error) => {
            // One message, because there is now one read (#98): the page and the
            // views behind it fail together, and `error` says which statement
            // did. Two messages for one event is the drift ADR-0011 rule 6 is
            // about — a Listener's history is gone either way.
            warn!(%error, since, "live-feed Backfill failed");
            return Backfill::unreadable(since);
        }
    };
    Backfill {
        since,
        calls: window
            .calls
            .into_iter()
            .map(|(seq, call)| Emitted {
                seq,
                call: Arc::new(call),
            })
            .collect(),
        truncated: window.truncated,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::LogCapture;
    use rstest::rstest;

    fn call(system_ref: i64, talkgroup_ref: i64) -> StoredCall {
        call_with_patches(system_ref, talkgroup_ref, vec![])
    }

    fn call_with_patches(system_ref: i64, talkgroup_ref: i64, patches: Vec<i64>) -> StoredCall {
        StoredCall {
            id: 1,
            system_ref,
            system_label: None,
            talkgroup_ref,
            talkgroup_label: None,
            talkgroup_group: None,
            talkgroup_tag: None,
            led: None,
            patches,
            frequency: None,
            source: None,
            timestamp: None,
            audio_mime: None,
            duration_ms: None,
            emergency: false,
            encrypted: false,
            site_ref: None,
            object_key: String::new(),
            audio_url: Some(String::new()),
        }
    }

    /// A Call as it went out, at an emission nothing here depends on — these
    /// tests are about *whether* a Call is delivered, not where it sits in the
    /// sequence.
    fn emitted(call: StoredCall) -> Emitted {
        emitted_at(1, call)
    }

    fn emitted_at(seq: Emission, call: StoredCall) -> Emitted {
        Emitted {
            seq,
            call: Arc::new(call),
        }
    }

    fn selection(pairs: &[(&str, &str)], all: bool) -> Selection {
        let mut sel: HashMap<String, HashMap<String, bool>> = HashMap::new();
        for (system, talkgroup) in pairs {
            sel.entry((*system).to_string())
                .or_default()
                .insert((*talkgroup).to_string(), true);
        }
        Selection { sel, all }
    }

    /// A connection with the given Selection, full v1 access scope.
    fn conn(pairs: &[(&str, &str)]) -> Connection {
        conn_scoped(pairs, AccessScope::All)
    }

    fn conn_scoped(pairs: &[(&str, &str)], scope: AccessScope) -> Connection {
        let mut connection = Connection::new(scope);
        connection.sub = selection(pairs, false);
        connection
    }

    /// A connection listening to everything it is allowed to.
    fn conn_all() -> Connection {
        let mut connection = Connection::new(AccessScope::All);
        connection.sub = selection(&[], true);
        connection
    }

    /// A scope permitting exactly these Talkgroups of System 11.
    fn only(talkgroups: [i64; 1]) -> AccessScope {
        AccessScope::Systems(HashMap::from([(
            11,
            TalkgroupScope::Only(HashSet::from(talkgroups)),
        )]))
    }

    /// Every frame a round of actions sends, parsed.
    fn frames(actions: &[Action]) -> Vec<serde_json::Value> {
        actions
            .iter()
            .filter_map(|action| match action {
                Action::Send(text) => Some(serde_json::from_str(text).expect("a JSON frame")),
                _ => None,
            })
            .collect()
    }

    // --- The emission sequence ----------------------------------------------

    /// A fresh fanout has emitted nothing, however it was made — `Default` is
    /// there because `new` takes no arguments, and the two must not drift.
    #[test]
    fn a_fresh_fanout_starts_the_emission_sequence_at_one() {
        assert_eq!(LiveFeed::new().next_emission(), 1);
        assert_eq!(LiveFeed::default().next_emission(), 1);
    }

    /// Resuming continues *after* what the archive already holds. Starting over
    /// would hand new Calls numbers a connected Listener's cursor is already
    /// past, so their next Backfill would come back empty.
    #[test]
    fn resuming_continues_after_what_the_archive_holds() {
        let feed = LiveFeed::new();

        feed.resume_from(41);

        assert_eq!(feed.next_emission(), 42);
        assert_eq!(feed.next_emission(), 43, "and keeps going from there");
    }

    // --- Selection + patch matching (Connection::wants) -----------------------

    #[test]
    fn wants_exact_system_and_talkgroup() {
        assert!(conn(&[("11", "54241")]).wants(&call(11, 54241)));
    }

    #[test]
    fn does_not_want_other_talkgroup_or_system() {
        let c = conn(&[("11", "54241")]);
        assert!(!c.wants(&call(11, 99999)), "wrong talkgroup");
        assert!(!c.wants(&call(22, 54241)), "wrong system");
    }

    #[test]
    fn explicitly_disabled_talkgroup_is_not_wanted() {
        let mut c = conn(&[]);
        c.sub
            .sel
            .entry("11".to_string())
            .or_default()
            .insert("54241".to_string(), false);
        assert!(!c.wants(&call(11, 54241)));
    }

    #[test]
    fn all_wants_everything() {
        let c = conn_all();
        assert!(c.wants(&call(1, 2)));
        assert!(c.wants(&call(999, 888)));
    }

    #[test]
    fn empty_subscription_wants_nothing() {
        assert!(!conn(&[]).wants(&call(11, 54241)));
    }

    /// **Hold System** (#11, spec US 11): the Listener narrows to the System
    /// that's talking. There is no way to enumerate its Talkgroups — the client
    /// only knows the ones it has heard — so the matrix takes a `"*"` key
    /// meaning "every Talkgroup in this System".
    #[test]
    fn system_wildcard_wants_every_talkgroup_in_that_system() {
        let held = conn(&[("11", "*")]);

        assert!(held.wants(&call(11, 54241)));
        assert!(held.wants(&call(11, 1)), "any talkgroup of the held system");
        assert!(!held.wants(&call(22, 54241)), "other systems stay filtered");
    }

    /// **Avoid** (#11, spec US 14) has to work against the all-on Selection a
    /// Listener starts with, so an explicit entry is an exception to `all`
    /// rather than something `all` overrules.
    #[test]
    fn explicit_entry_overrides_global_all() {
        let mut avoiding = conn_all();
        avoiding
            .sub
            .sel
            .entry("11".to_string())
            .or_default()
            .insert("54241".to_string(), false);

        assert!(!avoiding.wants(&call(11, 54241)), "avoided talkgroup");
        assert!(
            avoiding.wants(&call(11, 999)),
            "everything else still plays"
        );
        assert!(avoiding.wants(&call(22, 54241)), "same ref, other system");
    }

    /// A wildcard is the System's default, not its law: an avoided Talkgroup
    /// inside a held System stays avoided.
    #[test]
    fn explicit_entry_overrides_the_system_wildcard() {
        let mut held = conn(&[("11", "*")]);
        held.sub
            .sel
            .entry("11".to_string())
            .or_default()
            .insert("54241".to_string(), false);

        assert!(!held.wants(&call(11, 54241)), "avoided inside the hold");
        assert!(held.wants(&call(11, 999)), "the rest of the system plays");
    }

    /// A Selection of nothing but exclusions is still "all off" — there is no
    /// point resolving patches for it.
    #[test]
    fn a_selection_of_only_exclusions_is_all_off() {
        let mut nothing = conn(&[]);
        nothing
            .sub
            .sel
            .entry("11".to_string())
            .or_default()
            .insert("54241".to_string(), false);

        assert!(!nothing.wants(&call(11, 999)));
    }

    /// Patch fanout: a Call on a Talkgroup the Listener didn't select still
    /// reaches them if they subscribe to one it's patched to (same System).
    #[test]
    fn wants_call_via_patched_talkgroup() {
        let c = conn(&[("11", "300")]); // subscribed to 300 only
        let patched = call_with_patches(11, 100, vec![200, 300]); // call on 100, patched to 300
        assert!(c.wants(&patched), "patched talkgroup 300 should match");
    }

    /// A patch only matches within the Call's own System.
    #[test]
    fn patch_match_is_system_scoped() {
        let c = conn(&[("22", "300")]); // 300 but under a different system
        let patched = call_with_patches(11, 100, vec![300]);
        assert!(!c.wants(&patched), "patch is same-system only");
    }

    /// An all-off subscription short-circuits even when a patch would otherwise
    /// be considered.
    #[test]
    fn all_off_short_circuits_patch_matching() {
        let c = conn(&[]); // nothing selected
        let patched = call_with_patches(11, 100, vec![200, 300]);
        assert!(!c.wants(&patched));
    }

    // --- Access scope (AccessScope::permits) ---------------------------------

    #[test]
    fn scope_all_permits_anything() {
        assert!(AccessScope::All.permits(1, 2));
        assert!(AccessScope::All.permits(999, 888));
    }

    #[test]
    fn scope_systems_all_permits_the_whole_system_only() {
        let scope = AccessScope::Systems(HashMap::from([(11, TalkgroupScope::All)]));
        assert!(scope.permits(11, 54241), "any tg in the permitted system");
        assert!(!scope.permits(22, 54241), "other system denied");
    }

    #[test]
    fn scope_systems_only_permits_listed_talkgroups() {
        let scope = AccessScope::Systems(HashMap::from([(
            11,
            TalkgroupScope::Only(HashSet::from([100, 200])),
        )]));
        assert!(scope.permits(11, 100));
        assert!(!scope.permits(11, 300), "tg not in the allow-list");
        assert!(!scope.permits(22, 100), "other system denied");
    }

    /// The scope gate applies on top of the Selection: subscribing to a
    /// Talkgroup you're not permitted to hear delivers nothing.
    #[test]
    fn scope_denies_even_a_subscribed_talkgroup() {
        let c = conn_scoped(&[("11", "300")], only([100])); // subscribed to 300, only allowed 100
        assert!(!c.wants(&call(11, 300)));
    }

    /// Scope gates patch matching too: a patched Talkgroup outside the scope
    /// doesn't leak the Call.
    #[test]
    fn scope_gates_patched_talkgroup() {
        // Subscribed to both, but only 300 is in scope; call on 100 patched to 300.
        let c = conn_scoped(&[("11", "100"), ("11", "300")], only([300]));
        let patched = call_with_patches(11, 100, vec![300]);
        assert!(c.wants(&patched), "matches on the in-scope patch 300");

        // Now only 100 is in scope, but the Listener didn't subscribe to 100.
        let c2 = conn_scoped(&[("11", "300")], only([100]));
        assert!(
            !c2.wants(&patched),
            "subscribed to 300 (out of scope) and not to 100"
        );
    }

    /// A **restricted connection**, driven the way a real one is: opened with a
    /// scope, told to listen to everything, and then offered Calls.
    ///
    /// This is the promise ADR-0008 makes, asserted through the table rather
    /// than through `permits` alone — and it holds on *both* delivery paths,
    /// because a restriction that applied only to live Calls would hand the
    /// archive to anyone who reconnected with a cursor.
    #[test]
    fn a_restricted_connection_receives_only_what_its_scope_permits() {
        let mut restricted = Connection::new(only([100]));
        restricted.on(Event::Text(r#"{"t":"sub","all":true}"#));

        assert_eq!(
            restricted.on(Event::Broadcast(Ok(emitted(call(11, 100))))),
            vec![Action::Send(call_frame(&emitted(call(11, 100))))],
            "in scope, and selected"
        );
        assert_eq!(
            restricted.on(Event::Broadcast(Ok(emitted(call(11, 300))))),
            vec![],
            "selected by `all`, but outside the scope"
        );
        assert_eq!(
            restricted.on(Event::Broadcast(Ok(emitted(call(22, 100))))),
            vec![],
            "a System the scope says nothing about"
        );

        let backfilled = restricted.on(Event::Backfilled(Backfill {
            since: 0,
            calls: vec![
                emitted_at(1, call(11, 100)),
                emitted_at(2, call(11, 300)),
                emitted_at(3, call(22, 100)),
            ],
            truncated: false,
        }));
        assert_eq!(
            frames(&backfilled)
                .iter()
                .map(|frame| frame["seq"].clone())
                .collect::<Vec<_>>(),
            vec![serde_json::json!(1)],
            "a Backfill is gated by the same scope the live path is"
        );
    }

    // (The Selection algebra itself — `selects`, `is_all_off` — is tested in
    // `crate::selection`, which the push sender shares.)

    // --- The table: one event in, a list of actions out ----------------------

    /// A connection greets before anything else, announcing what a client needs
    /// in order to time its own reconnect.
    #[test]
    fn opening_greets_with_the_protocol_and_the_cadence() {
        let mut fresh = Connection::new(AccessScope::All);

        let greeting = frames(&fresh.on(Event::Opened));

        assert_eq!(greeting.len(), 1);
        assert_eq!(greeting[0]["t"], "hello");
        assert_eq!(greeting[0]["protocol"], PROTOCOL_VERSION);
        assert_eq!(greeting[0]["heartbeatMs"], HEARTBEAT.as_millis() as u64);
    }

    /// A `sub` acks and does nothing else when it names neither a push
    /// subscription nor a cursor — the ordinary first connect.
    #[test]
    fn subscribing_acks_and_takes_no_claim() {
        let mut fresh = Connection::new(AccessScope::All);

        let actions = fresh.on(Event::Text(r#"{"t":"sub","sel":{"11":{"100":true}}}"#));

        assert_eq!(
            actions,
            vec![
                Action::Release,
                Action::Send(subscribed_frame().to_string())
            ]
        );
        assert!(fresh.wants(&call(11, 100)), "and the Selection is live");
    }

    /// Re-subscribing **replaces** the Selection rather than merging into it:
    /// what a Listener turned off has to actually go quiet.
    #[test]
    fn resubscribing_replaces_the_selection() {
        let mut c = Connection::new(AccessScope::All);
        c.on(Event::Text(r#"{"t":"sub","sel":{"11":{"100":true}}}"#));

        c.on(Event::Text(r#"{"t":"sub","sel":{"11":{"200":true}}}"#));

        assert!(!c.wants(&call(11, 100)), "the old Talkgroup went quiet");
        assert!(c.wants(&call(11, 200)));
    }

    /// **Release before reclaim** (#16). A Listener who re-subscribes with the
    /// subscription this socket already holds must not be released *after* the
    /// new claim is taken — that leaves them detached with the feed open, being
    /// notified about Calls they are watching arrive.
    ///
    /// An ordering in a returned list, so it is asserted rather than reviewed:
    /// swapping the two lines that produce it fails here.
    #[test]
    fn a_push_claim_is_released_before_the_next_one_is_taken() {
        let mut fresh = Connection::new(AccessScope::All);

        let actions = fresh.on(Event::Text(r#"{"t":"sub","all":true,"push":"a-token"}"#));

        assert_eq!(
            actions,
            vec![
                Action::Release,
                Action::Claim("a-token".to_string()),
                Action::Send(subscribed_frame().to_string()),
            ]
        );
    }

    /// A cursor asks for a **Backfill**, *after* the ack — so a client is never
    /// sent history for a Selection it has not been told is live.
    #[test]
    fn a_cursor_asks_for_a_backfill_after_the_ack() {
        let mut fresh = Connection::new(AccessScope::All);

        let actions = fresh.on(Event::Text(r#"{"t":"sub","all":true,"since":41}"#));

        assert_eq!(
            actions,
            vec![
                Action::Release,
                Action::Send(subscribed_frame().to_string()),
                Action::Backfill(41),
            ]
        );
        assert_eq!(
            frames(&actions)
                .iter()
                .map(|frame| frame["t"].clone())
                .collect::<Vec<_>>(),
            vec![serde_json::json!("subscribed")],
            "the ack is the only thing that reaches the wire"
        );
    }

    /// Malformed and unknown frames do nothing at all — no ack, no close. The
    /// protocol is versioned and expected to grow.
    #[rstest]
    #[case::not_json("not json {")]
    #[case::an_unknown_message(r#"{"t":"bogus"}"#)]
    #[case::a_sub_that_is_not_one(r#"{"t":"sub","sel":"not a matrix"}"#)]
    fn an_unusable_frame_is_ignored(#[case] text: &str) {
        let mut fresh = Connection::new(AccessScope::All);

        assert_eq!(fresh.on(Event::Text(text)), vec![]);
    }

    #[test]
    fn broadcast_delivers_a_matching_call() {
        let mut c = conn(&[("11", "54241")]);
        assert_eq!(
            c.on(Event::Broadcast(Ok(emitted(call(11, 54241))))),
            vec![Action::Send(call_frame(&emitted(call(11, 54241))))]
        );
    }

    #[test]
    fn broadcast_skips_a_non_matching_call() {
        let mut c = conn(&[("11", "54241")]);
        assert_eq!(c.on(Event::Broadcast(Ok(emitted(call(11, 99999))))), vec![]);
    }

    /// A lagging Listener is told (so it can refetch from the archive) *and*
    /// written down: a client that cannot keep up is a symptom the operator
    /// owns, and the count is the measure of it (#29).
    #[test]
    fn broadcast_lag_becomes_a_lagged_notice_and_a_warning() {
        let capture = LogCapture::start();
        let mut c = conn(&[("11", "54241")]);

        let actions = c.on(Event::Broadcast(Err(broadcast::error::RecvError::Lagged(
            7,
        ))));

        assert_eq!(actions, vec![Action::Send(lagged_frame(7))]);
        let logged = capture.text();
        assert!(logged.contains(" WARN "), "{logged}");
        assert!(logged.contains("skipped=7"), "{logged}");
    }

    /// Delivering a Call the Listener wanted is not news — the ordinary case
    /// runs per Call per connection and must stay silent (ADR-0011 rule 8).
    #[test]
    fn an_ordinary_delivery_logs_nothing() {
        let capture = LogCapture::start();
        let mut c = conn(&[("11", "54241")]);

        c.on(Event::Broadcast(Ok(emitted(call(11, 54241)))));
        c.on(Event::Broadcast(Ok(emitted(call(11, 99999)))));

        assert_eq!(capture.text(), "", "one line per frame is a hot loop");
    }

    #[test]
    fn broadcast_closed_ends_the_connection() {
        let mut c = conn(&[("11", "54241")]);
        assert_eq!(
            c.on(Event::Broadcast(Err(broadcast::error::RecvError::Closed))),
            vec![Action::Close]
        );
    }

    /// A peer that hangs up ends the connection, whether it said so with a close
    /// frame, an error, or by going away.
    #[test]
    fn a_peer_that_is_gone_ends_the_connection() {
        let mut c = conn_all();
        assert_eq!(c.on(Event::Gone), vec![Action::Close]);
    }

    // --- Heartbeat and reaping, as events ------------------------------------

    /// The first tick pings; a second with nothing in between reaps. No sleep is
    /// involved in either — a tick is a value.
    #[test]
    fn an_unanswered_ping_reaps_on_the_next_tick() {
        let capture = LogCapture::start();
        let mut c = conn_all();

        assert_eq!(c.on(Event::Tick), vec![Action::Ping], "first tick pings");
        assert_eq!(
            c.on(Event::Tick),
            vec![Action::Close],
            "unanswered -> reaped"
        );

        let logged = capture.text();
        assert!(logged.contains(" WARN "), "{logged}");
        assert!(logged.contains("heartbeat_ms=30000"), "{logged}");
    }

    /// Any inbound frame is liveness — a pong, a binary frame, a `sub`. A client
    /// that answers is pinged again rather than reaped.
    #[rstest]
    #[case::a_pong(Event::Quiet)]
    #[case::a_subscribe(Event::Text(r#"{"t":"sub","all":true}"#))]
    fn an_answered_ping_pings_again(#[case] answer: Event<'_>) {
        let mut c = conn_all();
        assert_eq!(c.on(Event::Tick), vec![Action::Ping]);

        c.on(answer);

        assert_eq!(
            c.on(Event::Tick),
            vec![Action::Ping],
            "answered -> ping again, not reaped"
        );
    }

    /// A frame with nothing to say says nothing back.
    #[test]
    fn a_quiet_frame_produces_no_actions() {
        let mut c = conn_all();
        assert_eq!(c.on(Event::Quiet), vec![]);
    }

    // --- Backfill delivery ---------------------------------------------------

    /// A **Backfill** is delivered oldest-emission-first, filtered by the
    /// Selection, and flagged so the client files it as history.
    #[test]
    fn a_backfill_is_filtered_and_flagged() {
        let mut c = conn(&[("11", "100"), ("11", "300")]);

        let actions = c.on(Event::Backfilled(Backfill {
            since: 0,
            calls: vec![
                emitted_at(1, call(11, 100)),
                emitted_at(2, call(11, 200)),
                emitted_at(3, call(11, 300)),
            ],
            truncated: false,
        }));

        let sent = frames(&actions);
        assert_eq!(
            sent.iter()
                .map(|frame| frame["call"]["talkgroupRef"].clone())
                .collect::<Vec<_>>(),
            vec![serde_json::json!(100), serde_json::json!(300)],
            "200 was not selected"
        );
        assert!(sent.iter().all(|frame| frame["catchup"] == true));
        assert_eq!(
            sent.iter()
                .map(|frame| frame["seq"].clone())
                .collect::<Vec<_>>(),
            vec![serde_json::json!(1), serde_json::json!(3)],
            "each carries the emission it went out as, so the cursor stays exact"
        );
    }

    /// A Backfill that hit the bound tells the Listener their history has a
    /// gap — **first**, because the page keeps the newest Calls and whatever was
    /// dropped is older than everything in it.
    ///
    /// Told at all because a silent truncation is indistinguishable from having
    /// missed nothing (CONTEXT.md, **Backfill**), and only archive search (#13)
    /// can fill it.
    #[test]
    fn a_truncated_backfill_says_the_history_has_a_gap() {
        let mut c = conn_all();

        let actions = c.on(Event::Backfilled(Backfill {
            since: 7,
            calls: vec![emitted_at(108, call(11, 100))],
            truncated: true,
        }));

        let sent = frames(&actions);
        assert_eq!(sent[0]["t"], "gap", "the gap precedes what survived it");
        assert_eq!(sent[0]["since"], 7, "and names where the hole starts");
        assert_eq!(sent[1]["t"], "call");
    }

    /// A Backfill that reached far enough says nothing about gaps — a `gap` on
    /// every reconnect would train a client to ignore it.
    #[test]
    fn an_untruncated_backfill_says_nothing_about_gaps() {
        let mut c = conn_all();

        let actions = c.on(Event::Backfilled(Backfill {
            since: 0,
            calls: vec![emitted_at(1, call(11, 100))],
            truncated: false,
        }));

        assert!(frames(&actions).iter().all(|frame| frame["t"] != "gap"));
    }

    /// An empty Backfill sends nothing at all: a Listener who missed nothing
    /// gets no frames, not an announcement of an empty page.
    #[test]
    fn an_empty_backfill_sends_nothing() {
        let mut c = conn_all();

        assert_eq!(
            c.on(Event::Backfilled(Backfill::unreadable(3))),
            vec![],
            "including one that could not be read — the adapter said why"
        );
    }

    // --- Frame builders ------------------------------------------------------

    #[test]
    fn call_and_catchup_frames_differ_only_by_the_flag() {
        let c = emitted(call(11, 54241));
        let live: serde_json::Value = serde_json::from_str(&call_frame(&c)).unwrap();
        let catchup: serde_json::Value = serde_json::from_str(&catchup_frame(&c)).unwrap();
        assert_eq!(live["t"], "call");
        assert!(live.get("catchup").is_none(), "live has no catchup flag");
        assert_eq!(catchup["t"], "call");
        assert_eq!(catchup["catchup"], true);
        assert_eq!(live["call"], catchup["call"], "same call payload");
        assert_eq!(live["seq"], 1, "and both carry the emission (#94)");
        assert_eq!(catchup["seq"], 1);
    }

    #[test]
    fn lagged_frame_reports_the_skip_count() {
        let value: serde_json::Value = serde_json::from_str(&lagged_frame(42)).unwrap();
        assert_eq!(value["t"], "lagged");
        assert_eq!(value["skipped"], 42);
    }

    // --- Client message parsing ----------------------------------------------

    #[test]
    fn sub_message_parses_since_cursor() {
        let msg: ClientMessage =
            serde_json::from_str(r#"{"t":"sub","sel":{"11":{"100":true}},"since":42}"#).unwrap();
        let ClientMessage::Sub {
            since, all, push, ..
        } = msg;
        assert_eq!(since, Some(42));
        assert!(!all);
        assert_eq!(push, None, "a client without notifications sends none");
    }

    /// The push subscription a socket stands in for (#16): while it is open, its
    /// Listener is not notified. A token rather than an Id, so a client cannot
    /// name a subscription it was never given.
    #[test]
    fn sub_message_parses_the_push_subscription() {
        let msg: ClientMessage =
            serde_json::from_str(r#"{"t":"sub","all":true,"push":"a-token"}"#).unwrap();
        let ClientMessage::Sub { push, .. } = msg;
        assert_eq!(push.as_deref(), Some("a-token"));
    }

    #[test]
    fn sub_message_without_since_defaults_to_none() {
        let msg: ClientMessage = serde_json::from_str(r#"{"t":"sub","all":true}"#).unwrap();
        let ClientMessage::Sub {
            since,
            all,
            sel,
            push,
        } = msg;
        assert_eq!(since, None);
        assert!(all);
        assert!(sel.is_empty());
        assert_eq!(push, None);
    }

    // --- The socket edge -----------------------------------------------------

    /// Every kind of frame a real client can send, and what the table makes of
    /// it. A close frame, a socket error and a stream that ended are one
    /// outcome — the peer is gone — and everything that is not text is
    /// liveness.
    #[rstest]
    #[case::text(Some(Ok(Message::Text("hi".into()))), Some(Inbound::Text("hi".to_string())))]
    #[case::ping(Some(Ok(Message::Ping(bytes::Bytes::new()))), Some(Inbound::Quiet))]
    #[case::pong(Some(Ok(Message::Pong(bytes::Bytes::new()))), Some(Inbound::Quiet))]
    #[case::binary(Some(Ok(Message::Binary(bytes::Bytes::new()))), Some(Inbound::Quiet))]
    #[case::close(Some(Ok(Message::Close(None))), None)]
    #[case::error(Some(Err(axum::Error::new(std::io::Error::other("gone")))), None)]
    #[case::ended(None, None)]
    fn what_a_websocket_frame_means_to_the_table(
        #[case] received: Option<Result<Message, axum::Error>>,
        #[case] expected: Option<Inbound>,
    ) {
        assert_eq!(inbound_of(received), expected);
    }

    /// And back the other way: an action becomes the frame that carries it.
    #[test]
    fn an_outbound_frame_carries_its_action() {
        assert!(matches!(
            Message::from(Outbound::Text("hi".to_string())),
            Message::Text(text) if text.as_str() == "hi"
        ));
        assert!(matches!(
            Message::from(Outbound::Ping),
            Message::Ping(payload) if payload.is_empty()
        ));
    }

    // --- The adapter, over a substituted socket ------------------------------

    /// A socket a test owns both ends of: it hands the loop the frames the test
    /// queued, and forwards everything the loop sent back to the test.
    ///
    /// `recv` waits forever once the queue is empty rather than answering `None`,
    /// because "the peer is quiet" and "the peer has gone" are different events
    /// and a fake that confused them could never let a heartbeat fire.
    struct FakeSocket {
        inbound: tokio::sync::mpsc::UnboundedReceiver<Option<Inbound>>,
        outbound: tokio::sync::mpsc::UnboundedSender<String>,
        /// Every send from here on fails, as they do to a peer that went away.
        broken: bool,
    }

    /// What a test holds of a [`FakeSocket`]: a way to feed it, and a way to read
    /// what it was told to send. Awaited rather than polled, so a test asserts on
    /// an ordering rather than on a machine being fast enough.
    struct Peer {
        inbound: tokio::sync::mpsc::UnboundedSender<Option<Inbound>>,
        outbound: tokio::sync::mpsc::UnboundedReceiver<String>,
    }

    impl Peer {
        /// A text frame from the client.
        fn says(&self, text: &str) {
            let _ = self.inbound.send(Some(Inbound::Text(text.to_string())));
        }

        /// A frame with no protocol meaning — what a browser answers a ping with.
        fn pongs(&self) {
            let _ = self.inbound.send(Some(Inbound::Quiet));
        }

        /// The peer hangs up.
        fn hangs_up(&self) {
            let _ = self.inbound.send(None);
        }

        /// The next frame the server sent, waiting for it. `None` once the
        /// connection has ended and everything it sent has been read.
        async fn next(&mut self) -> Option<String> {
            self.outbound.recv().await
        }

        /// Every frame the server sent, once the connection has ended.
        async fn everything(&mut self) -> Vec<String> {
            let mut frames = Vec::new();
            while let Some(frame) = self.next().await {
                frames.push(frame);
            }
            frames
        }

        /// The same, parsed.
        async fn every_frame(&mut self) -> Vec<serde_json::Value> {
            self.everything()
                .await
                .iter()
                .filter(|text| *text != "PING")
                .map(|text| serde_json::from_str(text).expect("a JSON frame"))
                .collect()
        }
    }

    impl FakeSocket {
        fn pair() -> (FakeSocket, Peer) {
            let (inbound_tx, inbound_rx) = tokio::sync::mpsc::unbounded_channel();
            let (outbound_tx, outbound_rx) = tokio::sync::mpsc::unbounded_channel();
            (
                FakeSocket {
                    inbound: inbound_rx,
                    outbound: outbound_tx,
                    broken: false,
                },
                Peer {
                    inbound: inbound_tx,
                    outbound: outbound_rx,
                },
            )
        }

        /// A socket whose peer is already gone: every send fails.
        fn broken() -> (FakeSocket, Peer) {
            let (mut socket, peer) = FakeSocket::pair();
            socket.broken = true;
            (socket, peer)
        }
    }

    impl Socket for FakeSocket {
        async fn recv(&mut self) -> Option<Inbound> {
            // A closed channel is a test that has finished with this peer, which
            // is the same thing as a peer that went away.
            self.inbound.recv().await.flatten()
        }

        async fn send(&mut self, message: Outbound) -> Result<(), Disconnected> {
            if self.broken {
                return Err(Disconnected);
            }
            let frame = match message {
                Outbound::Text(text) => text,
                Outbound::Ping => "PING".to_string(),
            };
            self.outbound.send(frame).map_err(|_| Disconnected)
        }
    }

    /// An `AppState` over a real (empty) SQLite database and a real filesystem
    /// store — the `src/ingest.rs` precedent. What is substituted here is the
    /// *socket*; everything under it is the thing itself.
    async fn a_state() -> (AppState, tempfile::TempDir) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let db = crate::db::connect(&crate::testing::sqlite_url(&tmp))
            .await
            .expect("db");
        let store = Arc::new(crate::BlobStore::filesystem(tmp.path().join("audio")).expect("blob"));
        (
            AppState::new(store, db, crate::IngestConfig::default()),
            tmp,
        )
    }

    /// Store a Call and record that it went out as `seq`.
    async fn store_and_emit(state: &AppState, talkgroup_ref: i64, seq: Emission) {
        let stored = repo::insert_call(
            &state.db,
            &repo::NewCall::new(11, talkgroup_ref, 1_000 + talkgroup_ref),
            Some(crate::blob::StoredAudio::written(
                format!("k{talkgroup_ref}"),
                0,
            )),
            &repo::Resolved::default(),
            true,
            0,
        )
        .await
        .expect("store a Call");
        repo::emit_call(&state.db, stored.id, seq)
            .await
            .expect("emit it");
    }

    /// The greeting is the first thing on the wire, before any client frame is
    /// read.
    #[tokio::test]
    async fn the_loop_greets_before_it_listens() {
        let (state, _tmp) = a_state().await;
        let (socket, mut peer) = FakeSocket::pair();
        peer.hangs_up();

        run_connection(socket, state, AccessScope::All).await;

        let frames = peer.every_frame().await;
        assert_eq!(frames.len(), 1, "{frames:?}");
        assert_eq!(frames[0]["t"], "hello");
    }

    /// A peer that never answers is pinged once and reaped on the next period —
    /// the behaviour rdio has none of, proven **without a sleep**: time is
    /// paused, so thirty seconds costs nothing and the assertion is about an
    /// ordering rather than about a machine being fast enough.
    #[tokio::test]
    async fn a_silent_peer_is_pinged_and_then_reaped() {
        let (state, _tmp) = a_state().await;
        // Paused *after* the database is open: a paused clock advances whenever
        // the runtime is idle, and a pool acquire waiting on a blocking connect
        // looks exactly that idle — so pausing first times the pool out instead
        // of the heartbeat.
        tokio::time::pause();
        let (socket, mut peer) = FakeSocket::pair();

        // Returns *because* it was reaped: nothing else here ever ends it.
        run_connection(socket, state, AccessScope::All).await;

        let sent = peer.everything().await;
        assert_eq!(
            sent.iter().filter(|frame| *frame == "PING").count(),
            1,
            "one ping went out, and the silence after it ended the connection: {sent:?}"
        );
    }

    /// The first heartbeat waits a **full period** — the server does not ping on
    /// connect, so a proxy sees one ping every thirty seconds rather than two.
    ///
    /// Asserted as *when* the ping arrived rather than as "nothing arrived yet",
    /// because a paused clock advances only when the runtime is idle: the answer
    /// to "has it happened?" would depend on how many times the connection task
    /// had been polled, which is not something a test should be pinning.
    /// Reading the clock the ping actually fired on has no such race.
    #[tokio::test]
    async fn the_first_ping_waits_a_full_period() {
        let (state, _tmp) = a_state().await;
        tokio::time::pause();
        let (socket, mut peer) = FakeSocket::pair();
        let running = tokio::spawn(run_connection(socket, state, AccessScope::All));
        peer.next().await.expect("the greeting");

        // Nothing else is runnable, so waiting for the ping is what advances the
        // clock to the tick that sends it.
        let opened_at = Instant::now();
        assert_eq!(peer.next().await.as_deref(), Some("PING"));

        // A range, because a paused clock lands on the millisecond *past* a
        // deadline rather than exactly on it. A ping on connect reads as zero
        // here, and one a period late reads as sixty seconds.
        let waited = opened_at.elapsed();
        assert!(
            waited >= HEARTBEAT && waited < HEARTBEAT * 2,
            "the first ping waited {waited:?}, not one period"
        );
        running.abort();
    }

    /// A peer that answers its ping keeps the connection: the loop pings again
    /// rather than reaping, for as long as the answers keep coming.
    #[tokio::test]
    async fn a_peer_that_answers_is_not_reaped() {
        let (state, _tmp) = a_state().await;
        tokio::time::pause();
        let (socket, mut peer) = FakeSocket::pair();
        let running = tokio::spawn(run_connection(socket, state, AccessScope::All));

        let mut pings = 0;
        // Answer the first three pings, then stop — which is what ends the test.
        while let Some(frame) = peer.next().await {
            if frame == "PING" {
                pings += 1;
                if pings > 3 {
                    break;
                }
                peer.pongs();
            }
        }
        running.await.expect("the connection loop");

        assert_eq!(pings, 4, "three answered pings, then one that was not");
    }

    /// The whole of a subscribe, over the adapter: the ack goes out, the
    /// Selection takes effect, and a published Call reaches the wire carrying
    /// its emission.
    #[tokio::test]
    async fn a_subscribed_peer_is_sent_the_calls_it_selected() {
        let (state, _tmp) = a_state().await;
        let (socket, mut peer) = FakeSocket::pair();
        let running = tokio::spawn(run_connection(socket, state.clone(), AccessScope::All));

        assert!(
            peer.next().await.expect("the greeting").contains("hello"),
            "the greeting comes first"
        );
        peer.says(r#"{"t":"sub","sel":{"11":{"54241":true}}}"#);
        // The ack is the point at which the Selection is live, so publishing
        // before it would race the subscribe rather than test it.
        assert_eq!(peer.next().await.as_deref(), Some(subscribed_frame()));

        state.publish(Arc::new(call(11, 99999))).await;
        state.publish(Arc::new(call(11, 54241))).await;
        let delivered: serde_json::Value =
            serde_json::from_str(&peer.next().await.expect("a Call")).expect("a JSON frame");
        peer.hangs_up();
        running.await.expect("the connection loop");

        assert_eq!(
            delivered["call"]["talkgroupRef"], 54241,
            "the unselected Talkgroup never reached the wire"
        );
        assert_eq!(delivered["seq"], 2, "carrying the emission it went out as");
    }

    /// A send that fails ends the connection then and there. The greeting is
    /// where it matters most: a peer that has already gone must not be given a
    /// fanout subscription and a ticker for the next thirty seconds.
    #[tokio::test]
    async fn a_peer_gone_before_the_greeting_ends_the_connection() {
        let (state, _tmp) = a_state().await;
        let (socket, mut peer) = FakeSocket::broken();

        run_connection(socket, state, AccessScope::All).await;

        assert!(
            peer.everything().await.is_empty(),
            "nothing reached a gone peer"
        );
    }

    /// A **Backfill** over the adapter: the cursor names an emission, and what
    /// comes back is what was emitted after it — read from a real archive.
    #[tokio::test]
    async fn a_cursor_backfills_from_the_archive() {
        let (state, _tmp) = a_state().await;
        store_and_emit(&state, 100, 1).await;
        store_and_emit(&state, 200, 2).await;

        let (socket, mut peer) = FakeSocket::pair();
        let running = tokio::spawn(run_connection(socket, state, AccessScope::All));
        peer.says(r#"{"t":"sub","all":true,"since":1}"#);
        // Greeting, ack, then the page.
        peer.next().await;
        peer.next().await;
        let backfilled: serde_json::Value =
            serde_json::from_str(&peer.next().await.expect("a backfilled Call")).expect("json");
        peer.hangs_up();
        running.await.expect("the connection loop");

        assert_eq!(
            backfilled["call"]["talkgroupRef"], 200,
            "only what followed 1"
        );
        assert_eq!(backfilled["catchup"], true);
        assert_eq!(backfilled["seq"], 2);
    }
}
