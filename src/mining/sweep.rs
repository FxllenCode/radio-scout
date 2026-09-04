//! The **Mining** sweep (#48, spec US 13): the Archive that was already there.
//!
//! [`super`] is the dialect and [`crate::ingest`] is where a new Call goes
//! through it. This is the other half of the ticket: an Operator who has been
//! running SDRTrunk for a year has an Archive full of MP3s with a configured
//! radio alias and a tower's name inside every one of them, and nothing has
//! ever looked. This walks it.
//!
//! **Deliberately not called a Backfill.** CONTEXT.md spends that word on the
//! Calls a Listener missed while **Feed down**, which `live.rs` replays on
//! reconnect; this is a periodic pass over the Archive under a policy, which is
//! a **Sweep** — the same shape [`crate::retention`] runs.
//!
//! Four things shape it, and they are the ticket's own acceptance criteria:
//!
//! - **Off the ingest path entirely.** It is a [`Worker`] on a ticker, like the
//!   retention sweeper — a recorder's upload never waits behind it.
//! - **Resumable.** The cursor is a column, `calls.mined_at_ms`, so a restart
//!   halfway through resumes rather than restarts. It is stamped whatever is
//!   found, *including nothing*, because the question the sweep asks is "has
//!   this been read?" — anything else and every barren Call in the Archive is
//!   re-read on every sweep forever.
//! - **Rate-bounded.** A batch per tick, both configurable. At the defaults a
//!   hundred thousand Calls are mined in something under nine hours, and an
//!   Operator on metered object storage can slow it down or switch it off.
//! - **Nothing here can fail a Call, and nothing can stall the sweep.** A tag
//!   that is malformed, absent or somebody else's settles as
//!   [`Settled::Nothing`] with a reason; audio that cannot be read settles as
//!   [`Settled::Failed`]. What gets *stamped* is [`sweep`]'s one rule, and it
//!   is the rule that keeps this alive — see it for why a page that read
//!   nothing claims nothing, and why a page that read something claims all of
//!   it.
//!
//! It never touches audio bytes. Mining is a read.

use std::fmt::Display;
use std::sync::Arc;

use tokio::time::MissedTickBehavior;
use tracing::{Instrument, Level, debug, error, info, span, warn};

use super::{Mined, MiningConfig};
use crate::AppState;
use crate::call::CallId;
use crate::db::entities::call;
use crate::db::repo;
use crate::worker::{Meter, Worker};

/// What this Worker is called on a status surface (#93, #70).
pub const WORKER: &str = "mining";

// -- What the sweep needs of the world ------------------------------------

/// What mining a stored Call needs of the **Archive** (CONTEXT.md) — which
/// Calls nothing has read, the bytes behind one, and the two ways of settling
/// it.
///
/// A port for the reason [`crate::enhance::Archive`] is one (#37, #97): the
/// arms that matter most here are the ones a broken object store produces, and
/// they are unreachable while the only store is a filesystem that works.
#[async_trait::async_trait]
pub trait Archive: Send + Sync {
    /// Calls nothing has looked inside yet, newest first, at most `limit`.
    async fn unmined(&self, limit: u64) -> Result<Vec<call::Model>, Failure>;

    /// The bytes behind an object key, or `None` if the object is gone.
    ///
    /// **The whole object**, though only its first few hundred bytes are read.
    /// A prefix range would need the object's size to bound it — `get_range`
    /// passes straight through to `object_store`, which errors past the end —
    /// and `calls.audio_size` is `NULL` on exactly the rows this sweep exists
    /// for, since it predates them. Falling back per Call would buy an S3
    /// operator some egress at the cost of an arm that reports "no metadata"
    /// when it truncated a tag. Left whole, deliberately, and said out loud
    /// because it is the reason `[mining] sweep` exists as a switch.
    async fn read(&self, key: &str) -> Result<Option<bytes::Bytes>, Failure>;

    /// Fold what was mined into the Call. `true` if anything was added.
    async fn store_mined(&self, call: &call::Model, mined: &Mined) -> Result<bool, Failure>;

    /// Stamp these Calls looked-at — the sweep's only way of saying so.
    async fn mark_mined(&self, call_ids: &[CallId]) -> Result<(), Failure>;
}

/// Why one of the Archive's answers could not be given.
///
/// The cause as text, for the reason [`crate::enhance::Failure`] is: it becomes
/// `cause=` on a line an Operator reads (ADR-0011 rule 4), and keeping sea-orm
/// and `object_store` out of this port's signature is most of why the port
/// exists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure(String);

impl Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl<E: std::error::Error> From<E> for Failure {
    fn from(error: E) -> Self {
        Failure(error.to_string())
    }
}

/// How one Call's mining ended.
///
/// **Settled**, not "outcome": CONTEXT.md reserves that word for what **Ingest**
/// decided about a Call, and [`crate::enhance::Settled`] is named for the same
/// reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Settled {
    /// Something was folded into the Call.
    Mined,
    /// Read, and there was nothing to take.
    Nothing(&'static str),
    /// Could not be read. Whether it is stamped anyway is [`sweep`]'s call,
    /// not this one's — it depends on whether *anything* in the page could be.
    Failed {
        reason: &'static str,
        cause: Failure,
    },
}

/// An **Encrypted Call** stores no audio object at all (spec US 9), so there is
/// nothing to look inside.
const NO_AUDIO: &str = "no-audio";
/// The row is there and the object is not — a prune interrupted between the
/// two, or an orphan-GC that won. Nothing will ever be there.
const AUDIO_GONE: &str = "audio-gone";
/// The file carries no embedded metadata: every WAV and M4A there has ever
/// been, and most MP3s.
const NO_METADATA: &str = "no-embedded-metadata";
/// It carries a tag, and no SDRTrunk wrote it.
const NOT_SDRTRUNK: &str = "not-sdrtrunk";
/// SDRTrunk's, and everything in it was already known.
const NOTHING_NEW: &str = "nothing-new";
/// The audio could not be read out of the store.
const READ_FAILED: &str = "read-failed";
/// What was mined could not be written back.
const WRITE_FAILED: &str = "write-failed";

/// Mine one stored Call.
///
/// Pure enough to be read in one sitting: everything that can go wrong is an
/// arm here, and every arm is a value a test constructs a substitute Archive to
/// produce.
pub async fn step(archive: &dyn Archive, call: &call::Model) -> Settled {
    if !call.has_audio() {
        return Settled::Nothing(NO_AUDIO);
    }
    let audio = match archive.read(&call.object_key).await {
        Ok(Some(audio)) => audio,
        Ok(None) => return Settled::Nothing(AUDIO_GONE),
        Err(cause) => {
            return Settled::Failed {
                reason: READ_FAILED,
                cause,
            };
        }
    };
    // The same three-byte gate ingest uses: a container that does not open with
    // an ID3 tag cannot hold anything SDRTrunk wrote, and probing it would be a
    // full container parse bought for a certain answer of "nothing".
    if !audio.starts_with(crate::audio_meta::ID3) {
        return Settled::Nothing(NO_METADATA);
    }
    let Some(mined) = super::mine(&crate::audio_meta::read(&audio)) else {
        return Settled::Nothing(NOT_SDRTRUNK);
    };
    match archive.store_mined(call, &mined).await {
        Ok(true) => Settled::Mined,
        Ok(false) => Settled::Nothing(NOTHING_NEW),
        Err(cause) => Settled::Failed {
            reason: WRITE_FAILED,
            cause,
        },
    }
}

/// What one sweep did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SweepReport {
    /// Calls something was folded into.
    pub mined: u64,
    /// Calls read, found barren, and stamped.
    pub barren: u64,
    /// Calls that could not be read, left for the next sweep.
    pub failed: u64,
}

impl SweepReport {
    /// Whether this sweep found nothing at all to do — the steady state, and
    /// what every tick looks like once the Archive has been walked.
    pub fn is_noop(&self) -> bool {
        *self == SweepReport::default()
    }
}

/// One pass: read a bounded page of Calls nothing has looked inside, mine each,
/// and stamp what it managed to look at.
///
/// **One rule decides what is stamped, and it is the rule that keeps the sweep
/// alive**: a pass that could read *nothing at all* makes no progress and says
/// so; any other pass stamps every Call it reached, including the ones it could
/// not read.
///
/// Both halves are load-bearing, and both were bugs before they were rules:
///
/// - Stamping nothing when the store is unreachable is what stops an Instance
///   booted with bad credentials from silently marking its whole Archive read.
///   The next tick asks for the same page and gets it right once the store is
///   back.
/// - Stamping the individually-unreadable ones is what stops a single corrupt
///   object at the head of `ORDER BY id DESC` from blocking every older Call
///   **forever** — the page never advances past it, so nothing behind it is
///   ever mined. What is lost is the name on a Call whose audio cannot be read
///   anyway.
pub async fn sweep(archive: &dyn Archive, batch_size: u64) -> Result<SweepReport, Failure> {
    let calls = archive.unmined(batch_size).await?;
    // An Archive with nothing left is the steady state for the whole life of
    // an Instance, so it returns before anything below can have an opinion
    // about it — and the rule below is then only ever asked about a page that
    // really held Calls, which is what lets it be `failed == considered` alone.
    if calls.is_empty() {
        return Ok(SweepReport::default());
    }
    let considered = calls.len();
    let mut report = SweepReport::default();
    // Stamped together at the end rather than one at a time: on a Trunk
    // Recorder archive nothing is ever mined, and a round-trip per Call would
    // be the whole cost of the sweep. One list, so "looked at" has one writer.
    let mut looked = Vec::with_capacity(considered);
    // The first thing that went wrong, kept for the one line below. Not a list:
    // a store that is refusing gives the same answer a hundred times, and an
    // Operator needs it once.
    let mut first_failure = None;
    for call in &calls {
        looked.push(call.id);
        match step(archive, call).await {
            Settled::Mined => report.mined += 1,
            Settled::Nothing(reason) => {
                report.barren += 1;
                debug!(call_id = call.id, reason, "nothing to mine");
            }
            Settled::Failed { reason, cause } => {
                report.failed += 1;
                // Per-Call detail at DEBUG, and **one** WARN for the sweep
                // below. At the shipped defaults a store that refuses
                // everything would otherwise write 12 000 identical WARNs an
                // hour, forever — the `logsink` rule, which reports a count
                // once per drained batch and never once per loss.
                debug!(call_id = call.id, reason, %cause, "could not mine a Call");
                first_failure.get_or_insert((reason, cause));
            }
        }
    }
    if let Some((reason, cause)) = first_failure {
        // WARN: work was dropped and an Operator would want to know (rule 7).
        warn!(
            reason,
            %cause,
            failed = report.failed,
            of = considered,
            "some Calls could not be mined"
        );
    }
    // Nothing read at all: the store is unreachable rather than one object
    // being bad, so this pass claims nothing and the next one asks again.
    if report.failed as usize == considered {
        return Ok(report);
    }
    archive.mark_mined(&looked).await?;
    Ok(report)
}

// -- The Worker --------------------------------------------------------------

/// The **Mining** sweep as an Instance's Worker.
pub struct Sweeper {
    archive: Arc<dyn Archive>,
    config: MiningConfig,
    meter: Arc<Meter>,
}

impl Sweeper {
    /// A sweeper over this Archive under this policy.
    pub fn new(archive: Arc<dyn Archive>, config: MiningConfig) -> Self {
        Sweeper {
            archive,
            config,
            meter: Meter::new(),
        }
    }

    /// Sweep on `config.interval` until the Worker is stopped, starting
    /// **immediately** — the retention sweeper's shape and its reasoning: an
    /// Instance that restarts more often than the interval would otherwise
    /// never sweep at all.
    ///
    /// Depth is `1` while a sweep runs and `0` between, so a depth stuck at `1`
    /// is a sweep that never ended; the settled count is the number of sweeps,
    /// which is the only way from outside to tell the scheduler from its first
    /// tick.
    pub fn start(self) -> Worker {
        let Sweeper {
            archive,
            config,
            meter,
        } = self;
        let period = config.effective_interval();
        // Owed from the moment this returns rather than from whenever the
        // runtime first polls the task, so an Instance is never observed idle
        // in the window before its first sweep has begun (#93).
        let mut boot = Some(meter.admit());

        Worker::start(WORKER, meter.clone(), move |mut stop| async move {
            let mut ticker = tokio::time::interval(period);
            ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
            loop {
                tokio::select! {
                    // Cancellation first, always: a stop asked for while a tick
                    // is also ready must not lose a coin toss.
                    biased;
                    _ = stop.cancelled() => break,
                    _ = ticker.tick() => {}
                }
                let _sweeping = boot.take().unwrap_or_else(|| meter.admit());
                let work = async {
                    log_sweep(&sweep(archive.as_ref(), config.batch_size).await);
                }
                .instrument(span!(Level::ERROR, "mining"));
                tokio::select! {
                    biased;
                    // Cancelled mid-sweep is safe: every Call is stamped
                    // independently, so the sweep is consistent wherever it
                    // stops and the next one resumes from there.
                    _ = stop.cancelled() => break,
                    () = work => {}
                }
            }
        })
    }
}

/// Say what one sweep did, at the level it deserves (ADR-0011 rule 7).
///
/// An Archive with nothing left to mine is the steady state — every tick,
/// forever — so it is DEBUG; a per-tick INFO about nothing is how a log stops
/// being read. A sweep that enriched Calls is a notable normal event. A sweep
/// that could not run at all is an ERROR, because until it does the Archive
/// stays as poor as it was.
fn log_sweep(swept: &Result<SweepReport, Failure>) {
    match swept {
        Ok(report) if report.is_noop() => debug!("mining sweep found nothing left to mine"),
        Ok(report) => info!(
            mined = report.mined,
            barren = report.barren,
            failed = report.failed,
            "mining sweep finished"
        ),
        Err(cause) => error!(%cause, "mining sweep failed"),
    }
}

/// Start the sweep Worker, or nothing at all where an Operator turned it off.
pub fn spawn(state: AppState) -> Option<Worker> {
    let config = state.mining.clone();
    config
        .sweep
        .then(|| Sweeper::new(Arc::new(state), config).start())
}

// -- The Instance's own Archive ----------------------------------------------

#[async_trait::async_trait]
impl Archive for AppState {
    async fn unmined(&self, limit: u64) -> Result<Vec<call::Model>, Failure> {
        Ok(repo::unmined_calls(&self.db, limit).await?)
    }

    async fn read(&self, key: &str) -> Result<Option<bytes::Bytes>, Failure> {
        Ok(self.audio.get(key).await?)
    }

    async fn store_mined(&self, call: &call::Model, mined: &Mined) -> Result<bool, Failure> {
        Ok(repo::apply_mined(
            &self.db,
            call,
            mined,
            self.ingest.auto_populate,
            self.clock.now_ms(),
        )
        .await?)
    }

    async fn mark_mined(&self, call_ids: &[CallId]) -> Result<(), Failure> {
        Ok(repo::mark_mined(&self.db, call_ids, self.clock.now_ms()).await?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mining::MinedUnit;
    use crate::testing::LogCapture;
    use crate::testing::id3::{comment, tagged_mp3, text};
    use rstest::rstest;
    use std::sync::Mutex;

    /// An Archive that answers exactly what a test told it to.
    ///
    /// The point of the port (#37, #97): the arms that matter most here are a
    /// store that will not answer and a write that will not land, and neither is
    /// reachable while the only store is a filesystem that works.
    #[derive(Default)]
    struct FakeArchive {
        audio: Option<Vec<u8>>,
        read_fails: bool,
        write_fails: bool,
        /// Whether [`repo::apply_mined`]'s real counterpart would have changed
        /// anything — the one thing `step` cannot decide for itself.
        adds_something: bool,
        unmined: Vec<call::Model>,
        list_fails: bool,
        /// What the last `store_mined` was handed — the dialect's answer, which
        /// nothing else here can check.
        captured: Mutex<Option<Mined>>,
        /// What actually happened, in order.
        seen: Mutex<Vec<String>>,
    }

    impl FakeArchive {
        fn holding(audio: Vec<u8>) -> Self {
            FakeArchive {
                audio: Some(audio),
                adds_something: true,
                ..Default::default()
            }
        }

        fn log(&self, entry: impl Into<String>) {
            self.seen.lock().expect("the log").push(entry.into());
        }

        fn seen(&self) -> Vec<String> {
            self.seen.lock().expect("the log").clone()
        }
    }

    #[async_trait::async_trait]
    impl Archive for FakeArchive {
        async fn unmined(&self, limit: u64) -> Result<Vec<call::Model>, Failure> {
            match self.list_fails {
                true => Err(Failure("the database refused".into())),
                false => Ok(self.unmined.iter().take(limit as usize).cloned().collect()),
            }
        }

        async fn read(&self, key: &str) -> Result<Option<bytes::Bytes>, Failure> {
            self.log(format!("read {key}"));
            match self.read_fails {
                true => Err(Failure("the store refused".into())),
                false => Ok(self.audio.clone().map(bytes::Bytes::from)),
            }
        }

        async fn store_mined(&self, call: &call::Model, mined: &Mined) -> Result<bool, Failure> {
            self.log(format!("store {}", call.id));
            *self.captured.lock().expect("the slot") = Some(mined.clone());
            match self.write_fails {
                true => Err(Failure("the database refused".into())),
                false => Ok(self.adds_something),
            }
        }

        async fn mark_mined(&self, call_ids: &[CallId]) -> Result<(), Failure> {
            self.log(format!("mark {call_ids:?}"));
            Ok(())
        }
    }

    /// A stored Call, pointing at `key` — or at nothing, which is what an
    /// **Encrypted Call** looks like.
    fn stored(id: CallId, key: &str) -> call::Model {
        call::Model {
            id,
            system_id: 1,
            talkgroup_id: 1,
            talkgroup_ref: None,
            call_at_ms: 1_000,
            frequency: None,
            object_key: key.to_string(),
            audio_mime: None,
            audio_name: None,
            audio_size: None,
            duration_ms: None,
            stop_at_ms: None,
            emergency: false,
            encrypted: false,
            quiet_state: call::QuietState::NONE.into(),
            quiet: None,
            priority: None,
            audio_type: None,
            site_id: None,
            enhancement: call::EnhancementState::NONE.to_string(),
            tone: call::ToneState::NONE.to_string(),
            emitted_seq: None,
            mined_at_ms: None,
            starred_at_ms: None,
            created_at_ms: 0,
        }
    }

    /// A complete SDRTrunk MP3 — the file this whole feature exists for.
    fn sdrtrunk_mp3() -> Vec<u8> {
        tagged_mp3(vec![
            text(b"TCOM", "sdrtrunk v0.6.1"),
            text(b"TPE1", "1234567 Engine 1"),
            comment("Site:Downtown;Decoder:P25 Phase 1;"),
        ])
    }

    // -- One Call ---------------------------------------------------------

    /// The happy path: a stored SDRTrunk Call gives up what is in it.
    #[tokio::test]
    async fn a_stored_sdrtrunk_call_is_mined() {
        let archive = FakeArchive::holding(sdrtrunk_mp3());

        let settled = step(&archive, &stored(7, "a/b.mp3")).await;

        assert_eq!(settled, Settled::Mined);
        assert_eq!(archive.seen(), vec!["read a/b.mp3", "store 7"]);
    }

    /// An **Encrypted Call** stores no object at all (spec US 9), so there is
    /// nothing to look inside — and, crucially, no read is even attempted. A
    /// sweep over a system with encrypted traffic must not spend a round-trip
    /// per Call discovering that.
    #[tokio::test]
    async fn a_call_with_no_audio_is_settled_without_touching_the_store() {
        let archive = FakeArchive::holding(sdrtrunk_mp3());

        let settled = step(&archive, &stored(7, "")).await;

        assert_eq!(settled, Settled::Nothing(NO_AUDIO));
        assert!(archive.seen().is_empty(), "{:?}", archive.seen());
    }

    /// Every arm that ends in "there was nothing to take". Each is stamped, so
    /// none of them is ever read a second time.
    #[rstest]
    #[case::the_object_is_gone(None, AUDIO_GONE)]
    #[case::a_wav_carries_no_tag(Some(b"RIFF\x00\x00\x00\x00WAVE".to_vec()), NO_METADATA)]
    #[case::an_untagged_mp3(Some(vec![0xFF, 0xFB, 0x90, 0xC0]), NO_METADATA)]
    #[tokio::test]
    async fn audio_with_nothing_in_it_settles_with_a_reason(
        #[case] audio: Option<Vec<u8>>,
        #[case] reason: &'static str,
    ) {
        let archive = FakeArchive {
            audio,
            ..Default::default()
        };

        assert_eq!(
            step(&archive, &stored(7, "a/b.wav")).await,
            Settled::Nothing(reason)
        );
    }

    /// A tag somebody else wrote is read, understood to be nobody's dialect,
    /// and left alone — the gate that keeps a podcast's artist field from being
    /// read as a radio id.
    #[tokio::test]
    async fn a_tag_no_sdrtrunk_wrote_settles_as_nobodys_dialect() {
        let archive = FakeArchive::holding(tagged_mp3(vec![
            text(b"TCOM", "LAME 3.100"),
            text(b"TPE1", "50 Cent"),
        ]));

        assert_eq!(
            step(&archive, &stored(7, "a/b.mp3")).await,
            Settled::Nothing(NOT_SDRTRUNK)
        );
    }

    /// SDRTrunk's, and the Call already knew all of it — the second sweep over
    /// an Archive somebody re-mined, and every Call ingested since #48.
    #[tokio::test]
    async fn an_sdrtrunk_call_that_adds_nothing_settles_as_nothing_new() {
        let archive = FakeArchive {
            adds_something: false,
            ..FakeArchive::holding(sdrtrunk_mp3())
        };

        assert_eq!(
            step(&archive, &stored(7, "a/b.mp3")).await,
            Settled::Nothing(NOTHING_NEW)
        );
    }

    /// **A transient failure is not an answer.** A store that will not answer,
    /// or a database that will not take the write, leaves the Call unsettled —
    /// and `sweep` below is what proves it also leaves it unstamped.
    #[rstest]
    #[case::the_store_refused(true, false, READ_FAILED)]
    #[case::the_write_refused(false, true, WRITE_FAILED)]
    #[tokio::test]
    async fn what_could_not_be_read_or_written_is_a_failure_and_not_an_answer(
        #[case] read_fails: bool,
        #[case] write_fails: bool,
        #[case] reason: &'static str,
    ) {
        let archive = FakeArchive {
            read_fails,
            write_fails,
            ..FakeArchive::holding(sdrtrunk_mp3())
        };

        assert!(matches!(step(&archive, &stored(7, "a/b.mp3")).await,
                     Settled::Failed { reason: r, .. } if r == reason));
    }

    // -- One sweep --------------------------------------------------------

    /// **Every barren Call is stamped in one statement.** On a Trunk Recorder
    /// archive every Call is barren, so a round-trip each would be the whole
    /// cost of the sweep.
    #[tokio::test]
    async fn a_sweep_stamps_every_barren_call_together() {
        let archive = FakeArchive {
            unmined: vec![stored(1, ""), stored(2, ""), stored(3, "")],
            ..Default::default()
        };

        let report = sweep(&archive, 10).await.expect("the sweep ran");

        assert_eq!(
            report,
            SweepReport {
                mined: 0,
                barren: 3,
                failed: 0
            }
        );
        assert_eq!(archive.seen(), vec!["mark [1, 2, 3]"]);
    }

    /// A real batch is mixed, and the report has to count each kind as its own
    /// — an Operator reading `mined=0 barren=100` on the first sweep of an
    /// SDRTrunk archive would be looking at a broken feature, and reading
    /// `mined=100` over a Trunk Recorder one would be looking at a lie.
    ///
    /// Both kinds are stamped, in the same statement: what the sweep records is
    /// that it *looked*.
    #[tokio::test]
    async fn a_mixed_batch_is_counted_and_stamped_by_what_each_call_gave_up() {
        let archive = FakeArchive {
            // An SDRTrunk Call, and an **Encrypted Call** with no object.
            unmined: vec![stored(1, "a/b.mp3"), stored(2, "")],
            ..FakeArchive::holding(sdrtrunk_mp3())
        };

        let report = sweep(&archive, 10).await.expect("the sweep ran");

        assert_eq!(
            report,
            SweepReport {
                mined: 1,
                barren: 1,
                failed: 0
            }
        );
        assert_eq!(
            archive.seen(),
            vec!["read a/b.mp3", "store 1", "mark [1, 2]"]
        );
    }

    /// **A store that answers nothing claims nothing.** Every Call in the page
    /// failed, so the store is unreachable rather than one object being bad —
    /// and an Instance booted with the wrong credentials must not quietly mark
    /// its entire Archive as read. The next tick asks for the same page.
    #[tokio::test]
    async fn a_sweep_that_could_read_nothing_at_all_stamps_nothing() {
        let archive = FakeArchive {
            unmined: vec![stored(1, "a/b.mp3"), stored(2, "c/d.mp3")],
            read_fails: true,
            ..FakeArchive::holding(sdrtrunk_mp3())
        };

        let report = sweep(&archive, 10).await.expect("the sweep ran");

        assert_eq!(report.failed, 2);
        assert_eq!(
            archive.seen(),
            vec!["read a/b.mp3", "read c/d.mp3"],
            "not one Call was stamped, so the next sweep asks again"
        );
    }

    /// **...but one bad object never blocks the Archive behind it.**
    ///
    /// The page is `ORDER BY id DESC`, so a Call that can never be read — a
    /// corrupt object, a prefix the credentials cannot reach — sits at the head
    /// of every future page. Left unstamped it would block every older Call
    /// *forever*, and emit a WARN per Call per tick while doing it. It is
    /// stamped along with the rest: what is lost is the name on a Call whose
    /// audio cannot be read anyway.
    #[tokio::test]
    async fn one_unreadable_call_never_blocks_the_archive_behind_it() {
        let archive = FakeArchive {
            // The head fails; the Call behind it is an **Encrypted Call**,
            // which is settled without a read at all.
            unmined: vec![stored(1, "a/b.mp3"), stored(2, "")],
            read_fails: true,
            ..FakeArchive::holding(sdrtrunk_mp3())
        };

        let report = sweep(&archive, 10).await.expect("the sweep ran");

        assert_eq!(
            report,
            SweepReport {
                mined: 0,
                barren: 1,
                failed: 1
            }
        );
        assert_eq!(
            archive.seen(),
            vec!["read a/b.mp3", "mark [1, 2]"],
            "the one that could not be read is stamped with the rest, so the \
             page advances past it"
        );
    }

    /// The rate bound. A sweep reads its batch and no more, however much is
    /// waiting — which is what keeps a sweep over a county's archive from
    /// being the thing an Operator notices.
    #[tokio::test]
    async fn a_sweep_reads_no_more_than_its_batch() {
        let archive = FakeArchive {
            unmined: (1..=50).map(|id| stored(id, "")).collect(),
            ..Default::default()
        };

        assert_eq!(sweep(&archive, 4).await.expect("swept").barren, 4);
    }

    /// An Archive with nothing left is the steady state, forever — and it has
    /// to be distinguishable from one that did something, because that is what
    /// decides whether a line is written at all (ADR-0011 rule 7).
    #[tokio::test]
    async fn a_sweep_over_a_mined_archive_did_nothing() {
        let archive = FakeArchive::default();

        assert!(sweep(&archive, 10).await.expect("swept").is_noop());
    }

    /// ...and a sweep that did *anything* is not that, whichever of the three
    /// things it was. An hourly line about nothing is how a log stops being
    /// read; a silent sweep that enriched a hundred Calls is worse.
    #[rstest]
    #[case::mined(SweepReport { mined: 1, ..Default::default() })]
    #[case::barren(SweepReport { barren: 1, ..Default::default() })]
    #[case::failed(SweepReport { failed: 1, ..Default::default() })]
    fn a_sweep_that_did_something_is_never_read_as_having_done_nothing(
        #[case] report: SweepReport,
    ) {
        assert!(!report.is_noop());
    }

    /// **What a sweep did reaches the log, and so does why it could not.**
    ///
    /// The three arms an Operator ever sees, at the three levels ADR-0011
    /// rule 7 asks for — and the failing ones carry their `cause`, which is the
    /// only thing that turns "the sweep is not progressing" into something
    /// actionable. Without this the whole [`Failure`] type could render as an
    /// empty string and every line would still be written.
    #[test]
    fn a_sweep_says_what_it_did_and_why_it_could_not() {
        let quiet = LogCapture::start();
        log_sweep(&Ok(SweepReport::default()));
        let logged = quiet.text();
        assert!(logged.contains("DEBUG"), "{logged}");
        assert!(!logged.contains("INFO"), "{logged}");

        let busy = LogCapture::start();
        log_sweep(&Ok(SweepReport {
            mined: 7,
            barren: 2,
            failed: 1,
        }));
        let logged = busy.text();
        assert!(logged.contains("INFO"), "{logged}");
        for field in ["mined=7", "barren=2", "failed=1"] {
            assert!(logged.contains(field), "no {field} in {logged}");
        }

        let broken = LogCapture::start();
        log_sweep(&Err(Failure("the database refused".into())));
        let logged = broken.text();
        assert!(logged.contains("ERROR"), "{logged}");
        assert!(
            logged.contains("the database refused"),
            "a sweep that failed must say why: {logged}"
        );
    }

    /// ...and the same for one Call: a store that would not answer names itself
    /// on the WARN line, because "some Calls are not being mined" with no cause
    /// beside it is a report an Operator cannot act on.
    #[tokio::test]
    async fn a_call_that_could_not_be_read_says_why_on_the_line() {
        let archive = FakeArchive {
            unmined: vec![stored(1, "a/b.mp3")],
            read_fails: true,
            ..FakeArchive::holding(sdrtrunk_mp3())
        };

        let capture = LogCapture::start();
        sweep(&archive, 10).await.expect("the sweep ran");

        let logged = capture.text();
        assert!(logged.contains("WARN"), "{logged}");
        assert!(logged.contains(READ_FAILED), "{logged}");
        assert!(
            logged.contains("the store refused"),
            "the cause has to ride the line: {logged}"
        );
    }

    /// A store that cannot even be asked which Calls are unmined fails the
    /// whole sweep rather than reporting an empty one — an empty report reads
    /// as "the Archive is fully mined", which would be a lie.
    #[tokio::test]
    async fn a_sweep_that_cannot_list_fails_rather_than_reporting_nothing() {
        let archive = FakeArchive {
            list_fails: true,
            unmined: vec![stored(1, "a/b.mp3")],
            ..Default::default()
        };

        assert!(sweep(&archive, 10).await.is_err());
        assert!(archive.seen().is_empty(), "and nothing was read or stamped");
    }

    /// What `mined` actually holds, so the `store_mined` above is standing in
    /// for something real — the fake cannot check the dialect, and this does.
    #[tokio::test]
    async fn what_reaches_the_writer_is_what_was_in_the_file() {
        let archive = FakeArchive::holding(sdrtrunk_mp3());

        step(&archive, &stored(7, "a/b.mp3")).await;

        assert_eq!(
            archive.captured.lock().expect("the slot").clone(),
            Some(Mined {
                unit: Some(MinedUnit {
                    unit_ref: 1234567,
                    label: "Engine 1".into()
                }),
                site: Some("Downtown".into()),
                decoder: Some("P25 Phase 1".into()),
                frequency: None,
            })
        );
    }
}
