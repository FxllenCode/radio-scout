//! A **Tone profile** (#55, spec US 20) — a station's paging sequence, written
//! down against the Talkgroup it is paged on.
//!
//! The sequence itself is one JSON column rather than a child table, which is
//! the [`super::webhook`] shape and for its reason: it is read whole, matched in
//! memory and never joined or filtered on, so rows would buy ordering
//! bookkeeping and an extra statement per profile for nothing.

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "tone_profiles")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    pub talkgroup_id: i64,
    /// What a match is called — "Station 12". **Required**, unlike a
    /// [`super::webhook`]'s label: a mark that fired without saying which
    /// station was paged answers the wrong question.
    pub label: String,
    /// How far off a tone may be, as a percentage of the tone
    /// ([`crate::tone::DEFAULT_TOLERANCE_PCT`]).
    pub tolerance_pct: f64,
    /// How long a silence between two tones may be before the sequence has been
    /// broken ([`crate::tone::DEFAULT_GAP_MAX_MS`]).
    pub gap_max_ms: i64,
    /// The tones, in order, as `[{"hz":1122.5,"minMs":800},…]`
    /// ([`crate::tone::steps_of`]).
    pub steps: String,
    /// Switched off without being deleted — the [`super::downstream`] and
    /// [`super::webhook`] convention, and what an Operator reaches for when a
    /// profile is paging on somebody else's tones and they want to stop it
    /// *now* and work out why later.
    pub disabled: bool,
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
}

impl Related<super::talkgroup::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Talkgroup.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
