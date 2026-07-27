//! A **System** — a radio network Radio-Scout receives calls from. Owns
//! talkgroups, units, and sites (CONTEXT.md). `ref` is the external,
//! recorder-supplied id; `id` is our internal key.

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "systems")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    #[sea_orm(unique)]
    pub r#ref: i64,
    pub label: Option<String>,
    /// Per-system auto-populate toggle (#8, ADR-0001). When the global toggle is
    /// off, unknown talkgroups/units under this system are still auto-created if
    /// this is set. A brand-new *system* is only auto-created when the *global*
    /// toggle is on (mirrors rdio-scanner: new systems need `Options.AutoPopulate`).
    pub auto_populate: bool,
    /// Comma-separated Talkgroup Refs never ingested for this system (#8). `NULL`
    /// or empty blacklists nothing. Mirrors rdio-scanner's per-system `blacklists`.
    pub blacklist: Option<String>,
    /// Whether Calls on this System are enhanced (#20). `NULL` inherits the
    /// instance-wide `[enhancement] mode`; `false` keeps a chatty System off a
    /// Pi's CPU. Nullable on purpose — a plain boolean has no way to say
    /// "follow the instance", which is why `auto_populate` above is awkward to
    /// turn off for one System.
    pub enhancement: Option<bool>,
    pub created_at_ms: i64,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(has_many = "super::talkgroup::Entity")]
    Talkgroup,
    #[sea_orm(has_many = "super::call::Entity")]
    Call,
}

impl Related<super::talkgroup::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Talkgroup.def()
    }
}

impl Related<super::call::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Call.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
