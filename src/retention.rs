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
//! [`sweep`] is one pass; [`spawn`] runs it on an interval.

use std::sync::Arc;
use std::time::Duration;

use object_store::Error as ObjectError;
use sea_orm::{DatabaseConnection, DbErr, TransactionTrait};
use tokio::time::MissedTickBehavior;
use tracing::{debug, error, info, warn};

use crate::blob::{self, BlobStore};
use crate::call::CallId;
use crate::db::repo::{self, PrunableCall};
use crate::now_ms;

/// Milliseconds in a day.
const MS_PER_DAY: i64 = 86_400_000;

/// Bytes in a binary gigabyte (GiB, 2^30) — how `retention.max_size_gb` is read.
const BYTES_PER_GB: f64 = 1_073_741_824.0;

/// Default sweep cadence, matching rdio-scanner's hourly prune ticker.
const DEFAULT_INTERVAL: Duration = Duration::from_secs(3600);

/// Retention policy, as the sweeper works in it. The operator-facing units —
/// days and gigabytes — are [`crate::config::Retention`]'s (#17).
#[derive(Debug, Clone)]
pub struct RetentionConfig {
    /// Prune Calls older than this many days. `0` disables age-based pruning
    /// (rdio-scanner's `pruneDays` semantics).
    pub days: u32,
    /// Optional cap on total stored audio; the oldest Calls are pruned until
    /// the archive fits. `None` means no size cap.
    pub max_size_bytes: Option<u64>,
    /// How often [`spawn`]'s background task runs a [`sweep`]. Zero is read as
    /// "unset" and falls back to the default cadence.
    pub interval: Duration,
    /// Calls deleted per batch. Bounds how long any single write-lock is held.
    pub batch_size: u64,
    /// How long an unreferenced object is left alone before orphan-GC reclaims
    /// it. Must comfortably exceed the ingest window between writing an audio
    /// object and committing its row, or the GC would delete audio out from
    /// under a Call that is mid-ingest.
    pub orphan_grace: Duration,
}

impl Default for RetentionConfig {
    fn default() -> Self {
        RetentionConfig {
            // rdio-scanner's default, and the reason this feature exists: an
            // unbounded archive fills a Pi's SD card.
            days: 7,
            max_size_bytes: None,
            interval: DEFAULT_INTERVAL,
            batch_size: 500,
            orphan_grace: Duration::from_secs(3600),
        }
    }
}

impl RetentionConfig {
    /// The `call_at_ms` below which a Call has aged out, or `None` when
    /// age-based pruning is disabled (`days == 0`).
    pub fn cutoff_ms(&self, now_ms: i64) -> Option<i64> {
        if self.days == 0 {
            return None;
        }
        // `u32::MAX` days is ~3.7e17 ms, comfortably inside i64; only a clock
        // near i64::MIN could underflow, and saturating there is still "nothing
        // is old enough", which is the safe direction.
        Some(now_ms.saturating_sub(i64::from(self.days) * MS_PER_DAY))
    }

    /// The cadence [`spawn`] will actually sweep on. A zero interval is read as
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
            interval_secs,
            batch_size = self.batch_size,
            "retention policy"
        );
    }

    /// Set the size cap from `retention.max_size_gb` (binary GiB). A
    /// non-positive or non-finite value disables the cap.
    pub fn with_max_size_gb(mut self, gb: f64) -> Self {
        self.max_size_bytes = (gb.is_finite() && gb > 0.0).then_some((gb * BYTES_PER_GB) as u64);
        self
    }
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
    db: &DatabaseConnection,
    store: &BlobStore,
    config: &RetentionConfig,
    now_ms: i64,
) -> Result<SweepReport, SweepError> {
    let mut report = SweepReport::default();

    if let Some(cutoff_ms) = config.cutoff_ms(now_ms) {
        loop {
            let batch = repo::calls_older_than(db, cutoff_ms, config.batch_size).await?;
            if batch.is_empty() {
                break;
            }
            report.aged_out += batch.len() as u64;
            prune_batch(db, store, &batch, &mut report).await?;
        }
    }

    if let Some(cap) = config.max_size_bytes {
        let mut total = repo::total_audio_bytes(db).await?;
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
            total = repo::total_audio_bytes(db).await?;
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

/// Run [`sweep`] forever on `config.interval`, starting **immediately** rather
/// than one interval in — rdio-scanner's hourly ticker first fires at +1h, so an
/// instance that restarts more often than that never prunes at all.
///
/// Returns the task handle so a caller (today: tests) can stop it; the binary
/// simply lets it run for the process's life.
pub fn spawn(
    db: DatabaseConnection,
    store: Arc<BlobStore>,
    config: RetentionConfig,
) -> tokio::task::JoinHandle<()> {
    let period = config.effective_interval();
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(period);
        // A sweep that overruns its interval must not then run back-to-back.
        ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
        loop {
            ticker.tick().await; // the first tick completes immediately
            log_sweep(&sweep(&db, &store, &config, now_ms()).await);
        }
    })
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
    db: &DatabaseConnection,
    store: &BlobStore,
    batch: &[PrunableCall],
    report: &mut SweepReport,
) -> Result<(), SweepError> {
    let ids: Vec<CallId> = batch.iter().map(|call| call.id).collect();
    let txn = db.begin().await?;
    repo::delete_calls(&txn, &ids).await?;
    txn.commit().await?;

    for call in batch {
        match store.delete(&call.object_key).await {
            Ok(()) => report.bytes_freed += call.audio_size as u64,
            Err(error) => {
                // Say *why*, or the operator gets a count and no lead. The row is
                // already gone, so the object is an orphan the GC pass retries.
                warn!(
                    object_key = %call.object_key,
                    %error,
                    "could not delete pruned audio object"
                );
                report.object_errors += 1;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;
    use crate::db::entities::call;
    use crate::testing::LogCapture;
    use proptest::prelude::*;
    use rstest::rstest;
    use sea_orm::{EntityTrait, PaginatorTrait};

    /// A round wall-clock instant to anchor the worked cutoffs below.
    const NOW: i64 = 1_000_000_000_000;

    /// A DB + blob store in a temp dir, with nothing stored yet.
    async fn empty_archive() -> (DatabaseConnection, Arc<BlobStore>, tempfile::TempDir) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let db = db::connect(&crate::testing::sqlite_url(&tmp))
            .await
            .expect("db");
        let store = Arc::new(BlobStore::filesystem(tmp.path().join("audio")).expect("blob"));
        (db, store, tmp)
    }

    /// An archive holding one Call old enough that any sweep with a non-zero
    /// retention window prunes it.
    async fn archive_with_one_stale_call() -> (DatabaseConnection, Arc<BlobStore>, tempfile::TempDir)
    {
        let (db, store, tmp) = empty_archive().await;
        add_stale_call(&db, &store, 1).await;
        (db, store, tmp)
    }

    /// Store one Call dated the epoch (so it is always past any retention
    /// window), with its audio object, exactly as ingest would.
    async fn add_stale_call(db: &DatabaseConnection, store: &BlobStore, talkgroup_ref: i64) {
        let object_key = format!("aa/{talkgroup_ref}.wav");
        store
            .put(&object_key, bytes::Bytes::from_static(b"xxxxxxxx"))
            .await
            .expect("put");
        repo::insert_call(
            db,
            &repo::NewCall {
                system_ref: 11,
                talkgroup_ref,
                call_at_ms: 0,
                object_key,
                audio_size: Some(8),
                ..Default::default()
            },
            true,
            0,
        )
        .await
        .expect("insert");
    }

    /// How long a scheduler test waits for a sweep's effect. Generous — it only
    /// ever runs out when something is actually wrong.
    const WAIT: Duration = Duration::from_secs(5);

    /// Wait for the scheduler to empty the archive, panicking with `context` if
    /// it never does. The scheduler is a real background task doing real
    /// database work, so tests wait on its observable effect rather than on a
    /// clock. (A paused tokio clock is no help here: it auto-advances whenever
    /// the runtime idles on sqlx I/O, which trips the pool's own timeout.)
    async fn await_empty_archive(db: &DatabaseConnection, context: &str) {
        tokio::time::timeout(WAIT, async {
            while call::Entity::find().count(db).await.unwrap() > 0 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("{context}"));
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

        let task = spawn(db.clone(), store.clone(), config);
        await_empty_archive(&db, "the startup sweep should have pruned the stale call").await;
        task.abort();
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

        let task = spawn(db.clone(), store.clone(), config);
        tokio::time::sleep(Duration::from_millis(100)).await;
        add_stale_call(&db, &store, 2).await;
        await_empty_archive(&db, "a later tick should have pruned the new stale call").await;
        task.abort();
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

        let task = spawn(db.clone(), store.clone(), config);
        await_empty_archive(&db, "a zero interval should still sweep at startup").await;
        let alive = !task.is_finished();
        task.abort();

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
        let task = spawn(db.clone(), store, config);
        await_empty_archive(&db, "the startup sweep should have run").await;

        // Pull the database out from under the ticker, and confirm sweeps really
        // do fail now — otherwise the rest of this proves nothing.
        db.close().await.expect("close");
        assert!(
            sweep(&probe_db, &probe_store, &probe_config, now_ms())
                .await
                .is_err(),
            "a closed pool should make sweeps fail"
        );

        // Give the ticker several chances to land on the broken database.
        tokio::time::sleep(Duration::from_millis(150)).await;
        let alive = !task.is_finished();
        task.abort();

        assert!(alive, "a failing sweep must not kill the scheduler");
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

        let report = tokio::time::timeout(WAIT, sweep(&db, &store, &config, now_ms()))
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
    fn only_an_empty_report_is_a_noop(#[case] report: SweepReport, #[case] expected: bool) {
        assert_eq!(report.is_noop(), expected);
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
        let report = sweep(&db, &store, &config, now_ms() + 60_000).await;
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
    #[case(7, None, 3600, &["days=7", "max_size_bytes=None", "interval_secs=3600"])]
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

    #[rstest]
    #[case(1.0, Some(1_073_741_824))]
    #[case(0.5, Some(536_870_912))]
    #[case(2.5, Some(2_684_354_560))]
    // Nonsense values disable the cap rather than clamping to something arbitrary.
    #[case(0.0, None)]
    #[case(-1.0, None)]
    #[case(f64::NAN, None)]
    #[case(f64::INFINITY, None)]
    fn size_cap_from_gigabytes(#[case] gb: f64, #[case] expected: Option<u64>) {
        assert_eq!(
            RetentionConfig::default()
                .with_max_size_gb(gb)
                .max_size_bytes,
            expected
        );
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
