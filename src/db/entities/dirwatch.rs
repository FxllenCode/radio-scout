//! A **Dirwatch** (CONTEXT.md, #72): one folder a Recorder drops Calls into,
//! and how to read what it drops.
//!
//! An Operator's row, curated from the browser (`crate::curate::dirwatches`),
//! and bounded by `[dirwatch] roots` — the folder must be inside one of them, a
//! rule checked when it is saved *and* every time it is started, since the TOML
//! can change under a row that was fine when it was written (ADR-0021).

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "dirwatches")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    /// What an Operator calls it. Cosmetic.
    pub label: Option<String>,
    /// The folder, as it was saved — absolute, and inside a root when it was.
    pub directory: String,
    /// Which Recorder's folder this is: [`crate::dirwatch::Format`]'s slug.
    pub format: String,
    /// The audio extension read here, without its dot. `None` is the format's
    /// own default — Trunk Recorder's `wav`, SDRTrunk's `mp3`.
    pub extension: Option<String>,
    /// An rdio filename mask, for the `mask` format and no other.
    pub mask: Option<String>,
    /// The System every file here is filed under, outranking the file.
    pub system_ref: Option<i64>,
    /// The Talkgroup, likewise.
    pub talkgroup_ref: Option<i64>,
    /// A frequency, in hertz, for files that carry none.
    pub frequency: Option<i64>,
    /// How long a file must have been left alone before it is read — rdio's
    /// "delay", the time a Recorder needs to finish writing.
    pub delay_ms: i64,
    /// Remove a file once it is ingested — or refused as a duplicate or by the
    /// blacklist, which are answers too. Never one that failed to store.
    pub delete_after: bool,
    /// Look for files on a timer instead of being told about them — for a
    /// network share, where the OS has no events to give.
    pub poll: bool,
    pub disabled: bool,
    /// **The watermark** (#72): every file written at or before this instant,
    /// unix milliseconds, has been handled. What lets a watch that keeps its
    /// files backfill exactly the ones that arrived while the Instance was down,
    /// with one column rather than a row per file. Starts at the moment the
    /// watch was created, so pointing one at a folder of history does not
    /// import it — a watch that deletes as it goes needs none, since whatever
    /// is still there is still owed.
    pub seen_through_ms: i64,
    pub created_at_ms: i64,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
