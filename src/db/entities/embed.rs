//! An **Embed** (CONTEXT.md, #75): a **Selection** an Operator publishes for
//! another site to frame — a fire department's homepage, a local newsroom.
//!
//! An Operator's row, curated from the browser (`crate::curate::embeds`). The
//! host's snippet names the row by its token, so what the host plays is the
//! Operator's to change — re-scope it and the same `<iframe>` plays the new
//! Selection, delete it and the frame says it is no longer available — without
//! anybody editing somebody else's HTML.
//!
//! # The token is an address, not a credential
//!
//! It is printed into a stranger's public page source, so it can open nothing a
//! Listener holding no code could not already hear: an embed reaches **open
//! listening** intersected with its Selection, and a restricted channel never
//! (`crate::embed`). That is why, unlike a **Share link**'s token or a
//! **Webhook**'s URL, it is shown on the admin listing — the snippet is the thing
//! an Operator came there for. It still rides a URL's query string, where
//! `http_log` never looks, because an Operator's log has no reason to be a list
//! of who frames them.
//!
//! It is random rather than the row's id so that the embeds an Operator has
//! made are not a sequence anybody can walk, and so that a host misusing one is
//! dealt with by deleting it without renumbering anybody else's.

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "embeds")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    /// What the page says at its top, and what the snippet's `title` reads to a
    /// screen reader. Required for the second reason: an `<iframe>` with no
    /// title is a frame an assistive browser can only call "frame".
    pub name: String,
    /// The address in the snippet — see the module note. Unique so a lookup is
    /// an index hit, and so a (vanishingly improbable) collision is a refused
    /// insert rather than two embeds behind one address.
    #[sea_orm(unique)]
    pub token: String,
    /// What it plays, as the same [`crate::selection::Selection`] JSON the live
    /// feed subscribes with — which is literally what the page sends the socket.
    pub selection: String,
    pub created_at_ms: i64,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
