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
    /// The primary transmitting unit's `ref`, if known.
    pub source_ref: Option<i64>,
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

impl ActiveModelBehavior for ActiveModel {}
