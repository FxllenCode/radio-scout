//! **How many people are listening** (#62, spec US 41) — counted now, and
//! written down every so often so it can be charted later.
//!
//! Three things live here: the counter every live-feed connection passes
//! through ([`Listeners`]), the **Worker** that samples it onto a ticker
//! ([`Sampler`]), and the bucketed read the chart is drawn from ([`read`]).
//!
//! # Counts, never identities
//!
//! ADR-0011 rule 5: a Listener's address never appears above DEBUG, and a public
//! Instance must not accumulate a record of who listened and when. This is the
//! feature that would break that if it were built the obvious way, so it is
//! built the other way round — a single integer per tick, with nothing beside
//! it. No session, no address, no user agent, and deliberately **no
//! per-Talkgroup breakdown**: on a quiet channel with one listener, "who was on
//! Fire Dispatch at 3am" is exactly the identity-shaped fact this avoids.
//! `listener_samples` has three columns and there is nothing in it to
//! de-anonymise.
//!
//! rdio-scanner is the counter-example on both halves. It logs every listener's
//! IP and access-code ident at INFO (`client.go:152`) and keeps no history at
//! all, so an Operator there has a permanent record of *who* and no answer to
//! *how many*.
//!
//! # The sample is a peak, not a reading
//!
//! [`Listeners::take_peak`] answers with the most Listeners connected at once
//! since the previous sample — not the count at the instant the ticker fired.
//! The instantaneous reading is cheaper by one atomic and wrong in the one place
//! this feature exists for: a burst shorter than the sampling interval is
//! invisible to it, and a peak chart that silently under-reports its peaks looks
//! exactly like a peak chart that does not.
//!
//! # A bucket with no samples reads zero, and that is honest
//!
//! [`read`] fills the gaps with `0` rather than with an absence, which is the
//! shared [`crate::activity::Series`] shape and is also the truth: a stretch
//! with no samples in it is a stretch where this Instance was not running, and
//! an Instance that is not running has nobody listening to it.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;

use axum::extract::{Query, State};
use sea_orm::{ColumnTrait, ConnectionTrait, DbErr, EntityTrait, QueryFilter, QuerySelect, Set};
use serde::{Deserialize, Serialize};
use tokio::time::MissedTickBehavior;
use tracing::warn;

use crate::activity::{Axis, Series, bucket_expr};
use crate::db::Db;
use crate::db::entities::listener_sample;
use crate::failure::{Failure, Stage};
use crate::worker::{Meter, Worker};
use crate::{AppState, Clock};

/// What this Worker is called on a status surface (#93, #70).
pub const WORKER: &str = "listeners";

/// How long a listener chart covers when the request names no range.
///
/// A day, which is the question an Operator opens this page to ask ("was
/// anybody on last night?"). Longer ranges are a parameter, not a default: the
/// table is dense — one row a minute — so charting the whole retained history
/// by default would read ninety days of rows to draw a picture nobody asked for.
pub const DEFAULT_SPAN_MS: i64 = 24 * 60 * 60 * 1000;

/// How often the count is written down, and whether it is at all — the
/// `[listeners]` section (#87).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "snake_case", deny_unknown_fields)]
pub struct ListenerConfig {
    /// Record at all. **On**, like `[quiet] enabled` and the **Mining** sweep
    /// and unlike `[enhancement] mode`: history cannot be recovered later, so
    /// an Operator who has to find a setting first has already lost whatever
    /// happened before they found it. Off is for the Instance that would rather
    /// keep nothing.
    pub enabled: bool,
    /// How often to write a row. Zero is read as "unset" and falls back to the
    /// default cadence, the way [`crate::retention::RetentionConfig::interval`]
    /// is, rather than panicking a ticker.
    #[serde(rename = "interval_secs", with = "crate::config::secs")]
    pub interval: Duration,
}

/// A minute: fine enough that a chart of a busy evening has shape, coarse
/// enough that a day is 1,440 rows of sixteen bytes.
const DEFAULT_INTERVAL: Duration = Duration::from_secs(60);

impl Default for ListenerConfig {
    fn default() -> Self {
        ListenerConfig {
            enabled: true,
            interval: DEFAULT_INTERVAL,
        }
    }
}

impl ListenerConfig {
    /// The cadence [`Sampler`] will actually sample on.
    pub fn effective_interval(&self) -> Duration {
        if self.interval.is_zero() {
            DEFAULT_INTERVAL
        } else {
            self.interval
        }
    }
}

/// How many Listeners are connected, and how many there have been since anyone
/// last asked.
///
/// Held by [`crate::AppState`] and counted by the live-feed connection itself,
/// so "a Listener" means exactly what CONTEXT.md says a **Feed off** listener is
/// not: an open connection. Switching the feed off closes the socket, so nobody
/// is counted for a radio that is not playing.
///
/// Two counters rather than a lock, because the increment happens on a
/// connection's own path and the read happens once a minute.
#[derive(Clone, Default)]
pub struct Listeners(Arc<Counts>);

#[derive(Default)]
struct Counts {
    current: AtomicI64,
    peak: AtomicI64,
}

impl Listeners {
    /// Count one Listener for as long as the returned guard is held.
    ///
    /// A guard rather than a pair of calls, so every way a connection can end —
    /// hung up, reaped, dropped mid-frame, the task cancelled at shutdown —
    /// leaves the count honest without an arm having to remember. This is
    /// [`crate::worker::Ticket`]'s bargain one layer along.
    pub fn arrive(&self) -> Present {
        let now = self.0.current.fetch_add(1, Ordering::Relaxed) + 1;
        self.0.peak.fetch_max(now, Ordering::Relaxed);
        Present(self.0.clone())
    }

    /// How many are connected right now — what a status page (#70) shows live,
    /// where this module's own chart shows what has been.
    pub fn current(&self) -> i64 {
        self.0.current.load(Ordering::Relaxed)
    }

    /// The most that were connected at once since this was last asked, and
    /// start a fresh window.
    ///
    /// **The new window starts at whoever is still here**, not at zero: a
    /// Listener who arrived an hour ago and has not left is still a Listener,
    /// and a window seeded at zero would report nobody until the next arrival —
    /// which on a quiet evening with one steady listener is every bucket.
    ///
    /// The two steps are in this order on purpose. A connection arriving
    /// between them raises the peak past anything this could re-seed it with,
    /// so its own `fetch_max` wins; one *leaving* between them leaves the seed
    /// at the higher count it was, which is the truth about the window that has
    /// just begun.
    pub fn take_peak(&self) -> i64 {
        let peak = self.0.peak.swap(0, Ordering::Relaxed);
        self.0
            .peak
            .fetch_max(self.0.current.load(Ordering::Relaxed), Ordering::Relaxed);
        peak
    }
}

/// One Listener, counted while this is alive.
pub struct Present(Arc<Counts>);

impl Drop for Present {
    fn drop(&mut self) {
        self.0.current.fetch_sub(1, Ordering::Relaxed);
    }
}

/// The **Worker** that writes the count down on a ticker (#93).
///
/// Its depth is `1` while a sample is being written and `0` between samples —
/// [`crate::retention::Sweeper`]'s reading, and for its reason: a worker with no
/// queue still has a health signal, and a depth stuck at `1` is a write that
/// never came back. Its settled count is the number of samples taken, which is
/// what lets a test wait for the second one rather than sleep for a minute.
///
/// Not `Clone`, and [`Sampler::start`] takes `self` by value: two samplers would
/// write two rows a tick and halve every peak they disagreed about. That is
/// [`crate::worker`]'s "double-spawn is structurally impossible" rule, in the
/// same shape the retention sweeper takes it.
pub struct Sampler {
    db: Db,
    listeners: Listeners,
    config: ListenerConfig,
    clock: Clock,
    meter: Arc<Meter>,
}

impl Sampler {
    /// A sampler for this Instance's Listeners.
    pub fn new(db: Db, listeners: Listeners, config: ListenerConfig, clock: Clock) -> Self {
        Sampler {
            db,
            listeners,
            config,
            clock,
            meter: Meter::new(),
        }
    }

    /// Sample on `config.interval` until the Worker is stopped, or nothing at
    /// all when `[listeners] enabled` is off.
    ///
    /// The first tick is **not** immediate, which is the one place this differs
    /// from the retention sweeper. A sweep at boot is catching up on a policy
    /// that may have been unenforced for hours; a sample at boot is a window
    /// nobody was connected for, and an Instance that restarts often would fill
    /// its chart with zeroes it created itself.
    pub fn start(self) -> Option<Worker> {
        let Sampler {
            db,
            listeners,
            config,
            clock,
            meter,
        } = self;
        if !config.enabled {
            return None;
        }
        let period = config.effective_interval();

        Some(Worker::start(
            WORKER,
            meter.clone(),
            move |mut stop| async move {
                let mut ticker = tokio::time::interval(period);
                ticker.reset();
                // A write that overran its interval must not then run back-to-back,
                // which on a stalled database would queue up a burst of samples all
                // claiming the same window.
                ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
                loop {
                    tokio::select! {
                        // Cancellation first, always: a stop asked for while a tick
                        // is also ready must not lose a coin toss.
                        biased;
                        _ = stop.cancelled() => break,
                        _ = ticker.tick() => {}
                    }
                    let _sampling = meter.admit();
                    let peak = listeners.take_peak();
                    if let Err(error) = record(&db, clock.now_ms(), peak).await {
                        // WARN, not ERROR (ADR-0011 rule 7): a lost sample costs a
                        // gap in a chart an Operator reads occasionally, and
                        // nothing a Listener can hear. Once per failed sample is
                        // once a minute at worst, which rule 8 allows — this is not
                        // a hot loop.
                        warn!(%error, "a listener count could not be recorded");
                    }
                }
            },
        ))
    }
}

/// Write one sample down.
async fn record<C: ConnectionTrait>(db: &C, at_ms: i64, listeners: i64) -> Result<(), DbErr> {
    listener_sample::Entity::insert(listener_sample::ActiveModel {
        at_ms: Set(at_ms),
        listeners: Set(listeners),
        ..Default::default()
    })
    .exec(db)
    .await
    .map(|_| ())
}

/// One bucket of the listener series, as the grouped query answers it.
#[derive(Debug, sea_orm::FromQueryResult)]
struct PeakBucket {
    bucket: i64,
    peak: i64,
}

/// **Peak Listeners, bucket by bucket** — the read behind
/// `GET /api/admin/listeners`.
///
/// `MAX` per bucket rather than an average, because the question is "how many
/// people were on at once" and a mean over a quiet night answers a different
/// one. Every sample is already a peak over its own minute (see
/// [`Listeners::take_peak`]), so a bucket's peak is the peak of peaks, which is
/// the peak.
pub async fn read<C: ConnectionTrait>(db: &C, axis: Axis) -> Result<Series, DbErr> {
    let rows = listener_sample::Entity::find()
        .select_only()
        .column_as(bucket_expr(listener_sample::Column::AtMs, &axis), "bucket")
        .column_as(listener_sample::Column::Listeners.max(), "peak")
        .filter(listener_sample::Column::AtMs.gte(axis.from_ms()))
        .filter(listener_sample::Column::AtMs.lt(axis.to_ms()))
        .group_by(bucket_expr(listener_sample::Column::AtMs, &axis))
        .into_model::<PeakBucket>()
        .all(db)
        .await?;

    Ok(axis.series(rows.into_iter().map(|row| (row.bucket, row.peak))))
}

/// `GET /api/admin/listeners` — the peak-listener chart (#62, spec US 41).
///
/// **Behind the admin session**, unlike every other read surface in this
/// process. The Archive is open because listening is open (ADR-0008); how many
/// people take it up is the Operator's own business, and publishing that to
/// anyone who asks is not a decision a Listener should be able to make on the
/// Operator's behalf.
pub async fn history(
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Series, Failure> {
    let axis = crate::activity::parse_axis(&params, state.clock.now_ms(), DEFAULT_SPAN_MS)?;

    read(&state.db, axis)
        .await
        .map_err(Stage::LoadListenerHistory.failed())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A Listener is counted while connected and uncounted when the guard goes,
    /// however it goes.
    #[test]
    fn a_listener_is_counted_for_as_long_as_it_is_connected() {
        let listeners = Listeners::default();
        assert_eq!(listeners.current(), 0);

        let one = listeners.arrive();
        let two = listeners.arrive();
        assert_eq!(listeners.current(), 2);

        drop(one);
        assert_eq!(listeners.current(), 1);
        drop(two);
        assert_eq!(listeners.current(), 0);
    }

    /// **The whole reason a sample is a peak.** Somebody who arrived and left
    /// between two ticks is still somebody who was listening, and an
    /// instantaneous reading would have no idea they had been there.
    #[test]
    fn a_listener_who_came_and_went_between_samples_is_still_counted() {
        let listeners = Listeners::default();

        let brief = listeners.arrive();
        drop(brief);
        assert_eq!(listeners.current(), 0, "gone by the time the ticker fires");

        assert_eq!(listeners.take_peak(), 1, "but they were here");
        assert_eq!(listeners.take_peak(), 0, "and the next window is its own");
    }

    /// A window starts at whoever is still here, or a steady listener would be
    /// reported once and then never again.
    #[test]
    fn a_listener_who_stays_is_counted_in_every_window() {
        let listeners = Listeners::default();
        let _staying = listeners.arrive();

        assert_eq!(listeners.take_peak(), 1);
        assert_eq!(listeners.take_peak(), 1, "still here, still counted");
        assert_eq!(listeners.take_peak(), 1);
    }

    /// The peak is the high-water mark, not the last reading — three arrivals
    /// and two departures inside one window is a peak of three.
    #[test]
    fn the_peak_is_the_most_that_were_ever_on_at_once() {
        let listeners = Listeners::default();

        let one = listeners.arrive();
        let two = listeners.arrive();
        let three = listeners.arrive();
        drop(one);
        drop(two);

        assert_eq!(listeners.current(), 1);
        assert_eq!(listeners.take_peak(), 3);
        drop(three);
    }

    /// A zero interval is read as "unset" rather than panicking a ticker, which
    /// is `[retention] interval_secs`' reading of the same mistake.
    #[test]
    fn a_zero_interval_falls_back_to_the_default() {
        let config = ListenerConfig {
            interval: Duration::ZERO,
            ..ListenerConfig::default()
        };

        assert_eq!(config.effective_interval(), DEFAULT_INTERVAL);
        assert_eq!(
            ListenerConfig::default().effective_interval(),
            DEFAULT_INTERVAL
        );
    }

    /// Switched off, there is no Worker at all — not one that ticks and writes
    /// nothing, which would still wake a sleeping Pi once a minute forever.
    #[tokio::test]
    async fn a_disabled_sampler_starts_nothing() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let db = crate::db::connect(&crate::testing::sqlite_url(&tmp))
            .await
            .expect("a database");

        let worker = Sampler::new(
            db,
            Listeners::default(),
            ListenerConfig {
                enabled: false,
                ..ListenerConfig::default()
            },
            Clock::frozen(0),
        )
        .start();

        assert!(worker.is_none());
    }

    /// A sample is a row: what a chart is later drawn from, written by the one
    /// path that writes it.
    #[tokio::test]
    async fn a_sample_is_a_row_a_chart_can_be_read_from() {
        use crate::activity::Grain;

        let tmp = tempfile::tempdir().expect("tempdir");
        let db = crate::db::connect(&crate::testing::sqlite_url(&tmp))
            .await
            .expect("a database");

        record(&db, 1_000, 3).await.expect("a sample");
        record(&db, 2_000, 7).await.expect("another");
        record(&db, 2_500, 2).await.expect("and a quieter one");

        let series = read(&db, Axis::over(1_000, 2_999, Grain::Width(1_000)))
            .await
            .expect("a series");

        // The busier of the two samples in the second bucket is the one that
        // shows: the question is how many were on at once, not on average.
        assert_eq!(series.values, vec![3, 7]);
    }
}
