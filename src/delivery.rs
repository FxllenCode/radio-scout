//! **The durable outbound queue**, written once (#52, #54).
//!
//! Radio-Scout sends Calls out of this Instance in two directions, and they are
//! different in every way except the one that is hard: [`crate::downstream`]
//! POSTs a Call's audio to an rdio-compatible peer, [`crate::webhook`] POSTs a
//! marked Call's facts as JSON to an Operator's own URL. What they share is the
//! *machinery* — a delivery row written where the Call is, a Worker that drains
//! it per sink head-first, a retry policy, and the accounting that makes
//! `settle()` (#93) answerable — and that machinery is subtle enough that two
//! copies of it would be two chances to get the subtlety wrong in only one of
//! them.
//!
//! So the shape is: **this module owns the algorithm, each sink owns its
//! errand.** [`Outbox`] is the four questions the loop asks of a sink, and
//! everything below it — what a Call looks like on the wire, what a refusal
//! means to an Operator, which rows are read — belongs to the sink.
//!
//! # What is genuinely shared, and why each piece is
//!
//! - [`Verdict`] — *"will these same bytes ever be accepted?"*. One answer, so a
//!   `417` cannot wedge one queue and drain the other.
//! - [`Retry`] — exponential backoff with a ceiling, bounded before it is
//!   applied so a sink down for a week cannot overflow a shift.
//! - [`plan`] — the ordering rule: one attempt in flight per sink, and a head
//!   that is not yet due **stops** that sink's queue rather than being stepped
//!   over. Stepping over it would reorder a sink's archive in a way it cannot
//!   detect.
//! - [`Dispatcher`] — the meter, the wake-up, the double-spawn guard, and the
//!   settled counter. Its accounting is the piece with the most ways to be
//!   subtly wrong (see [`Dispatcher::owes`]), and it is now wrong or right in
//!   exactly one place.
//! - [`run`] — the loop itself, including the check-then-act race
//!   [`Dispatcher::caught_up`] exists to avoid.
//!
//! # What is deliberately *not* shared
//!
//! The **port** below one attempt. `downstream::sender::Deliveries` asks for a
//! Call in the rdio dialect and its audio bytes; `webhook::sender::Sinks` asks
//! for a Call as a Listener sees it and no bytes at all. Merging those would
//! produce an interface that is the union of two errands and the shape of
//! neither — the opposite of what #37's seams are for.
//!
//! The **sentences**. Each sink's log lines are its own; what this module fixes
//! is the *level* each settlement earns, which is the policy (ADR-0011 rule 7)
//! rather than the wording — #92's rule that a surface's shape may differ where
//! the policy underneath may not.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use tracing::{debug, info, warn};

use crate::call::CallId;
use crate::worker::{Meter, Worker};

/// Why one of an [`Outbox`]'s answers could not be given.
///
/// The cause as text, the way [`crate::enhance::Failure`] and
/// [`crate::mining::sweep::Failure`] each carry theirs: all anything here does
/// with it is put it on a WARN line an Operator reads (ADR-0011 rule 4), and
/// keeping a driver's error type in the port's signature would put sea-orm and
/// `object_store` in the interface of something whose subject is sinks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure(String);

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl<E: std::error::Error> From<E> for Failure {
    fn from(error: E) -> Self {
        Failure(error.to_string())
    }
}

/// Something this Instance delivers to, as the loop needs to know it.
///
/// One method, because the loop only ever needs to say *which* queue it is
/// looking at. Everything else about a sink — where it is, what it is scoped to,
/// what credential it holds — is the sink module's business and never reaches
/// here.
pub trait Sink: Clone + Send + Sync + 'static {
    /// The row id its queue is keyed on.
    fn id(&self) -> i64;
}

/// One delivery at the head of a sink's queue.
///
/// The row's own fields, minus the ones only the writer needs — so a substitute
/// builds one without inventing a `queued_at_ms` nothing reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Queued {
    pub id: i64,
    pub call_id: CallId,
    pub attempts: i32,
    pub next_attempt_ms: i64,
}

/// What happens to a delivery once its attempt has ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Settled {
    /// The sink took it. The row goes.
    Taken,
    /// The sink will never take it. The row goes, and a Call has been lost —
    /// which is why this one is said out loud and the retry is not.
    Abandoned { why: String },
    /// Not now. The row stays, due again at `next_attempt_ms`.
    Deferred {
        attempts: i32,
        next_attempt_ms: i64,
        why: String,
    },
}

/// What a sink's answer means for the delivery that produced it — **the whole
/// retry policy**, as one closed decision.
///
/// The split that matters is not "did it work" but **"will the same bytes ever
/// be accepted?"**. A sink answering `401` has an Operator who mistyped a key
/// and will fix it, so the backlog must survive; a sink answering `417
/// Incomplete call data` is telling us this Call is unacceptable and will say so
/// forever, and retrying it blocks every Call behind it for as long as the sink
/// exists.
///
/// **Both directions apply to a Webhook too**, and the awkward case is worth
/// naming: a Discord webhook that has been deleted in Discord answers `404`
/// forever, and this policy retries it. That is deliberate and is the same
/// judgement as `401` — an Operator fixes it by pasting a new URL in, and the
/// backlog is the thing that makes fixing it worth doing. What must never happen
/// is the reverse: abandoning on a status the Operator *can* fix loses Calls
/// silently.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// The sink took it. The queue row goes.
    Delivered,
    /// Not now. The row stays and is tried again after [`Retry::backoff`].
    Retry,
    /// Not ever. The row goes, with a line saying why — because unlike a
    /// retry, this one loses a Call, and nothing else would record it.
    Abandon,
}

impl Verdict {
    /// What an HTTP status from a sink means.
    pub fn of_status(status: u16) -> Verdict {
        match status {
            200..=299 => Verdict::Delivered,
            // The body is the problem, and it will not change: rdio answers 417
            // `Incomplete call data` for a Call it cannot use, Discord answers
            // 400 for an embed it will not render, and 413/415/422 are the
            // standard spellings of the same thing. Retrying wedges the queue
            // behind a Call the sink has already judged.
            400 | 413 | 415 | 417 | 422 => Verdict::Abandon,
            // Everything else is the sink or the path *right now* — a wrong key
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
/// `reason=sink-refused` greps and `"the sink refused it"` does not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failed {
    /// The sink answered, unhappily. Carries the status, because "which 4xx" is
    /// the whole diagnostic and an Operator has to see it beside the slug.
    Status(u16),
    /// The request never got an answer — DNS, connection refused, TLS, timeout.
    Unreachable,
    /// The Call is no longer in the Archive, or its audio object is not there.
    /// Retention is entitled to prune a Call a sink has been down longer than.
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
            Failed::Status(_) => "sink-refused",
            Failed::Unreachable => "sink-unreachable",
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
    /// sending request for url (…)", and both sinks' URLs are Operator-supplied
    /// strings that may carry a query parameter — a Webhook's *is* its
    /// credential. The slug says everything actionable: a sink that cannot be
    /// reached is a sink that cannot be reached.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Failed::Status(status) => write!(f, "{} ({status})", self.slug()),
            other => f.write_str(other.slug()),
        }
    }
}

/// How hard a sink is tried again — the `retry_initial_secs`/`retry_max_secs`
/// pair every sink's configuration section carries.
///
/// A value rather than a method on each configuration type, so [`decide`] can be
/// written once: the two sections spell their own keys and their own defaults
/// (a Webhook's timeout is a few hundred bytes of JSON, a Downstream's is a
/// minute of audio), but what a doubling *means* is not theirs to differ on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Retry {
    pub initial: Duration,
    pub max: Duration,
}

impl Retry {
    /// How long after `attempts` failures the next try is due.
    ///
    /// Exponential from [`Retry::initial`], capped at [`Retry::max`]. Pure, and
    /// separated from the senders for the reason every policy here is: the
    /// interesting cases are the extremes — the first retry, the hundredth, and
    /// a configuration whose initial delay is already above its own cap — and
    /// none of them is reachable by leaving a sink down for an afternoon.
    pub fn backoff(&self, attempts: i32) -> Duration {
        let initial = self.initial;
        if initial.is_zero() {
            return Duration::ZERO;
        }
        // `attempts` is the count *including* the one that just failed, so the
        // first retry waits exactly `initial`. Shifting is bounded before it is
        // applied: a sink down for a week reaches attempt counts that would
        // overflow a doubling long before they reach the cap by arithmetic.
        // 31, not 32: the shift below is on a `u32`, and `1u32 << 32` is not a
        // large number, it is a panic. Nothing on this path may be the thing
        // that brings a scanner down.
        let doublings = attempts.saturating_sub(1).clamp(0, 31) as u32;
        let scaled = initial.saturating_mul(1u32 << doublings);
        scaled.min(self.max.max(initial))
    }
}

/// The four questions the draining loop asks of a sink.
///
/// Everything *inside* one attempt — which rows to read, what to build, where to
/// POST it, how to settle the row — is the sink's own, behind its own seam. What
/// this trait fixes is only the shape of the loop around it.
#[async_trait::async_trait]
pub trait Outbox: Send + Sync + 'static {
    /// What this sink's queues are keyed on.
    type Sink: Sink;

    /// Every sink currently accepting deliveries.
    async fn roster(&self) -> Result<Vec<Self::Sink>, Failure>;

    /// The head of one sink's queue — due or not.
    async fn head(&self, sink_id: i64) -> Result<Option<Queued>, Failure>;

    /// One delivery attempt, start to finish. Reports nothing: everything it
    /// could say, it says itself — including the span its lines ride in, which
    /// is the sink's because only the sink knows what to call itself.
    async fn attempt(&self, sink: &Self::Sink, head: &Queued);

    /// How many deliveries a previous process left, across every sink.
    async fn depth(&self) -> Result<u64, Failure>;

    /// The Instance's clock, not the machine's (#90): a test that arranges "this
    /// delivery is due" must measure from the same instant the sender stamps
    /// with.
    fn now_ms(&self) -> i64;
}

/// What one pass of the loop should do.
///
/// `PartialEq` and not `Eq`, because a [`crate::downstream::Downstream`]'s scope
/// is a [`crate::selection::Selection`] (a `HashMap`), which has no total
/// equality.
#[derive(Debug, PartialEq)]
pub struct Plan<S> {
    /// The sinks with a delivery due now, one each.
    pub due: Vec<(S, Queued)>,
    /// The earliest moment an *undue* head becomes due, if any is waiting.
    pub next_due: Option<i64>,
}

// Derived `Default` would demand `S: Default`, which no sink is — a sink is a
// stored row and there is no such thing as a blank one.
impl<S> Default for Plan<S> {
    fn default() -> Self {
        Plan {
            due: Vec::new(),
            next_due: None,
        }
    }
}

/// Decide what to attempt, and when to wake up next.
///
/// **The ordering rule lives here.** A sink already attempting is left alone, so
/// nothing overtakes; and a head that is not yet due stops that sink's queue
/// rather than being stepped over, because stepping over it would reorder a
/// sink's archive in a way it cannot detect.
///
/// Everything that can fail is a *skip*, never a stop: a queue that cannot be
/// read is asked again on the next wake-up, and the rows are in the same
/// database either way.
pub async fn plan<O: Outbox + ?Sized>(
    outbox: &O,
    in_flight: &HashSet<i64>,
    now_ms: i64,
) -> Plan<O::Sink> {
    let roster = match outbox.roster().await {
        Ok(roster) => roster,
        // The roster is unreadable — the database is down or mid-migration.
        // Nothing is lost, and nothing is worth waking a clock for.
        Err(error) => {
            warn!(reason = %"roster-unreadable", %error, "could not read a delivery roster");
            return Plan::default();
        }
    };

    let mut plan = Plan::default();
    for sink in roster {
        if in_flight.contains(&sink.id()) {
            continue;
        }
        let head = match outbox.head(sink.id()).await {
            Ok(Some(head)) => head,
            Ok(None) => continue,
            Err(error) => {
                let sink = sink.id();
                warn!(reason = %"queue-unreadable", sink, %error, "could not read a delivery queue");
                continue;
            }
        };
        match head.next_attempt_ms > now_ms {
            // Waiting out a backoff. The *soonest* of them is when this loop
            // has something to do again — take the minimum rather than the last
            // one seen, or a sink backing off for five minutes would hold up one
            // due in five milliseconds.
            true => {
                plan.next_due = Some(match plan.next_due {
                    Some(soonest) => soonest.min(head.next_attempt_ms),
                    None => head.next_attempt_ms,
                });
            }
            false => plan.due.push((sink, head)),
        }
    }
    plan
}

/// What an attempt's outcome does to the queue row — **pure**, so every arm is a
/// value a test constructs.
pub fn decide(outcome: Option<Failed>, head: &Queued, retry: Retry, now_ms: i64) -> Settled {
    let Some(failed) = outcome else {
        return Settled::Taken;
    };
    let attempts = head.attempts.saturating_add(1);
    let why = failed.to_string();
    match failed.verdict() {
        Verdict::Abandon => Settled::Abandoned { why },
        // `Delivered` cannot reach here — `outcome` is `Some`, which only a
        // failure produces — so it is folded in with the retry rather than given
        // an unreachable arm of its own.
        _ => Settled::Deferred {
            attempts,
            next_attempt_ms: now_ms.saturating_add(retry.backoff(attempts).as_millis() as i64),
            why,
        },
    }
}

/// Write down what happened to one delivery (ADR-0011 rules 3, 7 and 8).
///
/// `sink` is a **field**, not part of the message (rule 6): one static sentence
/// per outcome, and `sink=downstream` / `sink=webhook` tells an Operator which
/// direction it was going.
pub fn say(settled: &Settled, sink: &'static str) {
    match settled {
        // DEBUG: one line per delivered Call per sink, forever (rule 8). The
        // reading an Operator wants is `last_success_ms`, which is a column
        // rather than a line to grep for.
        Settled::Taken => debug!(%sink, "delivered"),
        // WARN, and **the only record a Call was lost** (rule 3): the sink will
        // never take it, so nothing further will ever mention it again.
        Settled::Abandoned { why } => warn!(
            reason = %"abandoned",
            failure = %why,
            %sink,
            "a Call was dropped for this sink and will not be retried"
        ),
        Settled::Deferred {
            attempts,
            next_attempt_ms,
            why,
        } => {
            // **INFO, not WARN, and the level is the whole judgement.** Rule 7
            // spends WARN on something *rejected or dropped*; nothing has been.
            // The queue has absorbed the outage, which is the feature working —
            // a notable normal event. What earns a WARN is a Call actually lost,
            // which is the `Abandoned` arm above and the only line that says one
            // was.
            //
            // On the first failure only. Every one after it is DEBUG, because a
            // sink down for a night would otherwise write a line every few
            // minutes saying exactly the same thing (rule 8), and the standing
            // reading an Operator acts on is `consecutive_failures` and
            // `last_failure` on the row — which is what the admin screen shows
            // and what #70 will read.
            if *attempts == 1 {
                info!(
                    failure = %why,
                    %sink,
                    "a delivery sink stopped accepting Calls; they are queued and will be retried"
                );
            }
            debug!(
                failure = %why,
                %sink,
                attempts,
                next_attempt_ms,
                "a delivery will be retried"
            );
        }
    }
}

/// Sleep until `at_ms`, or forever if there is nothing to wait for.
///
/// The `forever` arm is what an Instance with no sinks rests in — the `select!`
/// disables this branch, so it is never actually polled, and it exists so the
/// branch type-checks without an `Option` dance at the call site.
async fn sleep_until(at_ms: Option<i64>) {
    match at_ms {
        Some(at_ms) => {
            // Against the wall clock rather than a stored deadline, because the
            // row's `next_attempt_ms` is absolute and the process may have
            // restarted since it was written. Saturating: a clock that jumped
            // backwards must produce a long sleep, never a panic.
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|since| since.as_millis() as i64)
                .unwrap_or(0);
            tokio::time::sleep(std::time::Duration::from_millis(
                at_ms.saturating_sub(now).max(0) as u64,
            ))
            .await;
        }
        None => std::future::pending().await,
    }
}

/// The accounting and the wake-up every outbound sink subsystem holds.
///
/// [`crate::downstream::Downstreams`] and [`crate::webhook::Webhooks`] are each
/// a configuration section wrapped around one of these. Splitting it out is not
/// tidiness: [`Dispatcher::owes`] and [`Dispatcher::caught_up`] between them
/// encode two races that took three wrong answers to get right, and a second
/// copy would be a second chance to get one of them wrong in only one direction.
pub struct Dispatcher {
    /// Attempts owed — see [`Dispatcher::owes`] for what one unit of this
    /// Worker's work is.
    meter: Arc<Meter>,
    /// Poked when something is enqueued, so a Call goes out the moment it is
    /// stored rather than on the next timer.
    wake: tokio::sync::Notify,
    /// One client for the life of the Instance, so a sink's TLS session and
    /// connection are reused across deliveries rather than renegotiated per
    /// Call — which on a Pi taking a Call a second is most of the cost.
    client: reqwest::Client,
    /// Deliveries that have left the queue since this Instance started — see
    /// [`Dispatcher::deliveries_settled`].
    settled: tokio::sync::watch::Sender<u64>,
    /// The right to be the sender, taken once by the subsystem's `spawn` (#93).
    ///
    /// The owning type is `Clone` and hangs off `AppState`, so `self`-by-value
    /// cannot be the guard here the way it is for `retention::Sweeper`. What
    /// this holds is the right to *drain*, and two holders would be worse than
    /// merely wasteful: they would each attempt the same head, so a sink would
    /// receive one Call twice and the in-order promise would stop being one —
    /// and [`Dispatcher::woken`] is a `notify_one`, so a wake-up would go to one
    /// of them rather than to both.
    start: crate::worker::Handoff<()>,
}

impl Dispatcher {
    /// A dispatcher whose client gives a sink `timeout` to answer one delivery.
    pub fn new(timeout: Duration) -> Arc<Self> {
        let client = reqwest::Client::builder()
            .timeout(timeout)
            .build()
            // A builder carrying nothing but a timeout has no failure mode of
            // its own — `build` reports a TLS backend that will not initialise
            // or a malformed proxy setting, and this configures neither.
            .expect("an HTTP client with only a timeout configured");
        Arc::new(Dispatcher {
            meter: Meter::new(),
            wake: tokio::sync::Notify::new(),
            client,
            settled: tokio::sync::watch::Sender::new(0),
            start: crate::worker::Handoff::new(()),
        })
    }

    /// What the sender owes, for the Worker envelope and the status registry.
    pub fn meter(&self) -> Arc<Meter> {
        self.meter.clone()
    }

    /// Claim the right to be the sender. `None` means one is already running.
    pub fn claim(&self) -> Option<()> {
        self.start.take()
    }

    /// The client every delivery is sent with.
    pub fn client(&self) -> &reqwest::Client {
        &self.client
    }

    /// Resolves the next time something is queued.
    ///
    /// `notify_one` **stores a permit** when nobody is waiting, so a Call queued
    /// while the sender was mid-attempt still wakes it on the next pass rather
    /// than waiting for a backoff that may be minutes away. That is the whole
    /// reason this is a `Notify` and not a channel the sender might have been
    /// looking away from.
    pub async fn woken(&self) {
        self.wake.notified().await;
    }

    /// Note that `count` deliveries have been queued, and wake the sender.
    ///
    /// **One unit of this Worker's work is "the sender has caught up with what
    /// was handed to it"** — not one delivery, and not one attempt. Getting this
    /// right took two wrong answers, and both are worth writing down because
    /// both look obviously correct:
    ///
    /// - *One admission per delivery, settled when it lands.* A sink that is
    ///   down never lands anything, so the Worker never goes idle and
    ///   `app.settle()` (#93) hangs for **every** test in the suite the moment
    ///   one sink is unreachable.
    /// - *One admission per delivery, settled when it has been attempted.* Only
    ///   the **head** of a sink's queue is ever attempted — that is what makes
    ///   in-order draining true — so three queued Calls behind a stuck head hold
    ///   two admissions that nothing will ever settle. Same hang, harder to see.
    ///
    /// So an admission means "there is something new for you to look at", and
    /// the sender discharges it by *looking*: [`run`] settles everything
    /// outstanding at the end of any pass that leaves nothing in flight. On a
    /// working sink that is after the queue has drained, because a pass with an
    /// attempt running never settles; on a sink that is down it is after one
    /// attempt has failed, which is exactly the honest answer — the sender has
    /// caught up, and what it caught up to is a backlog.
    ///
    /// A **retry** is therefore deliberately outside this accounting: nobody
    /// handed it over, and counting it is the first wrong answer above. What a
    /// test waits on to watch a recovery is
    /// [`Dispatcher::deliveries_settled`].
    ///
    /// The Operator-facing **queue depth** is a different number entirely:
    /// `COUNT(*)` on the queue table, per sink, which is what the admin screen
    /// shows and what survives a restart.
    ///
    /// Called **after** the storing transaction has committed, never inside it:
    /// an admission for a delivery that then rolled back would be a debt nothing
    /// could ever settle, and every later `settle()` in the process would wait
    /// on it forever.
    pub fn owes(&self, count: usize) {
        for _ in 0..count {
            self.meter.admit_untracked();
        }
        if count > 0 {
            self.wake.notify_one();
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
    pub fn owes_a_first_pass(&self) {
        self.meter.admit_untracked();
    }

    /// What the sender is owed **right now** — read at the top of a pass, before
    /// it looks at anything.
    fn outstanding(&self) -> u64 {
        self.meter.load().depth
    }

    /// Discharge the `owed` items the sender had been handed *before* it looked.
    ///
    /// **The count has to be the one read before the pass, and that is the whole
    /// of this method's correctness.** Settling the depth as it stands *after* a
    /// pass discharges anything admitted while the pass was running — an upload
    /// that committed its delivery row a microsecond after the queue was read —
    /// so `settle()` returns having promised that a Call was considered when it
    /// was not, and the delivery goes out some time later.
    ///
    /// It is a check-then-act race and it reads as a flake: it failed
    /// `tests/downstream.rs::re_scoping_a_peer_keeps_the_key_it_already_had` on
    /// Postgres in CI and never once locally on SQLite, because the window is
    /// exactly as wide as a database round trip.
    ///
    /// Work admitted *during* a pass is simply left outstanding — the wake-up
    /// that came with it guarantees another pass, which will discharge it.
    fn caught_up(&self, owed: u64) {
        self.meter.settle_n(owed);
    }

    /// Note that one delivery has left the queue — taken by the sink, or
    /// abandoned.
    ///
    /// **This Worker's `done` count, in the only place it can live.**
    /// [`crate::worker::Load::done`] is "work settled", and for every other
    /// Worker that is the same thing as "items completed" — but this one's unit
    /// of work is *"I have caught up"* ([`Dispatcher::owes`]), so its meter
    /// cannot also answer "how many Calls have gone out". A sink's own
    /// `last_success_ms` and `consecutive_failures` are the readings an Operator
    /// acts on, and this is the process-lifetime total beside them.
    ///
    /// Published **after** the row is gone, which is also what makes it the
    /// signal a recovery is asserted on: the sink having answered is a moment
    /// earlier than the queue having shrunk, and a test that read the depth on
    /// the former would race the delete on the latter.
    pub fn delivery_settled(&self) {
        self.settled.send_modify(|settled| *settled += 1);
    }

    /// Wait until `n` deliveries have left the queue since this Instance
    /// started.
    ///
    /// The wait a **retry** needs, and the reason it is not an *attempt* count:
    /// only the head of a sink's queue is attempted per pass, so how many
    /// failures happen before a sink comes back is a matter of timing, while how
    /// many Calls end up leaving the queue is not.
    pub async fn deliveries_settled(&self, n: u64) {
        // `Err` is a dropped sender, which cannot happen: it lives in the `Arc`
        // this borrow is holding.
        let _ = self
            .settled
            .subscribe()
            .wait_for(|settled| *settled >= n)
            .await;
    }
}

/// Drain `outbox` for the life of this Instance.
///
/// # The three rules it exists to keep
///
/// **It never blocks ingest or the fanout.** An upload's only contact with a
/// sink is the rows written inside its own transaction; everything after that
/// happens here, on a Worker, behind a wake-up.
///
/// **It drains in order, per sink.** Only the *head* of a sink's queue is ever
/// attempted, and a head that is not yet due is waited for rather than skipped —
/// see [`plan`].
///
/// **One sink's trouble is its own.** Attempts run in a `JoinSet`, one in flight
/// per sink, so a sink that takes its whole configured timeout to fail delays
/// nothing but its own queue. That is the arrangement rdio-scanner does not have
/// and cannot: it POSTs inline on the ingest goroutine, so a slow peer is
/// backpressure on the recorder.
pub fn run<O: Outbox>(name: &'static str, outbox: Arc<O>, dispatch: Arc<Dispatcher>) -> Worker {
    let meter = dispatch.meter();
    Worker::start(name, meter, move |mut stop| async move {
        // What a previous process left. Admitted before the loop's first pass,
        // so there is no window in which an Instance reads idle because the
        // sender has not woken up yet (#93) — and, less obviously, so that the
        // counters *balance*: every pass this Worker completes discharges what
        // was handed over, and a leftover row whose admission nobody made would
        // push `done` permanently ahead of `admitted` and make every later
        // `idle()` return early.
        //
        // A read that fails costs the accounting, never the boot: the rows are
        // still there and will still be sent, and refusing to start a scanner
        // over a `COUNT` would be the wrong trade by a distance.
        match outbox.depth().await {
            Ok(depth) => dispatch.owes(depth as usize),
            Err(error) => warn!(
                reason = %"resume-failed",
                %error,
                sink = %name,
                "could not count the deliveries a previous process left; the sender's depth will read low until they drain"
            ),
        }

        // One attempt in flight per sink, and the set is what remembers which.
        let mut attempts: tokio::task::JoinSet<i64> = tokio::task::JoinSet::new();
        let mut in_flight: HashSet<i64> = HashSet::new();

        loop {
            // Read **before** the pass looks at anything: these are the items
            // this pass is about to consider, and discharging any more than them
            // would discharge work nobody has looked at yet.
            let owed = dispatch.outstanding();
            let Plan { due, next_due } = plan(outbox.as_ref(), &in_flight, outbox.now_ms()).await;
            for (sink, head) in due {
                let sink_id = sink.id();
                in_flight.insert(sink_id);
                let outbox = outbox.clone();
                attempts.spawn(async move {
                    outbox.attempt(&sink, &head).await;
                    sink_id
                });
            }

            // **Where the Worker goes idle**, and the only place it does.
            // Nothing in flight means every sink has either been tried just now
            // or is waiting out a backoff nobody asked for — either way the
            // sender has caught up with what was handed to it, which is what an
            // admission means here. A pass with an attempt running settles
            // nothing, so on a working sink this is reached only once the queue
            // has drained.
            if attempts.is_empty() {
                dispatch.caught_up(owed);
            }

            tokio::select! {
                biased;
                _ = stop.cancelled() => break,
                Some(finished) = attempts.join_next(), if !attempts.is_empty() => {
                    // One attempt ended, however it ended — including by
                    // panicking, which `join_next` reports rather than swallows.
                    if let Ok(sink_id) = finished {
                        in_flight.remove(&sink_id);
                    }
                }
                // A Call has just been queued: send it now rather than on some
                // timer, because a scanner sink is only useful if it is prompt.
                () = dispatch.woken() => {}
                // The earliest moment a backing-off head becomes due. `None` is
                // "nothing is waiting on a clock", and the wake-up above is then
                // the only thing that can start work — which is exactly the
                // steady state of an Instance with no sinks.
                () = sleep_until(next_due), if next_due.is_some() => {}
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::LogCapture;
    use rstest::rstest;

    /// **Will these same bytes ever be accepted?** — the question the whole
    /// retry policy turns on. A mistyped key is fixed by an Operator and the
    /// backlog is what makes fixing it worthwhile; a Call the sink calls
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
    fn a_sinks_answer_decides_whether_it_is_worth_asking_again(
        #[case] status: u16,
        #[case] expected: Verdict,
    ) {
        assert_eq!(Verdict::of_status(status), expected, "status {status}");
    }

    /// Every way a delivery can fail says one word, and that word decides
    /// whether the Call survives.
    #[rstest]
    #[case(Failed::Unreachable, "sink-unreachable", Verdict::Retry)]
    #[case(Failed::Unreadable, "archive-unreadable", Verdict::Retry)]
    #[case(Failed::Vanished, "call-gone", Verdict::Abandon)]
    #[case(Failed::Status(503), "sink-refused", Verdict::Retry)]
    #[case(Failed::Status(417), "sink-refused", Verdict::Abandon)]
    fn a_failure_carries_its_slug_and_its_consequence(
        #[case] failed: Failed,
        #[case] slug: &str,
        #[case] verdict: Verdict,
    ) {
        assert_eq!(failed.slug(), slug);
        assert_eq!(failed.verdict(), verdict);
    }

    /// What gets written down names the status, because "the sink refused it"
    /// without a number sends an Operator nowhere — and never renders the
    /// transport error, which would carry the sink's URL.
    #[test]
    fn a_stored_failure_names_the_status_and_nothing_else() {
        assert_eq!(Failed::Status(401).to_string(), "sink-refused (401)");
        assert_eq!(Failed::Unreachable.to_string(), "sink-unreachable");
    }

    /// The first retry waits the configured delay and each one after it doubles,
    /// up to the cap — so a sink restarting is not hammered and a sink down
    /// overnight is polled twice a minute rather than a thousand times.
    #[test]
    fn backoff_doubles_up_to_its_ceiling() {
        let retry = Retry {
            initial: Duration::from_secs(5),
            max: Duration::from_secs(60),
        };

        assert_eq!(retry.backoff(1), Duration::from_secs(5));
        assert_eq!(retry.backoff(2), Duration::from_secs(10));
        assert_eq!(retry.backoff(3), Duration::from_secs(20));
        assert_eq!(retry.backoff(4), Duration::from_secs(40));
        assert_eq!(retry.backoff(5), Duration::from_secs(60), "capped");
        // A sink down for a week reaches attempt counts whose doubling would
        // overflow long before the arithmetic reaches the cap. Nothing on this
        // path may be the thing that panics.
        assert_eq!(retry.backoff(i32::MAX), Duration::from_secs(60));
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
        #[case] initial: Duration,
        #[case] max: Duration,
        #[case] expected: Duration,
        #[case] what: &str,
    ) {
        assert_eq!(Retry { initial, max }.backoff(1), expected, "{what}");
    }

    /// The backoff is applied from *now*, and the attempt count climbs — which
    /// is what makes a sink down for a night polled twice a minute rather than
    /// thousands of times.
    #[test]
    fn a_retry_is_scheduled_from_now_and_counts_the_attempt() {
        let head = Queued {
            id: 7,
            call_id: 107,
            attempts: 2,
            next_attempt_ms: 0,
        };

        let settled = decide(
            Some(Failed::Unreachable),
            &head,
            Retry {
                initial: Duration::from_secs(5),
                max: Duration::from_secs(60),
            },
            1_000,
        );

        assert_eq!(
            settled,
            Settled::Deferred {
                attempts: 3,
                // The third attempt failed, so the fourth waits four doublings
                // in: 5s * 2^2.
                next_attempt_ms: 1_000 + 20_000,
                why: String::from("sink-unreachable"),
            }
        );
    }

    #[test]
    fn a_delivered_call_settles_as_taken() {
        let head = Queued {
            id: 7,
            call_id: 107,
            attempts: 0,
            next_attempt_ms: 0,
        };

        assert_eq!(
            decide(
                None,
                &head,
                Retry {
                    initial: Duration::from_secs(5),
                    max: Duration::from_secs(60)
                },
                1_000
            ),
            Settled::Taken
        );
    }

    /// **What each settlement writes down** (ADR-0011 rules 3, 7 and 8).
    ///
    /// The abandon is the one that matters: it is the only record that a Call
    /// was lost, because nothing will ever mention that delivery again. The
    /// retry is INFO on its *first* failure and DEBUG after, so a sink down for
    /// a night does not write a line every few minutes saying the same thing.
    #[rstest]
    #[case::taken(Settled::Taken, "", "")]
    #[case::abandoned(
        Settled::Abandoned { why: String::from("sink-refused (417)") },
        " WARN ",
        "abandoned"
    )]
    #[case::first_failure(
        Settled::Deferred { attempts: 1, next_attempt_ms: 0, why: String::from("x") },
        " INFO ",
        "stopped accepting Calls"
    )]
    #[case::later_failure(
        Settled::Deferred { attempts: 2, next_attempt_ms: 0, why: String::from("x") },
        "",
        ""
    )]
    fn a_settlement_says_what_an_operator_has_to_act_on(
        #[case] settled: Settled,
        #[case] level: &str,
        #[case] needle: &str,
    ) {
        let capture = LogCapture::start();

        say(&settled, "downstream");

        let logged = capture.text();
        // An empty expectation means nothing above DEBUG, which is what a Call
        // that simply worked — and a retry an Operator has already been told
        // about — must cost on a Pi taking a Call a second.
        assert!(logged.contains(level), "{logged}");
        assert!(logged.contains(needle), "{logged}");
        if level.is_empty() {
            assert!(!logged.contains(" WARN "), "{logged}");
            assert!(!logged.contains(" INFO "), "{logged}");
        }
    }

    /// Which direction a line is about is a **field**, so one grep separates a
    /// peer's outage from a Discord webhook's without two vocabularies.
    #[test]
    fn every_line_names_the_sink_it_is_about() {
        let capture = LogCapture::start();

        say(
            &Settled::Abandoned {
                why: String::from("sink-refused (417)"),
            },
            "webhook",
        );

        let logged = capture.text();
        assert!(logged.contains("sink=webhook"), "{logged}");
    }

    /// **A pass discharges only what it was handed before it looked** — the
    /// check-then-act race that broke
    /// `tests/downstream.rs::re_scoping_a_peer_keeps_the_key_it_already_had` on
    /// Postgres in CI and never once on SQLite.
    ///
    /// The sender reads what it is owed, reads the queue, then discharges.
    /// Discharging the depth *as it stands afterwards* also discharges the
    /// upload that committed its delivery row while that read was in flight — so
    /// `settle()` returns having claimed the Call was considered, and the
    /// delivery goes out some time later, by which point the test has already
    /// asserted the sink received nothing. The window is exactly one database
    /// round trip wide, which is why a slower dialect finds it and a faster one
    /// does not.
    #[test]
    fn a_pass_discharges_only_what_it_was_handed_before_it_looked() {
        let dispatch = Dispatcher::new(Duration::from_secs(30));
        assert_eq!(
            dispatch.outstanding(),
            0,
            "a sender nobody has handed anything owes nothing"
        );
        dispatch.owes(1);

        // The pass begins: it reads what it is owed, and *then* reads the queue.
        let owed = dispatch.outstanding();
        // ...and an upload commits while it is doing so.
        dispatch.owes(1);
        dispatch.caught_up(owed);

        assert_eq!(
            dispatch.outstanding(),
            1,
            "the Call that arrived mid-pass is still owed a look"
        );
    }

    /// The right to drain is taken, not checked: a second `spawn` finds it gone.
    #[test]
    fn only_one_sender_can_claim_a_dispatcher() {
        let dispatch = Dispatcher::new(Duration::from_secs(30));

        assert!(dispatch.claim().is_some());
        assert!(dispatch.claim().is_none(), "a second sender claimed it");
    }

    /// A sleep with no deadline never completes — which is what an Instance with
    /// no sinks rests in, and the arm a `select!` would spin on if it returned.
    #[tokio::test(start_paused = true)]
    async fn a_schedule_with_nothing_waiting_sleeps_forever() {
        let mut forever = std::pin::pin!(sleep_until(None));
        assert!(futures_util::poll!(forever.as_mut()).is_pending());

        tokio::time::advance(Duration::from_secs(86_400)).await;

        assert!(
            futures_util::poll!(forever.as_mut()).is_pending(),
            "a day later, still nothing to do"
        );
    }

    /// A deadline already past is due *now*, not a negative sleep — the state
    /// every row left by a previous process is in, and the one a clock that
    /// jumped backwards would otherwise turn into a panic.
    #[tokio::test(start_paused = true)]
    async fn a_deadline_in_the_past_is_due_immediately() {
        sleep_until(Some(0)).await;
    }

    // -- Planning, over a substituted world -----------------------------------

    /// What a refused question says. One string, so an assertion about the cause
    /// reaching a log line is about *this* test's failure and not a driver's
    /// phrasing.
    #[derive(Debug)]
    struct Refused(&'static str);

    impl std::fmt::Display for Refused {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "the fake world refused to {}", self.0)
        }
    }

    impl std::error::Error for Refused {}

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct Peer(i64);

    impl Sink for Peer {
        fn id(&self) -> i64 {
            self.0
        }
    }

    #[derive(Default)]
    struct FakeOutbox {
        roster: Vec<Peer>,
        heads: std::collections::HashMap<i64, Queued>,
        fails: Option<&'static str>,
        attempted: std::sync::Mutex<Vec<i64>>,
    }

    impl FakeOutbox {
        fn refuses(&self, question: &'static str) -> bool {
            self.fails == Some(question)
        }
    }

    #[async_trait::async_trait]
    impl Outbox for FakeOutbox {
        type Sink = Peer;

        async fn roster(&self) -> Result<Vec<Peer>, Failure> {
            match self.refuses("read the roster") {
                true => Err(Refused("read the roster").into()),
                false => Ok(self.roster.clone()),
            }
        }

        async fn head(&self, sink_id: i64) -> Result<Option<Queued>, Failure> {
            if self.refuses("read the queue") {
                return Err(Refused("read the queue").into());
            }
            // A queue that **drains**: an attempt is what removes a row, so a
            // head that survived being attempted would make [`run`] a spin loop
            // rather than a worker that finishes. Modelling that is the whole
            // difference between a fake and a mock here.
            match self.attempted.lock().expect("attempted").contains(&sink_id) {
                true => Ok(None),
                false => Ok(self.heads.get(&sink_id).cloned()),
            }
        }

        async fn attempt(&self, sink: &Peer, _head: &Queued) {
            self.attempted.lock().expect("attempted").push(sink.id());
        }

        async fn depth(&self) -> Result<u64, Failure> {
            match self.refuses("count the queue") {
                true => Err(Refused("count the queue").into()),
                false => Ok(0),
            }
        }

        fn now_ms(&self) -> i64 {
            1_000
        }
    }

    fn queued(id: i64, next_attempt_ms: i64) -> Queued {
        Queued {
            id,
            call_id: 100 + id,
            attempts: 0,
            next_attempt_ms,
        }
    }

    fn working() -> FakeOutbox {
        FakeOutbox {
            roster: vec![Peer(1)],
            heads: [(1, queued(7, 0))].into_iter().collect(),
            ..FakeOutbox::default()
        }
    }

    #[tokio::test]
    async fn a_due_delivery_is_planned() {
        let plan = plan(&working(), &HashSet::new(), 1_000).await;

        assert_eq!(plan.due.len(), 1);
        assert_eq!(plan.due[0].0.id(), 1);
        assert_eq!(plan.next_due, None);
    }

    /// **A sink already attempting is left alone.** One in flight per sink is
    /// what makes in-order draining true: a second attempt started beside the
    /// first could land its Call ahead of the one in front of it.
    #[tokio::test]
    async fn a_sink_already_attempting_is_not_given_a_second() {
        let plan = plan(&working(), &HashSet::from([1]), 1_000).await;

        assert!(plan.due.is_empty());
        assert_eq!(plan.next_due, None, "and nothing is waiting on a clock");
    }

    /// **The soonest** of the backing-off heads, not the last one seen — a sink
    /// backing off for five minutes must not hold up one due in five
    /// milliseconds.
    #[tokio::test]
    async fn the_next_wake_up_is_the_soonest_of_the_sinks_waiting() {
        let world = FakeOutbox {
            roster: vec![Peer(1), Peer(2), Peer(3)],
            heads: [
                (1, queued(7, 9_000)),
                (2, queued(8, 2_000)),
                (3, queued(9, 5_000)),
            ]
            .into_iter()
            .collect(),
            ..FakeOutbox::default()
        };

        let plan = plan(&world, &HashSet::new(), 1_000).await;

        assert!(plan.due.is_empty(), "none of them is due yet");
        assert_eq!(plan.next_due, Some(2_000));
    }

    /// A head exactly due now goes, rather than waiting a further tick — the
    /// boundary a `>` and a `>=` disagree about, and the one every row left by a
    /// previous process sits on.
    #[tokio::test]
    async fn a_head_due_exactly_now_is_attempted() {
        let world = FakeOutbox {
            heads: [(1, queued(7, 1_000))].into_iter().collect(),
            ..working()
        };

        assert_eq!(plan(&world, &HashSet::new(), 1_000).await.due.len(), 1);
    }

    /// A sink with an empty queue is neither attempted nor waited for.
    #[tokio::test]
    async fn a_sink_owed_nothing_is_skipped() {
        let world = FakeOutbox {
            heads: std::collections::HashMap::new(),
            ..working()
        };

        assert_eq!(plan(&world, &HashSet::new(), 1_000).await, Plan::default());
    }

    /// **A database that will not answer costs a pass, never the Worker.** The
    /// rows are in that same database, so nothing is lost by asking again — and
    /// an Operator is told, because a sender that has silently stopped
    /// delivering is exactly what this feature exists to prevent.
    #[rstest]
    #[case("read the roster", "roster-unreadable")]
    #[case("read the queue", "queue-unreadable")]
    #[tokio::test]
    async fn an_unreadable_database_says_so_and_plans_nothing(
        #[case] refuses: &'static str,
        #[case] slug: &str,
    ) {
        let capture = LogCapture::start();
        let world = FakeOutbox {
            fails: Some(refuses),
            ..working()
        };

        let plan = plan(&world, &HashSet::new(), 1_000).await;

        assert_eq!(plan, Plan::default());
        let logged = capture.text();
        assert!(logged.contains(" WARN "), "{logged}");
        assert!(logged.contains(slug), "{logged}");
        assert!(logged.contains("the fake world refused"), "{logged}");
    }

    /// The loop drains what it is given and then goes idle — the whole contract
    /// `app.settle()` rests on, over a world that answers instantly.
    #[tokio::test]
    async fn a_run_attempts_what_is_due_and_then_owes_nothing() {
        let dispatch = Dispatcher::new(Duration::from_secs(30));
        let outbox = Arc::new(working());
        dispatch.owes_a_first_pass();

        let worker = run("test-sink", outbox.clone(), dispatch.clone());
        worker.idle().await;

        assert_eq!(*outbox.attempted.lock().expect("attempted"), vec![1]);
        worker.stop().await;
    }

    /// A resume that cannot be counted says so and keeps going: the rows are
    /// still in the database, and refusing to start a scanner over a `COUNT`
    /// would be the wrong trade by a distance.
    #[tokio::test]
    async fn a_resume_that_cannot_be_counted_says_so_and_still_runs() {
        let capture = LogCapture::start();
        let dispatch = Dispatcher::new(Duration::from_secs(30));
        let outbox = Arc::new(FakeOutbox {
            fails: Some("count the queue"),
            ..working()
        });
        dispatch.owes_a_first_pass();

        let worker = run("test-sink", outbox.clone(), dispatch);
        worker.idle().await;
        worker.stop().await;

        let logged = capture.text();
        assert!(logged.contains("resume-failed"), "{logged}");
        assert!(logged.contains(" WARN "), "{logged}");
        assert_eq!(
            *outbox.attempted.lock().expect("attempted"),
            vec![1],
            "the queue is still drained; only the accounting was lost"
        );
    }
}
