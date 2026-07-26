//! The live feed: a raw WebSocket (ADR-0004) that pushes call metadata to
//! subscribed listeners. Audio never rides the socket — only compact JSON.
//!
//! Ticket #9 turns the skeleton's broadcast+filter into the full protocol, and
//! deliberately *improves* on rdio-scanner rather than cloning it:
//!
//! - **Patch fanout** — a Call reaches a subscriber of any Talkgroup it's patched
//!   to, not just its own (rdio's `IsEnabled`, plus we carry `patches[]` on the
//!   wire so the client can display cross-patched traffic).
//! - **Access scope** — every delivery is gated by both the subscription matrix
//!   and an access scope (ADR-0008). v1 listening is open (`AccessScope::All`);
//!   the restricted variant is the v2 access-code seam.
//! - **Heartbeat + dead-connection reaping** — rdio has no heartbeat of its own.
//!   The server pings on an interval and reaps half-open connections, keeping
//!   proxies warm and freeing resources promptly.
//! - **Reconnect catch-up** — rdio silently drops any Call that arrives while a
//!   listener is briefly disconnected (the core mobile pain). A reconnecting
//!   client sends the last Call id it saw as `since`; the server backfills what
//!   it missed (bounded) before resuming live.
//! - **`hello` greeting + `lagged` notice** — the server announces its protocol
//!   version + heartbeat cadence on connect, and tells a lagging client how many
//!   Calls it skipped so the client can refetch from the archive (#13).
//!
//! The pure decision logic (`ConnState::wants`, `AccessScope::permits`,
//! `Heartbeat`, `on_broadcast`) is factored out of the socket I/O so every branch
//! is unit-testable; `handle_socket` is the thin async glue over it.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use axum::extract::State;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::response::Response;
use serde::Deserialize;
use tokio::sync::broadcast;
use tokio::time::{Instant, MissedTickBehavior, interval_at};
use tracing::{Instrument, Span, debug, info, warn};

use crate::AppState;
use crate::call::{CallId, StoredCall};
use crate::db::repo;

/// Live-feed protocol version, announced in the `hello` frame. Bumped on a
/// breaking wire change so clients can negotiate.
const PROTOCOL_VERSION: u32 = 1;

/// Channel capacity for the fanout broadcast. Ample for the low-hundreds of
/// listeners this targets; a slow client that lags is told and skipped forward.
const LIVE_FEED_CAPACITY: usize = 1024;

/// Default server heartbeat period. Long enough to be negligible on a Pi, short
/// enough to reap a dead connection within ~1 minute — one ping goes out, and if
/// it's still unanswered a period later (two missed intervals) the peer is reaped.
const DEFAULT_HEARTBEAT: Duration = Duration::from_secs(30);

/// Upper bound on Calls replayed to a reconnecting client (catch-up, #9). A
/// client returning after a long gap gets a recent slice and falls back to
/// archive search (#13) for the rest — the live socket never replays the world.
const CATCHUP_MAX_CALLS: u64 = 100;

/// A clonable handle to the live-feed fanout. Cloning shares one channel.
#[derive(Clone)]
pub struct LiveFeed {
    tx: broadcast::Sender<Arc<StoredCall>>,
    heartbeat: Duration,
}

impl LiveFeed {
    /// A hub with the default heartbeat period.
    pub fn new() -> Self {
        Self::with_heartbeat(DEFAULT_HEARTBEAT)
    }

    /// A hub with a custom heartbeat period (tests drive this short).
    pub fn with_heartbeat(heartbeat: Duration) -> Self {
        let (tx, _rx) = broadcast::channel(LIVE_FEED_CAPACITY);
        LiveFeed { tx, heartbeat }
    }

    fn subscribe(&self) -> broadcast::Receiver<Arc<StoredCall>> {
        self.tx.subscribe()
    }

    /// Publish a stored call to all connected listeners. Sending with no
    /// receivers is not an error — it just means nobody is connected.
    pub fn publish(&self, call: Arc<StoredCall>) {
        let _ = self.tx.send(call);
    }

    fn heartbeat(&self) -> Duration {
        self.heartbeat
    }
}

impl Default for LiveFeed {
    fn default() -> Self {
        Self::new()
    }
}

/// A listener's Talkgroup access within one System (the v2 access-code shape).
///
/// v1 listening is open ([`AccessScope::All`]), so these restricted variants are
/// never constructed at runtime yet — they're the documented ADR-0008 seam for
/// v2 access codes, and [`AccessScope::permits`]'s handling of them is proven by
/// unit tests so the logic is ready when v2 wires a PIN to a scope.
#[allow(dead_code)]
#[derive(Debug, Clone)]
enum TalkgroupScope {
    /// Every Talkgroup in the System.
    All,
    /// Only these Talkgroup Refs.
    Only(HashSet<i64>),
}

/// A connection's access scope (ADR-0008). v1 listening is open, so every
/// connection is `All`; v2 access codes will populate `Systems` from a
/// per-listener PIN (the documented scope model `"*"` | `[{id, talkgroups}]`).
/// A Call is delivered only when **both** the subscription matrix and the access
/// scope admit it.
#[derive(Debug, Clone)]
enum AccessScope {
    /// Full access (v1 default; scope `"*"`).
    All,
    /// Restricted to specific Systems, each optionally down to specific
    /// Talkgroups (v2; scope `[{id, talkgroups}]`). Not constructed at runtime in
    /// v1 (open listening) — the ADR-0008 seam, exercised by unit tests.
    #[allow(dead_code)]
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

/// The listener's subscription matrix: `systemRef -> talkgroupRef -> enabled`.
/// JSON object keys are strings, so refs are compared as strings. `all` is the
/// spec's global all-on (story 21) — a "monitor everything" client.
#[derive(Debug, Default)]
struct Subscription {
    selection: HashMap<String, HashMap<String, bool>>,
    all: bool,
}

/// The Talkgroup key meaning "every Talkgroup in this System" (#11). A client
/// holding a System can't enumerate its Talkgroups — it only knows the ones it
/// has heard — and rdio only avoids the problem by shipping its whole config to
/// the client, so the matrix gets a wildcard instead.
const TALKGROUP_WILDCARD: &str = "*";

impl Subscription {
    /// Is `(system_ref, talkgroup_ref)` selected?
    ///
    /// Most specific wins: an explicit entry for the Talkgroup, then the
    /// System's wildcard, then global-all. That ordering is what lets a listener
    /// **avoid** one Talkgroup out of an all-on selection (spec US 14) or out of
    /// a held System (US 11) — an exception to a default, rather than something
    /// the default overrules.
    fn selects(&self, system_ref: i64, talkgroup_ref: i64) -> bool {
        let system = self.selection.get(&system_ref.to_string());
        if let Some(explicit) = system.and_then(|tgs| tgs.get(&talkgroup_ref.to_string())) {
            return *explicit;
        }
        if let Some(wildcard) = system.and_then(|tgs| tgs.get(TALKGROUP_WILDCARD)) {
            return *wildcard;
        }
        self.all
    }

    /// Nothing is selected at all (rdio's `IsAllOff`): no global-all and no
    /// enabled entry — exclusions alone select nothing. Lets [`ConnState::wants`]
    /// skip patch resolution for an idle connection.
    fn is_all_off(&self) -> bool {
        !self.all
            && self
                .selection
                .values()
                .all(|talkgroups| talkgroups.values().all(|&on| !on))
    }
}

/// Per-connection filtering state: the subscription matrix, the access scope, and
/// the heartbeat tracker.
struct ConnState {
    sub: Subscription,
    scope: AccessScope,
    heartbeat: Heartbeat,
}

impl ConnState {
    /// A fresh connection: nothing selected, full access (v1 open listening).
    fn new() -> Self {
        ConnState {
            sub: Subscription::default(),
            scope: AccessScope::All,
            heartbeat: Heartbeat::new(),
        }
    }

    /// Does this connection receive `call`? Yes when some Talkgroup the Call
    /// reaches — its own, or any patched one within the Call's System — is both
    /// selected by the subscription matrix and permitted by the access scope.
    /// Mirrors rdio's `IsEnabled` (primary OR patch) and adds the scope gate.
    fn wants(&self, call: &StoredCall) -> bool {
        if self.sub.is_all_off() {
            return false;
        }
        let system_ref = call.system_ref;
        std::iter::once(call.talkgroup_ref)
            .chain(call.patches.iter().copied())
            .any(|talkgroup_ref| {
                self.sub.selects(system_ref, talkgroup_ref)
                    && self.scope.permits(system_ref, talkgroup_ref)
            })
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
    fn new() -> Self {
        Heartbeat::default()
    }

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

/// What to do with a broadcast result for one connection — a pure decision kept
/// separate from the socket I/O so every branch (deliver, filter, lag, close) is
/// unit-testable.
#[derive(Debug, PartialEq, Eq)]
enum BroadcastAction {
    /// Send this text frame to the client.
    Send(String),
    /// This Call isn't for this connection — do nothing.
    Skip,
    /// The broadcast channel is gone — end the connection.
    Close,
}

/// Decide what a connection does with a fanout result.
fn on_broadcast(
    result: Result<Arc<StoredCall>, broadcast::error::RecvError>,
    conn: &ConnState,
) -> BroadcastAction {
    match result {
        Ok(call) if conn.wants(&call) => BroadcastAction::Send(call_frame(&call)),
        Ok(_) => BroadcastAction::Skip,
        // A slow client fell behind the fanout: tell it how many Calls it missed
        // so it can refetch from the archive (#13) rather than silently losing
        // them (rdio just drops them). The operator is told too — a listener
        // that cannot keep up is a real symptom, and the count is the measure of
        // it (#29). Rare by construction: the channel holds 1024 Calls.
        Err(broadcast::error::RecvError::Lagged(skipped)) => {
            warn!(skipped, "live-feed listener lagged behind the fanout");
            BroadcastAction::Send(lagged_frame(skipped))
        }
        Err(broadcast::error::RecvError::Closed) => BroadcastAction::Close,
    }
}

/// Messages a client sends to the server.
#[derive(Debug, Deserialize)]
#[serde(tag = "t")]
enum ClientMessage {
    /// Replace the subscription matrix, optionally with a reconnect catch-up
    /// cursor.
    #[serde(rename = "sub")]
    Sub {
        #[serde(default)]
        sel: HashMap<String, HashMap<String, bool>>,
        #[serde(default)]
        all: bool,
        /// The last Call id the client received. When present, the server
        /// backfills matching Calls with a greater id before resuming live.
        #[serde(default)]
        since: Option<CallId>,
    },
}

/// The greeting sent on connect: protocol version + heartbeat cadence so the
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

/// A live Call push.
fn call_frame(call: &StoredCall) -> String {
    serde_json::json!({ "t": "call", "call": call }).to_string()
}

/// A reconnect-catch-up Call push — same shape as a live one, flagged so the
/// client can enqueue it as history rather than as fresh live activity.
fn catchup_frame(call: &StoredCall) -> String {
    serde_json::json!({ "t": "call", "call": call, "catchup": true }).to_string()
}

/// A notice that the client lagged the fanout and `skipped` Calls were dropped.
fn lagged_frame(skipped: u64) -> String {
    serde_json::json!({ "t": "lagged", "skipped": skipped }).to_string()
}

/// `GET /api/live` — upgrade to a WebSocket and run the per-connection loop.
pub async fn ws_handler(ws: WebSocketUpgrade, State(state): State<AppState>) -> Response {
    // `on_upgrade` runs the connection in a task of its own, which would
    // otherwise lose the request span the upgrade was logged under (#28).
    // Carrying it means everything this socket says stays attributable to the
    // request that opened it.
    let span = Span::current();
    ws.on_upgrade(move |socket| handle_socket(socket, state).instrument(span))
}

/// A connection's lifetime, bracketed by the two lines that are the socket's
/// answer to the request log (#28): one when a listener arrives and one when it
/// leaves — never one per frame, however much crosses it (ADR-0011 rule 8).
///
/// No address on either line: a listener's IP never appears above DEBUG (rule
/// 5), and rdio-scanner's habit of logging every listener's IP and access-code
/// ident at info (`client.go:152`) is the thing we are deliberately not doing.
async fn handle_socket(socket: WebSocket, state: AppState) {
    info!("live-feed listener connected");
    let connected_at = Instant::now();
    run_connection(socket, state).await;
    // `connected_ms`, not `duration_ms`: a Call already has a `duration_ms` (its
    // audio length) and the request line has a `duration_us`. One grep, one
    // meaning.
    let connected_ms = connected_at.elapsed().as_millis() as u64;
    info!(connected_ms, "live-feed listener disconnected");
}

async fn run_connection(mut socket: WebSocket, state: AppState) {
    let mut receiver = state.live.subscribe();
    let heartbeat_period = state.live.heartbeat();
    let mut conn = ConnState::new();

    // Greet the client before anything else.
    if socket
        .send(Message::Text(hello_frame(heartbeat_period).into()))
        .await
        .is_err()
    {
        return;
    }

    // First heartbeat fires one full period from now — no ping on connect.
    let mut ticker = interval_at(Instant::now() + heartbeat_period, heartbeat_period);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            incoming = socket.recv() => {
                match incoming {
                    Some(Ok(message)) => {
                        // Any inbound frame proves the connection is alive.
                        conn.heartbeat.on_activity();
                        match message {
                            Message::Text(text) => {
                                if handle_text(&mut socket, &state, &mut conn, text.as_str())
                                    .await
                                    .is_err()
                                {
                                    break;
                                }
                            }
                            Message::Close(_) => break,
                            // ping/pong/binary: liveness already recorded above.
                            _ => {}
                        }
                    }
                    Some(Err(_)) | None => break,
                }
            }
            broadcasted = receiver.recv() => {
                match on_broadcast(broadcasted, &conn) {
                    BroadcastAction::Send(text) => {
                        if socket.send(Message::Text(text.into())).await.is_err() {
                            break;
                        }
                    }
                    BroadcastAction::Skip => {}
                    BroadcastAction::Close => break,
                }
            }
            _ = ticker.tick() => {
                match conn.heartbeat.on_tick() {
                    Beat::Ping => {
                        if socket.send(Message::Ping(bytes::Bytes::new())).await.is_err() {
                            break;
                        }
                    }
                    // A half-open connection is a listener who stopped hearing
                    // anything and doesn't know it — worth saying out loud, and
                    // the thing rdio leaves lingering in silence. Bounded by
                    // connections, not frames (rule 8).
                    Beat::Dead => {
                        let heartbeat_ms = heartbeat_period.as_millis() as u64;
                        warn!(
                            heartbeat_ms,
                            "live-feed listener reaped after an unanswered heartbeat"
                        );
                        break;
                    }
                }
            }
        }
    }
}

/// Signals that a socket send failed — the peer is gone, so the caller ends the
/// connection loop. A named marker beats a bare `Err(())` for intent.
struct Disconnected;

/// Apply a client text message. Fails only if the socket died mid-send (the
/// caller then ends the loop); malformed/unknown frames are ignored.
async fn handle_text(
    socket: &mut WebSocket,
    state: &AppState,
    conn: &mut ConnState,
    text: &str,
) -> Result<(), Disconnected> {
    let Ok(ClientMessage::Sub { sel, all, since }) = serde_json::from_str::<ClientMessage>(text)
    else {
        return Ok(());
    };
    conn.sub.selection = sel;
    conn.sub.all = all;
    // Protocol detail, so DEBUG (rule 7): a listener re-subscribes every time
    // they toggle a Talkgroup. The shape of the selection, never its contents —
    // what someone listens to is theirs (rule 5's spirit).
    let systems = conn.sub.selection.len();
    let catchup = since.is_some();
    debug!(systems, all, catchup, "live-feed subscription updated");
    // Ack so the client knows the subscription is live before it relies on
    // receiving matching Calls.
    socket
        .send(Message::Text(subscribed_frame().into()))
        .await
        .map_err(|_| Disconnected)?;
    if let Some(since) = since {
        send_catchup(socket, &state.db, conn, since).await?;
    }
    Ok(())
}

/// Backfill the Calls a reconnecting client missed (#9): every matching Call with
/// `id > since`, bounded by [`CATCHUP_MAX_CALLS`], oldest-first, each flagged
/// `catchup`. Best-effort — a DB error is swallowed so a transient failure never
/// kills the live connection; only a dead socket propagates.
///
/// Delivery is **at-least-once**: a Call ingested in the window between the
/// client's connect and this query can be delivered both here (catch-up) and via
/// the live stream. Call ids are unique, so the client dedups by id — which it
/// does anyway to drive replay/history — rather than the server tracking a
/// high-water mark (unsafe, since concurrent ingests can broadcast out of id
/// order).
async fn send_catchup(
    socket: &mut WebSocket,
    db: &sea_orm::DatabaseConnection,
    conn: &ConnState,
    since: CallId,
) -> Result<(), Disconnected> {
    let models = match repo::recent_calls_since(db, since, CATCHUP_MAX_CALLS).await {
        Ok(models) => models,
        Err(error) => {
            // Swallowed for the connection's sake, never for the operator's: a
            // catch-up that quietly returns nothing is indistinguishable from a
            // client that missed nothing (#29).
            warn!(%error, since, "live-feed catch-up query failed");
            return Ok(());
        }
    };
    // Hitting the bound means the client's history has a gap only archive search
    // (#13) can fill — the one fact about a backfill worth reading.
    let truncated = models.len() as u64 == CATCHUP_MAX_CALLS;
    let mut sent = 0u64;
    for model in models {
        if let Ok(Some(view)) = repo::stored_call(db, model.id).await
            && conn.wants(&view)
        {
            socket
                .send(Message::Text(catchup_frame(&view).into()))
                .await
                .map_err(|_| Disconnected)?;
            sent += 1;
        }
    }
    // One line per reconnect, never one per Call (rule 8).
    debug!(since, sent, truncated, "live-feed catch-up sent");
    Ok(())
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
            date_time: None,
            timestamp: None,
            audio_mime: None,
            object_key: String::new(),
            audio_url: String::new(),
        }
    }

    fn subscription(pairs: &[(&str, &str)], all: bool) -> Subscription {
        let mut selection: HashMap<String, HashMap<String, bool>> = HashMap::new();
        for (system, talkgroup) in pairs {
            selection
                .entry((*system).to_string())
                .or_default()
                .insert((*talkgroup).to_string(), true);
        }
        Subscription { selection, all }
    }

    /// A connection with the given selection, full v1 access scope.
    fn conn(pairs: &[(&str, &str)]) -> ConnState {
        conn_scoped(pairs, AccessScope::All)
    }

    fn conn_scoped(pairs: &[(&str, &str)], scope: AccessScope) -> ConnState {
        ConnState {
            sub: subscription(pairs, false),
            scope,
            heartbeat: Heartbeat::new(),
        }
    }

    // --- Subscription matrix + patch matching (ConnState::wants) --------------

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
            .selection
            .entry("11".to_string())
            .or_default()
            .insert("54241".to_string(), false);
        assert!(!c.wants(&call(11, 54241)));
    }

    #[test]
    fn all_wants_everything() {
        let c = ConnState {
            sub: subscription(&[], true),
            scope: AccessScope::All,
            heartbeat: Heartbeat::new(),
        };
        assert!(c.wants(&call(1, 2)));
        assert!(c.wants(&call(999, 888)));
    }

    #[test]
    fn empty_subscription_wants_nothing() {
        assert!(!conn(&[]).wants(&call(11, 54241)));
    }

    /// **Hold System** (#11, spec US 11): the listener narrows to the System
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

    /// **Avoid** (#11, spec US 14) has to work against the all-on selection a
    /// listener starts with, so an explicit entry is an exception to `all`
    /// rather than something `all` overrules.
    #[test]
    fn explicit_entry_overrides_global_all() {
        let mut avoiding = ConnState {
            sub: subscription(&[], true),
            scope: AccessScope::All,
            heartbeat: Heartbeat::new(),
        };
        avoiding
            .sub
            .selection
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
            .selection
            .entry("11".to_string())
            .or_default()
            .insert("54241".to_string(), false);

        assert!(!held.wants(&call(11, 54241)), "avoided inside the hold");
        assert!(held.wants(&call(11, 999)), "the rest of the system plays");
    }

    /// A selection of nothing but exclusions is still "all off" — there is no
    /// point resolving patches for it.
    #[test]
    fn a_selection_of_only_exclusions_is_all_off() {
        let mut nothing = conn(&[]);
        nothing
            .sub
            .selection
            .entry("11".to_string())
            .or_default()
            .insert("54241".to_string(), false);

        assert!(!nothing.wants(&call(11, 999)));
    }

    /// Patch fanout: a Call on a Talkgroup the listener didn't select still
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

    /// The scope gate applies on top of the subscription: subscribing to a
    /// Talkgroup you're not permitted to hear delivers nothing.
    #[test]
    fn scope_denies_even_a_subscribed_talkgroup() {
        let scope = AccessScope::Systems(HashMap::from([(
            11,
            TalkgroupScope::Only(HashSet::from([100])),
        )]));
        let c = conn_scoped(&[("11", "300")], scope); // subscribed to 300, only allowed 100
        assert!(!c.wants(&call(11, 300)));
    }

    /// Scope gates patch matching too: a patched Talkgroup outside the scope
    /// doesn't leak the Call.
    #[test]
    fn scope_gates_patched_talkgroup() {
        let scope = AccessScope::Systems(HashMap::from([(
            11,
            TalkgroupScope::Only(HashSet::from([300])),
        )]));
        // Subscribed to both, but only 300 is in scope; call on 100 patched to 300.
        let c = conn_scoped(&[("11", "100"), ("11", "300")], scope);
        let patched = call_with_patches(11, 100, vec![300]);
        assert!(c.wants(&patched), "matches on the in-scope patch 300");

        // Now only 100 is in scope, but the listener didn't subscribe to 100.
        let scope2 = AccessScope::Systems(HashMap::from([(
            11,
            TalkgroupScope::Only(HashSet::from([100])),
        )]));
        let c2 = conn_scoped(&[("11", "300")], scope2);
        assert!(
            !c2.wants(&patched),
            "subscribed to 300 (out of scope) and not to 100"
        );
    }

    // --- is_all_off ----------------------------------------------------------

    #[rstest]
    #[case(subscription(&[], false), true)] // truly empty
    #[case(subscription(&[("11", "100")], false), false)] // one enabled
    #[case(subscription(&[], true), false)] // global all-on
    fn is_all_off_cases(#[case] sub: Subscription, #[case] expected: bool) {
        assert_eq!(sub.is_all_off(), expected);
    }

    #[test]
    fn is_all_off_with_only_disabled_entries() {
        let mut sub = subscription(&[], false);
        sub.selection
            .entry("11".to_string())
            .or_default()
            .insert("100".to_string(), false);
        assert!(sub.is_all_off(), "an explicit-false entry is still all-off");
    }

    // --- LiveFeed ------------------------------------------------------------

    #[test]
    fn default_livefeed_uses_the_default_heartbeat() {
        assert_eq!(LiveFeed::default().heartbeat(), DEFAULT_HEARTBEAT);
    }

    #[test]
    fn with_heartbeat_sets_the_period() {
        let feed = LiveFeed::with_heartbeat(Duration::from_millis(250));
        assert_eq!(feed.heartbeat(), Duration::from_millis(250));
    }

    // --- Heartbeat -----------------------------------------------------------

    #[test]
    fn heartbeat_pings_then_declares_dead() {
        let mut hb = Heartbeat::new();
        assert_eq!(hb.on_tick(), Beat::Ping, "first tick pings");
        assert_eq!(hb.on_tick(), Beat::Dead, "unanswered ping -> dead");
    }

    #[test]
    fn heartbeat_activity_resets_liveness() {
        let mut hb = Heartbeat::new();
        assert_eq!(hb.on_tick(), Beat::Ping);
        hb.on_activity(); // pong (or any frame) arrived
        assert_eq!(hb.on_tick(), Beat::Ping, "answered -> ping again, not dead");
    }

    // --- on_broadcast decision ----------------------------------------------

    #[test]
    fn broadcast_delivers_a_matching_call() {
        let c = conn(&[("11", "54241")]);
        let action = on_broadcast(Ok(Arc::new(call(11, 54241))), &c);
        assert_eq!(action, BroadcastAction::Send(call_frame(&call(11, 54241))));
    }

    #[test]
    fn broadcast_skips_a_non_matching_call() {
        let c = conn(&[("11", "54241")]);
        assert_eq!(
            on_broadcast(Ok(Arc::new(call(11, 99999))), &c),
            BroadcastAction::Skip
        );
    }

    /// A lagging listener is told (so it can refetch from the archive) *and*
    /// written down: a client that cannot keep up is a symptom the operator owns,
    /// and the count is the measure of it (#29).
    #[test]
    fn broadcast_lag_becomes_a_lagged_notice_and_a_warning() {
        let capture = LogCapture::start();
        let c = conn(&[("11", "54241")]);

        let action = on_broadcast(Err(broadcast::error::RecvError::Lagged(7)), &c);

        assert_eq!(action, BroadcastAction::Send(lagged_frame(7)));
        let logged = capture.text();
        assert!(logged.contains(" WARN "), "{logged}");
        assert!(logged.contains("skipped=7"), "{logged}");
    }

    /// Delivering a Call the listener wanted is not news — the ordinary case
    /// runs per Call per connection and must stay silent (ADR-0011 rule 8).
    #[test]
    fn an_ordinary_delivery_logs_nothing() {
        let capture = LogCapture::start();
        let c = conn(&[("11", "54241")]);

        on_broadcast(Ok(Arc::new(call(11, 54241))), &c);
        on_broadcast(Ok(Arc::new(call(11, 99999))), &c);

        assert_eq!(capture.text(), "", "one line per frame is a hot loop");
    }

    #[test]
    fn broadcast_closed_ends_the_connection() {
        let c = conn(&[("11", "54241")]);
        assert_eq!(
            on_broadcast(Err(broadcast::error::RecvError::Closed), &c),
            BroadcastAction::Close
        );
    }

    // --- Frame builders ------------------------------------------------------

    #[test]
    fn hello_frame_carries_protocol_and_heartbeat() {
        let value: serde_json::Value =
            serde_json::from_str(&hello_frame(Duration::from_secs(30))).unwrap();
        assert_eq!(value["t"], "hello");
        assert_eq!(value["protocol"], PROTOCOL_VERSION);
        assert_eq!(value["heartbeatMs"], 30_000);
    }

    #[test]
    fn call_and_catchup_frames_differ_only_by_the_flag() {
        let c = call(11, 54241);
        let live: serde_json::Value = serde_json::from_str(&call_frame(&c)).unwrap();
        let catchup: serde_json::Value = serde_json::from_str(&catchup_frame(&c)).unwrap();
        assert_eq!(live["t"], "call");
        assert!(live.get("catchup").is_none(), "live has no catchup flag");
        assert_eq!(catchup["t"], "call");
        assert_eq!(catchup["catchup"], true);
        assert_eq!(live["call"], catchup["call"], "same call payload");
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
        let ClientMessage::Sub { since, all, .. } = msg;
        assert_eq!(since, Some(42));
        assert!(!all);
    }

    #[test]
    fn sub_message_without_since_defaults_to_none() {
        let msg: ClientMessage = serde_json::from_str(r#"{"t":"sub","all":true}"#).unwrap();
        let ClientMessage::Sub { since, all, sel } = msg;
        assert_eq!(since, None);
        assert!(all);
        assert!(sel.is_empty());
    }
}
