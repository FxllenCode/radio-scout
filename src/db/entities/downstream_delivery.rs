//! One **Call** owed to one **Downstream** (#52) — the durable queue itself.
//!
//! rdio-scanner has no equivalent: its forwarder POSTs inline on the ingest
//! goroutine and, when a peer is unreachable, logs the error and drops the Call
//! (`downstream.go:416`). A peer down for ten minutes silently loses ten minutes
//! of traffic, and nothing anywhere records what went missing.
//!
//! A row here is written **inside the transaction that stores the Call**, so
//! "the Call exists" and "the Call is owed to this peer" are one fact rather
//! than two that a crash can separate. It is deleted when the peer has taken it,
//! or when the delivery is abandoned for a reason worth logging.
//!
//! **Ordering is `id` ascending, per Downstream, and only the head is ever
//! attempted.** That is what makes "drain in order on recovery" true by
//! construction rather than by a sort: a Call cannot overtake the one in front
//! of it, and one peer's backlog cannot delay another's because the head is
//! chosen per peer.

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "downstream_deliveries")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    pub downstream_id: i64,
    pub call_id: i64,
    /// Attempts made so far. Drives the backoff, and — since only the head of a
    /// peer's queue is ever attempted — is effectively that peer's own retry
    /// counter, which is why there is no second one on the Downstream row.
    pub attempts: i32,
    /// The earliest this may be tried again. A delivery that is not yet due is
    /// **waited for rather than skipped**: skipping it would send the Call
    /// behind it first, and the order is the promise.
    pub next_attempt_ms: i64,
    pub queued_at_ms: i64,
    /// Why the last attempt failed — a slug, so the admin screen and the log
    /// line say the same word.
    pub last_failure: Option<String>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
