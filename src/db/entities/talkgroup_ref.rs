//! A **member Ref** of a Talkgroup (#45, CONTEXT.md) — one of the additional
//! external Refs a channel answers to, beside the primary Ref that
//! [`super::talkgroup::Model::r#ref`] holds.
//!
//! # Why a side table rather than a pointer on the Talkgroup
//!
//! The alternative was a `merged_into_id` self-FK: fold points the source row at
//! its owner and keeps it. That makes unfold one `UPDATE`, and costs every
//! *other* query in the codebase a `WHERE merged_into_id IS NULL` — the catalog,
//! the filter facets, the CSV upsert, the blacklist, and every admin surface
//! #49 is about to add. One forgotten clause is a duplicate row in a
//! county-scale panel, which is precisely the churn this feature exists to
//! collapse.
//!
//! A member Ref that is not a Talkgroup row cannot appear in a query that looks
//! for Talkgroup rows. So the surfaces stay correct **by construction**, and the
//! only code that changes is resolution itself
//! ([`crate::db::repo::resolve_talkgroup_ref`]).
//!
//! `system_id` is denormalized off the owning Talkgroup so the unique index that
//! makes a Ref unique *within its System* can exist here at all — the same rule
//! `idx_talkgroups_system_ref` enforces for primary Refs, and the reason a
//! member Ref can never be silently owned twice.

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "talkgroup_refs")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    pub talkgroup_id: i64,
    /// The System the Ref is unique within — the owning Talkgroup's own, kept
    /// here so the uniqueness index has both columns to stand on.
    pub system_id: i64,
    pub r#ref: i64,
    /// Where this Ref sits in the Talkgroup's ordered set of members. The order
    /// is the operator's (the CSV column's order, then the admin list's), and it
    /// is what #50 renders; nothing about resolution depends on it.
    pub position: i32,
    /// What the Talkgroup called itself when it was folded in — the one field of
    /// a folded row worth carrying, so unfolding gives an operator back the name
    /// they curated rather than the number a recorder sent.
    ///
    /// `NULL` for a Ref that was never a Talkgroup of its own (an operator
    /// naming a patch TGID ahead of first hearing it), which unfolds to the
    /// auto-populate default exactly as a first sighting would.
    pub label: Option<String>,
    pub created_at_ms: i64,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::talkgroup::Entity",
        from = "Column::TalkgroupId",
        to = "super::talkgroup::Column::Id"
    )]
    Talkgroup,
    #[sea_orm(
        belongs_to = "super::system::Entity",
        from = "Column::SystemId",
        to = "super::system::Column::Id"
    )]
    System,
}

impl Related<super::talkgroup::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Talkgroup.def()
    }
}

impl Related<super::system::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::System.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
