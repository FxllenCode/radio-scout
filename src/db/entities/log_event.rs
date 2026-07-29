//! A **log event** (#30, ADR-0011): one line the server wrote, kept so an
//! operator with no shell can read it back.
//!
//! The console is still the primary surface — this table is an *additional*
//! sink ([`crate::logsink`]), never the one a request waits on. What may be
//! stored is rule 5's business and not this table's: the sink refuses to run
//! below INFO, which is the level a listener's address would first appear at.
//!
//! The columns are an event's parts rather than a rendered line, because
//! rendering is the client's job and searching a sentence is not searching at
//! all: `level` and `at_ms` are what the Logs view filters on, `message` is the
//! static string ADR-0011 rule 6 asks for, and `fields` is the variable half as
//! a JSON object. `request_id` is #28's correlation id, so a 5xx reported to a
//! listener as `internal error (ref: …)` can be found here by that ref alone.

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "logs")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    /// When the event was recorded, unix milliseconds — the archive's clock
    /// (`crate::now_ms`), so retention can compare the two the same way.
    pub at_ms: i64,
    /// `ERROR`, `WARN` or `INFO`, as `tracing` spells them.
    pub level: String,
    /// The module the event came from (`radio_scout::ingest`).
    pub target: String,
    /// The event's static message.
    pub message: String,
    /// Its structured fields as a JSON object, or `NULL` when it had none.
    pub fields: Option<String>,
    /// The request this was logged during (#28's `x-request-id`), when it was
    /// logged during one at all.
    pub request_id: Option<String>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
