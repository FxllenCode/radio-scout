//! **RF health** (#71, spec US 51) — what a **Recorder** said about the radio
//! conditions behind one Call, folded into the history a chart is drawn from.
//!
//! Trunk Recorder writes four numbers into every call's `.json` that nothing had
//! ever read: `freq_error` (how far off frequency the demodulator had to pull,
//! in Hz), `signal` and `noise` (dBm), and `source_num` (which SDR did it). Its
//! `freqList` carries the decode `error_count` and `spike_count` that
//! [`crate::db::entities::call_frequency`] has stored since #42. Together those
//! are the whole of "a dying dongle announces itself" — and every one of them is
//! discarded the moment **Retention** takes the Call.
//!
//! So this module is two pure functions and the arithmetic between them:
//! [`readings`] turns a Call a Recorder just uploaded into [`Bucket`]s, and
//! [`crate::db::repo::add_frequency_health`] accumulates those into
//! `frequency_health`. Nothing here awaits, reads a clock or touches a database,
//! so every rule about it is a value a test constructs.
//!
//! # Why the reading is per *frequency entry* and not per Call
//!
//! A Call's `freqList` is one entry per stretch the recorder spent on a
//! frequency, and on a multi-frequency conversation those are genuinely
//! different receive conditions. A Call with no entries at all — which is every
//! rdio-dialect upload, and every SDRTrunk one — still yields exactly one
//! reading, at the Call's own frequency and instant, or an Instance fed only by
//! SDRTrunk would chart nothing at all.
//!
//! # Every received copy counts, once
//!
//! A **Copy**. #46 keeps the better of two copies of one transmission, but both
//! were *received*, and which SDR decoded which copy how badly is precisely what
//! US 51 asks to see. So every copy ingest authorized contributes its own
//! readings exactly once — the first as it is stored, a **Replacement** beside
//! the copy it displaced rather than over it, and a copy refused as a duplicate
//! by `ingest::heard_again` — whatever order they arrived in. The chart is a
//! history of receptions, not of Calls.
//!
//! What is *not* counted is a copy that never reached the dedup question — an
//! unauthorized key, a blacklisted or unpopulated channel. Health written for
//! those would be a row no Operator could chase down to anything.

use std::collections::BTreeMap;

use axum::extract::{Query, State};
use sea_orm::{
    ColumnTrait, ConnectionTrait, DbErr, EntityTrait, QueryFilter, QuerySelect, QueryTrait,
};
use serde::Serialize;

use crate::AppState;
use crate::activity::{Axis, Grain, parse_grain};
use crate::db::entities::{frequency_health, system};
use crate::db::repo::NewCall;
use crate::failure::{Failure, Stage};
use crate::query::{Filtered, Params};

/// How wide one row of history is.
///
/// A quarter of an hour. A dying receiver declares itself over hours, so this is
/// far finer than the question needs — and it bounds the table at 96 rows per
/// day per (frequency, SDR) pair, which on a twenty-frequency county is under
/// 60,000 rows a month.
///
/// It is also a **floor on every chart**: a bucket is attributed whole to
/// whichever axis bucket its first instant falls in, so a chart asking for
/// finer bars than this would draw bars that are empty by construction.
/// [`crate::metrics`]' own `GAUGE_TTL` rule — a bound on the *readings*, so no
/// caller can make them finer than they are.
pub const BUCKET_MS: i64 = 15 * 60 * 1000;

/// Which SDR a reading came from when the Recorder did not say.
///
/// A sentinel rather than `NULL`, and the reason is the unique index: both
/// dialects treat two `NULL`s as distinct, so a nullable column would conflict
/// with nothing and every upsert would insert a fresh row — a rollup that grows
/// one row per Call while reading exactly like one that deduplicates.
pub const NO_SDR: i64 = -1;

/// Trunk Recorder's "never measured" value for a dB reading (`DB_UNSET`,
/// `global_structs.h:9`).
///
/// It is **999**, not zero and not absent, so a Call whose signal was never
/// measured writes `"signal": 999` into its `.json` like any other number. An
/// average that believed it would put a county's receive level at several
/// hundred dBm and draw every real reading as a flat line along the bottom —
/// the kind of wrong that looks like a working chart.
const DB_UNSET: i64 = 999;

/// One (frequency, moment) reading taken off a Call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reading {
    pub freq: i64,
    pub at_ms: i64,
    /// How much air time this reading covers.
    pub air_ms: i64,
    pub error_count: i64,
    pub spike_count: i64,
    pub signal_dbm: Option<i64>,
    pub noise_dbm: Option<i64>,
    /// Tuning error in Hz, **signed**: the mean of these is a drift, and a
    /// receiver pulling consistently one way is the thing worth seeing.
    pub drift_hz: Option<i64>,
    pub sdr: i64,
}

/// One quarter-hour of one frequency on one SDR, as it is stored.
///
/// Sums and counts rather than averages, because an average cannot be
/// re-averaged at a wider grain without the count that made it — see the entity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bucket {
    pub freq: i64,
    pub sdr: i64,
    pub bucket_at_ms: i64,
    pub samples: i64,
    pub air_ms: i64,
    pub error_count: i64,
    pub spike_count: i64,
    pub signal_sum: i64,
    pub signal_n: i64,
    pub noise_sum: i64,
    pub noise_n: i64,
    pub drift_sum: i64,
    pub drift_n: i64,
}

impl Bucket {
    /// An empty bucket at one key, for [`fold`] to accumulate into.
    fn at(freq: i64, sdr: i64, bucket_at_ms: i64) -> Self {
        Bucket {
            freq,
            sdr,
            bucket_at_ms,
            samples: 0,
            air_ms: 0,
            error_count: 0,
            spike_count: 0,
            signal_sum: 0,
            signal_n: 0,
            noise_sum: 0,
            noise_n: 0,
            drift_sum: 0,
            drift_n: 0,
        }
    }

    fn take(&mut self, reading: &Reading) {
        self.samples += 1;
        self.air_ms += reading.air_ms;
        self.error_count += reading.error_count;
        self.spike_count += reading.spike_count;
        if let Some(signal) = reading.signal_dbm {
            self.signal_sum += signal;
            self.signal_n += 1;
        }
        if let Some(noise) = reading.noise_dbm {
            self.noise_sum += noise;
            self.noise_n += 1;
        }
        if let Some(drift) = reading.drift_hz {
            self.drift_sum += drift;
            self.drift_n += 1;
        }
    }
}

/// Which bucket an instant belongs to — its **first** instant.
///
/// Euclidean division, so an instant before the epoch floors downward like every
/// other one rather than toward zero. Nothing should send one; a bucket that
/// silently belonged to the wrong quarter-hour is worse than one that is
/// negative.
pub fn bucket_of(at_ms: i64) -> i64 {
    at_ms.div_euclid(BUCKET_MS) * BUCKET_MS
}

/// A dB reading the Recorder actually took, or nothing.
///
/// Zero is a legitimate reading and [`DB_UNSET`] is not one at all.
fn measured(db: Option<i64>) -> Option<i64> {
    db.filter(|value| *value != DB_UNSET)
}

/// Every reading a Call carries.
///
/// One per `freqList` entry, else exactly one for the Call itself — see the
/// module docs. A Call whose Recorder named no frequency at all yields none:
/// there is nothing for the row to be *about*, and a bucket at frequency 0 would
/// draw a phantom channel on every chart.
pub fn readings_of(call: &NewCall) -> Vec<Reading> {
    let signal = measured(call.signal_dbm);
    let noise = measured(call.noise_dbm);
    // `freq_error` has no sentinel — the recorder initialises it to zero, which
    // is also a perfectly ordinary reading, so an absent one is only ever an
    // absent *field*.
    let drift = call.freq_error_hz;
    let sdr = call.source_num.unwrap_or(NO_SDR);

    if call.frequencies.is_empty() {
        return call
            .frequency
            .map(|freq| Reading {
                freq,
                at_ms: call.call_at_ms,
                air_ms: call.duration_ms.unwrap_or(0).max(0),
                error_count: 0,
                spike_count: 0,
                signal_dbm: signal,
                noise_dbm: noise,
                drift_hz: drift,
                sdr,
            })
            .into_iter()
            .collect();
    }

    call.frequencies
        .iter()
        .filter(|entry| entry.freq > 0)
        .map(|entry| Reading {
            freq: entry.freq,
            // The entry's own wall clock where it has one; a recorder that sent
            // positions and no times still belongs somewhere, and the Call's
            // instant is the closest true answer.
            at_ms: entry.at_ms.unwrap_or(call.call_at_ms),
            air_ms: entry.len_ms.unwrap_or(0).max(0),
            error_count: entry.error_count.unwrap_or(0).into(),
            spike_count: entry.spike_count.unwrap_or(0).into(),
            // The rdio dialect carries a level **per frequency**, which is a
            // sharper answer than the Call-wide one Trunk Recorder gives; where
            // there is one it wins.
            signal_dbm: entry.dbm.map(|dbm| dbm.round() as i64).or(signal),
            noise_dbm: noise,
            drift_hz: drift,
            sdr,
        })
        .collect()
}

/// Fold readings into the rows they are stored as.
///
/// Ordered by (frequency, SDR, bucket) rather than by whatever order the
/// readings arrived in, so the write below is deterministic — which is what lets
/// a test assert the rows whole instead of sorting them first, and what keeps
/// two Instances fed the same Calls writing the same thing.
pub fn fold(readings: impl IntoIterator<Item = Reading>) -> Vec<Bucket> {
    let mut buckets: std::collections::BTreeMap<(i64, i64, i64), Bucket> = Default::default();
    for reading in readings {
        let at = bucket_of(reading.at_ms);
        buckets
            .entry((reading.freq, reading.sdr, at))
            .or_insert_with(|| Bucket::at(reading.freq, reading.sdr, at))
            .take(&reading);
    }
    buckets.into_values().collect()
}

/// What one Call adds to the history — [`readings_of`] folded.
pub fn readings(call: &NewCall) -> Vec<Bucket> {
    fold(readings_of(call))
}

// ---------------------------------------------------------------------------
// The chart
// ---------------------------------------------------------------------------

/// The most channels one report may carry.
///
/// A channel is one (frequency, SDR) pair, and each carries five traces — so
/// this is the bound that stops a county with thirty voice frequencies across
/// four dongles asking a Pi to serialize six hundred arrays. Busiest first, and
/// [`HealthReport::omitted`] says how many were left out, because a cap nobody
/// is told about reads as "that is all there is" (#65's rule).
pub const MAX_CHANNELS: usize = 24;

/// How far back a report with no bounds of its own reaches, when the history is
/// empty and there is no extent to discover.
const EMPTY_SPAN_MS: i64 = 24 * 60 * 60 * 1000;

/// **How every frequency has been receiving** — the charts spec US 51 asks for.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HealthReport {
    /// The first instant covered, inclusive.
    pub from_ms: i64,
    /// The first instant past the end, exclusive.
    pub to_ms: i64,
    /// How wide one bucket is — the **resolved** width, never the one that was
    /// asked for, so a client whose grain was widened draws the truth rather
    /// than mislabelled bars ([`crate::activity::Series`]'s rule).
    pub bucket_ms: i64,
    /// How fine this report can ever be, whatever a caller asks: the rollup's
    /// own bucket. On the wire so a client can offer the grains that exist
    /// rather than ones that would draw empty bars.
    pub finest_bucket_ms: i64,
    pub channels: Vec<ChannelHealth>,
    /// Channels left out by [`MAX_CHANNELS`]. Never silently.
    pub omitted: usize,
}

crate::answers_json!(HealthReport);

/// One frequency on one SDR, over the whole axis.
///
/// The **traces are already divided**: the rollup stores sums and counts so that
/// re-bucketing stays exact, and turning those into the numbers a chart plots is
/// arithmetic that belongs in one place rather than in a second language. A
/// bucket with no reading is `null` and never `0` — a chart that draws "we did
/// not measure" as "perfectly quiet" is saying something false, which is the
/// same failure a minimum bar height exists to prevent one screen along (#62).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChannelHealth {
    pub system_ref: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system_label: Option<String>,
    pub freq: i64,
    /// Which SDR, or nothing where the Recorder never said — which is every
    /// Call that arrived in the rdio dialect.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sdr: Option<i64>,
    pub samples: i64,
    pub air_ms: i64,
    pub error_count: i64,
    pub spike_count: i64,
    /// Decode errors per minute of air. The rate rather than the count, because
    /// a count tracks how busy a channel was and the question here is how well
    /// it decoded.
    pub error_rate: Vec<Option<f64>>,
    pub spike_rate: Vec<Option<f64>>,
    pub signal_dbm: Vec<Option<f64>>,
    pub noise_dbm: Vec<Option<f64>>,
    pub drift_hz: Vec<Option<f64>>,
}

/// One row of the grouped read.
#[derive(Debug, sea_orm::FromQueryResult)]
struct HealthRow {
    bucket: i64,
    system_id: i64,
    freq: i64,
    sdr: i64,
    samples: i64,
    air_ms: i64,
    error_count: i64,
    spike_count: i64,
    signal_sum: i64,
    signal_n: i64,
    noise_sum: i64,
    noise_n: i64,
    drift_sum: i64,
    drift_n: i64,
}

/// How far the history reaches, for a report that named no bounds.
#[derive(Debug, sea_orm::FromQueryResult)]
struct Extent {
    first_ms: Option<i64>,
    last_ms: Option<i64>,
}

/// What a report was asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HealthQuery {
    pub after_ms: Option<i64>,
    pub before_ms: Option<i64>,
    pub system_ref: Option<i64>,
    pub freq: Option<i64>,
    pub sdr: Option<i64>,
}

impl HealthQuery {
    /// Read one out of a query string, on [`crate::query`]'s conventions: a bad
    /// value is refused by name, and a blank one is absent.
    fn read(params: &std::collections::HashMap<String, String>) -> Filtered<HealthQuery> {
        let read = Params::new(params);
        Ok(HealthQuery {
            after_ms: read.time("after")?,
            before_ms: read.time("before")?,
            system_ref: read.number("system")?,
            freq: read.number("freq")?,
            sdr: read.number("sdr")?,
        })
    }
}

/// `GET /api/admin/recorders/health` — the per-frequency, per-SDR charts.
pub async fn health(
    State(state): State<AppState>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> Result<HealthReport, Failure> {
    let query = HealthQuery::read(&params)?;
    // The rollup's own width is a **floor**: a bucket is attributed whole to
    // whichever axis bucket its first instant falls in, so anything finer would
    // draw bars that are empty by construction.
    let grain = match parse_grain(&params)? {
        Grain::Width(width) => Grain::Width(width.max(BUCKET_MS)),
        Grain::Count(count) => Grain::Count(count),
    };
    read_health(&state.db, &query, grain, state.clock.now_ms())
        .await
        .map_err(Stage::ReadFrequencyHealth.failed())
}

/// The report, as one grouped statement over the rollup — plus the one that
/// discovers the axis, and only when a bound is missing.
///
/// [`crate::archive::call_activity`]'s shape and its reasons: the aggregate is
/// bounded to the axis *explicitly* rather than trusting the caller's own
/// bounds, so every offset the integer division sees is non-negative and the two
/// dialects cannot disagree about where a bucket falls (`crate::activity`).
pub async fn read_health<C: ConnectionTrait>(
    db: &C,
    query: &HealthQuery,
    grain: Grain,
    now_ms: i64,
) -> Result<HealthReport, DbErr> {
    let axis = axis_over(db, query, grain, now_ms).await?;
    let rows: Vec<HealthRow> = frequency_health::Entity::find()
        .select_only()
        .filter(frequency_health::Column::BucketAtMs.gte(axis.from_ms()))
        .filter(frequency_health::Column::BucketAtMs.lt(axis.to_ms()))
        .apply_if(query.system_ref, |q, r#ref| {
            q.filter(system::Column::Ref.eq(r#ref))
        })
        .apply_if(query.freq, |q, freq| {
            q.filter(frequency_health::Column::Freq.eq(freq))
        })
        .apply_if(query.sdr, |q, sdr| {
            q.filter(frequency_health::Column::Sdr.eq(sdr))
        })
        .inner_join(system::Entity)
        .column_as(
            crate::activity::bucket_expr(frequency_health::Column::BucketAtMs, &axis),
            crate::activity::BUCKET,
        )
        .column(frequency_health::Column::SystemId)
        .column(frequency_health::Column::Freq)
        .column(frequency_health::Column::Sdr)
        .column_as(
            crate::db::sum_bigint(frequency_health::Column::Samples),
            "samples",
        )
        .column_as(
            crate::db::sum_bigint(frequency_health::Column::AirMs),
            "air_ms",
        )
        .column_as(
            crate::db::sum_bigint(frequency_health::Column::ErrorCount),
            "error_count",
        )
        .column_as(
            crate::db::sum_bigint(frequency_health::Column::SpikeCount),
            "spike_count",
        )
        .column_as(
            crate::db::sum_bigint(frequency_health::Column::SignalSum),
            "signal_sum",
        )
        .column_as(
            crate::db::sum_bigint(frequency_health::Column::SignalN),
            "signal_n",
        )
        .column_as(
            crate::db::sum_bigint(frequency_health::Column::NoiseSum),
            "noise_sum",
        )
        .column_as(
            crate::db::sum_bigint(frequency_health::Column::NoiseN),
            "noise_n",
        )
        .column_as(
            crate::db::sum_bigint(frequency_health::Column::DriftSum),
            "drift_sum",
        )
        .column_as(
            crate::db::sum_bigint(frequency_health::Column::DriftN),
            "drift_n",
        )
        // **By the output column's name**, never the expression again (#63):
        // sea-orm renders the origin and width as bound parameters, and Postgres
        // compares parameter *nodes* — so repeating it makes the grouped
        // expression a different one and refuses the whole statement.
        .group_by(crate::activity::bucket_group())
        .group_by(frequency_health::Column::SystemId)
        .group_by(frequency_health::Column::Freq)
        .group_by(frequency_health::Column::Sdr)
        .into_model::<HealthRow>()
        .all(db)
        .await?;

    let labels = system_labels(db, &rows).await?;
    Ok(assemble(&axis, rows, &labels))
}

/// Where a report's axis sits: what it asked for, else how far the history
/// reaches under its filters, else the last day.
///
/// The second statement is issued **only when a bound is missing**, which is
/// [`crate::archive`]'s rule: a dated request — every one the screen makes after
/// the first — should not pay to be told what it already said.
async fn axis_over<C: ConnectionTrait>(
    db: &C,
    query: &HealthQuery,
    grain: Grain,
    now_ms: i64,
) -> Result<Axis, DbErr> {
    if let (Some(after), Some(before)) = (query.after_ms, query.before_ms) {
        return Ok(Axis::over(after, before, grain));
    }

    let extent = frequency_health::Entity::find()
        .select_only()
        .apply_if(query.system_ref, |q, r#ref| {
            q.inner_join(system::Entity)
                .filter(system::Column::Ref.eq(r#ref))
        })
        .apply_if(query.freq, |q, freq| {
            q.filter(frequency_health::Column::Freq.eq(freq))
        })
        .apply_if(query.sdr, |q, sdr| {
            q.filter(frequency_health::Column::Sdr.eq(sdr))
        })
        .column_as(frequency_health::Column::BucketAtMs.min(), "first_ms")
        .column_as(frequency_health::Column::BucketAtMs.max(), "last_ms")
        .into_model::<Extent>()
        .one(db)
        .await?
        .and_then(|extent| Some((extent.first_ms?, extent.last_ms?)));

    // A bucket's stamp is its **first** instant, so the axis has to reach a
    // whole bucket past the newest one or the last quarter-hour of history
    // would sit exactly on the exclusive edge and never be drawn.
    let (first_ms, last_ms) = extent
        .map(|(first, last)| (first, last + BUCKET_MS - 1))
        .unwrap_or((now_ms - EMPTY_SPAN_MS + 1, now_ms));
    Ok(Axis::over(
        query.after_ms.unwrap_or(first_ms),
        query.before_ms.unwrap_or(last_ms),
        grain,
    ))
}

/// The Ref and label behind each System in a report — one statement for the
/// whole page, never one per channel (#86).
async fn system_labels<C: ConnectionTrait>(
    db: &C,
    rows: &[HealthRow],
) -> Result<BTreeMap<i64, (i64, Option<String>)>, DbErr> {
    let ids: Vec<i64> = rows
        .iter()
        .map(|row| row.system_id)
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    if ids.is_empty() {
        return Ok(BTreeMap::new());
    }
    Ok(system::Entity::find()
        .filter(system::Column::Id.is_in(ids))
        .all(db)
        .await?
        .into_iter()
        .map(|row| (row.id, (row.r#ref, row.label)))
        .collect())
}

/// Spread the grouped rows across the axis, one channel at a time.
///
/// Pure, so every rule about what a chart plots — the division, the `null`
/// where nothing was measured, the ordering, the cap — is a value a test
/// constructs rather than a database it has to arrange.
fn assemble(
    axis: &Axis,
    rows: Vec<HealthRow>,
    labels: &BTreeMap<i64, (i64, Option<String>)>,
) -> HealthReport {
    let buckets = axis.buckets();
    let mut channels: BTreeMap<(i64, i64, i64), Building> = BTreeMap::new();
    for row in rows {
        let Ok(slot) = usize::try_from(row.bucket) else {
            continue;
        };
        if slot >= buckets {
            continue;
        }
        let (system_ref, label) = labels
            .get(&row.system_id)
            .cloned()
            .unwrap_or((row.system_id, None));
        let channel = channels
            .entry((row.system_id, row.freq, row.sdr))
            .or_insert_with(|| Building::new(system_ref, label, row.freq, row.sdr, buckets));
        channel.take(slot, &row);
    }

    let mut built: Vec<ChannelHealth> = channels.into_values().map(Building::finish).collect();
    // Busiest first, so a cap takes the channels an Operator was least likely to
    // be looking at. Tie-broken on the frequency, so two equally quiet channels
    // do not swap places between refreshes.
    built.sort_by(|a, b| {
        b.samples
            .cmp(&a.samples)
            .then(a.freq.cmp(&b.freq))
            .then(a.sdr.cmp(&b.sdr))
    });
    let omitted = built.len().saturating_sub(MAX_CHANNELS);
    built.truncate(MAX_CHANNELS);

    HealthReport {
        from_ms: axis.from_ms(),
        to_ms: axis.to_ms(),
        bucket_ms: axis.bucket_ms(),
        finest_bucket_ms: BUCKET_MS,
        channels: built,
        omitted,
    }
}

/// One channel mid-assembly: the totals, and the sums each bucket is divided
/// from.
struct Building {
    system_ref: i64,
    system_label: Option<String>,
    freq: i64,
    sdr: i64,
    samples: i64,
    air_ms: i64,
    error_count: i64,
    spike_count: i64,
    errors: Vec<i64>,
    spikes: Vec<i64>,
    air: Vec<i64>,
    signal: Vec<(i64, i64)>,
    noise: Vec<(i64, i64)>,
    drift: Vec<(i64, i64)>,
}

impl Building {
    fn new(
        system_ref: i64,
        system_label: Option<String>,
        freq: i64,
        sdr: i64,
        buckets: usize,
    ) -> Self {
        Building {
            system_ref,
            system_label,
            freq,
            sdr,
            samples: 0,
            air_ms: 0,
            error_count: 0,
            spike_count: 0,
            errors: vec![0; buckets],
            spikes: vec![0; buckets],
            air: vec![0; buckets],
            signal: vec![(0, 0); buckets],
            noise: vec![(0, 0); buckets],
            drift: vec![(0, 0); buckets],
        }
    }

    fn take(&mut self, slot: usize, row: &HealthRow) {
        self.samples += row.samples;
        self.air_ms += row.air_ms;
        self.error_count += row.error_count;
        self.spike_count += row.spike_count;
        self.errors[slot] += row.error_count;
        self.spikes[slot] += row.spike_count;
        self.air[slot] += row.air_ms;
        self.signal[slot] = (
            self.signal[slot].0 + row.signal_sum,
            self.signal[slot].1 + row.signal_n,
        );
        self.noise[slot] = (
            self.noise[slot].0 + row.noise_sum,
            self.noise[slot].1 + row.noise_n,
        );
        self.drift[slot] = (
            self.drift[slot].0 + row.drift_sum,
            self.drift[slot].1 + row.drift_n,
        );
    }

    fn finish(self) -> ChannelHealth {
        ChannelHealth {
            system_ref: self.system_ref,
            system_label: self.system_label,
            freq: self.freq,
            sdr: (self.sdr != NO_SDR).then_some(self.sdr),
            samples: self.samples,
            air_ms: self.air_ms,
            error_count: self.error_count,
            spike_count: self.spike_count,
            error_rate: per_minute(&self.errors, &self.air),
            spike_rate: per_minute(&self.spikes, &self.air),
            signal_dbm: mean(&self.signal),
            noise_dbm: mean(&self.noise),
            drift_hz: mean(&self.drift),
        }
    }
}

/// Counts per minute of air, or `null` where no air was measured.
///
/// **Not zero.** A bucket nothing was heard in is a bucket this says nothing
/// about, and a chart that draws that as a perfect zero-error minute is saying
/// something false about a receiver that may have been dead.
fn per_minute(counts: &[i64], air_ms: &[i64]) -> Vec<Option<f64>> {
    counts
        .iter()
        .zip(air_ms)
        .map(|(count, air)| (*air > 0).then(|| *count as f64 * 60_000.0 / *air as f64))
        .collect()
}

/// Sums divided by their counts, or `null` where there was no reading.
fn mean(readings: &[(i64, i64)]) -> Vec<Option<f64>> {
    readings
        .iter()
        .map(|(sum, n)| (*n > 0).then(|| *sum as f64 / *n as f64))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::repo::NewCallFrequency;

    /// A grouped row the SQL should never produce: its bucket is off either end
    /// of the axis. It is dropped rather than indexed, so a division that ever
    /// disagreed with the filter would draw nothing where it would otherwise
    /// panic on a Pi.
    #[rstest::rstest]
    #[case::before_the_axis(-1)]
    #[case::past_the_axis(4)]
    fn a_row_outside_the_axis_lands_in_no_bucket(#[case] bucket: i64) {
        let axis = Axis::over(0, 4 * BUCKET_MS - 1, Grain::Width(BUCKET_MS));
        assert_eq!(axis.buckets(), 4);
        let row = |bucket| HealthRow {
            bucket,
            system_id: 1,
            freq: 851_012_500,
            sdr: 0,
            samples: 1,
            air_ms: 4_000,
            error_count: 2,
            spike_count: 1,
            signal_sum: -61,
            signal_n: 1,
            noise_sum: -94,
            noise_n: 1,
            drift_sum: -120,
            drift_n: 1,
        };

        let report = assemble(&axis, vec![row(bucket)], &BTreeMap::new());
        assert!(report.channels.is_empty(), "{:?}", report.channels);

        let report = assemble(&axis, vec![row(bucket), row(0)], &BTreeMap::new());
        assert_eq!(report.channels.len(), 1);
        assert_eq!(report.channels[0].samples, 1, "only the row inside counted");
    }

    /// A Call with the recorder truth a Trunk Recorder upload carries.
    fn tr_call(at_ms: i64) -> NewCall {
        NewCall {
            frequency: Some(851_012_500),
            duration_ms: Some(4_000),
            freq_error_hz: Some(-120),
            signal_dbm: Some(-61),
            noise_dbm: Some(-94),
            source_num: Some(1),
            ..NewCall::new(1, 100, at_ms)
        }
    }

    fn entry(freq: i64, at_ms: i64) -> NewCallFrequency {
        NewCallFrequency {
            freq,
            len_ms: Some(2_000),
            at_ms: Some(at_ms),
            error_count: Some(3),
            spike_count: Some(1),
            ..Default::default()
        }
    }

    /// The commonest upload there is: one frequency entry, one reading, one row.
    #[test]
    fn a_calls_one_frequency_entry_is_one_reading() {
        let mut call = tr_call(1_000_000_000_000);
        call.frequencies = vec![entry(851_012_500, 1_000_000_000_000)];

        let readings = readings_of(&call);

        assert_eq!(
            readings,
            vec![Reading {
                freq: 851_012_500,
                at_ms: 1_000_000_000_000,
                air_ms: 2_000,
                error_count: 3,
                spike_count: 1,
                signal_dbm: Some(-61),
                noise_dbm: Some(-94),
                drift_hz: Some(-120),
                sdr: 1,
            }]
        );
    }

    /// Every rdio-dialect upload and every SDRTrunk one: no `frequencies[]` at
    /// all. Charting nothing for those would leave an SDRTrunk-only Instance
    /// with an empty screen and no way to tell that from a healthy one.
    #[test]
    fn a_call_with_no_frequency_entries_still_reads_once() {
        let call = tr_call(1_000_000_000_000);

        let readings = readings_of(&call);

        assert_eq!(readings.len(), 1);
        assert_eq!(readings[0].freq, 851_012_500);
        assert_eq!(readings[0].at_ms, 1_000_000_000_000);
        // The Call's own length is the air time, since no entry declared one.
        assert_eq!(readings[0].air_ms, 4_000);
        assert_eq!(readings[0].error_count, 0);
    }

    /// A Call whose Recorder named no frequency has nothing for a row to be
    /// about — and a bucket at 0 Hz would draw a channel nobody has.
    #[test]
    fn a_call_on_no_frequency_reads_nothing() {
        let mut call = tr_call(1_000_000_000_000);
        call.frequency = None;

        assert!(readings_of(&call).is_empty());
    }

    /// `999` is Trunk Recorder's `DB_UNSET`, written like any other number. An
    /// average that believed it would put the whole county at +999 dBm.
    #[test]
    fn trunk_recorders_unmeasured_db_sentinel_is_not_a_reading() {
        let mut call = tr_call(1_000_000_000_000);
        call.signal_dbm = Some(999);
        call.noise_dbm = Some(999);

        let readings = readings_of(&call);

        assert_eq!(readings[0].signal_dbm, None);
        assert_eq!(readings[0].noise_dbm, None);
    }

    /// Zero dBm is a reading, and nothing about it resembles "not measured".
    #[test]
    fn zero_db_is_a_reading() {
        let mut call = tr_call(1_000_000_000_000);
        call.signal_dbm = Some(0);

        assert_eq!(readings_of(&call)[0].signal_dbm, Some(0));
    }

    /// rdio's `frequencies[]` carries a level per entry, which is sharper than
    /// the Call-wide figure Trunk Recorder gives — so where both are there, the
    /// entry wins.
    #[test]
    fn a_per_frequency_level_outranks_the_calls_own() {
        let mut call = tr_call(1_000_000_000_000);
        call.frequencies = vec![NewCallFrequency {
            dbm: Some(-77.4),
            ..entry(851_012_500, 1_000_000_000_000)
        }];

        assert_eq!(readings_of(&call)[0].signal_dbm, Some(-77));
    }

    /// A recorder that sent a Ref for nothing — `freq: 0` is what an
    /// uninitialised entry looks like, and it is not a frequency.
    #[test]
    fn a_frequency_entry_of_zero_is_not_a_reading() {
        let mut call = tr_call(1_000_000_000_000);
        call.frequencies = vec![entry(0, 1_000_000_000_000)];

        assert!(readings_of(&call).is_empty());
    }

    /// Where a recorder gave positions and no wall clock, the reading still
    /// belongs somewhere — and the Call's own instant is the closest true
    /// answer there is.
    #[test]
    fn an_entry_with_no_clock_falls_back_to_the_calls() {
        let mut call = tr_call(1_000_000_000_000);
        call.frequencies = vec![NewCallFrequency {
            at_ms: None,
            ..entry(851_012_500, 0)
        }];

        assert_eq!(readings_of(&call)[0].at_ms, 1_000_000_000_000);
    }

    /// The whole of the rollup: two readings on one frequency in one
    /// quarter-hour become one row whose numbers are the sum of theirs.
    #[test]
    fn two_readings_in_one_quarter_hour_are_one_row() {
        let mut call = tr_call(1_000_000_000_000);
        call.frequencies = vec![
            entry(851_012_500, 1_000_000_000_000),
            entry(851_012_500, 1_000_000_060_000),
        ];

        let folded = readings(&call);

        assert_eq!(folded.len(), 1);
        assert_eq!(folded[0].samples, 2);
        assert_eq!(folded[0].air_ms, 4_000);
        assert_eq!(folded[0].error_count, 6);
        assert_eq!(folded[0].spike_count, 2);
        assert_eq!(folded[0].signal_sum, -122);
        assert_eq!(folded[0].signal_n, 2);
        assert_eq!(folded[0].bucket_at_ms, bucket_of(1_000_000_000_000));
    }

    /// Two frequencies never share a row, however close together they were
    /// heard: the whole point is to tell one channel's receive conditions from
    /// another's.
    #[test]
    fn two_frequencies_are_two_rows() {
        let mut call = tr_call(1_000_000_000_000);
        call.frequencies = vec![
            entry(851_012_500, 1_000_000_000_000),
            entry(852_000_000, 1_000_000_000_000),
        ];

        let folded = readings(&call);

        assert_eq!(folded.len(), 2);
        assert_eq!(folded[0].freq, 851_012_500);
        assert_eq!(folded[1].freq, 852_000_000);
    }

    /// An unmeasured reading does not drag the average toward zero — it is not
    /// counted at all, which is what the separate `_n` columns are for.
    #[test]
    fn an_absent_reading_is_absent_from_the_count_too() {
        let mut first = tr_call(1_000_000_000_000);
        first.signal_dbm = None;
        let mut second = tr_call(1_000_000_000_000);
        second.signal_dbm = Some(-50);

        let folded = fold(
            readings_of(&first)
                .into_iter()
                .chain(readings_of(&second))
                .collect::<Vec<_>>(),
        );

        assert_eq!(folded.len(), 1);
        assert_eq!(folded[0].samples, 2);
        assert_eq!(folded[0].signal_n, 1);
        assert_eq!(folded[0].signal_sum, -50);
    }

    /// Drift is signed, and a mean over a magnitude would hide the one thing
    /// worth seeing — a receiver pulling consistently one way.
    #[test]
    fn drift_keeps_its_sign() {
        let mut low = tr_call(1_000_000_000_000);
        low.freq_error_hz = Some(-300);
        let mut high = tr_call(1_000_000_000_000);
        high.freq_error_hz = Some(100);

        let folded = fold(
            readings_of(&low)
                .into_iter()
                .chain(readings_of(&high))
                .collect::<Vec<_>>(),
        );

        assert_eq!(folded[0].drift_sum, -200);
        assert_eq!(folded[0].drift_n, 2);
    }

    /// Two SDRs on one frequency are two histories — which is the whole of
    /// "a dying dongle announces itself", since the other one is fine.
    #[test]
    fn two_sdrs_on_one_frequency_are_two_rows() {
        let mut zero = tr_call(1_000_000_000_000);
        zero.source_num = Some(0);
        let mut one = tr_call(1_000_000_000_000);
        one.source_num = Some(1);

        let folded = fold(
            readings_of(&zero)
                .into_iter()
                .chain(readings_of(&one))
                .collect::<Vec<_>>(),
        );

        assert_eq!(folded.len(), 2);
        assert_eq!(folded[0].sdr, 0);
        assert_eq!(folded[1].sdr, 1);
    }

    /// A recorder that named no SDR still gets a row — under the sentinel,
    /// because a `NULL` in the unique index would conflict with nothing.
    #[test]
    fn a_call_with_no_sdr_lands_under_the_sentinel() {
        let mut call = tr_call(1_000_000_000_000);
        call.source_num = None;

        assert_eq!(readings(&call)[0].sdr, NO_SDR);
    }

    /// Two quarter-hours are two rows, and the boundary is exactly where the
    /// arithmetic says.
    #[test]
    fn a_bucket_boundary_splits_the_history() {
        let start = bucket_of(1_000_000_000_000);

        assert_eq!(bucket_of(start), start);
        assert_eq!(bucket_of(start + BUCKET_MS - 1), start);
        assert_eq!(bucket_of(start + BUCKET_MS), start + BUCKET_MS);
    }

    /// An instant before the epoch floors downward like every other one. Nothing
    /// should send one; a bucket that quietly belonged to the wrong quarter-hour
    /// would be worse than one that is negative.
    #[test]
    fn a_bucket_before_the_epoch_floors_downward() {
        assert_eq!(bucket_of(-1), -BUCKET_MS);
    }
}
