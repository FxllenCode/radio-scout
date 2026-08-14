//! The **Downstream** sender (#52): the Worker that drains the durable queue.
//!
//! # The three rules it exists to keep
//!
//! **It never blocks ingest or the fanout.** An upload's only contact with
//! forwarding is the rows written inside its own transaction; everything after
//! that happens here, on a Worker, behind a wake-up.
//!
//! **It drains in order, per peer.** Only the *head* of a peer's queue is ever
//! attempted, and a head that is not yet due is waited for rather than skipped —
//! see [`Deliveries::head`]. Skipping would reorder a peer's archive in a way the peer
//! cannot detect.
//!
//! **One peer's trouble is its own.** Attempts run in a `JoinSet`, one in flight
//! per peer, so a peer that takes the full `[downstream] timeout_secs` to fail
//! delays nothing but its own queue. That is the arrangement rdio does not have
//! and cannot: it POSTs inline on the ingest goroutine, so a slow peer is
//! backpressure on the recorder.
//!
//! # What one unit of work is
//!
//! **"The sender has caught up with what was handed to it"** — see
//! [`super::Downstreams::owes`] for the two more obvious readings that both make
//! `app.settle()` (#93) hang, one of them only when a peer is unreachable and
//! the other only when a Call is stuck behind another. Work is admitted where it
//! is handed over (at ingest, and at boot for what a previous process left), and
//! discharged by the pass that finds nothing left in flight.
//!
//! # The seam
//!
//! [`Deliveries`] is the six questions forwarding asks of the world (#37, #97, the
//! [`crate::enhance::Archive`] precedent). Every interesting arm here is a
//! failure of one of them — a Call pruned while its peer was down, an object
//! store that will not answer, a queue row that cannot be written — and none is
//! reachable while the database answers and the store works. Naming the
//! dependency at the interface is what makes them values a test constructs,
//! rather than something to be provoked by damaging what is underneath.
//!
//! The two halves either side of it are pure and separately testable:
//! [`plan`] decides *what to attempt*, and [`decide`] turns one attempt's outcome
//! into what happens to the queue row.

use std::collections::HashSet;

use tracing::{Instrument, Level, debug, info, span, warn};

use super::{Downstream, DownstreamConfig, Failed, Verdict, dialect};
use crate::AppState;
use crate::call::CallId;
use crate::db::repo;
use crate::worker::Worker;

/// Why one of [`Deliveries`]' answers could not be given.
///
/// The cause as text, the way [`crate::enhance::Failure`] and
/// [`crate::mining::sweep::Failure`] each carry theirs: all anything here does
/// with it is put it on a WARN line an Operator reads (ADR-0011 rule 4), and
/// keeping a driver's error type in the port's signature would put sea-orm and
/// `object_store` in the interface of something whose subject is peers.
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

/// One delivery at the head of a peer's queue.
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
    /// The peer took it. The row goes.
    Taken,
    /// The peer will never take it. The row goes, and a Call has been lost —
    /// which is why this one is said out loud and the retry is not.
    Abandoned { why: String },
    /// Not now. The row stays, due again at `next_attempt_ms`.
    Deferred {
        attempts: i32,
        next_attempt_ms: i64,
        why: String,
    },
}

/// The six questions **one delivery** asks of the world — the queue it comes
/// off, the Call and audio it is made of, the peer it goes to, and how it ends.
///
/// A port for the reason [`crate::enhance::Archive`] is one: the arms worth
/// testing here are the failures, and a filesystem that works and a database
/// that answers cannot produce them.
#[async_trait::async_trait]
pub trait Deliveries: Send + Sync {
    /// Every Downstream currently forwarding.
    async fn forwarding(&self) -> Result<Vec<Downstream>, Failure>;

    /// The head of one peer's queue — due or not.
    async fn head(&self, downstream_id: i64) -> Result<Option<Queued>, Failure>;

    /// Everything the dialect can say about a Call, or `None` if it is gone.
    async fn forwardable(&self, call_id: CallId) -> Result<Option<dialect::Forwardable>, Failure>;

    /// The bytes behind an object key, or `None` if the object is gone.
    async fn audio(&self, key: &str) -> Result<Option<bytes::Bytes>, Failure>;

    /// POST one delivery. `Ok(status)` is an answer from the peer, whatever it
    /// says; `Err(())` is never having got one.
    ///
    /// The transport error is deliberately not carried out: reqwest's `Display`
    /// renders the URL, and a peer's URL is an Operator-supplied string that may
    /// carry a query parameter (ADR-0011 rule 2's instinct). What an Operator
    /// needs — that this peer cannot be reached — is the `Err` itself.
    async fn deliver(&self, peer: &Downstream, body: dialect::Body) -> Result<u16, ()>;

    /// Apply what was decided, to the queue row and to the peer's health.
    async fn settle(
        &self,
        delivery_id: i64,
        downstream_id: i64,
        now_ms: i64,
        settled: &Settled,
    ) -> Result<(), Failure>;
}

/// Start the sender. `None` means one is already running.
///
/// Started whatever the roster says, unlike the push and enhancement workers: a
/// Downstream is a **row**, so an Instance with no peers has an empty roster
/// rather than a disabled feature, and a peer created from the browser five
/// minutes from now must be forwarded to without a restart. With none
/// configured it sleeps on its wake-up and costs nothing.
///
/// The `Option` is therefore the **double-spawn guard** (#93) rather than a
/// feature switch: two senders would each attempt the same head, so a peer would
/// receive one Call twice and "drains in order" would stop being true.
pub fn spawn(state: AppState) -> Option<Worker> {
    let downstreams = state.downstreams.clone();
    // Taken, not checked: a second call finds it gone and starts nothing.
    downstreams.claim()?;
    let meter = downstreams.meter();
    // The first pass is owed before the task exists, so an Instance is never
    // observed idle in the window before the sender has looked — see
    // `Downstreams::owes_a_first_pass`.
    downstreams.owes_a_first_pass();

    // What a previous process left. Admitted before the task is spawned, so
    // there is no window in which an Instance reads idle because the sender has
    // not woken up yet (#93) — and, less obviously, so that the counters
    // *balance*: every pass this Worker completes discharges what was handed
    // over, and a leftover row whose admission nobody made would push `done`
    // permanently ahead of `admitted` and make every later `idle()` return
    // early.
    //
    // A read that fails costs the accounting, never the boot: the rows are still
    // there and will still be sent, and refusing to start a scanner over a
    // `COUNT` would be the wrong trade by a distance.
    let outstanding = state.downstreams.clone();
    let db = state.db.clone();

    Some(Worker::start(
        super::WORKER,
        meter,
        move |mut stop| async move {
            match repo::delivery_depths(&db).await {
                Ok(depths) => outstanding.owes(depths.values().sum::<i64>().max(0) as usize),
                Err(error) => warn!(
                    reason = %"resume-failed",
                    %error,
                    "could not count the deliveries a previous process left; the sender's depth will read low until they drain"
                ),
            }

            // One attempt in flight per peer, and the set is what remembers which.
            let mut attempts: tokio::task::JoinSet<i64> = tokio::task::JoinSet::new();
            let mut in_flight: HashSet<i64> = HashSet::new();

            loop {
                // Read **before** the pass looks at anything: these are the
                // items this pass is about to consider, and discharging any
                // more than them would discharge work nobody has looked at yet.
                let owed = downstreams.outstanding();
                let Plan { due, next_due } = plan(&state, &in_flight, state.clock.now_ms()).await;
                for (peer, head) in due {
                    let peer_id = peer.id;
                    let call_id = head.call_id;
                    in_flight.insert(peer_id);
                    let state = state.clone();
                    attempts.spawn(
                        async move {
                            // The Instance's clock, not the machine's (#90): a test
                            // that arranges "this delivery is due" must measure from
                            // the same instant the sender stamps with.
                            let now_ms = state.clock.now_ms();
                            attempt(&state, state.downstreams.config(), &peer, &head, now_ms).await;
                            peer_id
                        }
                        // Everything one attempt says names the peer and the Call,
                        // so a failure and the statements around it read as one
                        // story.
                        .instrument(span!(
                            Level::ERROR,
                            "forward",
                            downstream = peer_id,
                            call_id
                        )),
                    );
                }

                // **Where the Worker goes idle**, and the only place it does.
                // Nothing in flight means every peer has either been tried just now
                // or is waiting out a backoff nobody asked for — either way the
                // sender has caught up with what was handed to it, which is what an
                // admission means here. A pass with an attempt running settles
                // nothing, so on a working peer this is reached only once the queue
                // has drained.
                if attempts.is_empty() {
                    downstreams.caught_up(owed);
                }

                tokio::select! {
                    biased;
                    _ = stop.cancelled() => break,
                    Some(finished) = attempts.join_next(), if !attempts.is_empty() => {
                        // One attempt ended, however it ended — including by
                        // panicking, which `join_next` reports rather than swallows.
                        if let Ok(peer_id) = finished {
                            in_flight.remove(&peer_id);
                        }
                    }
                    // A Call has just been queued: forward it now rather than on some
                    // timer, because a scanner peer is only useful if it is prompt.
                    () = downstreams.woken() => {}
                    // The earliest moment a backing-off head becomes due. `None` is
                    // "nothing is waiting on a clock", and the wake-up above is then
                    // the only thing that can start work — which is exactly the
                    // steady state of an Instance with no peers.
                    () = sleep_until(next_due), if next_due.is_some() => {}
                }
            }
        },
    ))
}

/// What one pass of the loop should do.
///
/// `PartialEq` and not `Eq`, because a [`Downstream`]'s scope is a [`Selection`]
/// (a `HashMap`), which has no total equality.
#[derive(Debug, Default, PartialEq)]
pub struct Plan {
    /// The peers with a delivery due now, one each.
    pub due: Vec<(Downstream, Queued)>,
    /// The earliest moment an *undue* head becomes due, if any is waiting.
    pub next_due: Option<i64>,
}

/// Decide what to attempt, and when to wake up next.
///
/// **The ordering rule lives here.** A peer already attempting is left alone, so
/// nothing overtakes; and a head that is not yet due stops that peer's queue
/// rather than being stepped over, because stepping over it would reorder a
/// peer's archive in a way the peer cannot detect.
///
/// Everything that can fail is a *skip*, never a stop: a queue that cannot be
/// read is asked again on the next wake-up, and the rows are in the same
/// database either way.
pub async fn plan(peers: &dyn Deliveries, in_flight: &HashSet<i64>, now_ms: i64) -> Plan {
    let roster = match peers.forwarding().await {
        Ok(roster) => roster,
        // The roster is unreadable — the database is down or mid-migration.
        // Nothing is lost, and nothing is worth waking a clock for.
        Err(error) => {
            warn!(reason = %"roster-unreadable", %error, "could not read the Downstream roster");
            return Plan::default();
        }
    };

    let mut plan = Plan::default();
    for peer in roster {
        if in_flight.contains(&peer.id) {
            continue;
        }
        let head = match peers.head(peer.id).await {
            Ok(Some(head)) => head,
            Ok(None) => continue,
            Err(error) => {
                warn!(
                    reason = %"queue-unreadable",
                    downstream = peer.id,
                    %error,
                    "could not read a Downstream's queue"
                );
                continue;
            }
        };
        match head.next_attempt_ms > now_ms {
            // Waiting out a backoff. The *soonest* of them is when this loop
            // has something to do again — take the minimum rather than the last
            // one seen, or a peer backing off for five minutes would hold up one
            // due in five milliseconds.
            true => {
                plan.next_due = Some(match plan.next_due {
                    Some(soonest) => soonest.min(head.next_attempt_ms),
                    None => head.next_attempt_ms,
                });
            }
            false => plan.due.push((peer, head)),
        }
    }
    plan
}

/// Sleep until `at_ms`, or forever if there is nothing to wait for.
///
/// The `forever` arm is what an Instance with no peers rests in — the `select!`
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

/// One delivery attempt, start to finish: read the Call, read its audio, POST
/// it, and settle the row.
pub async fn attempt(
    peers: &dyn Deliveries,
    config: &DownstreamConfig,
    peer: &Downstream,
    head: &Queued,
    now_ms: i64,
) {
    let settled = decide(
        forward(peers, peer, head.call_id).await.err(),
        head,
        config,
        now_ms,
    );
    say(&settled);
    if let Err(error) = peers.settle(head.id, peer.id, now_ms, &settled).await {
        // The one failure this module cannot recover from by retrying: if the
        // row cannot be written, the same delivery is attempted again on the
        // next wake-up — which for a *successful* delivery means the peer
        // receives it twice. Its own dedup is what makes that survivable, and
        // saying so is what lets an Operator recognise it rather than wonder.
        warn!(
            reason = %"queue-unwritable",
            %error,
            "a Downstream delivery could not be settled; it may be sent again"
        );
    }
}

/// What an attempt's outcome does to the queue row — **pure**, so every arm is a
/// value a test constructs.
pub fn decide(
    outcome: Option<Failed>,
    head: &Queued,
    config: &DownstreamConfig,
    now_ms: i64,
) -> Settled {
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
            next_attempt_ms: now_ms.saturating_add(config.backoff(attempts).as_millis() as i64),
            why,
        },
    }
}

/// Write down what happened to one delivery (ADR-0011 rules 3, 7 and 8).
fn say(settled: &Settled) {
    match settled {
        // DEBUG: one line per forwarded Call per peer, forever (rule 8). The
        // reading an Operator wants is `last_success_ms`, which is a column
        // rather than a line to grep for.
        Settled::Taken => debug!("forwarded"),
        // WARN, and **the only record a Call was lost** (rule 3): the peer will
        // never take it, so nothing further will ever mention it again.
        Settled::Abandoned { why } => warn!(
            reason = %"abandoned",
            failure = %why,
            "a Call was dropped for this Downstream and will not be retried"
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
            // peer down for a night would otherwise write a line every few
            // minutes saying exactly the same thing (rule 8), and the standing
            // reading an Operator acts on is `consecutive_failures` and
            // `last_failure` on the row — which is what the admin screen shows
            // and what #70 will read.
            if *attempts == 1 {
                info!(
                    failure = %why,
                    "a Downstream stopped accepting Calls; they are queued and will be retried"
                );
            }
            debug!(
                failure = %why,
                attempts,
                next_attempt_ms,
                "a Downstream delivery will be retried"
            );
        }
    }
}

/// Read, build and POST. `Err` is the one word that explains what went wrong.
async fn forward(peers: &dyn Deliveries, peer: &Downstream, call_id: CallId) -> Result<(), Failed> {
    let call = peers
        .forwardable(call_id)
        .await
        .map_err(|error| {
            warn!(%error, "could not read a Call to forward");
            Failed::Unreadable
        })?
        // Retention is entitled to prune a Call a peer has been unreachable
        // longer than.
        .ok_or(Failed::Vanished)?;

    let audio = match peers.audio(&call.object_key).await {
        Ok(Some(audio)) => audio,
        Ok(None) => return Err(Failed::Vanished),
        Err(error) => {
            warn!(%error, "could not read the audio of a Call to forward");
            return Err(Failed::Unreadable);
        }
    };

    let body = dialect::body(
        &call,
        &peer.api_key,
        &audio,
        // A fresh boundary per request. It has to be absent from the body, and
        // a v4 UUID's 122 bits make that a certainty rather than a scan of the
        // audio on every delivery.
        &format!("radioscout{}", uuid::Uuid::new_v4().simple()),
    );

    match peers.deliver(peer, body).await {
        Ok(status) => match Verdict::of_status(status) {
            Verdict::Delivered => Ok(()),
            _ => Err(Failed::Status(status)),
        },
        Err(()) => Err(Failed::Unreachable),
    }
}

/// The world an `AppState` is: its database for the rows, its store for the
/// objects, its client for the peer.
///
/// The only implementation that ships, and deliberately thin — every decision is
/// above, and each of these is one call. This is the half a test cannot
/// substitute, so it is the half that must have nothing in it worth testing.
#[async_trait::async_trait]
impl Deliveries for AppState {
    async fn forwarding(&self) -> Result<Vec<Downstream>, Failure> {
        Ok(repo::forwarding_downstreams(&self.db).await?)
    }

    async fn head(&self, downstream_id: i64) -> Result<Option<Queued>, Failure> {
        Ok(repo::next_delivery(&self.db, downstream_id)
            .await?
            .map(|row| Queued {
                id: row.id,
                call_id: row.call_id,
                attempts: row.attempts,
                next_attempt_ms: row.next_attempt_ms,
            }))
    }

    async fn forwardable(&self, call_id: CallId) -> Result<Option<dialect::Forwardable>, Failure> {
        Ok(crate::archive::forwardable(&self.db, call_id).await?)
    }

    async fn audio(&self, key: &str) -> Result<Option<bytes::Bytes>, Failure> {
        // An **Encrypted Call** is never queued (it has no object), so an empty
        // key here would be a Call whose audio has gone — the same answer the
        // store gives for one that is missing, and asked of nothing.
        if key.is_empty() {
            return Ok(None);
        }
        Ok(self.audio.get(key).await?)
    }

    async fn deliver(&self, peer: &Downstream, body: dialect::Body) -> Result<u16, ()> {
        self.downstreams
            .client()
            .post(peer.upload_url())
            .header(http::header::CONTENT_TYPE, &body.content_type)
            .body(body.bytes)
            .send()
            .await
            .map(|response| response.status().as_u16())
            .map_err(|error| {
                // `without_url`, the rule [`crate::push`] follows: reqwest
                // renders as "error sending request for url (…)", and a peer's
                // URL is an Operator-supplied string that may carry a query
                // parameter.
                debug!(error = %error.without_url(), "a Downstream delivery did not complete");
            })
    }

    async fn settle(
        &self,
        delivery_id: i64,
        downstream_id: i64,
        now_ms: i64,
        settled: &Settled,
    ) -> Result<(), Failure> {
        match settled {
            Settled::Taken => {
                repo::drop_delivery(&self.db, delivery_id).await?;
                repo::downstream_succeeded(&self.db, downstream_id, now_ms).await?;
                self.downstreams.delivery_settled();
            }
            Settled::Abandoned { why } => {
                repo::downstream_failed(&self.db, downstream_id, now_ms, why).await?;
                repo::drop_delivery(&self.db, delivery_id).await?;
                self.downstreams.delivery_settled();
            }
            Settled::Deferred {
                attempts,
                next_attempt_ms,
                why,
            } => {
                repo::downstream_failed(&self.db, downstream_id, now_ms, why).await?;
                repo::defer_delivery(&self.db, delivery_id, *attempts, *next_attempt_ms, why)
                    .await?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::LogCapture;
    use rstest::rstest;
    use std::sync::Mutex;
    use std::time::Duration;

    /// **One sender, and only ever one** (#93). Two would each take the head of
    /// the same peer's queue, so that peer would receive one Call twice and
    /// "drains in order" would stop being true — and `notify_one` would hand a
    /// wake-up to one of them rather than to both.
    ///
    /// The right to drain *is* the guard, so a second start has nothing to drain
    /// with. `push` makes the same bargain with its coalescer.
    #[tokio::test]
    async fn a_second_sender_cannot_be_started() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let db = crate::db::connect(&crate::testing::sqlite_url(&tmp))
            .await
            .expect("db");
        let store = std::sync::Arc::new(crate::BlobStore::filesystem(tmp.path()).expect("store"));
        let state = AppState::new(store, db, crate::IngestConfig::default());

        let first = spawn(state.clone()).expect("the first sender starts");
        let second = spawn(state.clone());

        assert!(second.is_none(), "a second Downstream sender was started");
        first.stop().await;
    }

    /// A sleep with no deadline never completes — which is what an Instance with
    /// no peers rests in, and the arm a `select!` would spin on if it returned.
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

    // -- The world, substituted ----------------------------------------------

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

    #[derive(Default)]
    struct FakeWorld {
        roster: Vec<Downstream>,
        heads: std::collections::HashMap<i64, Queued>,
        call: Option<dialect::Forwardable>,
        audio: Option<bytes::Bytes>,
        /// What the peer answers, or `None` for a peer that cannot be reached.
        answer: Option<u16>,
        /// The one question this world refuses.
        fails: Option<&'static str>,
        settled: Mutex<Vec<Settled>>,
        delivered: Mutex<Vec<dialect::Body>>,
    }

    impl FakeWorld {
        fn refuse(&self, question: &'static str) -> Result<(), Refused> {
            match self.fails == Some(question) {
                true => Err(Refused(question)),
                false => Ok(()),
            }
        }

        fn settled(&self) -> Vec<Settled> {
            self.settled.lock().expect("settled").clone()
        }
    }

    #[async_trait::async_trait]
    impl Deliveries for FakeWorld {
        async fn forwarding(&self) -> Result<Vec<Downstream>, Failure> {
            self.refuse("read the roster")?;
            Ok(self.roster.clone())
        }

        async fn head(&self, downstream_id: i64) -> Result<Option<Queued>, Failure> {
            self.refuse("read the queue")?;
            Ok(self.heads.get(&downstream_id).cloned())
        }

        async fn forwardable(
            &self,
            _call_id: CallId,
        ) -> Result<Option<dialect::Forwardable>, Failure> {
            self.refuse("read the Call")?;
            Ok(self.call.clone())
        }

        async fn audio(&self, _key: &str) -> Result<Option<bytes::Bytes>, Failure> {
            self.refuse("read the audio")?;
            Ok(self.audio.clone())
        }

        async fn deliver(&self, _peer: &Downstream, body: dialect::Body) -> Result<u16, ()> {
            self.delivered.lock().expect("delivered").push(body);
            self.answer.ok_or(())
        }

        async fn settle(
            &self,
            _delivery_id: i64,
            _downstream_id: i64,
            _now_ms: i64,
            settled: &Settled,
        ) -> Result<(), Failure> {
            self.refuse("settle the delivery")?;
            self.settled.lock().expect("settled").push(settled.clone());
            Ok(())
        }
    }

    fn peer(id: i64) -> Downstream {
        Downstream {
            id,
            url: format!("https://peer{id}.example"),
            api_key: String::from("peer-key"),
            scope: crate::selection::Selection::default(),
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

    fn a_call() -> dialect::Forwardable {
        dialect::Forwardable {
            system_ref: 11,
            system_label: None,
            talkgroup_ref: 54241,
            talkgroup_label: None,
            talkgroup_name: None,
            talkgroup_tag: None,
            talkgroup_groups: vec![],
            call_at_ms: 1_000,
            frequency: None,
            site_ref: None,
            audio_name: None,
            audio_mime: None,
            patches: vec![],
            units: vec![],
            frequencies: vec![],
            object_key: String::from("calls/1.wav"),
        }
    }

    /// A world where everything works: one peer, one due delivery, a Call with
    /// audio, and a peer that takes it.
    fn working() -> FakeWorld {
        FakeWorld {
            roster: vec![peer(1)],
            heads: [(1, queued(7, 0))].into_iter().collect(),
            call: Some(a_call()),
            audio: Some(bytes::Bytes::from_static(b"RIFF....")),
            answer: Some(200),
            ..FakeWorld::default()
        }
    }

    // -- Planning ------------------------------------------------------------

    #[tokio::test]
    async fn a_due_delivery_is_planned() {
        let plan = plan(&working(), &HashSet::new(), 1_000).await;

        assert_eq!(plan.due.len(), 1);
        assert_eq!(plan.due[0].0.id, 1);
        assert_eq!(plan.next_due, None);
    }

    /// **A peer already attempting is left alone.** One in flight per peer is
    /// what makes in-order draining true: a second attempt started beside the
    /// first could land its Call ahead of the one in front of it.
    #[tokio::test]
    async fn a_peer_already_attempting_is_not_given_a_second() {
        let plan = plan(&working(), &HashSet::from([1]), 1_000).await;

        assert!(plan.due.is_empty());
        assert_eq!(plan.next_due, None, "and nothing is waiting on a clock");
    }

    /// **The soonest** of the backing-off heads, not the last one seen — a peer
    /// backing off for five minutes must not hold up one due in five
    /// milliseconds.
    #[tokio::test]
    async fn the_next_wake_up_is_the_soonest_of_the_peers_waiting() {
        let world = FakeWorld {
            roster: vec![peer(1), peer(2), peer(3)],
            heads: [
                (1, queued(7, 9_000)),
                (2, queued(8, 2_000)),
                (3, queued(9, 5_000)),
            ]
            .into_iter()
            .collect(),
            ..working()
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
        let world = FakeWorld {
            heads: [(1, queued(7, 1_000))].into_iter().collect(),
            ..working()
        };

        assert_eq!(plan(&world, &HashSet::new(), 1_000).await.due.len(), 1);
    }

    /// A peer with an empty queue is neither attempted nor waited for.
    #[tokio::test]
    async fn a_peer_owed_nothing_is_skipped() {
        let world = FakeWorld {
            heads: std::collections::HashMap::new(),
            ..working()
        };

        assert_eq!(plan(&world, &HashSet::new(), 1_000).await, Plan::default());
    }

    /// **A database that will not answer costs a pass, never the Worker.** The
    /// rows are in that same database, so nothing is lost by asking again — and
    /// an Operator is told, because a sender that has silently stopped
    /// forwarding is exactly what this feature exists to prevent.
    #[rstest]
    #[case("read the roster", "roster-unreadable")]
    #[case("read the queue", "queue-unreadable")]
    #[tokio::test]
    async fn an_unreadable_database_says_so_and_plans_nothing(
        #[case] refuses: &'static str,
        #[case] slug: &str,
    ) {
        let capture = LogCapture::start();
        let world = FakeWorld {
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

    // -- One attempt ---------------------------------------------------------

    #[tokio::test]
    async fn a_peer_that_takes_a_call_has_its_delivery_dropped() {
        let world = working();

        attempt(
            &world,
            &DownstreamConfig::default(),
            &peer(1),
            &queued(7, 0),
            1_000,
        )
        .await;

        assert_eq!(world.settled(), vec![Settled::Taken]);
        let sent = world.delivered.lock().expect("delivered");
        assert_eq!(sent.len(), 1);
        assert!(sent[0].content_type.starts_with("multipart/form-data;"));
    }

    /// Every way an attempt can fail, and what each one does to the queue.
    ///
    /// The three that are unreachable while the database answers and the store
    /// works are the whole reason [`Deliveries`] is a port: a Call pruned while its
    /// peer was down, an object store that will not answer, and a Call read that
    /// fails outright.
    #[rstest]
    #[case::gone(FakeWorld { call: None, ..working() }, "call-gone", true)]
    #[case::audio_gone(FakeWorld { audio: None, ..working() }, "call-gone", true)]
    #[case::call_unreadable(
        FakeWorld { fails: Some("read the Call"), ..working() }, "archive-unreadable", false
    )]
    #[case::audio_unreadable(
        FakeWorld { fails: Some("read the audio"), ..working() }, "archive-unreadable", false
    )]
    #[case::unreachable(FakeWorld { answer: None, ..working() }, "peer-unreachable", false)]
    #[case::refused(FakeWorld { answer: Some(503), ..working() }, "peer-refused (503)", false)]
    #[case::never(FakeWorld { answer: Some(417), ..working() }, "peer-refused (417)", true)]
    #[tokio::test]
    async fn every_failed_attempt_settles_the_way_its_reason_says(
        #[case] world: FakeWorld,
        #[case] why: &str,
        #[case] abandoned: bool,
    ) {
        attempt(
            &world,
            &DownstreamConfig::default(),
            &peer(1),
            &queued(7, 0),
            1_000,
        )
        .await;

        match (world.settled().first(), abandoned) {
            (Some(Settled::Abandoned { why: said }), true) => assert_eq!(said, why),
            (Some(Settled::Deferred { why: said, .. }), false) => assert_eq!(said, why),
            (settled, _) => panic!("{settled:?} is not the settlement {why:?} asks for"),
        }
    }

    /// **A queue row that cannot be written is said out loud**, because the
    /// consequence is a Call the peer receives twice — survivable only because
    /// the peer dedups, and impossible to recognise without the line.
    #[tokio::test]
    async fn a_delivery_that_cannot_be_settled_says_so() {
        let capture = LogCapture::start();
        let world = FakeWorld {
            fails: Some("settle the delivery"),
            ..working()
        };

        attempt(
            &world,
            &DownstreamConfig::default(),
            &peer(1),
            &queued(7, 0),
            1_000,
        )
        .await;

        let logged = capture.text();
        assert!(logged.contains(" WARN "), "{logged}");
        assert!(logged.contains("queue-unwritable"), "{logged}");
    }

    // -- What an outcome means ----------------------------------------------

    /// The backoff is applied from *now*, and the attempt count climbs — which
    /// is what makes a peer down for a night polled twice a minute rather than
    /// thousands of times.
    #[test]
    fn a_retry_is_scheduled_from_now_and_counts_the_attempt() {
        let config = DownstreamConfig {
            retry_initial: Duration::from_secs(5),
            ..DownstreamConfig::default()
        };
        let head = Queued {
            attempts: 2,
            ..queued(7, 0)
        };

        let settled = decide(Some(Failed::Unreachable), &head, &config, 1_000);

        assert_eq!(
            settled,
            Settled::Deferred {
                attempts: 3,
                // The third attempt failed, so the fourth waits four doublings
                // in: 5s * 2^2.
                next_attempt_ms: 1_000 + 20_000,
                why: String::from("peer-unreachable"),
            }
        );
    }

    #[test]
    fn a_delivered_call_settles_as_taken() {
        assert_eq!(
            decide(None, &queued(7, 0), &DownstreamConfig::default(), 1_000),
            Settled::Taken
        );
    }

    /// **What each settlement writes down** (ADR-0011 rules 3, 7 and 8).
    ///
    /// The abandon is the one that matters: it is the only record that a Call
    /// was lost, because nothing will ever mention that delivery again. The
    /// retry is INFO on its *first* failure and DEBUG after, so a peer down for
    /// a night does not write a line every few minutes saying the same thing.
    #[rstest]
    #[case::taken(Settled::Taken, "", "")]
    #[case::abandoned(
        Settled::Abandoned { why: String::from("peer-refused (417)") },
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

        say(&settled);

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
}
