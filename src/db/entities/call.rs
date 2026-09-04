//! A **Call** — a single recorded transmission: audio (in object storage,
//! referenced by `object_key`) plus metadata. Joins to its System and Talkgroup
//! by internal id; child tables hold the frequency/unit/patch detail.

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "calls")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    pub system_id: i64,
    pub talkgroup_id: i64,
    /// The Talkgroup Ref the recorder actually sent, where `talkgroup_id` is the
    /// channel it **resolved** to (#45). The two differ exactly when the Call
    /// arrived under a member Ref — a patch-minted TGID, a per-site duplicate.
    ///
    /// A bare number rather than a foreign key on purpose: it records what the
    /// radio network said, which may be a Ref no row has ever existed for.
    ///
    /// It is what makes a fold reversible. Resolution rewrites the Talkgroup
    /// before the row is written, so without this there would be no way to tell
    /// which of an owner's Calls used to belong to a folded channel, and
    /// unfolding could only ever give back an empty Talkgroup. `NULL` on every
    /// Call stored before the column existed, which reads as "under the
    /// Talkgroup's own Ref" — true for all of them, since nothing could have
    /// resolved yet.
    pub talkgroup_ref: Option<i64>,
    /// When the transmission happened, unix milliseconds (dialect-agnostic).
    pub call_at_ms: i64,
    pub frequency: Option<i64>,
    // A `source_ref` — the rdio dialect's singular `source` — sat here until
    // #47. It recorded one radio id that `call_units` already held, on the one
    // dialect that sends it: Trunk Recorder's native meta sends a `srcList` and
    // never a singular source, so the column was `NULL` on every TR Call and
    // the wire field built from it was absent on every one of them. The radio a
    // Call is *shown* under is now the first row of `call_units`, which every
    // dialect fills, so nothing read this and it is gone rather than kept
    // written.
    /// Where the audio lives in the object store (ADR-0002).
    pub object_key: String,
    pub audio_mime: Option<String>,
    pub audio_name: Option<String>,
    /// Size of the stored audio object in bytes, recorded at ingest (#10).
    /// Retention's size cap sums this instead of stat-ing the object store on
    /// every sweep. `NULL` for rows written before the column existed; those
    /// count as zero toward the cap.
    pub audio_size: Option<i64>,
    /// How long the transmission is, in milliseconds (#42, spec US 8). From the
    /// recorder when it said (Trunk Recorder's `call_length_ms`), else read
    /// from the audio's container header at ingest. `NULL` when neither could
    /// say — every Call stored before #42, and anything undecodable since.
    pub duration_ms: Option<i64>,
    /// When the transmission ended, unix milliseconds — TR's `stop_time`.
    /// `duration_ms` is the number everything reads; this is the recorder's own
    /// answer kept beside it, which is what the DVR (#63) walks.
    pub stop_at_ms: Option<i64>,
    /// The emergency bit the radio set on the transmission (#42, spec US 5).
    /// Not nullable: a recorder that says nothing said "no", and #53 alerts on
    /// this — a tri-state would make "unknown" indistinguishable from "quiet".
    pub emergency: bool,
    /// The call was on an encrypted talkgroup, so there is nothing to hear.
    /// The row exists so the *activity* is visible (spec US 9); no audio object
    /// is ever written for one, and `object_key` is therefore empty.
    pub encrypted: bool,
    /// The recorder's own priority for this call. TR's, verbatim; nullable
    /// because only TR's native meta carries one. Distinct from the listener's
    /// per-Talkgroup Priority (#58), which is Radio-Scout's own idea.
    pub priority: Option<i32>,
    /// What the recorder was demodulating — TR's `audio_type` (`digital`,
    /// `digital tdma`, `analog`). Free text: it is the recorder's vocabulary,
    /// not ours, and a new mode must not fail an ingest.
    pub audio_type: Option<String>,
    /// The Site this Call was heard on, for multi-site systems (spec US 11).
    /// rdio's generic `site` field resolves to a row here; TR's native meta has
    /// no site to give, and #48 fills it retroactively from SDRTrunk's ID3.
    ///
    /// **Deliberately not a declared foreign key**, unlike `system_id` and
    /// `talkgroup_id`. Those have been in `m0001`'s entity-derived DDL since the
    /// beginning; this column arrives by `ALTER` on every database that already
    /// exists, and SQLite cannot add a constraint to a table it already made. A
    /// relation here would therefore mean a fresh database and an upgraded one
    /// having *different schemas forever* — which is the divergence m0003 and
    /// m0004 were both written to close, and it would also make the column
    /// undroppable on SQLite, taking the `down` migration with it. Nothing is
    /// lost: `sites` rows are only ever created, never deleted, so there is no
    /// delete for `RESTRICT` to restrict.
    pub site_id: Option<i64>,
    /// Where this Call is in the enhancement pipeline (#20) — one of
    /// [`Enhancement`]'s four values. Stored as text rather than an integer so
    /// a `SELECT` is readable by a human debugging a stuck queue, and so a
    /// value added later cannot silently collide with an existing number.
    pub enhancement: String,
    /// This Call's place in the **emission** sequence (#94) — the order Calls
    /// went out on the live feed, which is what a **Backfill** replays and what
    /// a Listener's cursor names.
    ///
    /// Distinct from `id`, which is the order rows were *written*. The two
    /// coincide today because ingest stores and emits in one breath, and they
    /// stop coinciding the moment a **Delay** (#73) holds a Call back: it is
    /// stored on arrival and emitted later, so it carries a lower `id` than
    /// Calls already sent. A cursor over `id` would step past it silently, and
    /// the Listener who reconnected would simply never receive the Call a
    /// safety policy delayed.
    ///
    /// `NULL` means **stored but not yet emitted** — a Call being held, and a
    /// Call whose emission could not be recorded. Nothing backfills one, which
    /// is the right reading either way: it has not gone out yet.
    pub emitted_seq: Option<i64>,
    /// Where this Call is in **tone-out detection** (#55) — one of
    /// [`ToneState`]'s five values, and both halves of the answer in one
    /// column: whether the audio has been looked at, and whether it held a
    /// page.
    ///
    /// It is on the Call row rather than derived from `call_tones` because
    /// [`crate::archive::stored_calls`] denormalizes it on every live frame and
    /// every search row, and #86's rule is that a page costs a constant number
    /// of statements. A child-table read there would be one more statement per
    /// page for a fact that is one character wide.
    pub tone: String,
    /// When this Call's audio was looked inside for the metadata its Recorder
    /// embedded there — **Mining** (#48, CONTEXT.md).
    ///
    /// `NULL` means *never looked*, and that is the only thing it means. A Call
    /// whose audio held nothing, or held something no SDRTrunk wrote, is
    /// stamped exactly like one that gave up a radio's name: the question is
    /// "has this been read?", not "did it say anything?". Anything else and the
    /// sweep would re-read every barren Call in the Archive forever.
    ///
    /// Every Call stored since #48 is born stamped, because ingest mines in the
    /// same pass it reads a duration in. `NULL` is therefore exactly the
    /// Archive that existed before this — which is precisely the set the
    /// **Mining** sweep has to walk.
    ///
    /// A transient failure — an object store that would not answer — leaves it
    /// `NULL` on purpose, so the next sweep tries again.
    pub mined_at_ms: Option<i64>,
    /// Where this Call is in **quiet-span scanning** (#59) — one of
    /// [`QuietState`]'s four values.
    ///
    /// [`ToneState`]'s column with one question rather than two: *was the audio
    /// looked at*. What was found lives in [`Model::quiet`], and the two are not
    /// the same fact — `done` with no spans is a Call somebody said one thing
    /// on, which is most of them.
    pub quiet_state: String,
    /// Where nobody is talking, [`crate::quiet::pack`]ed (#59, spec US 23).
    ///
    /// `NULL` on every Call that was never scanned, could not be, or held no gap
    /// long enough to be worth a seek — which is most Calls. A column rather
    /// than a child table because nothing ever joins to it: it is read by
    /// whoever is about to *play* this exact Call and by nobody else, so rows
    /// would buy a `RESTRICT` foreign key on the retention sweeper's path for
    /// nothing (`call_tones`' cost, which a page-out earns by being searchable
    /// and this is not).
    pub quiet: Option<String>,
    /// When a **Listener** last starred this Call (#66, spec US 37), or `NULL`
    /// for the Calls nobody has.
    ///
    /// **The Instance's mark, not a browser's.** There is no starrer here and
    /// there is deliberately nowhere to put one: a table keyed on anything but
    /// the Call could be filled by anybody who can POST in a loop (#64's abuse
    /// bound), and a per-browser record of what somebody kept is the listening
    /// history ADR-0011 rule 5 exists to stop this process accumulating. So a
    /// Star is one row's column, bounded by the Calls table, which **Retention**
    /// already bounds — and any Listener can set or clear it.
    ///
    /// **A column and not a child table**, which is the other half of the same
    /// choice: `call_tones` and `share_links` each had to be remembered in
    /// [`crate::db::repo::delete_calls`], and forgetting one is invisible until
    /// the retention sweep meets its oldest marked Call and fails there
    /// forever. A column goes with its row.
    ///
    /// The *instant* rather than a boolean, because the only thing that reads
    /// it besides the star itself is a log line — and because "starred" is not
    /// the same question as "starred when", which an Operator wondering what is
    /// pinning their archive open would like an answer to.
    pub starred_at_ms: Option<i64>,
    pub created_at_ms: i64,
}

impl Model {
    /// Is there an audio object behind this Call?
    ///
    /// An empty `object_key` means no object was ever written — an **encrypted
    /// Call** (#42, spec US 9), which is a row recording that the channel was
    /// busy and nothing else. Everything downstream has to ask: the wire omits
    /// `audioUrl`, serving and downloading answer 404, Enhancement declines to
    /// queue it, and Retention has nothing to delete. One method rather than
    /// four `is_empty()` checks, so the next reason a Call has no audio is one
    /// edit and not a hunt.
    pub fn has_audio(&self) -> bool {
        !self.object_key.is_empty()
    }
}

/// The states a Call moves through as it is enhanced.
///
/// Named `EnhancementState`, not `Enhancement`: CONTEXT.md reserves
/// **Enhancement** for the *act* of reprocessing a Call's audio, and a type
/// holding `none`/`pending`/`done`/`skipped` is where that act has got to.
///
/// Constants rather than a Rust enum, because the value crosses a database
/// boundary in both directions. A row written by a newer version — or edited by
/// hand — must degrade to "not pending", which is the safe reading (serve it,
/// cache it normally); parsing into an enum would turn an unrecognised string
/// into an error on a read path that has no useful way to fail. SeaORM's
/// `ActiveEnum` is avoided for the related reason that it would bind the stored
/// spelling to a Rust type across two dialects.
pub struct EnhancementState;

impl EnhancementState {
    /// Stored exactly as the recorder sent it. Every Call that predates
    /// enhancement, and every Call ingested while it was off.
    pub const NONE: &'static str = "none";
    /// Queued or in flight. Audio serving must not mark this `immutable` — the
    /// object behind it is about to be replaced.
    pub const PENDING: &'static str = "pending";
    /// Enhanced; the object key points at the result.
    pub const DONE: &'static str = "done";
    /// Tried and could not be — undecodable audio, or a queue that was full.
    /// The Call keeps its passthrough audio and stays playable.
    pub const SKIPPED: &'static str = "skipped";
}

/// Where a Call has got to in **tone-out detection** (#55).
///
/// [`EnhancementState`]'s shape and, deliberately, one column carrying two
/// questions — *was it looked at* and *did it hold a page* — because the states
/// are mutually exclusive and total, and two columns would allow the fifth
/// combination that means nothing.
///
/// Constants rather than a Rust enum for [`EnhancementState`]'s reason: the
/// value crosses a database boundary in both directions, and a row written by a
/// newer version must degrade to "not pending, not matched", which is the safe
/// reading on every path that asks.
pub struct ToneState;

impl ToneState {
    /// Never offered — every Call ingested while no Tone profile existed, and
    /// every Call that predates this. **Deliberately never re-queued**, which
    /// is [`EnhancementState::NONE`]'s rule: adding a profile marks the Calls
    /// that follow it, and does not go back over an Operator's archive.
    pub const NONE: &'static str = "none";
    /// Queued or in flight. What a restart picks back up.
    pub const PENDING: &'static str = "pending";
    /// Looked at, and it held no page.
    pub const CLEAR: &'static str = "clear";
    /// Looked at, and it paged at least one profile — **the mark**. The rows
    /// saying which are [`super::call_tone`].
    pub const MATCHED: &'static str = "matched";
    /// Could not be looked at: undecodable audio, an object that had gone, or a
    /// queue that was full. The Call is untouched and stays playable.
    pub const SKIPPED: &'static str = "skipped";
}

/// Where a Call has got to in **quiet-span scanning** (#59).
///
/// [`ToneState`]'s shape minus its two "looked at, and here is what it said"
/// arms, because a scan's finding is a list and not a verdict: `done` with an
/// empty [`Model::quiet`] is a Call with nothing worth trimming, which is the
/// ordinary answer and not a separate state.
///
/// Constants rather than a Rust enum for [`EnhancementState`]'s reason: the
/// value crosses a database boundary in both directions, and a row written by a
/// newer version must degrade to "not pending", which is the safe reading
/// everywhere that asks.
pub struct QuietState;

impl QuietState {
    /// Never offered — every Call ingested while `[quiet] enabled = false`, and
    /// every Call that predates this. **Deliberately never re-queued**, which is
    /// [`EnhancementState::NONE`]'s rule and [`ToneState::NONE`]'s: switching
    /// scanning on marks the Calls that follow, and does not read an Operator's
    /// whole Archive back off their disk at the next boot.
    pub const NONE: &'static str = "none";
    /// Queued or in flight. What a restart picks back up.
    pub const PENDING: &'static str = "pending";
    /// Looked at. [`Model::quiet`] is what was found, and is very often nothing.
    pub const DONE: &'static str = "done";
    /// Could not be looked at: undecodable audio, an object that had gone, or a
    /// queue that was full. The Call is untouched and stays playable; **Catch-up**
    /// falls back to raising the rate alone.
    pub const SKIPPED: &'static str = "skipped";
}

impl Model {
    /// Does this Call carry the tone-out **Mark**?
    ///
    /// One method rather than a string comparison at each of the four places
    /// that ask — the wire view, the detail view, the webhook's mark set and
    /// the archive filter — so the vocabulary lives in one file.
    pub fn tone_matched(&self) -> bool {
        self.tone == ToneState::MATCHED
    }
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::system::Entity",
        from = "Column::SystemId",
        to = "super::system::Column::Id"
    )]
    System,
    #[sea_orm(
        belongs_to = "super::talkgroup::Entity",
        from = "Column::TalkgroupId",
        to = "super::talkgroup::Column::Id"
    )]
    Talkgroup,
    #[sea_orm(has_many = "super::call_frequency::Entity")]
    CallFrequency,
    #[sea_orm(has_many = "super::call_unit::Entity")]
    CallUnit,
    #[sea_orm(has_many = "super::call_patch::Entity")]
    CallPatch,
    #[sea_orm(has_many = "super::call_tone::Entity")]
    CallTone,
}

impl Related<super::system::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::System.def()
    }
}

impl Related<super::talkgroup::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Talkgroup.def()
    }
}

impl Related<super::call_frequency::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::CallFrequency.def()
    }
}

impl Related<super::call_unit::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::CallUnit.def()
    }
}

impl Related<super::call_patch::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::CallPatch.def()
    }
}

impl Related<super::call_tone::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::CallTone.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
