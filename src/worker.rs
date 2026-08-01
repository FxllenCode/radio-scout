//! One lifecycle envelope for every background worker (#93).
//!
//! A **Worker** (CONTEXT.md) is a background task an Instance owns. There are
//! four — the retention sweeper, the Web Push sender, the enhancement worker
//! and the operator log writer — and before this module each had invented its
//! own answers to the same four questions: how it is started, how it is
//! stopped, how much work it has in hand, and how anything else knows it has
//! settled.
//!
//! The loop bodies stay hand-written. A ticker, a bounded queue, a broadcast
//! subscription and a batching drain genuinely differ, and shapes that merely
//! rhyme should not share a skeleton. What is shared is the envelope around
//! them.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use tokio::sync::watch;

/// How much work a Worker has in hand, as an Operator reads it.
///
/// One shape for all four, because a status surface (#70) shows them in one
/// table. A worker with no queue still has a depth: the retention sweeper's is
/// `1` while a sweep is running and `0` while it waits for the next tick, which
/// is exactly its health reading — a depth stuck at `1` is a sweep that never
/// ended.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Load {
    /// Work admitted but not yet settled — the queue depth.
    pub depth: u64,
    /// Work settled since this Worker started. Monotonic, so two readings a
    /// minute apart give a rate without anything having to store one.
    pub done: u64,
}

impl Load {
    /// Whether this Worker owes nothing: everything admitted has settled.
    pub fn is_idle(&self) -> bool {
        self.depth == 0
    }
}

/// The count of work a Worker owes, shared between whoever admits it and the
/// Worker that settles it.
///
/// Two monotonic counters rather than one signed depth, for two reasons. The
/// admitting side is often a hot path — a `tracing` event, an ingest — and a
/// `fetch_add` costs no lock, where publishing a composite value would. And
/// `done` is worth having on its own: it is what turns a status page into a
/// rate.
///
/// Only the settling side wakes waiters. That is deliberate: a waiter that sees
/// `admitted == settled` and returns has been told the truth — everything
/// admitted *at the moment it looked* has settled — and work admitted after it
/// looked was never its to wait for.
#[derive(Debug)]
pub struct Meter {
    admitted: AtomicU64,
    /// The settled count, published so that awaiting it costs no polling.
    settled: watch::Sender<u64>,
}

impl Default for Meter {
    fn default() -> Self {
        Meter {
            admitted: AtomicU64::new(0),
            settled: watch::Sender::new(0),
        }
    }
}

impl Meter {
    /// A meter owing nothing.
    pub fn new() -> Arc<Self> {
        Arc::new(Meter::default())
    }

    /// Take on one item of work, and hand back the [`Ticket`] that settles it.
    ///
    /// The ticket settles on drop, so every way a worker can stop holding an
    /// item — finishing it, failing it, being cancelled mid-flight — leaves the
    /// depth honest without an arm having to remember.
    pub fn admit(self: &Arc<Self>) -> Ticket {
        self.admitted.fetch_add(1, Ordering::Relaxed);
        Ticket(Some(self.clone()))
    }

    /// Take on one item of work whose ticket nobody can hold.
    ///
    /// The Web Push sender's only one: a Call is admitted where it is published
    /// to the live-feed fanout, and the fanout hands every follower a clone, so
    /// there is no single owner to give the ticket to. Its partner is
    /// [`Meter::settle`].
    pub fn admit_untracked(&self) {
        self.admitted.fetch_add(1, Ordering::Relaxed);
    }

    /// Settle one item admitted by [`Meter::admit_untracked`].
    pub fn settle(&self) {
        self.settle_n(1);
    }

    /// Settle `n` at once — the Web Push sender's, when the fanout tells it how
    /// many Calls it fell behind by. Each was admitted where it was published,
    /// and a Call the sender will never see is not one it still owes.
    pub fn settle_n(&self, n: u64) {
        if n > 0 {
            self.settled.send_modify(|settled| *settled += n);
        }
    }

    /// Wait until this Worker owes nothing.
    ///
    /// The signal the suite waits on instead of sleeping. The predicate is
    /// re-read on every wake, so what this actually waits for is a *moment* at
    /// which nothing is outstanding — which for a worker fed only from outside
    /// is exactly "everything handed over before I asked", and for one that
    /// arms itself (the sweeper) is a gap between two runs. Either way a caller
    /// can only ever be told about work that already existed; work admitted
    /// after this returns was never its to wait for.
    pub async fn idle(&self) {
        // `Err` is a dropped sender, which cannot happen — it lives in `self`,
        // which this borrow is holding.
        let _ = self
            .settled
            .subscribe()
            .wait_for(|done| self.admitted.load(Ordering::Relaxed) <= *done)
            .await;
    }

    /// Wait until `n` items have settled since this Worker started.
    ///
    /// [`Meter::idle`]'s companion, for the workers that hand themselves their
    /// own work: a ticker is idle between every pair of runs, so "it ran again"
    /// is only sayable as a count.
    pub async fn settled_at_least(&self, n: u64) {
        let _ = self.settled.subscribe().wait_for(|done| *done >= n).await;
    }

    /// What this Worker owes right now.
    pub fn load(&self) -> Load {
        // **Settled first, and the order is the whole correctness argument.**
        // The two counters are read separately, so a concurrent admit or settle
        // lands between them. Reading `settled` first pairs an *older* settled
        // count with a *newer* admitted one, and since settled can never run
        // ahead of admitted, the difference can only ever come out too **high**
        // — a depth that briefly outlives the work.
        //
        // That is the direction to err in. The other order would pair a newer
        // settled count with an older admitted one and read **zero while work
        // was outstanding**, which is a status page saying idle about a queue
        // that is not, and an `is_idle` nobody could build on.
        //
        // `saturating_sub` is therefore unreachable-by-argument rather than
        // load-bearing, and stays as the cheap insurance against someone
        // swapping these two lines: an underflow here would panic a status
        // handler in debug and report `u64::MAX` in release.
        let done = *self.settled.borrow();
        Load {
            depth: self.admitted.load(Ordering::Relaxed).saturating_sub(done),
            done,
        }
    }
}

/// One item of work a Worker owes, settled when it is dropped.
pub struct Ticket(Option<Arc<Meter>>);

impl Drop for Ticket {
    fn drop(&mut self) {
        if let Some(meter) = self.0.take() {
            meter.settle();
        }
    }
}

/// One Worker's reading, as a status surface names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NamedLoad {
    pub name: &'static str,
    pub load: Load,
}

/// Every Worker's reading, in one place both an `AppState` and an `Instance`
/// can hold.
///
/// It exists because the two halves of a Worker are needed by two different
/// owners. Stopping and joining belong to the Instance, which is the only thing
/// that knows a run is over. But the *reading* has to reach a status handler
/// (#70), and a handler is given `AppState` and can never see an Instance. So
/// the meters — and only the meters — are shared.
///
/// Registration is open rather than fixed, because half the workers exist only
/// on some Instances: `[enhancement] mode = "off"` (what ships) spawns no
/// enhancement worker at all, and a status page must show what is running
/// rather than a row of zeroes for what is not.
#[derive(Clone, Default)]
pub struct Workers(Arc<std::sync::Mutex<Vec<Registration>>>);

/// One Worker's name and the meter it publishes to.
#[derive(Clone)]
struct Registration {
    name: &'static str,
    meter: Arc<Meter>,
}

impl Workers {
    /// Publish `name`'s reading. Called as each Worker is started, so the
    /// order a status surface shows is the order they came up in.
    pub fn register(&self, name: &'static str, meter: Arc<Meter>) {
        self.0
            .lock()
            .expect("workers")
            .push(Registration { name, meter });
    }

    /// Publish this Worker's reading, and hand the handle straight back to
    /// whoever will stop it. One call rather than two, so a Worker cannot be
    /// started and then silently left off the status surface.
    pub fn adopt(&self, worker: Worker) -> Worker {
        self.register(worker.name(), worker.meter());
        worker
    }

    /// One Worker's meter, for a caller that wants to wait on that one — a
    /// status card, or a test asking the sweeper how many sweeps it has done.
    pub fn meter(&self, name: &str) -> Option<Arc<Meter>> {
        self.0
            .lock()
            .expect("workers")
            .iter()
            .find(|registered| registered.name == name)
            .map(|registered| registered.meter.clone())
    }

    /// Every registered Worker's reading, oldest first.
    pub fn loads(&self) -> Vec<NamedLoad> {
        self.0
            .lock()
            .expect("workers")
            .iter()
            .map(|registered| NamedLoad {
                name: registered.name,
                load: registered.meter.load(),
            })
            .collect()
    }

    /// Wait until every registered Worker has handled what was handed to it.
    ///
    /// **Twice, and the second pass is not paranoia.** One Worker is fed by the
    /// others: every one of them logs, and a stored log event is work for the
    /// sink. So one pass settles the producers and a second settles what they
    /// handed the sink on their way out. A third could add nothing, because
    /// nothing the sink itself does is work for anybody — its own target and
    /// the driver's are exactly the two it refuses to store, so that they
    /// cannot feed back.
    ///
    /// **What this does not promise**, said plainly rather than left to be
    /// discovered: the retention sweeper arms *itself*, so there is no finite
    /// wait after which it is guaranteed quiet. A sweep beginning after the
    /// pass that checked it is not work anybody handed over, and is therefore
    /// not what a caller asking "has my work been done?" is asking about. That
    /// is also why this is two passes rather than "loop until a clean pass":
    /// against a self-arming worker on a short interval, a re-checking loop has
    /// no bound at all. This terminates always.
    ///
    /// So a test that wants a *particular* sweep wants
    /// [`Meter::settled_at_least`], not this.
    pub async fn idle(&self) {
        for _ in 0..2 {
            for registered in self.registrations() {
                registered.meter.idle().await;
            }
        }
    }

    /// A snapshot of the registrations, so nothing is awaited while the lock is
    /// held — `idle` would otherwise hold a `std::sync::Mutex` across an await.
    fn registrations(&self) -> Vec<Registration> {
        self.0.lock().expect("workers").clone()
    }
}

/// The right to start a Worker, held once and taken once.
///
/// The double-spawn guard for a worker whose owner is `Clone`: [`crate::push`]
/// and [`crate::enhance`] both hang off `AppState`, which is cloned into every
/// handler, so `self`-by-value cannot be the guard there the way it is for
/// [`crate::retention::Sweeper`]. What it holds is the piece of state a second
/// worker must not have a second of — a queue's receiving end, a coalescer — so
/// a second start finds it gone and starts nothing.
pub struct Handoff<T>(std::sync::Mutex<Option<T>>);

impl<T> Handoff<T> {
    /// Hold `value` for whoever starts the Worker first.
    pub fn new(value: T) -> Self {
        Handoff(std::sync::Mutex::new(Some(value)))
    }

    /// Take it, if nobody has. `None` is a Worker that is already running.
    pub fn take(&self) -> Option<T> {
        self.0.lock().expect("handoff").take()
    }
}

/// The signal a Worker's loop selects on to learn it is time to stop.
///
/// A `watch` rather than a `oneshot`, because a loop has to be able to *check*
/// for cancellation as well as wait for it, and a oneshot receiver is consumed
/// by the first attempt.
#[derive(Clone)]
pub struct Shutdown(watch::Receiver<bool>);

impl Shutdown {
    /// Resolves once the Worker has been asked to stop, and never otherwise.
    pub async fn cancelled(&mut self) {
        // `Err` is the sender dropped without a stop, which is the handle going
        // away — also a reason to stop, and the same outcome.
        let _ = self.0.wait_for(|stopping| *stopping).await;
    }
}

/// One background worker, in the one shape every background worker has.
///
/// The loop inside is the subsystem's own; what this owns is everything around
/// it — the name it is known by, what it owes, and the two-step stop that makes
/// "it is no longer running" a fact rather than a hope.
pub struct Worker {
    name: &'static str,
    meter: Arc<Meter>,
    /// Set to `true` to ask the loop to stop.
    tell_to_stop: watch::Sender<bool>,
    /// The task. Owned outright rather than behind an `Option`, because both
    /// verbs that reach it — [`Worker::stop`] and [`Worker::join`] — take
    /// `self`, so there is no second poll of a `JoinHandle` to guard against.
    task: tokio::task::JoinHandle<()>,
}

impl Worker {
    /// Spawn `body` as this Instance's `name` worker.
    ///
    /// `body` is handed the [`Shutdown`] it must select on. Taking a closure
    /// rather than a future means the [`Shutdown`] exists before the task does,
    /// so a stop asked for in the same tick as the start is still seen.
    pub fn start<F, Fut>(name: &'static str, meter: Arc<Meter>, body: F) -> Self
    where
        F: FnOnce(Shutdown) -> Fut,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let (tell_to_stop, stopping) = watch::channel(false);
        let task = tokio::spawn(body(Shutdown(stopping)));
        Worker {
            name,
            meter,
            tell_to_stop,
            task,
        }
    }

    /// What this Worker is known by on an Operator-facing surface.
    pub fn name(&self) -> &'static str {
        self.name
    }

    /// What it owes, shareable — this is the half a status handler holds.
    pub fn meter(&self) -> Arc<Meter> {
        self.meter.clone()
    }

    /// Whether its loop is still going.
    ///
    /// The other half of a health reading, and the one a depth cannot give: a
    /// Worker that panicked settles everything it was holding on the way out,
    /// so it reads as perfectly idle. `false` before a stop was asked for means
    /// something an Operator has to act on.
    pub fn is_running(&self) -> bool {
        !self.task.is_finished()
    }

    /// What it owes right now.
    pub fn load(&self) -> Load {
        self.meter.load()
    }

    /// Wait until everything admitted as of now has settled.
    pub async fn idle(&self) {
        self.meter.idle().await;
    }

    /// Wait until `n` items have settled since it started.
    pub async fn settled_at_least(&self, n: u64) {
        self.meter.settled_at_least(n).await;
    }

    /// Ask it to stop, and wait until it has.
    ///
    /// Both halves, always: an Instance that only asked would free its port
    /// while a sweeper still held the database the next run is about to open.
    /// A worker that panicked or was cancelled has still ended, which is what
    /// was being asked for — so neither is an error to report here.
    pub async fn stop(self) {
        // A receiver that is gone means the loop has already ended, which is
        // the outcome being asked for.
        let _ = self.tell_to_stop.send(true);
        self.join().await;
    }

    /// Wait for it to end **on its own**, without asking it to.
    ///
    /// The log writer's verb: its loop ends when the sink feeding it is
    /// dropped, which is the deterministic "everything that was going to be
    /// logged has been" moment. [`Worker::stop`] is this same wait with the
    /// asking done first.
    ///
    /// A worker that panicked or was cancelled has still ended, which is what
    /// was being asked for — so neither is an error to report here.
    pub async fn join(self) {
        let _ = self.task.await;
    }
}

/// Stop a run's Workers, **last started first**, and don't come back until
/// every one of them has ended.
///
/// Reverse order because that is the only ordering that is safe without knowing
/// the dependencies: a Worker started later may have been given something an
/// earlier one owns, never the other way round — the enhancement worker and the
/// push sender are both handed the `AppState` the sweeper was already running
/// beside. Teardown that ran forwards would stop a provider while a consumer
/// was still using it.
///
/// A function rather than a loop inside `Instance::stop`, because an ordering
/// nothing can observe is an ordering nothing can hold you to: here it has a
/// test.
pub async fn stop_all(workers: Vec<Worker>) {
    for worker in workers.into_iter().rev() {
        worker.stop().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The reading an Operator gets: what is owed, and what has been done.
    #[test]
    fn admitting_work_raises_the_depth_and_settling_it_raises_the_count() {
        let meter = Meter::new();
        assert_eq!(meter.load(), Load { depth: 0, done: 0 });

        let first = meter.admit();
        let second = meter.admit();
        assert_eq!(meter.load(), Load { depth: 2, done: 0 });

        drop(first);
        assert_eq!(meter.load(), Load { depth: 1, done: 1 });

        drop(second);
        assert_eq!(meter.load(), Load { depth: 0, done: 2 });
        assert!(meter.load().is_idle());

        // Settling *nothing* must claim nothing. The push sender settles by a
        // count the fanout hands it, and a count that turned into a phantom
        // completed item would make `done` a number an Operator cannot trust.
        meter.settle_n(0);
        assert_eq!(meter.load(), Load { depth: 0, done: 2 });
    }

    /// The signal that replaces a fixed sleep. A test that means "nothing was
    /// pushed" has to know the sender *considered* the Call and declined it,
    /// which is what waiting for the work in hand buys and what sleeping for
    /// 400ms only guesses at.
    ///
    /// Polled by hand rather than raced against a timeout, because a timeout
    /// long enough to be trustworthy is exactly the fixed sleep this ticket is
    /// deleting. Two polls of one future say the same thing with no clock in
    /// reach at all: pending while the work is outstanding, ready the moment it
    /// is settled.
    #[tokio::test]
    async fn awaiting_idle_waits_for_the_work_in_hand() {
        let meter = Meter::new();
        let ticket = meter.admit();
        let mut idle = std::pin::pin!(meter.idle());

        assert!(
            futures_util::poll!(idle.as_mut()).is_pending(),
            "idle returned while a Ticket was still outstanding"
        );

        drop(ticket);

        assert!(
            futures_util::poll!(idle.as_mut()).is_ready(),
            "idle did not return once the work was settled"
        );
    }

    /// Idle is not enough for a worker that starts its own work: the retention
    /// sweeper is idle between every pair of sweeps, so "it swept again" can
    /// only be said as a count. This is what makes its scheduler — as opposed
    /// to the boot sweep the first tick fires immediately — observable at all.
    #[tokio::test]
    async fn awaiting_a_count_waits_for_that_many_to_have_settled() {
        let meter = Meter::new();
        drop(meter.admit());
        let mut second = std::pin::pin!(meter.settled_at_least(2));

        assert!(
            futures_util::poll!(second.as_mut()).is_pending(),
            "the second item was reported settled after only one had been"
        );

        drop(meter.admit());

        assert!(futures_util::poll!(second.as_mut()).is_ready());
        // A count already reached is not a wait — a caller that arrives late
        // must not hang for work that has already been done.
        meter.settled_at_least(1).await;
    }

    /// Stopping is asking *and* waiting. An Instance that only asked would free
    /// its port while a sweeper still held the database it is about to hand to
    /// the next run — which is precisely what `abort()` used to leave open.
    #[tokio::test]
    async fn stopping_a_worker_does_not_return_until_it_has_actually_ended() {
        let ended = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = ended.clone();
        let worker = Worker::start("sweeper", Meter::new(), |mut stop| async move {
            stop.cancelled().await;
            // A last act, after the signal: `stop` may not return before it.
            tokio::task::yield_now().await;
            flag.store(true, Ordering::SeqCst);
        });
        assert_eq!(worker.name(), "sweeper");

        worker.stop().await;

        assert!(
            ended.load(Ordering::SeqCst),
            "stop returned while the worker was still running"
        );
    }

    /// **Last started, first stopped** — and the reason to test it rather than
    /// comment it is that a teardown order is invisible from outside: drop the
    /// `.rev()` and every assertion in the suite still passes, right up until
    /// something is stopped while the thing that was still using it runs on.
    #[tokio::test]
    async fn workers_are_stopped_in_reverse_order_of_starting() {
        let order = Arc::new(std::sync::Mutex::new(Vec::new()));
        let started: Vec<Worker> = ["retention", "push", "enhancement"]
            .into_iter()
            .map(|name| {
                let order = order.clone();
                Worker::start(name, Meter::new(), move |mut stop| async move {
                    stop.cancelled().await;
                    order.lock().expect("order").push(name);
                })
            })
            .collect();

        stop_all(started).await;

        assert_eq!(
            *order.lock().expect("order"),
            vec!["enhancement", "push", "retention"]
        );
    }

    /// The registry is what makes the depth reading reach an Operator: a status
    /// handler (#70) is given `AppState` and can never see an `Instance`, so
    /// the readings have to live somewhere both hold. Names and all, in the
    /// order they were registered — a table an operator reads should not
    /// reshuffle itself between refreshes.
    #[test]
    fn the_registry_reads_every_worker_by_name() {
        let workers = Workers::default();
        let sweeper = Meter::new();
        let sink = Meter::new();
        workers.register("retention", sweeper.clone());
        workers.register("log-sink", sink.clone());

        let ticket = sweeper.admit();
        drop(sink.admit());

        assert_eq!(
            workers.loads(),
            vec![
                NamedLoad {
                    name: "retention",
                    load: Load { depth: 1, done: 0 },
                },
                NamedLoad {
                    name: "log-sink",
                    load: Load { depth: 0, done: 1 },
                },
            ]
        );
        drop(ticket);
    }

    /// One await for the whole Instance — what a test says instead of sleeping
    /// long enough for all of them.
    #[tokio::test]
    async fn the_registry_is_idle_only_when_every_worker_is() {
        let workers = Workers::default();
        let sweeper = Meter::new();
        let sink = Meter::new();
        workers.register("retention", sweeper.clone());
        workers.register("log-sink", sink.clone());
        let sweeping = sweeper.admit();
        let storing = sink.admit();

        let mut idle = std::pin::pin!(workers.idle());
        assert!(futures_util::poll!(idle.as_mut()).is_pending());

        // The *second* worker settling is not enough while the first still owes.
        drop(storing);
        assert!(futures_util::poll!(idle.as_mut()).is_pending());

        drop(sweeping);
        assert!(futures_util::poll!(idle.as_mut()).is_ready());
    }
}
