//! An **Access code** — the Listener-facing secret that opens a **restricted**
//! channel (CONTEXT.md, ADR-0008, #68).
//!
//! # Two secrets on one row, and why
//!
//! An Operator picks the **code**: a word they say down a radio, print on a
//! briefing sheet, or text to thirty firefighters. That is a low-entropy human
//! secret, which is exactly what Argon2id exists for and exactly what an
//! unsalted SHA-256 is wrong for — `api_keys.key_hash` is the latter, and #51
//! already records why that is only defensible for 128 minted bits. So
//! `code_hash` is Argon2id, like the **admin password**.
//!
//! Argon2id costs ~12 ms here and some multiple of that on a Pi, which is the
//! whole point of it and also the reason it can never run on the path of an
//! audio range request. So it runs **once**, at [`crate::access::unlock`], and
//! what a browser carries afterwards is `grant` — 128 minted bits, stored the
//! way a **Share link**'s token is stored and guarded the way a **Webhook**'s
//! URL is:
//!
//! - it **never appears in a response** except to the browser that has just
//!   proved it knows the code ([`crate::curate::codes::CodeRow`] has no field
//!   for it), and
//! - it **never appears in a log line** (ADR-0011 rule 2), which is why it rides
//!   the **query string** — `http_log` writes a path and never a query, for
//!   exactly this class of secret.
//!
//! It is durable rather than per-unlock because it belongs to the *row*: there
//! is no session table to sweep, a restart logs nobody out, and revoking the
//! code revokes every browser holding it in one `DELETE`. Editing the code
//! re-mints it ([`crate::curate::codes`]), because changing a secret that has
//! leaked must not leave the browsers holding the old one connected.
//!
//! rdio-scanner stores its equivalent **in plaintext**, returns it from the
//! admin API, exports it in the configuration document, and writes the presented
//! one into the log on a failed attempt (`controller.go:462`).

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "access_codes")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    /// What the Operator calls this code — "Fire Ops", "County EMS". Shown in
    /// the admin listing and in the log line a delivery writes, so a refusal
    /// names something an Operator recognises without naming a secret.
    pub label: Option<String>,
    /// Argon2id (PHC format) of the code the Operator chose. Read only by
    /// [`crate::access::unlock`].
    pub code_hash: String,
    /// The minted credential a browser keeps and presents. Unique, so a lookup
    /// is an index hit rather than a scan and a (vanishingly improbable)
    /// collision is a refused insert rather than two codes behind one grant.
    #[sea_orm(unique)]
    pub grant: String,
    /// Which **restricted** channels this code opens, as JSON — `NULL` is every
    /// System, ADR-0008's `"*"`. The shape is
    /// `[{"ref": 11, "talkgroups": "*" | [101, 102]}]`, and
    /// [`crate::access::Grant`] is what parses it.
    ///
    /// A document rather than a join table for the reason `downstreams.scope`
    /// is one: it is read whole, on every resolution, and never queried across.
    pub scope: Option<String>,
    /// When the code stops working, unix milliseconds; `NULL` never expires.
    pub expires_at_ms: Option<i64>,
    /// How many live-feed connections may hold this code at once; `NULL` is
    /// unlimited. Connections, not requests — an HTTP read is not something one
    /// can hold open, and a limit that counted them would refuse a Listener for
    /// scrolling.
    pub max_connections: Option<i64>,
    /// The durable off. Keeps the row — and therefore the grant a browser is
    /// still holding — and refuses it, which is the property ADR-0008 asks of an
    /// **API key** and this inherits for the same reason.
    pub disabled: bool,
    pub created_at_ms: i64,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
