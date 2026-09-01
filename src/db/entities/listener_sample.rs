//! A **listener sample** (#62, spec US 41): how many people were listening,
//! written down every so often so that "peak listeners, and when" is a chart
//! rather than a guess.
//!
//! **Three columns, and that is the whole feature.** ADR-0011 rule 5 says a
//! Listener's address never appears above DEBUG and a public Instance must not
//! accumulate a record of who listened and when; a table of *counts* is what
//! satisfies both halves of that at once — an Operator learns their audience's
//! shape, and nothing here can be turned back into a person. There is
//! deliberately no session id, no address, no user agent, and no per-Talkgroup
//! breakdown (which on a quiet channel with one listener would be exactly the
//! identity-shaped fact this avoids).
//!
//! `listeners` is the **peak since the previous sample**, not the count at the
//! instant the sampler fired: a Listener who arrived and left between two ticks
//! would otherwise be invisible, and a peak that silently under-reports is worse
//! than no peak at all. See [`crate::listeners::Listeners`].
//!
//! A surrogate `id` rather than keying on `at_ms`, because a frozen clock —
//! which is how the suite makes time a fact rather than a sleep — would
//! otherwise make two samples collide.

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "listener_samples")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    /// When the sample was taken, unix milliseconds — the archive's clock, so
    /// retention compares it the same way it compares a Call's.
    pub at_ms: i64,
    /// The most Listeners connected at once since the previous sample.
    pub listeners: i64,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
