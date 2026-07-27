//! A **push subscription** (#16, ADR-0005): one browser's registration for Web
//! Push notifications, plus the [`crate::selection::Selection`] it wants to be
//! notified about.
//!
//! Distinct from a listener's Selection alone (CONTEXT.md) — this is the
//! delivery half: where to send, and the keys that make the message readable
//! only by the device that registered. `auth` is a shared secret and never
//! leaves this table or a log line (ADR-0011 rule 2); a subscription is
//! identified in logs by its **Id**.

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "push_subscriptions")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    /// The push service URL this device is reachable at. Unique: a browser that
    /// re-subscribes with the same endpoint is the same device, not a second
    /// one.
    #[sea_orm(unique)]
    pub endpoint: String,
    /// The unguessable handle this device proves itself with — presented on the
    /// live-feed socket to say "hold my notifications, I am listening", and to
    /// unsubscribe. A sequential **Id** would let anyone silence anyone else's
    /// notifications by counting.
    #[sea_orm(unique)]
    pub token: String,
    /// The device's public key, base64url (`PushSubscription.keys.p256dh`).
    pub p256dh: String,
    /// The device's auth secret, base64url (`PushSubscription.keys.auth`).
    pub auth: String,
    /// The listener's Selection, as the same JSON the live feed takes.
    pub selection: String,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
