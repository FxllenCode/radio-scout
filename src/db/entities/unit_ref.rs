//! A **member Ref** or **Range** of a Unit (#45, CONTEXT.md) — the radio ids an
//! apparatus answers to beside the primary Ref on [`super::unit::Model`].
//!
//! One table holds both, because a lone Ref *is* a Range of one: `ref_from ==
//! ref_to`. Two tables would mean two resolution queries, two uniqueness rules
//! and two CSV spellings for what an operator thinks of as one list — and a
//! fleet's `1200-1299` and its odd spare `4471` are the same sentence.
//!
//! Ranges are a Unit affair rather than a Talkgroup one (CONTEXT.md): fleets
//! number their radios in blocks, where a patch-minted TGID has no block to be
//! in. A Talkgroup's members are therefore [`super::talkgroup_ref`] — Refs only.

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "unit_refs")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    pub unit_id: i64,
    /// The System the Refs are unique within — the owning Unit's own.
    pub system_id: i64,
    /// First Ref of the span, inclusive.
    pub ref_from: i64,
    /// Last Ref of the span, **inclusive** — so `1200..=1200` is one radio and
    /// the empty span is not expressible. An exclusive end would let a Range
    /// that owns nothing be written down, and it would read as owning one.
    pub ref_to: i64,
    pub position: i32,
    pub created_at_ms: i64,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::unit::Entity",
        from = "Column::UnitId",
        to = "super::unit::Column::Id"
    )]
    Unit,
    #[sea_orm(
        belongs_to = "super::system::Entity",
        from = "Column::SystemId",
        to = "super::system::Column::Id"
    )]
    System,
}

impl Related<super::unit::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Unit.def()
    }
}

impl Related<super::system::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::System.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
