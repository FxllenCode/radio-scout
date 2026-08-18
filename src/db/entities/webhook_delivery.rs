//! One **Call** owed to one **Webhook** (#54) — the durable queue itself.
//!
//! The same table shape as [`super::downstream_delivery`], and deliberately a
//! *second table* rather than a discriminator column on that one. Two reasons,
//! and the second is the load-bearing one:
//!
//! - The queue-head index is the query that runs forever (`WHERE sink_id = ?
//!   ORDER BY id`), and a shared table would put both sinks' backlogs in one
//!   index whose leading column is a discriminator nobody filters on alone.
//! - **The two queues fill at completely different rates.** Every stored Call
//!   reaches a Downstream's queue; only a *marked* Call reaches a webhook's. A
//!   shared table would mean a Downstream outage's backlog — thousands of rows
//!   overnight — sitting in the same table the Emergency path reads, for no
//!   benefit at all.
//!
//! A row is written **inside the transaction that stores the Call**, so "the
//! Call exists" and "the Call is owed to this webhook" are one fact rather than
//! two that a crash can separate. It is deleted when the webhook has taken it,
//! or when the delivery is abandoned for a reason worth logging.
//!
//! **Ordering is `id` ascending, per Webhook, and only the head is ever
//! attempted** — the [`super::downstream_delivery`] rule, for its reason.

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "webhook_deliveries")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    pub webhook_id: i64,
    pub call_id: i64,
    /// Attempts made so far. Drives the backoff, and — since only the head of a
    /// webhook's queue is ever attempted — is effectively that webhook's own
    /// retry counter, which is why there is no second one on the Webhook row.
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
