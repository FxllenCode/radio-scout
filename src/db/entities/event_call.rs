//! One **Call** frozen into an **Event** (#67, spec US 38) — a *snapshot*, not a
//! pointer.
//!
//! # Why there is no foreign key to `calls`
//!
//! This is the whole feature. A member has to outlive the Call it was made
//! from: **Retention** will prune that Call, and the point of an Event is that
//! the incident survives. So this row holds its own copy of the audio object and
//! its own copy of what the Call *said*, and `call_id` is a plain integer
//! recording where it came from.
//!
//! That is [`super::call_tone`]'s rule taken one step further, and it is the
//! exact inverse of [`super::share_link`]'s. `call_tones` and `share_links` are
//! children that go **with** the Call, which is why
//! [`crate::db::repo::delete_calls`] has to name them — forget one and the
//! retention sweep fails at its oldest marked Call forever (#55's lesson). An
//! `event_calls` row must **survive** that same delete, so it must not be in
//! that list and must not carry a constraint that would make the delete fail.
//! A foreign key here would turn every Event into a wall the sweep cannot get
//! past.
//!
//! # The snapshot is the wire shape
//!
//! [`Model::snapshot`] is a serialized [`crate::call::StoredCall`] — the same
//! document a search page, a live frame and an export manifest carry. Two things
//! follow. A field added to the Archive's wire reaches a frozen Call with no
//! migration (the manifest's rule, #65); and an Event frozen by one release and
//! read by a later one gets the new field's default rather than a parse error,
//! which is what makes this safe to keep for years.
//!
//! **`object_key` is a column and not part of the snapshot**, because they say
//! different things: the snapshot is the Call as it was, and the column is where
//! *our copy* of its audio lives. The snapshot's own key is skipped on the wire
//! anyway, so there is nothing to disagree with.
//!
//! # ...and four facts are columns as well
//!
//! `call_at_ms`, `duration_ms` and `audio_size` are all inside the snapshot too,
//! and they are columns because SQL has to be able to add them up: an Event's
//! export measures itself in one statement the way a range export does (#65),
//! and **Retention**'s size cap has to count frozen bytes without parsing a
//! JSON document per row. `call_at_ms` also orders the members, and an incident
//! has one useful order.

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "event_calls")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    pub event_id: i64,
    /// The Call this was frozen from. **Not a foreign key** — see the module
    /// note. Unique within the Event, so adding a Call twice is one member
    /// rather than two copies of one transmission.
    pub call_id: i64,
    /// When the transmission was, unix milliseconds. The order members are
    /// listed, played and exported in.
    pub call_at_ms: i64,
    /// How long it lasted, where anybody measured it. `None` is why a member
    /// can be in an Event's zip and not in its stitched file — a timeline
    /// cannot reserve room for a length nobody knows (#65).
    pub duration_ms: Option<i64>,
    /// Where **the frozen copy** lives. Empty for an **Encrypted Call**, which
    /// is a row with no object at all and is frozen for the fact that the
    /// channel was busy.
    pub object_key: String,
    /// How big that copy is, so the size cap and the export's extent are both a
    /// `SUM()`.
    pub audio_size: i64,
    /// The Call as a Listener saw it, at the instant it was frozen — a
    /// serialized [`crate::call::StoredCall`].
    #[sea_orm(column_type = "Text")]
    pub snapshot: String,
    /// When it was added to the Event.
    pub added_at_ms: i64,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::event::Entity",
        from = "Column::EventId",
        to = "super::event::Column::Id"
    )]
    Event,
}

impl Related<super::event::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Event.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
