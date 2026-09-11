//! Retention (#10, ADR-0002 / spec US 41): keep the archive bounded so a Pi's
//! SD card can't fill.
//!
//! rdio-scanner prunes by age only, with one unbounded
//! `DELETE FROM calls WHERE timestamp < ?` on an hourly ticker
//! (`scheduler.go`, `call.go`). Radio-Scout keeps the age policy and its
//! `pruneDays == 0 means keep forever` semantics, then improves on it:
//!
//! - **Optional total-size cap** — rdio has none, so a busy system fills the
//!   disk between prunes no matter how short the window. Ours prunes the oldest
//!   Calls until the archive fits, measured off the `audio_size` column so the
//!   check is one `SUM()` rather than a walk of the object store.
//! - **Batched deletes** — one bounded page at a time keeps each SQLite
//!   write-lock window short, so a sweep over a large archive never stalls
//!   ingest on a Pi. rdio's single unbounded statement does exactly that.
//! - **Audio is pruned with its metadata** — rdio keeps audio in the DB, so a
//!   row delete is the whole story. Ours deletes the **row first, then the
//!   object** (ADR-0002), so an archive row never points at missing audio; a
//!   crash in that window leaves an orphan, which [`sweep`]'s GC pass reclaims.
//! - **Orphan-GC with a write grace period** — a naive "delete every object with
//!   no row" sweep would race ingest, which writes the object *before* the row.
//!   Only objects untouched for [`RetentionConfig::orphan_grace`] are reclaimed.
//! - **Sweeps at startup, not an hour in** — rdio's ticker fires first at +1h,
//!   so a box that restarts often never prunes at all.
//!
//! [`sweep`] is one pass; [`Sweeper`] runs it on an interval, as one of the
//! Instance's Workers (#93).

use std::sync::Arc;
use std::time::Duration;

use crate::db::Db;
use object_store::Error as ObjectError;
use sea_orm::DbErr;
use serde::{Deserialize, Serialize};
use tokio::time::MissedTickBehavior;
use tracing::{debug, error, info, warn};

use crate::Clock;
use crate::blob;
use crate::blob::AudioStore;
use crate::call::CallId;
use crate::db::repo::{self, PrunableCall};
use crate::worker::{Meter, Worker};

/// Milliseconds in a day.
const MS_PER_DAY: i64 = 86_400_000;

/// Bytes in a binary gigabyte (GiB, 2^30) — how `retention.max_size_gb` is read.
const BYTES_PER_GB: f64 = 1_073_741_824.0;

/// Default sweep cadence, matching rdio-scanner's hourly prune ticker.
const DEFAULT_INTERVAL: Duration = Duration::from_secs(3600);

/// Retention policy — and the `[retention]` section itself (#17, #87).
///
/// One type, in the units the sweeper works in; the two settings an operator
/// writes in coarser units keep those units at the serde boundary. `_secs`
/// fields go through [`crate::config::secs`], and the size cap through
/// [`gigabytes`] below — nobody sizes an archive in bytes, and nobody prunes in
/// gigabytes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RetentionConfig {
    /// Prune Calls older than this many days. `0` disables age-based pruning
    /// (rdio-scanner's `pruneDays` semantics).
    pub days: u32,
    /// Optional cap on total stored audio; the oldest Calls are pruned until
    /// the archive fits. `None` means no size cap.
    #[serde(
        rename = "max_size_gb",
        with = "gigabytes",
        skip_serializing_if = "Option::is_none"
    )]
    pub max_size_bytes: Option<u64>,
    /// Prune stored log events (#30) older than this many days. `0` keeps them
    /// forever, matching [`RetentionConfig::days`]'s reading.
    ///
    /// A window of its own rather than the archive's, because the two are
    /// different sizes of problem: a month of log lines is a few megabytes,
    /// a month of audio is not — and an operator keeping Calls forever
    /// (`days = 0`) must still not accumulate an unbounded logs table.
    pub log_days: u32,
    /// Prune listener-count samples (#62) older than this many days. `0` keeps
    /// them forever, matching [`RetentionConfig::days`]'s reading.
    ///
    /// Its own window again, and longer than the archive's for
    /// [`RetentionConfig::log_days`]'s reason twice over: a sample is sixteen
    /// bytes where a Call is a megabyte of audio, and "was last winter busier
    /// than this one" is a question about a period whose audio went months ago.
    pub listener_days: u32,
    /// How much longer a **Star** (#66, spec US 37) keeps a Call than
    /// [`RetentionConfig::days`] would — the operator policy the ticket asks
    /// for, and the only thing on this Instance that can hold a Call back from
    /// the age pass.
    ///
    /// **Absent is the default, and it means a Star changes nothing**: "where
    /// the operator allows" is opt-in, and an Instance shipping with `days = 7`
    /// and no size cap would otherwise let anybody who can POST in a loop
    /// commit its disk. `0` keeps a starred Call for good, the reading the
    /// three windows above already have; `90` keeps it a quarter.
    ///
    /// **A window, rather than the boolean the ticket's wording suggests**,
    /// because that is what bounds the two ways this could go wrong on its own:
    /// a Listener who stars a thousand Calls, and a Star nobody will ever come
    /// back for. It is also one number in the units this section already reads.
    ///
    /// **The size cap outranks it** — see [`sweep`] — because a cap a Listener
    /// can defeat is not a cap.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub starred_days: Option<u32>,
    /// How often [`Sweeper`]'s background task runs a [`sweep`]. Zero is read as
    /// "unset" and falls back to the default cadence.
    #[serde(rename = "interval_secs", with = "crate::config::secs")]
    pub interval: Duration,
    /// Calls deleted per batch. Bounds how long any single write-lock is held.
    pub batch_size: u64,
    /// How long an unreferenced object is left alone before orphan-GC reclaims
    /// it. Must comfortably exceed the ingest window between writing an audio
    /// object and committing its row, or the GC would delete audio out from
    /// under a Call that is mid-ingest.
    #[serde(rename = "orphan_grace_secs", with = "crate::config::secs")]
    pub orphan_grace: Duration,
}

impl Default for RetentionConfig {
    fn default() -> Self {
        RetentionConfig {
            // rdio-scanner's default, and the reason this feature exists: an
            // unbounded archive fills a Pi's SD card.
            days: 7,
            max_size_bytes: None,
            // Longer than the archive's week on purpose: logs are small, and
            // the question they answer ("when did my recorder stop?") is often
            // asked about a period whose audio has already been pruned.
            log_days: 30,
            // A quarter, so a season is comparable with the one before it — the
            // shape of the question this table exists to answer. Still under
            // 130,000 rows at the default cadence.
            listener_days: 90,
            // A Star holds nothing back until an Operator says it may. See the
            // field: the exemption is a disk commitment, and only they can make
            // one.
            starred_days: None,
            interval: DEFAULT_INTERVAL,
            batch_size: 500,
            orphan_grace: Duration::from_secs(3600),
        }
    }
}

/// How long a **Star** holds a Call back from the age pass (#66, spec US 37).
///
/// Three states rather than an `Option<i64>`, because there are three and the
/// two that an option could spell are not the two that matter: *not kept at
/// all* is what ships, *kept a while longer* is the ordinary policy, and *kept
/// for good* is the `0` every other window in [`RetentionConfig`] already
/// means "forever" by. Collapsing the first into "kept until now" would also
/// spend a `WHERE` clause on every sweep of every Instance that never turned
/// this on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StarKeep {
    /// Not at all: a starred Call ages out beside every other one, which is
    /// what an Instance nobody has configured does.
    None,
    /// Until it is older than this instant — `[retention] starred_days` days
    /// before now.
    Until(i64),
    /// For good — `starred_days = 0`.
    Forever,
}

/// What one age pass may take: everything older than `cutoff_ms`, except what
/// a **Star** is still holding back.
///
/// A value rather than two arguments, so the sweep and the query that serves it
/// cannot be given one and not the other — the [`crate::ingest::dedup_window`]
/// move, one policy along.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AgePass {
    /// A Call older than this instant has aged out.
    pub cutoff_ms: i64,
    /// What a **Star** does about that.
    pub stars: StarKeep,
}

impl RetentionConfig {
    /// The `call_at_ms` below which a Call has aged out, or `None` when
    /// age-based pruning is disabled (`days == 0`).
    pub fn cutoff_ms(&self, now_ms: i64) -> Option<i64> {
        cutoff_for(self.days, now_ms)
    }

    /// What this sweep's age pass may take at `now_ms`, or `None` when there is
    /// no age pass at all (`days == 0`, rdio's "keep forever").
    ///
    /// The Star window is read only when there is a pass to be spared from,
    /// which is what keeps an Operator who keeps everything forever from being
    /// able to configure a Star that *shortens* a Call's life.
    pub fn age_pass(&self, now_ms: i64) -> Option<AgePass> {
        let cutoff_ms = self.cutoff_ms(now_ms)?;
        let stars = match self.starred_days {
            None => StarKeep::None,
            Some(0) => StarKeep::Forever,
            Some(days) => StarKeep::Until(days_before(days, now_ms)),
        };
        Some(AgePass { cutoff_ms, stars })
    }

    /// The same, for stored log events and [`RetentionConfig::log_days`] (#30).
    pub fn log_cutoff_ms(&self, now_ms: i64) -> Option<i64> {
        cutoff_for(self.log_days, now_ms)
    }

    /// The same again, for listener samples and
    /// [`RetentionConfig::listener_days`] (#62).
    pub fn listener_cutoff_ms(&self, now_ms: i64) -> Option<i64> {
        cutoff_for(self.listener_days, now_ms)
    }

    /// The cadence [`Sweeper`] will actually sweep on. A zero interval is read as
    /// "unset" and falls back to the default rather than panicking the ticker.
    pub fn effective_interval(&self) -> Duration {
        if self.interval.is_zero() {
            DEFAULT_INTERVAL
        } else {
            self.interval
        }
    }

    /// Log the policy at startup, so an operator can see at a glance what will
    /// be deleted and how often — the one question a scanner that silently eats
    /// old Calls can't answer.
    ///
    /// Structured fields rather than a sentence (ADR-0011 rule 6): `days=0` is
    /// rdio's "keep forever", and `max_size_bytes` absent is "no cap".
    pub fn log(&self) {
        let interval_secs = self.effective_interval().as_secs();
        info!(
            days = self.days,
            max_size_bytes = ?self.max_size_bytes,
            log_days = self.log_days,
            listener_days = self.listener_days,
            starred_days = ?self.starred_days,
            interval_secs,
            batch_size = self.batch_size,
            "retention policy"
        );
    }

    /// The size cap `gb` binary gigabytes asks for, or why it cannot be one.
    ///
    /// Shared by the two layers that can carry the setting — the file, through
    /// [`gigabytes`], and the environment, through the settings table — so an
    /// operator who wrote `0` is told the same thing whichever they wrote it in.
    pub fn max_size_bytes_from_gb(gb: f64) -> Result<u64, &'static str> {
        match gb.is_finite() && gb > 0.0 {
            // A cap must be a size. `days = 0` is rdio's "keep forever" and
            // stays legal, but a zero *cap* would mean "prune everything",
            // which nobody ever means — and no key at all already says "no cap".
            true => Ok((gb * BYTES_PER_GB) as u64),
            false => Err(EXPECTED_MAX_SIZE_GB),
        }
    }
}

/// What an unusable `[retention] max_size_gb` is told it should have been.
pub const EXPECTED_MAX_SIZE_GB: &str =
    "a positive number of gigabytes, or no key at all for no cap";

/// Binary gigabytes in the file, bytes in the type.
///
/// The one place `max_size_gb` becomes `max_size_bytes`, and the one place a
/// value that cannot be a cap is refused. Rejecting it *here* rather than in a
/// later validation pass is the [`crate::config::ProxyNet`] move: the type that
/// owns the value refuses it wherever it was written, so the message carries the
/// line and column of the key the operator has to edit. The key names itself in
/// the message because a `serde` error is rendered by position alone, and
/// `radio-scout.toml:14:15: a positive number of gigabytes` would leave an
/// operator counting lines.
mod gigabytes {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    use super::{BYTES_PER_GB, RetentionConfig};

    pub fn serialize<S: Serializer>(value: &Option<u64>, serializer: S) -> Result<S::Ok, S::Error> {
        value
            .map(|bytes| bytes as f64 / BYTES_PER_GB)
            .serialize(serializer)
    }

    /// `f64` rather than `Option<f64>`: the struct's own `#[serde(default)]`
    /// answers an absent key without ever reaching here, and TOML has no null
    /// for a present one — so "no cap" is a key that isn't written, and an arm
    /// for it would be unreachable.
    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<u64>, D::Error> {
        let gb = f64::deserialize(deserializer)?;
        RetentionConfig::max_size_bytes_from_gb(gb)
            .map(Some)
            .map_err(|expected| {
                serde::de::Error::custom(crate::config::rejected(
                    "retention.max_size_gb",
                    gb,
                    expected,
                ))
            })
    }
}

/// The timestamp below which something `days` old has aged out, or `None` when
/// the window is `0` — rdio-scanner's `pruneDays` semantics, which both windows
/// share so an operator only has to learn them once.
fn cutoff_for(days: u32, now_ms: i64) -> Option<i64> {
    (days != 0).then(|| days_before(days, now_ms))
}

/// The instant `days` before `now_ms` — the arithmetic every window in this
/// section is measured by, with no reading of `0` attached to it.
///
/// Separate from [`cutoff_for`] because a **Star**'s window has already spent
/// `0` on "for good" by the time it needs this ([`RetentionConfig::age_pass`]),
/// so it wants the sum and not the option: threading it through `cutoff_for`
/// would put a `None` arm there that nothing can reach and nothing can kill.
///
/// `u32::MAX` days is ~3.7e17 ms, comfortably inside i64; only a clock near
/// `i64::MIN` could underflow, and saturating there is still "nothing is old
/// enough", which is the safe direction.
fn days_before(days: u32, now_ms: i64) -> i64 {
    now_ms.saturating_sub(i64::from(days) * MS_PER_DAY)
}

/// What one [`sweep`] did. Zero everywhere means the archive was already within
/// policy — the common case, and the one worth staying quiet about.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SweepReport {
    /// Calls pruned for being older than the retention window.
    pub aged_out: u64,
    /// Calls pruned to bring the archive under the size cap.
    pub over_cap: u64,
    /// Objects reclaimed by orphan-GC (audio with no Call row).
    pub orphans: u64,
    /// Bytes of audio reclaimed, summed over both prune policies and the
    /// orphan-GC pass.
    pub bytes_freed: u64,
    /// Stored log events pruned for being older than the log window (#30).
    pub logs_pruned: u64,
    /// Listener samples pruned for being older than their own window (#62).
    pub samples_pruned: u64,
    /// Objects whose delete failed. The row is already gone, so the Call is
    /// pruned as far as listeners are concerned; the object is now an orphan and
    /// a later sweep retries it. Counted rather than fatal so one unhappy object
    /// can't wedge retention forever.
    pub object_errors: u64,
}

impl SweepReport {
    /// Whether the sweep changed nothing.
    pub fn is_noop(&self) -> bool {
        *self == SweepReport::default()
    }

    /// Total Calls pruned, by either policy.
    pub fn calls_pruned(&self) -> u64 {
        self.aged_out + self.over_cap
    }
}

/// Why a sweep stopped early.
#[derive(Debug)]
pub enum SweepError {
    /// The metadata database refused a read or a delete.
    Db(DbErr),
    /// The object store refused to list. (Failures deleting an *individual*
    /// object aren't fatal — see [`SweepReport::object_errors`].)
    Store(ObjectError),
}

impl std::fmt::Display for SweepError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SweepError::Db(err) => write!(f, "database: {err}"),
            SweepError::Store(err) => write!(f, "object store: {err}"),
        }
    }
}

impl std::error::Error for SweepError {}

impl From<DbErr> for SweepError {
    fn from(err: DbErr) -> Self {
        SweepError::Db(err)
    }
}

impl From<ObjectError> for SweepError {
    fn from(err: ObjectError) -> Self {
        SweepError::Store(err)
    }
}

/// Run one retention pass at `now_ms`: age out, then enforce the size cap, then
/// reclaim orphans. Age first so the cap only has to deal with what a legitimate
/// retention window left behind.
pub async fn sweep(
    db: &Db,
    store: &dyn AudioStore,
    config: &RetentionConfig,
    now_ms: i64,
) -> Result<SweepReport, SweepError> {
    let mut report = SweepReport::default();

    if let Some(pass) = config.age_pass(now_ms) {
        loop {
            let batch = repo::calls_older_than(db, &pass, config.batch_size).await?;
            if batch.is_empty() {
                break;
            }
            report.aged_out += batch.len() as u64;
            prune_batch(db, store, &batch, &mut report).await?;
        }
    }

    // **The size cap outranks a Star** (#66): [`repo::oldest_calls`] does not
    // ask whether anybody starred anything, because a cap a Listener can defeat
    // is not a cap — and this is the one policy that can be defeated by
    // somebody holding no credential at all. The age pass above is where a Star
    // is honoured; the cap is the disk, and the disk is finite whatever anybody
    // meant to keep.
    //
    // **An Event's frozen copies are counted and never taken** (#67). They are
    // stored audio on the same disk as everything else, so a cap blind to them
    // would stop being true the moment an Operator curated an incident — and
    // this setting exists to stop an SD card filling. They cannot be *pruned*,
    // because being outside every policy is the whole of what freezing bought;
    // so the cap eats the Archive around them and, if that is not enough, stops
    // and says so below. That is the visible failure rather than the quiet one.
    //
    // Read **once**, outside the loop: pruning Calls cannot change it, and an
    // Operator curating an Event mid-sweep is a difference the next sweep picks
    // up — where re-reading would be a statement per batch, forever, to track a
    // number that almost never moves.
    if let Some(cap) = config.max_size_bytes {
        let frozen = repo::frozen_audio_bytes(db).await?;
        let mut total = repo::total_audio_bytes(db).await?.saturating_add(frozen);
        while total > cap {
            let page = repo::oldest_calls(db, config.batch_size).await?;
            if page.is_empty() {
                break;
            }
            // Take only as many of the oldest as it takes to fit under the cap.
            let mut projected = total;
            let mut batch = Vec::new();
            for call in page {
                projected = projected.saturating_sub(call.audio_size as u64);
                batch.push(call);
                if projected <= cap {
                    break;
                }
            }
            report.over_cap += batch.len() as u64;
            prune_batch(db, store, &batch, &mut report).await?;
            // Re-read rather than trusting the projection: the DB is the
            // authority, and one aggregate per batch is cheap next to the
            // deletes. It also guarantees progress — every round deletes at
            // least one row, so the loop terminates even if sizes are missing.
            total = repo::total_audio_bytes(db).await?.saturating_add(frozen);
        }
        if total > cap {
            // The Archive is empty (or unsized) and the disk is still over the
            // line, which on an Instance with Events means the Events are the
            // reason. **WARN and once per sweep** — an Operator must act, and
            // the only actions are deleting an Event or raising the cap, so the
            // line carries the number that decides which (ADR-0011 rules 7, 8).
            warn!(
                cap_bytes = cap,
                total_bytes = total,
                frozen_bytes = frozen,
                "archive is over its size cap and nothing more can be pruned"
            );
        }
    }

    // The operator log surface (#30). Its own window, and no object store to
    // reconcile with — a log event is only ever a row.
    if let Some(cutoff_ms) = config.log_cutoff_ms(now_ms) {
        loop {
            let pruned = repo::delete_logs_older_than(db, cutoff_ms, config.batch_size).await?;
            if pruned == 0 {
                break;
            }
            report.logs_pruned += pruned;
        }
    }

    // Listener counts (#62), on the same terms as the logs above: its own
    // window, a row and never an object.
    if let Some(cutoff_ms) = config.listener_cutoff_ms(now_ms) {
        loop {
            let pruned =
                repo::delete_listener_samples_older_than(db, cutoff_ms, config.batch_size).await?;
            if pruned == 0 {
                break;
            }
            report.samples_pruned += pruned;
        }
    }

    // Orphan-GC last, so it also picks up any object this sweep's own
    // row-then-object deletes failed to remove. It lists the whole store, which
    // is why it rides the retention interval rather than running more often.
    let referenced = repo::referenced_object_keys(db).await?;
    let written_before_ms = now_ms.saturating_sub(config.orphan_grace.as_millis() as i64);
    let gc = blob::orphan_gc(store, &referenced, written_before_ms).await?;
    report.orphans += gc.reclaimed.len() as u64;
    report.bytes_freed += gc.bytes();
    report.object_errors += gc.errors;

    Ok(report)
}

/// What this Worker is called on a status surface (#93).
pub const WORKER: &str = "retention";

/// The retention sweeper, before it is running.
///
/// Everything one sweep needs, in a value [`Sweeper::start`] **consumes** — so
/// a second sweeper racing the first over the same archive is a compile error
/// rather than a thing to remember not to do. That is what "double-spawn is
/// structurally impossible" means for a worker whose owner is not `Clone`;
/// [`crate::enhance`] and [`crate::downstream`] live behind a `Clone` surface and buy
/// the same guarantee at runtime instead.
pub struct Sweeper {
    db: Db,
    store: Arc<dyn AudioStore>,
    config: RetentionConfig,
    clock: Clock,
    meter: Arc<Meter>,
}

impl Sweeper {
    /// A sweeper for this archive under this policy.
    pub fn new(db: Db, store: Arc<dyn AudioStore>, config: RetentionConfig, clock: Clock) -> Self {
        Sweeper {
            db,
            store,
            config,
            clock,
            meter: Meter::new(),
        }
    }

    /// Run [`sweep`] on `config.interval` until the Worker is stopped, starting
    /// **immediately** rather than one interval in — rdio-scanner's hourly
    /// ticker first fires at +1h, so an instance that restarts more often than
    /// that never prunes at all.
    ///
    /// Its depth is `1` while a sweep is running and `0` between sweeps, which
    /// is the health reading a status surface wants from a worker with no
    /// queue: a depth stuck at `1` is a sweep that never ended. Its settled
    /// count is the number of sweeps, which is the only way from outside to
    /// tell the scheduler from its first tick.
    pub fn start(self) -> Worker {
        let Sweeper {
            db,
            store,
            config,
            clock,
            meter,
        } = self;
        let period = config.effective_interval();
        // The boot sweep is owed from the moment this returns — not from
        // whenever the runtime first polls the task. Admitted out here, an
        // Instance can never be *observed* idle in the window before its first
        // sweep has begun, which is the whole worth of the signal.
        let mut boot = Some(meter.admit());

        Worker::start(WORKER, meter.clone(), move |mut stop| async move {
            let mut ticker = tokio::time::interval(period);
            // A sweep that overruns its interval must not then run back-to-back.
            ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
            loop {
                tokio::select! {
                    // Cancellation first, always: a stop asked for while a tick
                    // is also ready must not lose a coin toss, or "stopping is
                    // prompt" would be true only most of the time.
                    biased;
                    _ = stop.cancelled() => break,
                    _ = ticker.tick() => {} // the first tick completes immediately
                }
                // Dropped however this iteration ends — finished, or cancelled
                // part-way through — so a stopped sweeper never leaves a depth
                // behind claiming it is still working.
                let _sweeping = boot.take().unwrap_or_else(|| meter.admit());
                tokio::select! {
                    biased;
                    // Cancelled mid-sweep is what `abort()` did before this, and
                    // is safe for the same reason: the deletes are transactional
                    // per batch, so the archive is consistent wherever it stops.
                    _ = stop.cancelled() => break,
                    outcome = sweep(&db, store.as_ref(), &config, clock.now_ms()) => log_sweep(&outcome),
                }
            }
        })
    }
}

/// Say what one sweep did, at the level it deserves (ADR-0011 rule 7).
///
/// An archive already inside its policy is the steady state — the common case,
/// every hour, forever — so it is DEBUG; an hourly INFO line about nothing is
/// how a log stops being read. A sweep that pruned is a notable normal event. A
/// sweep that *failed* is an ERROR: until it succeeds the archive is unbounded,
/// which on a Pi ends with a full SD card.
///
/// Split from [`spawn`]'s loop so every branch is assertable, the way `live.rs`
/// factors its decision logic out of the socket I/O.
fn log_sweep(outcome: &Result<SweepReport, SweepError>) {
    match outcome {
        Ok(report) if report.is_noop() => debug!("retention sweep found nothing to prune"),
        Ok(report) => {
            let calls_pruned = report.calls_pruned();
            info!(
                calls_pruned,
                aged_out = report.aged_out,
                over_cap = report.over_cap,
                orphans = report.orphans,
                bytes_freed = report.bytes_freed,
                logs_pruned = report.logs_pruned,
                samples_pruned = report.samples_pruned,
                // Not zero means audio is still on disk that nothing points at;
                // a later sweep retries it, but an operator should know.
                object_errors = report.object_errors,
                "retention sweep pruned calls"
            );
        }
        Err(error) => error!(%error, "retention sweep failed"),
    }
}

/// Delete one batch of Calls: **rows first (transactionally), then objects**
/// (ADR-0002). That ordering means a crash mid-batch leaves orphaned audio —
/// harmless, and reclaimed by the GC pass — rather than rows whose audio 404s.
async fn prune_batch(
    db: &Db,
    store: &dyn AudioStore,
    batch: &[PrunableCall],
    report: &mut SweepReport,
) -> Result<(), SweepError> {
    let pruned = prune(db, store, batch).await?;
    report.bytes_freed += pruned.bytes_freed;
    report.object_errors += pruned.object_errors;
    Ok(())
}

/// What one [`prune`] reclaimed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Pruned {
    pub bytes_freed: u64,
    /// Objects whose delete failed. The row is already gone, so orphan-GC picks
    /// them up on a later sweep.
    pub object_errors: u64,
}

/// Delete one batch of Calls: **rows first, then their audio objects.**
///
/// The order is ADR-0002's, inverted from the write: an object with no row is an
/// **Orphan** the GC pass reclaims, where a row with no object is a Call that
/// plays as a 404 for as long as it survives retention. A failed object delete is
/// therefore counted, never fatal — one unhappy object must not wedge a sweep
/// forever.
///
/// `pub(crate)` because curation's force-delete (#49) removes Calls too, and a
/// second copy of this is how audio gets stranded in a bucket. One pass, two
/// callers — the rule #46 applies to a Call's columns, applied to its removal.
pub(crate) async fn prune(
    db: &Db,
    store: &dyn AudioStore,
    batch: &[PrunableCall],
) -> Result<Pruned, SweepError> {
    let mut pruned = Pruned::default();
    let ids: Vec<CallId> = batch.iter().map(|call| call.id).collect();
    let txn = db.begin().await?;
    repo::delete_calls(&txn, &ids).await?;
    txn.commit().await?;

    for call in batch {
        // An encrypted Call is a row with no object behind it (#42, spec US 9).
        // Asking the store to delete the empty key would fail and be reported
        // as a broken object store — once per Call, on a System where they may
        // be most of the traffic.
        if call.object_key.is_empty() {
            continue;
        }
        match store.delete(&call.object_key).await {
            Ok(()) => pruned.bytes_freed += call.audio_size as u64,
            Err(error) => {
                // Say *why*, or the operator gets a count and no lead. The row is
                // already gone, so the object is an orphan the GC pass retries.
                warn!(
                    object_key = %call.object_key,
                    %error,
                    "could not delete pruned audio object"
                );
                pruned.object_errors += 1;
            }
        }
    }
    Ok(pruned)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::BlobStore;
    use crate::db;
    use crate::db::entities::call;
    use crate::now_ms;
    use crate::testing::LogCapture;
    use proptest::prelude::*;
    use rstest::rstest;
    use sea_orm::{EntityTrait, PaginatorTrait};

    /// A round wall-clock instant to anchor the worked cutoffs below.
    const NOW: i64 = 1_000_000_000_000;

    /// A DB + blob store in a temp dir, with nothing stored yet.
    async fn empty_archive() -> (Db, Arc<BlobStore>, tempfile::TempDir) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let db = db::connect(&crate::testing::sqlite_url(&tmp))
            .await
            .expect("db");
        let store = Arc::new(BlobStore::filesystem(tmp.path().join("audio")).expect("blob"));
        (db, store, tmp)
    }

    /// An archive holding one Call old enough that any sweep with a non-zero
    /// retention window prunes it.
    async fn archive_with_one_stale_call() -> (Db, Arc<BlobStore>, tempfile::TempDir) {
        let (db, store, tmp) = empty_archive().await;
        add_stale_call(&db, &store, 1).await;
        (db, store, tmp)
    }

    /// Store one Call dated the epoch (so it is always past any retention
    /// window), with its audio object, exactly as ingest would.
    async fn add_stale_call(db: &Db, store: &BlobStore, talkgroup_ref: i64) {
        let object_key = format!("aa/{talkgroup_ref}.wav");
        store
            .put(&object_key, bytes::Bytes::from_static(b"xxxxxxxx"))
            .await
            .expect("put");
        repo::insert_call(
            db,
            &repo::NewCall::new(11, talkgroup_ref, 0),
            Some(crate::blob::StoredAudio::written(object_key, 8)),
            &repo::Resolved::default(),
            true,
            0,
        )
        .await
        .expect("insert");
    }

    /// How long a scheduler test waits for a sweep's effect. Generous — it only
    /// ever runs out when something is actually wrong.
    const WAIT: Duration = Duration::from_secs(5);

    /// What a **Star** holds back is one reading of two settings, and it is the
    /// whole of the operator policy #66 asks for.
    ///
    /// Three states, and the third is the one an `Option<i64>` would have got
    /// wrong: *not kept*, *kept a while longer*, and *kept for good* — where
    /// "for good" is spelled `0`, because that is what every other window in
    /// this section already means by it.
    #[rstest]
    #[case::a_star_is_not_a_reprieve_unless_asked_for(None, StarKeep::None)]
    #[case::a_star_buys_a_quarter(Some(90), StarKeep::Until(NOW - 90 * MS_PER_DAY))]
    #[case::a_star_buys_forever(Some(0), StarKeep::Forever)]
    fn what_a_star_holds_a_call_back_from(
        #[case] starred_days: Option<u32>,
        #[case] expected: StarKeep,
    ) {
        let config = RetentionConfig {
            days: 7,
            starred_days,
            ..Default::default()
        };

        let pass = config.age_pass(NOW).expect("an age pass");

        assert_eq!(pass.cutoff_ms, NOW - 7 * MS_PER_DAY);
        assert_eq!(pass.stars, expected);
    }

    /// `days = 0` is rdio's "keep forever", and it outranks the Star window
    /// rather than combining with it: there is no age pass to be spared from.
    /// Which is also what stops a `starred_days` shorter than an unset `days`
    /// from *shortening* a starred Call's life.
    #[rstest]
    #[case::stars_unset(None)]
    #[case::stars_kept_a_week(Some(7))]
    fn keeping_everything_forever_leaves_no_age_pass_to_spare(#[case] starred_days: Option<u32>) {
        let config = RetentionConfig {
            days: 0,
            starred_days,
            ..Default::default()
        };

        assert_eq!(config.age_pass(NOW), None);
    }

    /// rdio-scanner's prune ticker first fires an *hour* after start, so an
    /// instance that restarts more often than that never prunes at all. Ours
    /// sweeps on startup: with an hour-long interval, a prompt prune can only
    /// have come from the startup pass.
    #[tokio::test]
    async fn scheduler_sweeps_at_startup_not_one_interval_in() {
        let (db, store, _tmp) = archive_with_one_stale_call().await;
        let config = RetentionConfig {
            days: 7,
            interval: Duration::from_secs(3600),
            ..Default::default()
        };

        let worker = Sweeper::new(db.clone(), store.clone(), config, Clock::system()).start();
        // The boot sweep is owed before `start` returns, so this is a wait for
        // that sweep in particular and not for whatever ran first.
        worker.idle().await;
        let load = worker.load();
        worker.stop().await;

        assert_eq!(
            call::Entity::find().count(&db).await.unwrap(),
            0,
            "the startup sweep should have pruned the stale call"
        );
        // Exactly one, an hour before the second is due: so the prune above can
        // only have come from the startup pass.
        assert_eq!(
            load,
            crate::worker::Load { depth: 0, done: 1 },
            "the archive was emptied, but not by the boot sweep alone"
        );
    }

    /// And it keeps going: a Call that ages out *after* startup is pruned by a
    /// later tick, not left until the next restart.
    #[tokio::test]
    async fn scheduler_keeps_sweeping_on_its_interval() {
        // Start from an archive the startup sweep has nothing to do with, so
        // only a subsequent tick can explain the prune below.
        let (db, store, _tmp) = empty_archive().await;
        let config = RetentionConfig {
            days: 7,
            interval: Duration::from_millis(50),
            ..Default::default()
        };

        let worker = Sweeper::new(db.clone(), store.clone(), config, Clock::system()).start();
        worker.idle().await;
        add_stale_call(&db, &store, 2).await;
        // Counted *after* the insert, never before. Two more sweeps from here:
        // the second cannot start until the first has finished, and the first
        // finishes after this reading — so the second provably began with the
        // row already in the archive.
        let seeded_at = worker.load().done;
        worker.settled_at_least(seeded_at + 2).await;
        worker.stop().await;

        assert_eq!(
            call::Entity::find().count(&db).await.unwrap(),
            0,
            "a later tick should have pruned the new stale call"
        );
    }

    /// A zero interval would panic `tokio::time::interval`. An operator who
    /// mis-configures it gets the default cadence — and still gets the startup
    /// sweep — rather than a dead server.
    #[tokio::test]
    async fn scheduler_survives_a_zero_interval() {
        let (db, store, _tmp) = archive_with_one_stale_call().await;
        let config = RetentionConfig {
            days: 7,
            interval: Duration::ZERO,
            ..Default::default()
        };

        let worker = Sweeper::new(db.clone(), store.clone(), config, Clock::system()).start();
        worker.idle().await;
        // Read before stopping, because stopping is what makes it stop.
        let alive = worker.is_running();
        worker.stop().await;

        assert_eq!(
            call::Entity::find().count(&db).await.unwrap(),
            0,
            "a zero interval should still sweep at startup"
        );
        // A ticker built on `Duration::ZERO` panics; this one fell back to the
        // hour-long default, so the loop is still there waiting on it. Not a
        // second sweep — an hour is not a thing to wait for.
        assert!(alive, "the task must not have panicked");
    }

    /// A sweep that fails — here, the database going away underneath it — is
    /// logged and retried on the next tick, never fatal. Retention falling over
    /// must not take the scanner with it.
    #[tokio::test]
    async fn scheduler_survives_a_failing_sweep() {
        let (db, store, _tmp) = archive_with_one_stale_call().await;
        let config = RetentionConfig {
            days: 7,
            interval: Duration::from_millis(50),
            ..Default::default()
        };

        let probe_store = store.clone();
        let probe_config = config.clone();
        let probe_db = db.clone();
        let worker = Sweeper::new(db.clone(), store, config, Clock::system()).start();
        worker.idle().await;
        assert_eq!(
            call::Entity::find().count(&db).await.unwrap(),
            0,
            "the startup sweep should have run"
        );

        // Pull the database out from under the ticker, and confirm sweeps really
        // do fail now — otherwise the rest of this proves nothing.
        db.close().await.expect("close");
        assert!(
            sweep(&probe_db, probe_store.as_ref(), &probe_config, now_ms())
                .await
                .is_err(),
            "a closed pool should make sweeps fail"
        );

        // Several more sweeps from here, so every one of them lands on the
        // broken database. A worker that died on the first would never reach
        // the count, so this is the assertion — no sleep long enough to feel
        // safe.
        let broken_at = worker.load().done;
        worker.settled_at_least(broken_at + 3).await;
        worker.stop().await;
    }

    /// `batch_size = 0` pages forever without ever deleting anything. The sweep
    /// bails out rather than spinning — a mis-typed config must not hang a
    /// background task on someone's Pi.
    #[tokio::test]
    async fn a_zero_batch_size_terminates_instead_of_spinning() {
        let (db, store, _tmp) = archive_with_one_stale_call().await;
        let config = RetentionConfig {
            days: 0,
            max_size_bytes: Some(0),
            batch_size: 0,
            ..Default::default()
        };

        let report = tokio::time::timeout(WAIT, sweep(&db, store.as_ref(), &config, now_ms()))
            .await
            .expect("sweep should terminate")
            .unwrap();

        assert_eq!(report.over_cap, 0);
        assert_eq!(call::Entity::find().count(&db).await.unwrap(), 1);
    }

    /// `is_noop` is what keeps the scheduler quiet on an archive already within
    /// policy. Every field has to break the silence — a sweep that only *failed*
    /// to delete objects especially, or an operator never learns their disk isn't
    /// being reclaimed.
    #[rstest]
    #[case(SweepReport::default(), true)]
    #[case(SweepReport { aged_out: 1, ..SweepReport::default() }, false)]
    #[case(SweepReport { over_cap: 1, ..SweepReport::default() }, false)]
    #[case(SweepReport { orphans: 1, ..SweepReport::default() }, false)]
    #[case(SweepReport { bytes_freed: 1, ..SweepReport::default() }, false)]
    #[case(SweepReport { object_errors: 1, ..SweepReport::default() }, false)]
    #[case(SweepReport { logs_pruned: 1, ..SweepReport::default() }, false)]
    fn only_an_empty_report_is_a_noop(#[case] report: SweepReport, #[case] expected: bool) {
        assert_eq!(report.is_noop(), expected);
    }

    /// Store `count` log events dated `at_ms`, as the sink would have.
    async fn add_log_events(db: &Db, at_ms: i64, count: usize) {
        let events: Vec<repo::NewLogEvent> = (0..count)
            .map(|n| repo::NewLogEvent {
                at_ms,
                level: "INFO".into(),
                target: "radio_scout::ingest".into(),
                message: format!("event {n}"),
                ..Default::default()
            })
            .collect();
        repo::insert_log_events(db, &events)
            .await
            .expect("store log events");
    }

    /// How many log events are stored.
    async fn stored_log_count(db: &Db) -> u64 {
        crate::db::entities::log_event::Entity::find()
            .count(db)
            .await
            .expect("count log events")
    }

    /// The operator log surface is bounded by the same sweeper the archive is
    /// (#30) — an unattended Pi must not fill its SD card with its own logging.
    /// The window is the logs' own, so it can outlive the audio it describes.
    #[tokio::test]
    async fn stored_logs_are_pruned_by_their_own_age_window() {
        let (db, store, _tmp) = empty_archive().await;
        let now = NOW;
        // One event a fortnight old, one from an hour ago.
        add_log_events(&db, now - 14 * MS_PER_DAY, 1).await;
        add_log_events(&db, now - 3_600_000, 1).await;
        let config = RetentionConfig {
            days: 0,
            log_days: 7,
            ..Default::default()
        };

        let report = sweep(&db, store.as_ref(), &config, now)
            .await
            .expect("sweep");

        assert_eq!(report.logs_pruned, 1);
        assert_eq!(stored_log_count(&db).await, 1, "the recent event survives");
    }

    /// `log_days = 0` is "keep forever", the reading `days` already has — so an
    /// operator who wants an unbounded log has a way to say so, and nobody
    /// reads a zero as "drop everything".
    #[tokio::test]
    async fn a_zero_log_window_keeps_every_event() {
        let (db, store, _tmp) = empty_archive().await;
        add_log_events(&db, 0, 3).await;
        let config = RetentionConfig {
            log_days: 0,
            ..Default::default()
        };

        let report = sweep(&db, store.as_ref(), &config, NOW)
            .await
            .expect("sweep");

        assert_eq!(report.logs_pruned, 0);
        assert_eq!(stored_log_count(&db).await, 3);
    }

    /// `listener_days = 0` is "keep forever" too — the third window, and the
    /// third time the same reading has to hold, because an Operator who wants
    /// an unbounded chart says so the same way everywhere.
    #[tokio::test]
    async fn a_zero_listener_window_keeps_every_sample() {
        use crate::db::entities::listener_sample;
        use sea_orm::{EntityTrait, PaginatorTrait, Set};

        let (db, store, _tmp) = empty_archive().await;
        listener_sample::Entity::insert(listener_sample::ActiveModel {
            at_ms: Set(0),
            listeners: Set(4),
            ..Default::default()
        })
        .exec(&db)
        .await
        .expect("a sample");
        let config = RetentionConfig {
            listener_days: 0,
            ..Default::default()
        };

        let report = sweep(&db, store.as_ref(), &config, NOW)
            .await
            .expect("sweep");

        assert_eq!(report.samples_pruned, 0);
        assert_eq!(
            listener_sample::Entity::find()
                .count(&db)
                .await
                .expect("count samples"),
            1
        );
    }

    /// Pruning is batched like the archive's, so a Pi that has been logging for
    /// a month never holds one write lock for the whole delete.
    #[tokio::test]
    async fn a_long_backlog_of_logs_is_pruned_in_batches() {
        let (db, store, _tmp) = empty_archive().await;
        add_log_events(&db, 0, 7).await;
        let config = RetentionConfig {
            days: 0,
            log_days: 1,
            batch_size: 2,
            ..Default::default()
        };

        let report = tokio::time::timeout(WAIT, sweep(&db, store.as_ref(), &config, NOW))
            .await
            .expect("the sweep terminates")
            .expect("sweep");

        assert_eq!(report.logs_pruned, 7);
        assert_eq!(stored_log_count(&db).await, 0);
    }

    /// An archive already within policy is the steady state — every hour, for
    /// the life of the process. It says so at DEBUG, or a Pi's log fills with
    /// hourly notices that nothing happened.
    #[test]
    fn a_sweep_that_pruned_nothing_stays_out_of_the_way() {
        let capture = LogCapture::start();
        log_sweep(&Ok(SweepReport::default()));

        let logged = capture.text();
        assert!(logged.contains("DEBUG"), "{logged}");
        assert!(!logged.contains("INFO"), "{logged}");
    }

    /// A sweep that pruned reports what it did as fields, not a sentence — every
    /// number an operator wondering where their disk went would ask for.
    #[test]
    fn a_sweep_that_pruned_reports_every_number_at_info() {
        let capture = LogCapture::start();
        log_sweep(&Ok(SweepReport {
            aged_out: 2,
            over_cap: 1,
            orphans: 3,
            bytes_freed: 40,
            object_errors: 2,
            logs_pruned: 9,
            samples_pruned: 4,
        }));

        let logged = capture.text();
        assert!(logged.contains("INFO"), "{logged}");
        for field in [
            "calls_pruned=3",
            "aged_out=2",
            "over_cap=1",
            "orphans=3",
            "bytes_freed=40",
            "object_errors=2",
            "logs_pruned=9",
            "samples_pruned=4",
        ] {
            assert!(logged.contains(field), "{field} missing from:\n{logged}");
        }
    }

    /// A failed sweep is an ERROR — until it succeeds the archive is unbounded —
    /// and it names which half of the system was unhappy.
    #[test]
    fn a_failed_sweep_is_an_error_naming_the_cause() {
        let capture = LogCapture::start();
        log_sweep(&Err(DbErr::Custom("boom".into()).into()));

        let logged = capture.text();
        assert!(logged.contains("ERROR"), "{logged}");
        assert!(logged.contains("database: Custom Error: boom"), "{logged}");
    }

    /// An object that won't delete leaves audio on a disk retention is supposed
    /// to be reclaiming. A count alone gives an operator no lead, so both the
    /// prune and the GC pass name the key and the cause.
    #[cfg(unix)]
    #[tokio::test]
    async fn an_undeletable_object_names_the_key_and_the_cause() {
        use std::os::unix::fs::PermissionsExt;

        let (db, store, tmp) = archive_with_one_stale_call().await;
        let shard = tmp.path().join("audio").join("aa");
        let restore = std::fs::metadata(&shard).expect("metadata").permissions();
        let mut readonly = restore.clone();
        readonly.set_mode(0o555);
        std::fs::set_permissions(&shard, readonly).expect("chmod");
        // Root bypasses directory permissions, so there'd be nothing to observe.
        if std::fs::write(shard.join("probe"), b"x").is_ok() {
            std::fs::set_permissions(&shard, restore).expect("restore");
            return;
        }

        let config = RetentionConfig {
            days: 7,
            orphan_grace: Duration::ZERO,
            ..Default::default()
        };
        let capture = LogCapture::start();
        let report = sweep(&db, store.as_ref(), &config, now_ms() + 60_000).await;
        std::fs::set_permissions(&shard, restore).expect("restore");

        assert!(
            report
                .expect("a stuck object must not fail the sweep")
                .object_errors
                > 0
        );
        let logged = capture.text();
        assert!(logged.contains("WARN"), "{logged}");
        // The prune's own delete, then the GC pass finding the same object.
        assert!(
            logged.contains("could not delete pruned audio object"),
            "{logged}"
        );
        assert!(
            logged.contains("orphan-gc could not delete object"),
            "{logged}"
        );
        assert!(logged.contains("aa/1.wav"), "{logged}");
    }

    /// The startup line has to make "what will be deleted" unambiguous — an
    /// operator who reads `days=0` should never find Calls missing, and one who
    /// reads `days=7` should never be surprised that they are.
    #[rstest]
    #[case(7, None, 3600, &["days=7", "max_size_bytes=None", "interval_secs=3600", "log_days=30"])]
    // `days=0` is rdio's "keep forever", not "drop everything".
    #[case(0, None, 3600, &["days=0", "max_size_bytes=None"])]
    #[case(0, Some(1024), 60, &["days=0", "max_size_bytes=Some(1024)", "interval_secs=60"])]
    #[case(30, Some(1024), 60, &["days=30", "max_size_bytes=Some(1024)"])]
    // A zero interval reports the cadence that will actually be used.
    #[case(7, None, 0, &["days=7", "interval_secs=3600"])]
    fn policy_is_logged_at_startup(
        #[case] days: u32,
        #[case] max_size_bytes: Option<u64>,
        #[case] interval_secs: u64,
        #[case] expected: &[&str],
    ) {
        let config = RetentionConfig {
            days,
            max_size_bytes,
            interval: Duration::from_secs(interval_secs),
            ..Default::default()
        };

        let capture = LogCapture::start();
        config.log();

        let logged = capture.text();
        assert!(logged.contains("INFO"), "{logged}");
        for field in expected {
            assert!(logged.contains(field), "{field} missing from:\n{logged}");
        }
    }

    /// The operator reading a failed-sweep line needs to know which half of the
    /// system is unhappy — the database or the object store.
    #[test]
    fn sweep_errors_name_the_subsystem_that_failed() {
        let db: SweepError = DbErr::Custom("boom".into()).into();
        assert_eq!(db.to_string(), "database: Custom Error: boom");

        let store: SweepError = ObjectError::NotImplemented {
            operation: "list".into(),
            implementer: "TestStore".into(),
        }
        .into();
        assert!(store.to_string().starts_with("object store: "), "{store}");
    }

    #[rstest]
    // `days = 0` is "keep forever" (rdio's pruneDays semantics), not "drop all".
    #[case(0, None)]
    // Worked cutoffs: NOW minus 1 / 7 / 30 days of milliseconds.
    #[case(1, Some(999_913_600_000))]
    #[case(7, Some(999_395_200_000))]
    #[case(30, Some(997_408_000_000))]
    fn cutoff_for_days(#[case] days: u32, #[case] expected: Option<i64>) {
        let config = RetentionConfig {
            days,
            ..Default::default()
        };
        assert_eq!(config.cutoff_ms(NOW), expected);
    }

    /// The operator's gigabytes, in the bytes the sweep counts.
    ///
    /// A value that cannot be a cap is **refused**, not quietly disabled: an
    /// absent key already says "no cap", so `max_size_gb = 0` is a typo, and
    /// reading a typo as a policy is rdio-scanner's failure mode
    /// (`server/config.go` falls back to a default on anything it can't parse).
    /// The refusal reaches an operator as a boot error naming the key — through
    /// [`gigabytes`] for the file, and through the settings table for the
    /// environment.
    #[rstest]
    #[case(1.0, Ok(1_073_741_824))]
    #[case(0.5, Ok(536_870_912))]
    #[case(2.5, Ok(2_684_354_560))]
    #[case(0.0, Err(EXPECTED_MAX_SIZE_GB))]
    #[case(-1.0, Err(EXPECTED_MAX_SIZE_GB))]
    #[case(f64::NAN, Err(EXPECTED_MAX_SIZE_GB))]
    #[case(f64::INFINITY, Err(EXPECTED_MAX_SIZE_GB))]
    fn size_cap_from_gigabytes(#[case] gb: f64, #[case] expected: Result<u64, &'static str>) {
        assert_eq!(RetentionConfig::max_size_bytes_from_gb(gb), expected);
    }

    proptest! {
        /// However absurd the configuration, a cutoff never panics and never
        /// lands in the future — pruning can't eat Calls that haven't aged out.
        #[test]
        fn cutoff_never_exceeds_now(days in 0u32..u32::MAX, now_ms in i64::MIN..i64::MAX) {
            if let Some(cutoff) = (RetentionConfig { days, ..Default::default() }).cutoff_ms(now_ms) {
                prop_assert!(cutoff <= now_ms, "cutoff {cutoff} > now {now_ms}");
            }
        }
    }
}
