//! **Downstream** forwarding (#52, spec US 1–2): the Calls this Instance sends
//! on to its rdio-compatible peers.
//!
//! Forwarding only. *Receiving* a peer's downstream is [`crate::ingest`] with an
//! **API key** and needs nothing built, which is why this module has no inbound
//! half.
//!
//! # The shape
//!
//! - [`Downstream`] is a configured peer as the sender uses it. Its scope is a
//!   [`Selection`] — **the same type and the same rule** the live feed and Web
//!   Push are scoped by ([`Selection::reaches_channels`]) — and [`routed_to`] is
//!   the routing decision, purely.
//! - A delivery is **enqueued inside the transaction that stores the Call**
//!   ([`crate::db::repo::queue_deliveries`]), so "the Call exists" and "the
//!   Call is owed to this peer" are one fact a crash cannot separate.
//! - [`sender`] drains that queue: per peer, head first, one attempt in flight,
//!   with [`backoff`] between tries.
//! - [`dialect`] builds the rdio multipart body, and is pure.
//!
//! # Improving on rdio-scanner
//!
//! rdio has this feature (`server/downstream.go`) and every difference below is
//! a defect it has, found by reading it:
//!
//! | rdio | Radio-Scout |
//! |---|---|
//! | POSTs inline on the ingest goroutine; an unreachable peer is logged and the Call **dropped** (`downstream.go:416`) | a durable queue, retried with backoff — an outage costs delay, not Calls |
//! | scope tests `call.Talkgroup.TalkgroupRef` only (`downstream.go:99`), so a **Patch** never reaches a peer subscribed to the patched channel | [`Selection::reaches_channels`], which walks the patches — the live feed's own rule |
//! | serializes `units`/`frequencies` with Go's **default struct-field names** (`CallUnit` has no JSON tags), which its own ingest parser does not read | the field names its parser actually reads, pinned by a fixture *and* by a real forward between two Instances |
//! | sends 14 fields, omitting `site` and `frequency` that its parser accepts | every field [`crate::ingest::call_upload`] reads, in an order that survives its parser |
//! | stores the peer's key in plaintext **and returns it** from the admin API | stored of necessity, never returned and never logged |
//!
//! The `units`/`frequencies` one is worth stating plainly because it is
//! invisible from either end: an rdio peer receives `[{"Id":0,"CallId":0,
//! "Offset":1.5,"UnitRef":4424000}]` and parses `id`/`label`/`offset` out of it,
//! finding none of them. Every radio and every frequency sample is silently lost
//! on every forwarded Call, and the receiving instance looks perfectly healthy.
//!
//! # What the dialect cannot carry
//!
//! The rdio upload dialect has no field for the **Emergency** flag or for
//! encryption — only Trunk Recorder's native meta does ([`crate::ingest`]) — so
//! neither survives a forward, to us or to rdio. An **Encrypted Call** is
//! therefore never queued at all: it has no audio object, and the dialect
//! requires one.

pub mod dialect;
pub mod sender;

use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::selection::Selection;
use crate::worker::Meter;

/// What this Worker is called on a status surface (#93, #70).
pub const WORKER: &str = "downstream";

/// The path a Downstream's base URL is joined with — rdio's own forwarder
/// appends the same one (`downstream.go:316`), so an Operator configures the
/// address they would give a recorder.
pub const UPLOAD_PATH: &str = "/api/call-upload";

/// How forwarding behaves — and the `[downstream]` section itself (ADR-0012,
/// #87).
///
/// Policy only. The peers themselves are **Curation** (CONTEXT.md): rows an
/// Operator edits in the browser, never anything written in `radio-scout.toml`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DownstreamConfig {
    /// How long a peer has to answer one delivery before it counts as failed.
    #[serde(rename = "timeout_secs", with = "crate::config::secs")]
    pub timeout: Duration,
    /// How long to wait before the first retry. Doubles per attempt up to
    /// [`DownstreamConfig::retry_max`].
    #[serde(rename = "retry_initial_secs", with = "crate::config::secs")]
    pub retry_initial: Duration,
    /// The ceiling on that doubling — how often a peer that has been down for
    /// hours is poked.
    #[serde(rename = "retry_max_secs", with = "crate::config::secs")]
    pub retry_max: Duration,
}

impl Default for DownstreamConfig {
    fn default() -> Self {
        DownstreamConfig {
            // rdio's own downstream client timeout, and a reasonable one: a peer
            // on the other side of a domestic uplink receiving a minute of audio
            // needs more than a browser's few seconds.
            timeout: Duration::from_secs(30),
            // Long enough that a peer restarting is not hammered, short enough
            // that a blip costs one Call's worth of delay.
            retry_initial: Duration::from_secs(5),
            // **The ceiling is also the recovery latency**, which is the number
            // that matters: a peer down overnight is polled once a minute rather
            // than thousands of times, *and* its backlog starts moving within a
            // minute of it returning. A longer cap saves a poll an hour and
            // costs every one of those Calls the same delay again.
            retry_max: Duration::from_secs(60),
        }
    }
}

impl DownstreamConfig {
    /// How long after `attempts` failures the next try is due.
    ///
    /// Exponential from [`DownstreamConfig::retry_initial`], capped at
    /// [`DownstreamConfig::retry_max`]. Pure, and separated from the sender for
    /// the reason every policy here is: the interesting cases are the extremes —
    /// the first retry, the hundredth, and a configuration whose initial delay
    /// is already above its own cap — and none of them is reachable by leaving a
    /// peer down for an afternoon.
    pub fn backoff(&self, attempts: i32) -> Duration {
        let initial = self.retry_initial;
        if initial.is_zero() {
            return Duration::ZERO;
        }
        // `attempts` is the count *including* the one that just failed, so the
        // first retry waits exactly `retry_initial`. Shifting is bounded before
        // it is applied: a peer down for a week reaches attempt counts that
        // would overflow a doubling long before they reach the cap by
        // arithmetic.
        // 31, not 32: the shift below is on a `u32`, and `1u32 << 32` is not a
        // large number, it is a panic. A peer down for a week reaches attempt
        // counts far past either bound, and nothing on this path may be the
        // thing that brings a scanner down.
        let doublings = attempts.saturating_sub(1).clamp(0, 31) as u32;
        let scaled = initial.saturating_mul(1u32 << doublings);
        scaled.min(self.retry_max.max(initial))
    }
}

/// **What a stored scope means, written once.**
///
/// A scope that will not parse selects **nothing** — the safe direction, since
/// the alternative is forwarding an Operator's whole County to a peer that was
/// scoped to one channel. Three places read one (the sender routing a Call, the
/// curation listing rendering it, the configuration document exporting it), and
/// the *screen* has to show the same empty scope that routing is applying rather
/// than a shape it invented — so the fallback is one function rather than three
/// `unwrap_or_default()`s that can drift apart.
pub fn scope_of(stored: &str) -> Selection {
    serde_json::from_str(stored).unwrap_or_default()
}

/// A configured Downstream, as the sender uses one.
///
/// The scope arrives parsed rather than as the JSON the row stores, because
/// [`scope_of`] decides what an unreadable one means once, where it can be said
/// out loud.
///
/// **`Downstream` is CONTEXT.md's word, and this is the type for it.** The
/// harness's `common::Peer` is deliberately something else: this is *our
/// configuration of* the far end, and that is the far end itself.
#[derive(Debug, Clone, PartialEq)]
pub struct Downstream {
    pub id: i64,
    pub url: String,
    pub api_key: String,
    pub scope: Selection,
}

impl Downstream {
    /// Read a stored row.
    pub fn from_row(row: &crate::db::entities::downstream::Model) -> Self {
        Downstream {
            id: row.id,
            url: row.url.clone(),
            api_key: row.api_key.clone(),
            scope: scope_of(&row.scope),
        }
    }

    /// Where a delivery is POSTed: the configured base URL with
    /// [`UPLOAD_PATH`] appended, joined so that a trailing slash on either side
    /// cannot produce `//api/call-upload`.
    pub fn upload_url(&self) -> String {
        format!("{}{UPLOAD_PATH}", self.url.trim_end_matches('/'))
    }

    /// Whether a Call on this System reaching these Talkgroups goes to this
    /// Downstream.
    ///
    /// The Talkgroups are the Call's own **plus its patches**, both canonical —
    /// the same set the live feed asks over, which is the half rdio gets wrong.
    pub fn admits(&self, system_ref: i64, talkgroups: impl Iterator<Item = i64>) -> bool {
        self.scope
            .reaches_channels(system_ref, talkgroups, |_, _| true)
    }
}

/// Which of these Downstreams a Call on `system_ref` reaching `talkgroups` is
/// owed to — **the routing decision, purely** (#96's shape).
///
/// It lives here rather than inside the write that uses it, because a write is
/// not where a policy belongs: [`crate::db::repo::queue_deliveries`] is handed
/// the ids and asks nothing about Selections, which keeps the data layer clear
/// of a domain module the way #98 left it.
pub fn routed_to(peers: &[Downstream], system_ref: i64, talkgroups: &[i64]) -> Vec<i64> {
    peers
        .iter()
        .filter(|peer| peer.admits(system_ref, talkgroups.iter().copied()))
        .map(|peer| peer.id)
        .collect()
}

/// What a peer's answer means for the delivery that produced it — **the whole
/// retry policy**, as one closed decision.
///
/// The split that matters is not "did it work" but **"will the same bytes ever
/// be accepted?"**. A peer answering `401` has an Operator who mistyped a key
/// and will fix it, so the backlog must survive; a peer answering `417
/// Incomplete call data` is telling us this Call is unacceptable and will say so
/// forever, and retrying it blocks every Call behind it for as long as the peer
/// exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// The peer took it. The queue row goes.
    Delivered,
    /// Not now. The row stays and is tried again after [`DownstreamConfig::backoff`].
    Retry,
    /// Not ever. The row goes, with a line saying why — because unlike a
    /// retry, this one loses a Call, and nothing else would record it.
    Abandon,
}

impl Verdict {
    /// What an HTTP status from a peer means.
    pub fn of_status(status: u16) -> Verdict {
        match status {
            200..=299 => Verdict::Delivered,
            // The body is the problem, and it will not change: rdio answers 417
            // `Incomplete call data` for a Call it cannot use, and 400/413/415/422
            // are the standard spellings of the same thing. Retrying wedges the
            // queue behind a Call the peer has already judged.
            400 | 413 | 415 | 417 | 422 => Verdict::Abandon,
            // Everything else is the peer or the path *right now* — a wrong key
            // (401/403), a wrong URL (404), a restart (502/503), a rate limit
            // (429). All of them are things an Operator fixes, and the backlog
            // is what makes fixing them worth doing.
            _ => Verdict::Retry,
        }
    }
}

/// Why a delivery ended, as one word — the slug a log line carries and the
/// admin screen shows, so the two cannot describe the same event differently.
///
/// A closed vocabulary rather than a formatted sentence, for ADR-0011 rule 6:
/// `reason=peer-refused` greps and `"the peer refused it"` does not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failed {
    /// The peer answered, unhappily. Carries the status, because "which 4xx" is
    /// the whole diagnostic and an Operator has to see it beside the slug.
    Status(u16),
    /// The request never got an answer — DNS, connection refused, TLS, timeout.
    Unreachable,
    /// The Call is no longer in the Archive, or its audio object is not there.
    /// Retention is entitled to prune a Call a peer has been down longer than.
    Vanished,
    /// The Archive could not be read at all. Distinct from [`Failed::Vanished`]
    /// because one is a Call that is gone and the other is a database that is
    /// not answering, and only the second is worth waking anybody for.
    Unreadable,
}

impl Failed {
    /// The machine-readable slug.
    pub fn slug(&self) -> &'static str {
        match self {
            Failed::Status(_) => "peer-refused",
            Failed::Unreachable => "peer-unreachable",
            Failed::Vanished => "call-gone",
            Failed::Unreadable => "archive-unreadable",
        }
    }

    /// What this failure does to the delivery.
    pub fn verdict(&self) -> Verdict {
        match self {
            Failed::Status(status) => Verdict::of_status(*status),
            Failed::Unreachable | Failed::Unreadable => Verdict::Retry,
            // There is nothing left to send and nothing that will bring it
            // back.
            Failed::Vanished => Verdict::Abandon,
        }
    }
}

impl std::fmt::Display for Failed {
    /// What gets stored on the row and shown to an Operator — the slug, plus
    /// the status where there is one.
    ///
    /// **Never the transport error.** reqwest's own `Display` renders as "error
    /// sending request for url (…)", and while a peer's host is not a listener's
    /// address, its URL is an Operator-supplied string that may carry a query
    /// parameter. The slug says everything actionable: a peer that cannot be
    /// reached is a peer that cannot be reached.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Failed::Status(status) => write!(f, "{} ({status})", self.slug()),
            other => f.write_str(other.slug()),
        }
    }
}

/// The forwarding subsystem, cloned into every handler.
///
/// Unlike [`crate::push::Push`] and [`crate::enhance::Enhancer`] there is no
/// disabled form: a Downstream is a **row**, so an Instance with no peers is one
/// whose roster is empty rather than one whose feature is off. The Worker it
/// spawns sleeps until something is enqueued and costs nothing until then.
///
/// It does share their double-spawn guard, though — see [`Inner::start`].
#[derive(Clone)]
pub struct Downstreams(Arc<Inner>);

struct Inner {
    config: DownstreamConfig,
    /// Attempts owed — see [`Downstreams::owes`] for what one unit of this
    /// Worker's work is.
    meter: Arc<Meter>,
    /// Poked when something is enqueued, so a Call is forwarded the moment it
    /// is stored rather than on the next timer.
    wake: tokio::sync::Notify,
    /// One client for the life of the Instance, so a peer's TLS session and
    /// connection are reused across deliveries rather than renegotiated per
    /// Call — which on a Pi forwarding a Call a second is most of the cost.
    client: reqwest::Client,
    /// Deliveries that have left the queue since this Instance started — see
    /// [`Downstreams::deliveries_settled`].
    settled: tokio::sync::watch::Sender<u64>,
    /// The right to be the sender, taken once by [`sender::spawn`] (#93).
    ///
    /// `Downstreams` is `Clone` and hangs off `AppState`, so `self`-by-value
    /// cannot be the guard here the way it is for `retention::Sweeper`. What
    /// this holds is the right to *drain*, and two holders would be worse than
    /// merely wasteful: they would each attempt the same head, so a peer would
    /// receive one Call twice and the in-order promise would stop being one —
    /// and [`Downstreams::woken`] is a `notify_one`, so a wake-up would go to
    /// one of them rather than to both.
    start: crate::worker::Handoff<()>,
}

impl Default for Downstreams {
    fn default() -> Self {
        Downstreams::new(DownstreamConfig::default())
    }
}

impl Downstreams {
    pub fn new(config: DownstreamConfig) -> Self {
        let client = reqwest::Client::builder()
            .timeout(config.timeout)
            .build()
            // A builder carrying nothing but a timeout has no failure mode of
            // its own — `build` reports a TLS backend that will not initialise
            // or a malformed proxy setting, and this configures neither.
            .expect("an HTTP client with only a timeout configured");
        Downstreams(Arc::new(Inner {
            config,
            meter: Meter::new(),
            wake: tokio::sync::Notify::new(),
            client,
            settled: tokio::sync::watch::Sender::new(0),
            start: crate::worker::Handoff::new(()),
        }))
    }

    /// What the sender owes, for the Worker envelope and the status registry.
    pub fn meter(&self) -> Arc<Meter> {
        self.0.meter.clone()
    }

    /// Claim the right to be the sender. `None` means one is already running.
    pub(crate) fn claim(&self) -> Option<()> {
        self.0.start.take()
    }

    /// The client every delivery is sent with.
    pub fn client(&self) -> &reqwest::Client {
        &self.0.client
    }

    /// Resolves the next time something is queued.
    ///
    /// `notify_one` **stores a permit** when nobody is waiting, so a Call queued
    /// while the sender was mid-attempt still wakes it on the next pass rather
    /// than waiting for a backoff that may be minutes away. That is the whole
    /// reason this is a `Notify` and not a channel the sender might have been
    /// looking away from.
    pub async fn woken(&self) {
        self.0.wake.notified().await;
    }

    /// Note that `count` deliveries have been queued, and wake the sender.
    ///
    /// **One unit of this Worker's work is "the sender has caught up with what
    /// was handed to it"** — not one delivery, and not one attempt. Getting this
    /// right took two wrong answers, and both are worth writing down because
    /// both look obviously correct:
    ///
    /// - *One admission per delivery, settled when it lands.* A peer that is
    ///   down never lands anything, so the Worker never goes idle and
    ///   `app.settle()` (#93) hangs for **every** test in the suite the moment
    ///   one peer is unreachable.
    /// - *One admission per delivery, settled when it has been attempted.* Only
    ///   the **head** of a peer's queue is ever attempted — that is what makes
    ///   in-order draining true — so three queued Calls behind a stuck head hold
    ///   two admissions that nothing will ever settle. Same hang, harder to see.
    ///
    /// So an admission means "there is something new for you to look at", and
    /// the sender discharges it by *looking*: [`sender`] settles everything
    /// outstanding at the end of any pass that leaves nothing in flight. On a
    /// working peer that is after the queue has drained, because a pass with an
    /// attempt running never settles; on a peer that is down it is after one
    /// attempt has failed, which is exactly the honest answer — the sender has
    /// caught up, and what it caught up to is a backlog.
    ///
    /// A **retry** is therefore deliberately outside this accounting: nobody
    /// handed it over, and counting it is the first wrong answer above. What a
    /// test waits on to watch a recovery is
    /// [`Downstreams::deliveries_settled`].
    ///
    /// The Operator-facing **queue depth** is a different number entirely:
    /// `COUNT(*)` on the queue table, per peer, which is what the admin screen
    /// shows and what survives a restart.
    ///
    /// Called **after** the storing transaction has committed, never inside it:
    /// an admission for a delivery that then rolled back would be a debt nothing
    /// could ever settle, and every later `settle()` in the process would wait
    /// on it forever.
    pub fn owes(&self, count: usize) {
        for _ in 0..count {
            self.0.meter.admit_untracked();
        }
        if count > 0 {
            self.0.wake.notify_one();
        }
    }

    /// Take on the sender's **first pass**, before its task is spawned.
    ///
    /// Without this a freshly started Instance reads idle before the sender has
    /// looked at anything, because a meter that has admitted nothing is idle by
    /// definition — so `settle()` would return while the roster read was still
    /// to come, and *any* statement-count assertion anywhere in the suite would
    /// be a race with it. That is not hypothetical: it is how
    /// `tests/mining.rs::mining_one_stored_call_costs_a_fixed_number_of_statements`
    /// began failing on Postgres and not on SQLite, which is the worst way to
    /// find out.
    ///
    /// It is also just the #93 rule applied honestly — work is owed from where
    /// it is handed over, and a boot hands the sender a pass.
    pub(crate) fn owes_a_first_pass(&self) {
        self.0.meter.admit_untracked();
    }

    /// Discharge everything outstanding — the sender has looked and has nothing
    /// in flight.
    ///
    /// Reading the depth and settling that many can never overshoot: anything
    /// admitted between the two reads simply stays outstanding, which is the
    /// direction that keeps `idle()` honest.
    pub(crate) fn caught_up(&self) {
        self.0.meter.settle_n(self.0.meter.load().depth);
    }

    /// Note that one delivery has left the queue — taken by the peer, or
    /// abandoned.
    ///
    /// **This Worker's `done` count, in the only place it can live.**
    /// [`crate::worker::Load::done`] is "work settled", and for every other
    /// Worker that is the same thing as "items completed" — but this one's unit
    /// of work is *"I have caught up"* ([`Downstreams::owes`]), so its meter
    /// cannot also answer "how many Calls have gone out". A peer's own
    /// `last_success_ms` and `consecutive_failures` are the readings an Operator
    /// acts on, and this is the process-lifetime total beside them.
    ///
    /// Published **after** the row is gone, which is also what makes it the
    /// signal a recovery is asserted on: the peer having answered is a moment
    /// earlier than the queue having shrunk, and a test that read the depth on
    /// the former would race the delete on the latter.
    pub(crate) fn delivery_settled(&self) {
        self.0.settled.send_modify(|settled| *settled += 1);
    }

    /// Wait until `n` deliveries have left the queue since this Instance
    /// started.
    ///
    /// The wait a **retry** needs, and the reason it is not an *attempt* count:
    /// only the head of a peer's queue is attempted per pass, so how many
    /// failures happen before a peer comes back is a matter of timing, while how
    /// many Calls end up leaving the queue is not.
    pub async fn deliveries_settled(&self, n: u64) {
        // `Err` is a dropped sender, which cannot happen: it lives in the `Arc`
        // this borrow is holding.
        let _ = self
            .0
            .settled
            .subscribe()
            .wait_for(|settled| *settled >= n)
            .await;
    }

    /// The policy this Instance forwards under.
    pub fn config(&self) -> &DownstreamConfig {
        &self.0.config
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    fn peer(scope: serde_json::Value) -> Downstream {
        Downstream::from_row(&crate::db::entities::downstream::Model {
            id: 1,
            label: None,
            url: String::from("https://peer.example"),
            api_key: String::from("s3cret"),
            scope: scope.to_string(),
            disabled: false,
            last_success_ms: None,
            last_failure_ms: None,
            last_failure: None,
            consecutive_failures: 0,
            created_at_ms: 0,
        })
    }

    /// The one that matters, and the one rdio gets wrong: a **Patch** puts a
    /// transmission on a channel that is genuinely not the one it was
    /// transmitted on, and a peer subscribed to *that* channel is entitled to
    /// it. rdio's `HasAccess` compares `call.Talkgroup.TalkgroupRef` and stops
    /// there, so this Call reaches nobody.
    #[test]
    fn a_patched_call_reaches_a_peer_scoped_to_the_patched_channel() {
        let peer = peer(serde_json::json!({ "sel": { "11": { "500": true } } }));

        assert!(
            peer.admits(11, [100, 500].into_iter()),
            "patched onto a scoped channel"
        );
        assert!(
            !peer.admits(11, [100].into_iter()),
            "the same Call unpatched"
        );
    }

    /// Scope is the live feed's algebra whole, not a subset of it — including
    /// the exception, which is what lets an Operator forward a System *except*
    /// one sensitive channel.
    #[rstest]
    #[case(serde_json::json!({ "all": true }), 11, 100, true, "everything")]
    #[case(serde_json::json!({}), 11, 100, false, "a peer nobody has scoped")]
    #[case(serde_json::json!({ "sel": { "11": { "*": true } } }), 11, 999, true, "a whole System")]
    #[case(serde_json::json!({ "sel": { "11": { "*": true } } }), 22, 999, false, "another System")]
    #[case(
        serde_json::json!({ "all": true, "sel": { "11": { "100": false } } }),
        11, 100, false, "an exception to everything"
    )]
    fn scope_is_the_live_feeds_own_rule(
        #[case] scope: serde_json::Value,
        #[case] system_ref: i64,
        #[case] talkgroup_ref: i64,
        #[case] expected: bool,
        #[case] what: &str,
    ) {
        assert_eq!(
            peer(scope).admits(system_ref, std::iter::once(talkgroup_ref)),
            expected,
            "{what}"
        );
    }

    /// A scope that will not parse selects nothing. The other direction —
    /// treating it as "everything" — would forward an Operator's whole County to
    /// a peer they had scoped to one channel, which is the failure you cannot
    /// take back.
    #[test]
    fn an_unparseable_scope_forwards_nothing() {
        assert!(!peer(serde_json::json!("not a selection")).admits(11, std::iter::once(100)));
    }

    /// An Operator pastes the address they would give a recorder, with or
    /// without the slash they happened to copy.
    #[rstest]
    #[case("https://peer.example")]
    #[case("https://peer.example/")]
    #[case("https://peer.example//")]
    fn the_upload_path_is_appended_once(#[case] url: &str) {
        let mut peer = peer(serde_json::json!({}));
        peer.url = url.to_string();

        assert_eq!(peer.upload_url(), "https://peer.example/api/call-upload");
    }

    /// **Will these same bytes ever be accepted?** — the question the whole
    /// retry policy turns on. A mistyped key is fixed by an Operator and the
    /// backlog is what makes fixing it worthwhile; a Call the peer calls
    /// incomplete would block every Call behind it forever.
    #[rstest]
    #[case(200, Verdict::Delivered)]
    #[case(204, Verdict::Delivered)]
    #[case(400, Verdict::Abandon)]
    #[case(413, Verdict::Abandon)]
    #[case(415, Verdict::Abandon)]
    #[case(417, Verdict::Abandon)]
    #[case(422, Verdict::Abandon)]
    #[case(401, Verdict::Retry)]
    #[case(403, Verdict::Retry)]
    #[case(404, Verdict::Retry)]
    #[case(429, Verdict::Retry)]
    #[case(500, Verdict::Retry)]
    #[case(503, Verdict::Retry)]
    fn a_peers_answer_decides_whether_it_is_worth_asking_again(
        #[case] status: u16,
        #[case] expected: Verdict,
    ) {
        assert_eq!(Verdict::of_status(status), expected, "status {status}");
    }

    /// Every way a delivery can fail says one word, and that word decides
    /// whether the Call survives.
    #[rstest]
    #[case(Failed::Unreachable, "peer-unreachable", Verdict::Retry)]
    #[case(Failed::Unreadable, "archive-unreadable", Verdict::Retry)]
    #[case(Failed::Vanished, "call-gone", Verdict::Abandon)]
    #[case(Failed::Status(503), "peer-refused", Verdict::Retry)]
    #[case(Failed::Status(417), "peer-refused", Verdict::Abandon)]
    fn a_failure_carries_its_slug_and_its_consequence(
        #[case] failed: Failed,
        #[case] slug: &str,
        #[case] verdict: Verdict,
    ) {
        assert_eq!(failed.slug(), slug);
        assert_eq!(failed.verdict(), verdict);
    }

    /// What gets written down names the status, because "the peer refused it"
    /// without a number sends an Operator nowhere — and never renders the
    /// transport error, which would carry the peer's URL.
    #[test]
    fn a_stored_failure_names_the_status_and_nothing_else() {
        assert_eq!(Failed::Status(401).to_string(), "peer-refused (401)");
        assert_eq!(Failed::Unreachable.to_string(), "peer-unreachable");
    }

    /// The first retry waits the configured delay and each one after it doubles,
    /// up to the cap — so a peer restarting is not hammered and a peer down
    /// overnight is polled twice a minute rather than a thousand times.
    #[test]
    fn backoff_doubles_up_to_its_ceiling() {
        let config = DownstreamConfig {
            retry_initial: Duration::from_secs(5),
            retry_max: Duration::from_secs(60),
            ..DownstreamConfig::default()
        };

        assert_eq!(config.backoff(1), Duration::from_secs(5));
        assert_eq!(config.backoff(2), Duration::from_secs(10));
        assert_eq!(config.backoff(3), Duration::from_secs(20));
        assert_eq!(config.backoff(4), Duration::from_secs(40));
        assert_eq!(config.backoff(5), Duration::from_secs(60), "capped");
        // A peer down for a week reaches attempt counts whose doubling would
        // overflow long before the arithmetic reaches the cap. Nothing on this
        // path may be the thing that panics.
        assert_eq!(config.backoff(i32::MAX), Duration::from_secs(60));
    }

    /// Two settings an Operator can legitimately write, and neither may divide
    /// by anything or return something absurd: no wait at all, and a first
    /// delay already above the ceiling meant to bound it.
    #[rstest]
    #[case(
        Duration::ZERO,
        Duration::from_secs(60),
        Duration::ZERO,
        "no wait at all"
    )]
    #[case(
        Duration::from_secs(90),
        Duration::from_secs(60),
        Duration::from_secs(90),
        "an initial delay above its own cap is honoured, not shortened"
    )]
    fn backoff_survives_the_settings_that_look_wrong(
        #[case] retry_initial: Duration,
        #[case] retry_max: Duration,
        #[case] expected: Duration,
        #[case] what: &str,
    ) {
        let config = DownstreamConfig {
            retry_initial,
            retry_max,
            ..DownstreamConfig::default()
        };

        assert_eq!(config.backoff(1), expected, "{what}");
    }
}
