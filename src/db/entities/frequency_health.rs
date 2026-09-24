//! **Frequency health** (#71, spec US 51): how one frequency behaved on one SDR
//! over one stretch of time, so that a dying dongle announces itself before the
//! Listeners do.
//!
//! **A rollup, and that is what "persisted compactly" means.** Every number here
//! is already in `calls` and `call_frequencies` — but those are bounded by
//! **Retention**, so the history dies with the audio, and a county's month of
//! them is the largest scan on the Instance. A row here is one quarter-hour of
//! one frequency on one SDR ([`crate::rf::BUCKET_MS`]), it is written in the
//! transaction that stores the Call, and it outlives the Call on a window of its
//! own (`[retention] health_days`). "Was this receiver always this bad" is a
//! question about a period whose audio went months ago — the **listener
//! sample**'s argument, one subject along.
//!
//! **Sums and counts, never averages.** An average cannot be re-averaged: a
//! chart asking for hourly bars over quarter-hourly rows would have to weight
//! four of them by how many readings each held, which is exactly the count an
//! average throws away. So the aggregate is exact at any width the chart asks
//! for, and the arithmetic lives in one place ([`crate::rf`]).
//!
//! **`sdr` is `NOT NULL` and spells "not said" as [`crate::rf::NO_SDR`].**
//! Both dialects treat `NULL`s in a unique index as distinct from each other, so
//! a nullable column here would conflict with nothing — every upsert would
//! insert a *new* row and the rollup would grow one row per Call forever while
//! reading as though it were deduplicating. The sentinel is ugly and the failure
//! it prevents is silent.

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "frequency_health")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    pub system_id: i64,
    /// The frequency these readings were taken on, in Hz.
    pub freq: i64,
    /// Which SDR the **Recorder** was using, TR's `source_num` — or
    /// [`crate::rf::NO_SDR`] where it did not say, which is every Call that
    /// arrived in the rdio dialect.
    pub sdr: i64,
    /// The first instant of this bucket, unix milliseconds — a multiple of
    /// [`crate::rf::BUCKET_MS`].
    pub bucket_at_ms: i64,
    /// How many readings were folded in. A **Copy** of a transmission counts
    /// too, deliberately: the question here is how a *receiver* is doing, and a
    /// duplicate that a second SDR decoded badly is exactly the evidence this
    /// exists to surface.
    pub samples: i64,
    /// How much air time those readings cover, in milliseconds — the
    /// denominator of every rate on the chart. Without it an "error count" is a
    /// measure of how busy the channel was rather than of how well it decoded.
    pub air_ms: i64,
    pub error_count: i64,
    pub spike_count: i64,
    /// Summed signal readings, in dBm, and how many there were. Absent readings
    /// are not counted — which is why the count is here and not implied by
    /// [`Model::samples`].
    pub signal_sum: i64,
    pub signal_n: i64,
    pub noise_sum: i64,
    pub noise_n: i64,
    /// Summed tuning error, in Hz, and how many readings gave one. The recorder
    /// reports this **signed**, so the mean is a drift and not a magnitude.
    pub drift_sum: i64,
    pub drift_n: i64,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::system::Entity",
        from = "Column::SystemId",
        to = "super::system::Column::Id"
    )]
    System,
}

impl Related<super::system::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::System.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
