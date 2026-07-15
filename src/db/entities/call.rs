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
    pub duration_ms: Option<i64>,
    pub created_at_ms: i64,
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
