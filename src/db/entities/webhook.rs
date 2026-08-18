//! A **Webhook** (CONTEXT.md, #54): an Operator-configured URL that receives the
//! **Calls** they asked to hear about, as JSON.
//!
//! A sibling of [`super::downstream`], not of anything Listener-facing: an
//! Operator wiring up their own inbox is a different act from this Instance
//! waking a **Listener**, which it does not do ([ADR-0014]). The trigger is a
//! **mark on a Call** — the **Emergency** bit today, a **Tone profile** match
//! once #55 lands — never a notification, and never anything derived from
//! speech (ADR-0013).
//!
//! # The URL *is* the credential
//!
//! This is the one way this row differs from a Downstream's, and every rule
//! below follows from it. A Discord webhook URL ends in a token: anyone holding
//! it can post into that channel forever. A Downstream separates the two — a
//! public base URL plus an `api_key` — so its URL can be shown and only its key
//! is guarded. Here there is nothing to separate.
//!
//! So the URL is treated exactly as a Downstream's `api_key` is, one notch
//! stricter:
//!
//! - It **never appears in a response**. [`crate::curate::webhooks::WebhookRow`]
//!   has no field for it, and carries [`crate::webhook::host_of`]'s answer
//!   instead — `discord.com` is not a secret and is what lets an Operator tell
//!   two rows apart.
//! - It **never appears in a log line**, at any level, in any form (ADR-0011
//!   rule 2). A Webhook is named in logs by its **Id**, which is also why
//!   `crate::delivery`'s transport errors are rendered through `without_url`.
//! - It is **never exported**. #51's configuration document is a backup an
//!   Operator emails and commits, and its hard rule is that no secret leaves in
//!   any form — so webhooks are left out of it entirely rather than exported
//!   into a file that would then have to be handled like a password vault.
//!
//! [ADR-0014]: https://github.com/FxllenCode/radio-scout/blob/next/docs/adr/0014-no-notifications.md

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "webhooks")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    /// What an Operator calls this webhook. Cosmetic, and — unlike a
    /// Downstream, whose URL is its identity — the *only* thing that
    /// distinguishes two rows on the screen, since the URL can never be shown.
    pub label: Option<String>,
    /// Where the POST goes, verbatim. The credential — see the module note.
    pub url: String,
    /// Which shape the body takes: [`crate::webhook::Format`]'s slug.
    pub format: String,
    /// Which **marks** on a Call this webhook asked for, as a JSON array of
    /// [`crate::webhook::Mark`] slugs. An empty array fires for nothing, which
    /// is the safe direction and what a webhook nobody has configured should do.
    pub marks: String,
    /// Which Calls can reach it, as the same [`crate::selection::Selection`]
    /// JSON the live feed is scoped by. Scope and marks are **both** required:
    /// an Emergency outside the scope is not this webhook's business.
    pub scope: String,
    /// Delivery is off for this webhook, and nothing is queued for it.
    pub disabled: bool,
    /// When a delivery last succeeded — the health reading an Operator acts on,
    /// beside the queue depth.
    pub last_success_ms: Option<i64>,
    /// When one last failed, and why. The reason is a **slug plus a status**,
    /// never a transport error rendered whole, which would carry the URL.
    pub last_failure_ms: Option<i64>,
    pub last_failure: Option<String>,
    /// Failed attempts since the last success. `0` is a healthy webhook; a
    /// number that climbs is the one an Operator is being asked to look at.
    pub consecutive_failures: i32,
    pub created_at_ms: i64,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
