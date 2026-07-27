//! A **Call** — a single recorded transmission: audio (in object storage,
//! referenced by `object_key`) plus metadata. Joins to its System and Talkgroup
//! by internal id; child tables hold the frequency/unit/patch detail.

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "calls")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    pub system_id: i64,
    pub talkgroup_id: i64,
    /// When the transmission happened, unix milliseconds (dialect-agnostic).
    pub call_at_ms: i64,
    pub frequency: Option<i64>,
    /// The primary transmitting unit's `ref`, if known.
    pub source_ref: Option<i64>,
    /// Where the audio lives in the object store (ADR-0002).
    pub object_key: String,
    pub audio_mime: Option<String>,
    pub audio_name: Option<String>,
    /// Size of the stored audio object in bytes, recorded at ingest (#10).
    /// Retention's size cap sums this instead of stat-ing the object store on
    /// every sweep. `NULL` for rows written before the column existed; those
    /// count as zero toward the cap.
    pub audio_size: Option<i64>,
    pub duration_ms: Option<i64>,
    /// Where this Call is in the enhancement pipeline (#20) — one of
    /// [`Enhancement`]'s four values. Stored as text rather than an integer so
    /// a `SELECT` is readable by a human debugging a stuck queue, and so a
    /// value added later cannot silently collide with an existing number.
    pub enhancement: String,
    pub created_at_ms: i64,
}

/// The states a Call moves through as it is enhanced.
///
/// Deliberately not an enum in the entity: SeaORM's `ActiveEnum` would bind the
/// stored spelling to a Rust type across two dialects, and this is a column two
/// queries filter on and nothing joins to. The constants are the single source
/// of the spellings.
pub struct Enhancement;

impl Enhancement {
    /// Stored exactly as the recorder sent it. Every Call that predates
    /// enhancement, and every Call ingested while it was off.
    pub const NONE: &'static str = "none";
    /// Queued or in flight. Audio serving must not mark this `immutable` — the
    /// object behind it is about to be replaced.
    pub const PENDING: &'static str = "pending";
    /// Enhanced; the object key points at the result.
    pub const DONE: &'static str = "done";
    /// Tried and could not be — undecodable audio, or a queue that was full.
    /// The Call keeps its passthrough audio and stays playable.
    pub const SKIPPED: &'static str = "skipped";
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::system::Entity",
        from = "Column::SystemId",
        to = "super::system::Column::Id"
    )]
    System,
    #[sea_orm(
        belongs_to = "super::talkgroup::Entity",
        from = "Column::TalkgroupId",
        to = "super::talkgroup::Column::Id"
    )]
    Talkgroup,
    #[sea_orm(has_many = "super::call_frequency::Entity")]
    CallFrequency,
    #[sea_orm(has_many = "super::call_unit::Entity")]
    CallUnit,
    #[sea_orm(has_many = "super::call_patch::Entity")]
    CallPatch,
}

impl Related<super::system::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::System.def()
    }
}

impl Related<super::talkgroup::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Talkgroup.def()
    }
}

impl Related<super::call_frequency::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::CallFrequency.def()
    }
}

impl Related<super::call_unit::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::CallUnit.def()
    }
}

impl Related<super::call_patch::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::CallPatch.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
