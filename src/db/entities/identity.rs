//! The **Instance identity** (CONTEXT.md, #77): what an Operator has curated
//! about the Instance itself rather than any entity in it.
//!
//! **One row, or none.** Its key is always [`ROW`]; an Instance that has never
//! been given an identity has no row at all and reads every default — which is
//! what lets a fresh install serve exactly what it served before this table
//! existed. Typed columns rather than a key–value table, so a value the code
//! cannot read is a column the database refuses rather than a string
//! discovered at the next boot.
//!
//! Curation, not configuration (ADR-0012): nothing here is ever in the TOML,
//! because an Operator changes it from a browser while Listeners are connected.

use sea_orm::entity::prelude::*;

/// The key of the one row there is.
pub const ROW: i64 = 1;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "identity")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: i64,
    /// What the Instance is called — the header, the tab, the installed app.
    /// `NULL` is no name, and the product's own is shown.
    pub name: Option<String>,
    /// Where a Listener goes for help: an `https://` page or a `mailto:`.
    pub help_url: Option<String>,
    /// `"24h"` or `"12h"` — [`crate::identity::TimeFormat`].
    pub time_format: String,
    /// The Talkgroups panel's order for a Profile that has not chosen one —
    /// [`crate::identity::PanelSort`].
    pub panel_sort: String,
    pub playback_goes_live: bool,
    /// Seconds of quiet before the Live display dims; `0` never.
    pub dimmer_secs: i32,
    pub show_listeners: bool,
    /// The **Reference page**'s markdown. `NULL` is no page, and no entry for one.
    #[sea_orm(column_type = "Text", nullable)]
    pub reference: Option<String>,
    /// When the page was last published.
    pub reference_at_ms: Option<i64>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
