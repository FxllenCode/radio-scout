//! One **Event** (#67, spec US 38): a named, curated collection of **Calls**,
//! frozen against **Retention** — the one thing in the **Archive** that is meant
//! to outlive it.
//!
//! # What is here, and what is deliberately not
//!
//! The row is the incident: what it is called, what an **Operator** wrote about
//! it, and whether it is being shared. Its members are
//! [`super::event_call`] rows, and *they* hold the frozen copies — a member is a
//! snapshot rather than a pointer, which is what "frozen" means.
//!
//! **No `calls` count and no byte total.** Both are one `SUM()` over the member
//! table and neither can be kept correct by a column: a denormalised total is
//! two writes that a crash can separate, and this is the one table whose whole
//! job is surviving things going wrong.
//!
//! # The share token lives here
//!
//! A **Share link** for a Call is a row in a table of its own, because minting
//! one takes no credential and the table therefore needs an abuse bound (#64).
//! An Event is **curated**, so there is no such table to bound: a token is a
//! nullable column, set when an Operator turns sharing on and cleared when they
//! turn it off, and it goes when the Event does — [`crate::star`]'s "a column
//! goes with its row", which is what keeps this out of
//! [`crate::db::repo::delete_calls`]'s list of things to remember.
//!
//! **And it does not expire.** A Call's link does, because it is minted by a
//! stranger and a URL pasted into a group chat should not be a permanent public
//! endpoint. Neither half applies here — the Operator is the one publishing, and
//! an Event is the durable thing by definition, so a link to it that died on
//! Friday would be the wrong promise about the one collection that was made to
//! last. Turning sharing off is how it ends, and `[share] enabled` still closes
//! every door at once.

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "events")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    /// What the Operator called it. Required, and deliberately **not unique**:
    /// two house fires on the same street a year apart are two Events with one
    /// name, and refusing the second would be this surface inventing a rule
    /// nobody asked for.
    pub name: String,
    /// Whatever else they wanted to write down. `None` and `Some("")` are the
    /// same thing to a reader and are kept apart anyway, because
    /// [`crate::curate::nullable`] is how a `PATCH` clears a field.
    pub notes: Option<String>,
    /// The secret in the share URL, or `None` when this Event is not shared.
    ///
    /// Unique so a lookup is an index hit rather than a scan, and so a
    /// (vanishingly improbable) collision is a refused insert rather than two
    /// Events behind one link. Both dialects allow many `NULL`s in a unique
    /// index, which is what makes "not shared" expressible at all.
    #[sea_orm(unique)]
    pub share_token: Option<String>,
    /// When it was first assembled.
    pub created_at_ms: i64,
    /// When it was last changed — a rename, a note, a member added or dropped.
    pub updated_at_ms: i64,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(has_many = "super::event_call::Entity")]
    Members,
}

impl Related<super::event_call::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Members.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
