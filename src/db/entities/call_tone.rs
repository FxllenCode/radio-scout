//! One **Tone profile** match on one **Call** (#55) — the detail behind the
//! tone-out **Mark**.
//!
//! The Call row itself carries whether it was marked ([`super::call::ToneState`]),
//! because that is read on every live frame and every search row and must cost
//! no statement. This table is *which* profiles fired and *where*, which one
//! Call's detail view asks for one Call at a time.
//!
//! **The label is a snapshot, and `profile_id` is not a foreign key.** A row
//! here records what happened, so a profile renamed next year does not rewrite
//! last year's pages, and a profile deleted does not erase them. That is the
//! opposite of the rule for a Talkgroup's label — which *is* joined, because a
//! channel renamed is the same channel — and the difference is that a page-out
//! is an event and a channel is a thing.

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "call_tones")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    pub call_id: i64,
    /// Which profile fired. Nullable and unconstrained: the profile may be
    /// gone, and the page still happened.
    pub profile_id: Option<i64>,
    /// What that profile was called when it fired.
    pub label: String,
    /// How far into the Call the sequence began, in milliseconds.
    pub at_ms: i64,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::call::Entity",
        from = "Column::CallId",
        to = "super::call::Column::Id"
    )]
    Call,
}

impl Related<super::call::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Call.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
