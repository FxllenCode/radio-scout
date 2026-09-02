//! One **Share link** (#64, spec US 32): the expiring public URL a **Listener**
//! minted for one **Call**.
//!
//! # One row per Call
//!
//! `call_id` is unique, and that is the whole abuse bound. Minting is
//! unauthenticated — a Listener holds no credential, and US 32 is a Listener's
//! story — so a table keyed on anything else would let anyone fill a Pi's disk
//! by POSTing in a loop. Keyed on the Call, the table is bounded by the Calls
//! table, which **Retention** already bounds; the row goes when its Call does
//! ([`crate::db::repo::delete_calls`] names it, the #55 lesson).
//!
//! Two Listeners sharing the same Call therefore hand out the same link, and
//! revoking it revokes it for both. That is the deliberate cost of the bound,
//! and it makes an Operator's "revoke" mean the thing they meant: *this Call is
//! no longer shared*.
//!
//! # The token is stored as it is handed out
//!
//! An **API key** is hashed (ADR-0008) because nothing ever needs it back. This
//! is the opposite case: a second mint for a Call already shared returns *the
//! link that is already out there*, which no hash can answer. So it is kept the
//! way a **Downstream** peer's key is kept — recoverably, because the shape of
//! the feature requires it — and guarded the way a **Webhook**'s URL is:
//!
//! - it **never appears in a response** except to the Listener who just minted
//!   it ([`crate::curate::shares::ShareRow`] has no field for it, so the admin
//!   listing cannot start carrying one), and
//! - it **never appears in a log line** (ADR-0011 rule 2), which is why it rides
//!   the **query string** rather than the path — `http_log` logs the path and
//!   never the query, for exactly this class of secret.
//!
//! It is a capability over one Call for a bounded window, which is a far smaller
//! thing than an API key's licence to write to the Archive for years.

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "share_links")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    /// The Call this link opens, and the only one it can reach. Unique — see
    /// the module note.
    #[sea_orm(unique)]
    pub call_id: i64,
    /// The secret in the URL. Unique so a lookup is an index hit rather than a
    /// scan, and so a (vanishingly improbable) collision is a refused insert
    /// rather than two Calls behind one link.
    #[sea_orm(unique)]
    pub token: String,
    /// When the link stops working, unix milliseconds. Compared with `<`, in
    /// one place ([`crate::share::live_at`]), so the page and its audio cannot
    /// disagree about the edge.
    pub expires_at_ms: i64,
    /// When the row was first written — the instant an admin listing shows, and
    /// deliberately *not* reset by a re-mint: "shared since" is the question an
    /// Operator is asking.
    pub created_at_ms: i64,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::call::Entity",
        from = "Column::CallId",
        to = "super::call::Column::Id"
    )]
    Call,
}

impl Related<super::call::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Call.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
