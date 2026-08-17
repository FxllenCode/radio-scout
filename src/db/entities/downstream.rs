//! A **Downstream** (CONTEXT.md, #52): another instance this one forwards
//! matching **Calls** to, speaking the rdio upload dialect.
//!
//! Forwarding only. *Receiving* a peer's downstream is just [`crate::ingest`]
//! with an **API key**, which is why there is no inbound half of this feature.
//!
//! `api_key` is **the peer's secret, stored recoverably** — unlike our own
//! [`super::api_key`] roster, which is hashed, because a hash cannot be sent and
//! this one has to go on the wire on every delivery. Two rules follow from that
//! and are enforced above this row: it never appears in a response
//! ([`crate::curate::downstreams::DownstreamRow`] has no field for it), and it
//! never appears in a log line at any level (ADR-0011 rule 2) — a Downstream is
//! named in logs by its **Id**.

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "downstreams")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    /// What an Operator calls this peer. Cosmetic — the URL is the identity.
    pub label: Option<String>,
    /// The peer's **base** URL. `/api/call-upload` is appended at send time, the
    /// way rdio's own forwarder does (`downstream.go:316`), so an Operator
    /// pastes the same address they would put in a recorder.
    pub url: String,
    /// The API key the peer issued us. Plaintext of necessity — see the module
    /// note.
    pub api_key: String,
    /// Which Calls reach this peer, as the same [`crate::selection::Selection`]
    /// JSON the live feed is scoped by. An empty document selects
    /// nothing, which is what a peer nobody has scoped yet should receive.
    pub scope: String,
    /// Forwarding is off for this peer, and nothing is queued for it.
    pub disabled: bool,
    /// When a delivery to this peer last succeeded — the health reading an
    /// Operator acts on, beside the queue depth.
    pub last_success_ms: Option<i64>,
    /// When one last failed, and why. The reason is a **slug plus a status**,
    /// never a transport error rendered whole: reqwest's `Display` carries the
    /// URL, and a peer's URL carries no secret but its query string might.
    pub last_failure_ms: Option<i64>,
    pub last_failure: Option<String>,
    /// Failed attempts since the last success. `0` is a healthy peer; a number
    /// that climbs is the one an Operator is being asked to look at.
    pub consecutive_failures: i32,
    pub created_at_ms: i64,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
