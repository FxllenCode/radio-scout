//! Repository functions over the domain entities.
//!
//! **Auto-populate** (#8, ADR-0001) is the policy that keeps the archive usable
//! with zero manual config: an unknown System/Talkgroup/Unit is created the first
//! time a Call for it is ingested, using the recorder's labels and falling back
//! to rdio-scanner's defaults (`Untagged` Tag, `Unknown` Group, numeric Talkgroup
//! label, `Talkgroup <ref>` name, lowest-free Ref for new Systems). Two toggles
//! gate it — a global one ([`IngestConfig`](crate::ingest::IngestConfig)) and a
//! per-system one ([`system::Model::auto_populate`]) — and a per-system blacklist
//! drops chosen Talkgroups outright. [`disposition`] is the single place that
//! decision is made — purely, over the [`Channel`] [`resolve_channel`] read for
//! the Call (#96) — and [`insert_call`] applies the defaults on create.
//!
//! The archive-search query filters via joins + `DISTINCT` (portable across
//! SQLite/Postgres). It deliberately does **no** DB-side list aggregation
//! (`GROUP_CONCAT`/`STRING_AGG` diverge by dialect, ADR-0003) — a call's groups
//! are loaded separately and assembled in Rust.

use std::collections::HashMap;

use sea_orm::sea_query::{Alias, Expr};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DbErr, EntityTrait, IntoActiveModel, JoinType,
    PaginatorTrait, QueryFilter, QueryOrder, QuerySelect, RelationTrait, Set,
};

use crate::blob::StoredAudio;
use crate::call::{CallId, Candidate, Emission, Quality};
use crate::db::entities::{
    api_key, call, call_frequency, call_patch, call_unit, group, log_event, push_subscription,
    site, system, tag, talkgroup, talkgroup_group, talkgroup_ref, unit, unit_ref,
};

/// Default Tag label for an auto-populated Talkgroup the recorder sent no tag for
/// (rdio-scanner `controller.go`).
pub const DEFAULT_TAG: &str = "Untagged";
/// Default Group label for an auto-populated Talkgroup the recorder sent no group
/// for (rdio-scanner `controller.go`).
pub const DEFAULT_GROUP: &str = "Unknown";

/// Find a System by its Ref, creating it if absent.
pub async fn resolve_or_create_system<C: ConnectionTrait>(
    db: &C,
    ext_ref: i64,
    label: Option<String>,
    now_ms: i64,
) -> Result<system::Model, DbErr> {
    if let Some(found) = system::Entity::find()
        .filter(system::Column::Ref.eq(ext_ref))
        .one(db)
        .await?
    {
        return Ok(found);
    }
    system::ActiveModel {
        r#ref: Set(ext_ref),
        label: Set(label),
        // Per-system auto-populate defaults off (rdio-scanner); the global toggle
        // governs unless an operator flips this on later (#8). `blacklist` is left
        // unset (NULL — nothing blacklisted).
        auto_populate: Set(false),
        created_at_ms: Set(now_ms),
        ..Default::default()
    }
    .insert(db)
    .await
}

/// Find a Tag by name, creating it if absent.
pub async fn resolve_or_create_tag<C: ConnectionTrait>(
    db: &C,
    name: &str,
    now_ms: i64,
) -> Result<tag::Model, DbErr> {
    if let Some(found) = tag::Entity::find()
        .filter(tag::Column::Name.eq(name))
        .one(db)
        .await?
    {
        return Ok(found);
    }
    tag::ActiveModel {
        name: Set(name.to_owned()),
        created_at_ms: Set(now_ms),
        ..Default::default()
    }
    .insert(db)
    .await
}

/// Find a Group by name, creating it if absent.
pub async fn resolve_or_create_group<C: ConnectionTrait>(
    db: &C,
    name: &str,
    now_ms: i64,
) -> Result<group::Model, DbErr> {
    if let Some(found) = group::Entity::find()
        .filter(group::Column::Name.eq(name))
        .one(db)
        .await?
    {
        return Ok(found);
    }
    group::ActiveModel {
        name: Set(name.to_owned()),
        created_at_ms: Set(now_ms),
        ..Default::default()
    }
    .insert(db)
    .await
}

/// Associate a Talkgroup with a Group (idempotent).
pub async fn link_talkgroup_group<C: ConnectionTrait>(
    db: &C,
    talkgroup_id: i64,
    group_id: i64,
) -> Result<(), DbErr> {
    let exists = talkgroup_group::Entity::find_by_id((talkgroup_id, group_id))
        .one(db)
        .await?
        .is_some();
    if !exists {
        talkgroup_group::ActiveModel {
            talkgroup_id: Set(talkgroup_id),
            group_id: Set(group_id),
        }
        .insert(db)
        .await?;
    }
    Ok(())
}

/// The Unit a radio id belongs to on this System (#45): the Unit whose own Ref
/// it is, else the Unit owning a member Ref or a **Range** it falls inside.
///
/// Primary before member, for the reason [`resolve_talkgroup`] spells out. A
/// lone member Ref is stored as a Range of one, so both are the same query —
/// `ref_from <= r <= ref_to` — and a fleet's `1200-1299` and its odd spare
/// `4471` cost the same lookup.
pub async fn resolve_unit<C: ConnectionTrait>(
    db: &C,
    system_id: i64,
    ext_ref: i64,
) -> Result<Option<unit::Model>, DbErr> {
    if let Some(primary) = unit::Entity::find()
        .filter(unit::Column::SystemId.eq(system_id))
        .filter(unit::Column::Ref.eq(ext_ref))
        .one(db)
        .await?
    {
        return Ok(Some(primary));
    }
    let Some(member) = unit_ref::Entity::find()
        .filter(unit_ref::Column::SystemId.eq(system_id))
        .filter(unit_ref::Column::RefFrom.lte(ext_ref))
        .filter(unit_ref::Column::RefTo.gte(ext_ref))
        .one(db)
        .await?
    else {
        return Ok(None);
    };
    unit::Entity::find_by_id(member.unit_id).one(db).await
}

/// [`resolve_unit`] for a whole page at once (#47): the Unit owning each
/// `(System Id, radio id)` pair, in **two** statements rather than two per pair.
///
/// The batched form exists because the denormalizer behind every search page and
/// every live-feed frame now names the radio a Call is shown under, and the
/// per-Ref form there would be an N+1 of exactly the kind #86 deleted — fifty
/// Calls costing a hundred round-trips on a Pi.
///
/// Both statements are bounded by the *page*, never by the archive or by how
/// many Ranges an Operator has written down: the spans are matched by an `OR` of
/// the ids actually asked about (the access path
/// `idx_unit_refs_system_span` exists for), and the Units are then fetched by
/// primary key and by pair together. Nothing asked, nothing spent.
pub async fn units_owning<C: ConnectionTrait>(
    db: &C,
    pairs: &[(i64, i64)],
) -> Result<HashMap<(i64, i64), unit::Model>, DbErr> {
    use sea_orm::Condition;

    let pairs: Vec<(i64, i64)> = distinct(pairs.iter().copied());
    if pairs.is_empty() {
        return Ok(HashMap::new());
    }
    let system_ids: Vec<i64> = distinct(pairs.iter().map(|(system_id, _)| *system_id));
    let refs: Vec<i64> = distinct(pairs.iter().map(|(_, ext_ref)| *ext_ref));

    // Which Ranges cover any of the ids asked about. A `Range` of one is how a
    // lone member Ref is stored (#45), so this finds both.
    let spans = unit_ref::Entity::find()
        .filter(unit_ref::Column::SystemId.is_in(system_ids.clone()))
        .filter(refs.iter().fold(Condition::any(), |any, ext_ref| {
            any.add(
                unit_ref::Column::RefFrom
                    .lte(*ext_ref)
                    .and(unit_ref::Column::RefTo.gte(*ext_ref)),
            )
        }))
        .all(db)
        .await?;

    // The primaries and the Range owners in one read. A Unit reached both ways
    // comes back once; which claim wins is decided below, not by the query.
    let units: HashMap<i64, unit::Model> = unit::Entity::find()
        .filter(
            Condition::any()
                .add(
                    unit::Column::SystemId
                        .is_in(system_ids)
                        .and(unit::Column::Ref.is_in(refs)),
                )
                .add(unit::Column::Id.is_in(spans.iter().map(|span| span.unit_id))),
        )
        .all(db)
        .await?
        .into_iter()
        .map(|unit| (unit.id, unit))
        .collect();
    // `(system_id, ref)` is unique (`idx_units_system_ref`), so this holds every
    // primary claim and holds each of them once.
    let primaries: HashMap<(i64, i64), i64> = units
        .values()
        .map(|unit| ((unit.system_id, unit.r#ref), unit.id))
        .collect();

    let mut owned = HashMap::new();
    for (system_id, ext_ref) in pairs {
        // Primary before member, which is [`resolve_unit`]'s rule and the one
        // that makes a Range covering somebody else's own Ref harmless.
        let owner = primaries.get(&(system_id, ext_ref)).copied().or_else(|| {
            spans
                .iter()
                .find(|span| {
                    span.system_id == system_id
                        && span.ref_from <= ext_ref
                        && ext_ref <= span.ref_to
                })
                .map(|span| span.unit_id)
        });
        if let Some(unit) = owner.and_then(|id| units.get(&id)) {
            owned.insert((system_id, ext_ref), unit.clone());
        }
    }
    Ok(owned)
}

/// **Every Ref the apparatus owning `unit_ref` answers to** (#45, #47, spec
/// US 16) — what searching by a radio actually searches for.
///
/// Always at least the Ref asked about, so an uncurated archive (where no Unit
/// owns anything) answers with exactly that radio rather than with nothing. When
/// a Unit does own it, the whole set comes back: its own Ref, its **Ranges**,
/// and its lone member Refs. A Listener who taps "Engine 1" wants the
/// apparatus's night, not whichever portable happened to key.
///
/// **The answer is per System, and takes no System as an argument.** A Ref is
/// unique only within one, so a fleet's block on System 11 says nothing about
/// radio 1210 on System 200 — and a flat list of spans applied to a search that
/// named no System quietly answers with the other System's radios. Carrying the
/// System *in the scope* rather than narrowing the lookup by one makes that
/// impossible in both directions, and it is why the cascading filter options can
/// clear the System filter (which is what they do) without the scope going stale:
/// the scope is a fact about a Ref, never about the search that asked.
///
/// Two statements when the Ref is a Unit's own or nobody's, three when a Range
/// owns it — once per request, never per Call.
pub async fn unit_scope<C: ConnectionTrait>(
    db: &C,
    unit_ref: i64,
) -> Result<crate::merge::UnitScope, DbErr> {
    use crate::merge::{Range, UnitScope};

    let owners: Vec<unit::Model> = unit::Entity::find()
        .filter(unit::Column::Ref.eq(unit_ref))
        .all(db)
        .await?;

    // Primary before member, [`resolve_unit`]'s rule: a Range covering somebody
    // else's own Ref never speaks for it, so the Range is only asked about when
    // no Unit claims the Ref as its own. Applied per System, because that is the
    // scope the rule was written for — one System's Unit owning the Ref does not
    // settle it for a System that has never heard of it.
    let owned_elsewhere: Vec<unit::Model> = unit_ref::Entity::find()
        .filter(unit_ref::Column::RefFrom.lte(unit_ref))
        .filter(unit_ref::Column::RefTo.gte(unit_ref))
        .filter(unit_ref::Column::SystemId.is_not_in(owners.iter().map(|owner| owner.system_id)))
        .find_also_related(unit::Entity)
        .all(db)
        .await?
        .into_iter()
        // The owning Unit rides back with the span, so the Calls a fleet's
        // *mobile* keyed are the apparatus's too — and it costs no second read
        // to know which Ref that is.
        .filter_map(|(_, owner)| owner)
        .collect();

    let owners: Vec<unit::Model> = owners.into_iter().chain(owned_elsewhere).collect();
    if owners.is_empty() {
        return Ok(UnitScope::bare(unit_ref));
    }

    let mut spans: HashMap<i64, Vec<Range>> = HashMap::new();
    for owner in &owners {
        // The Unit's own Ref, which is not a member Ref and so is in no span.
        spans
            .entry(owner.system_id)
            .or_default()
            .push(Range::new(owner.r#ref, owner.r#ref));
    }
    for span in unit_ref::Entity::find()
        .filter(unit_ref::Column::UnitId.is_in(owners.iter().map(|owner| owner.id)))
        .all(db)
        .await?
    {
        spans
            .entry(span.system_id)
            .or_default()
            .push(Range::new(span.ref_from, span.ref_to));
    }

    let mut owned: Vec<(i64, Vec<Range>)> = spans
        .into_iter()
        .map(|(system_id, mut ranges)| {
            ranges.sort();
            ranges.dedup();
            (system_id, ranges)
        })
        .collect();
    // Ordered so the condition built from this is the same SQL twice running —
    // a `HashMap`'s order is not, and a query that changes shape between two
    // identical requests is one no cache and no test can pin.
    owned.sort();
    Ok(UnitScope {
        asked: unit_ref,
        owned,
    })
}

/// The **Ranges** and lone member Refs a Unit answers to beside its own Ref
/// (#45), in the order an Operator wrote them.
pub async fn member_spans<C: ConnectionTrait>(
    db: &C,
    unit_id: i64,
) -> Result<Vec<crate::merge::Range>, DbErr> {
    Ok(unit_ref::Entity::find()
        .filter(unit_ref::Column::UnitId.eq(unit_id))
        .order_by_asc(unit_ref::Column::Position)
        .order_by_asc(unit_ref::Column::Id)
        .all(db)
        .await?
        .into_iter()
        .map(|span| crate::merge::Range::new(span.ref_from, span.ref_to))
        .collect())
}

/// The distinct values of an iterator, order-insensitive — the `IN (…)` list for
/// a batched lookup, and the pair list one is built from.
pub(crate) fn distinct<T: Eq + std::hash::Hash>(items: impl Iterator<Item = T>) -> Vec<T> {
    items
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect()
}

/// Find the Unit a radio id belongs to, creating one if no Unit owns it. A Ref
/// is unique only within its System.
///
/// A Unit that already has an alias **keeps** it (#8 auto-populate), and a Unit
/// reached through a Range keeps it for the same reason: an apparatus is named
/// once, not renamed by whichever of its portables keyed last. A Unit with *no*
/// alias takes the one offered, which is a different act — filling a blank
/// rather than rewriting curation — and a real case from #47 onwards, since a
/// unit CSV may create a Unit for nothing but the **Range** it owns. Without it
/// that apparatus stays a bare number forever while every Call under it carries
/// the name.
pub async fn resolve_or_create_unit<C: ConnectionTrait>(
    db: &C,
    system_id: i64,
    ext_ref: i64,
    label: Option<String>,
    now_ms: i64,
) -> Result<unit::Model, DbErr> {
    if let Some(found) = resolve_unit(db, system_id, ext_ref).await? {
        let Some(name) = label.filter(|_| found.label.is_none()) else {
            return Ok(found);
        };
        let mut named = found.into_active_model();
        named.label = Set(Some(name));
        return named.update(db).await;
    }
    unit::ActiveModel {
        system_id: Set(system_id),
        r#ref: Set(ext_ref),
        label: Set(label),
        created_at_ms: Set(now_ms),
        ..Default::default()
    }
    .insert(db)
    .await
}

/// Find a Site by (System, Ref), creating it if absent (#42, spec US 11), and
/// name it if it has no name and one was offered (#48).
///
/// Unlike a Unit this is never gated on the auto-populate flag. A Site Ref is a
/// fact about the System's own infrastructure, not a radio somebody keyed — an
/// operator who turned auto-populate off did so to stop unknown Talkgroups
/// filling their panel, and unnamed towers are what makes simulcast coverage
/// legible rather than clutter.
///
/// **A Ref identifies and a name names**, which is why `label` is a separate
/// argument rather than an alternative to `ext_ref`: the two answer different
/// questions and a recorder may know either, both or neither. A Site that
/// already has a name keeps it — the auto-populate rule (#8), so **Mining**
/// cannot overwrite what an Operator curated.
pub async fn resolve_or_create_site<C: ConnectionTrait>(
    db: &C,
    system_id: i64,
    ext_ref: i64,
    label: Option<&str>,
    now_ms: i64,
) -> Result<site::Model, DbErr> {
    if let Some(found) = site::Entity::find()
        .filter(site::Column::SystemId.eq(system_id))
        .filter(site::Column::Ref.eq(ext_ref))
        .one(db)
        .await?
    {
        return name_site(db, found, label).await;
    }
    site::ActiveModel {
        system_id: Set(system_id),
        r#ref: Set(ext_ref),
        label: Set(label.map(str::to_string)),
        created_at_ms: Set(now_ms),
        ..Default::default()
    }
    .insert(db)
    .await
}

/// Find a Site by (System, name), creating it if absent — the only way an
/// SDRTrunk Call ever gets one (#48, spec US 13).
///
/// SDRTrunk's rdio broadcaster sends no `site` field at all (its `FormField`
/// enum has none), so every SDRTrunk Call has arrived with `site_id` null since
/// the beginning. What its ID3 carries is a *name* — the tower an Operator
/// configured the channel for — and there is no Ref anywhere to pair it with.
///
/// So one is **minted**, exactly as [`lowest_free_system_ref`] mints one for a
/// System that Trunk Recorder identified by name alone (#8): the lowest
/// positive Ref this System is not already using. The Ref is ours rather than
/// the radio network's, which is the honest reading — nobody outside this
/// Instance ever assigned this tower a number, and the name is what identifies
/// it here and everywhere it is shown.
pub async fn resolve_or_create_site_named<C: ConnectionTrait>(
    db: &C,
    system_id: i64,
    label: &str,
    now_ms: i64,
) -> Result<site::Model, DbErr> {
    if let Some(found) = site::Entity::find()
        .filter(site::Column::SystemId.eq(system_id))
        .filter(site::Column::Label.eq(label))
        .one(db)
        .await?
    {
        return Ok(found);
    }
    site::ActiveModel {
        system_id: Set(system_id),
        r#ref: Set(lowest_free_site_ref(db, system_id).await?),
        label: Set(Some(label.to_string())),
        created_at_ms: Set(now_ms),
        ..Default::default()
    }
    .insert(db)
    .await
}

/// Give a Site a name if it hasn't got one. A named Site keeps its name.
async fn name_site<C: ConnectionTrait>(
    db: &C,
    site: site::Model,
    label: Option<&str>,
) -> Result<site::Model, DbErr> {
    let Some(label) = label.filter(|_| site.label.is_none()) else {
        return Ok(site);
    };
    let mut named = site.into_active_model();
    named.label = Set(Some(label.to_string()));
    named.update(db).await
}

/// The lowest positive Ref not yet used by any Site of this System.
///
/// Scoped to one System, where [`lowest_free_system_ref`] is global, because
/// that is what `idx_sites_system_ref` makes unique — tower 1 of two Systems is
/// two towers.
async fn lowest_free_site_ref<C: ConnectionTrait>(db: &C, system_id: i64) -> Result<i64, DbErr> {
    let taken: std::collections::HashSet<i64> = site::Entity::find()
        .select_only()
        .column(site::Column::Ref)
        .filter(site::Column::SystemId.eq(system_id))
        .into_tuple()
        .all(db)
        .await?
        .into_iter()
        .collect();
    let mut next = 1;
    while taken.contains(&next) {
        next += 1;
    }
    Ok(next)
}

/// The Site a Call was heard on, as this Recorder identified it — resolved to
/// (or created as) a `sites` row.
///
/// One function because there are two ways in and both writers need the same
/// answer: a Ref (the rdio dialect's `site`), a name (**Mining**, #48), or
/// both, in which case the Ref decides *which* tower and the name decides what
/// it is called.
async fn site_of<C: ConnectionTrait>(
    db: &C,
    system_id: i64,
    new: &NewCall,
    now_ms: i64,
) -> Result<Option<i64>, DbErr> {
    let label = new.site_label.as_deref();
    match (new.site_ref, label) {
        (Some(site_ref), _) => Ok(Some(
            resolve_or_create_site(db, system_id, site_ref, label, now_ms)
                .await?
                .id,
        )),
        (None, Some(label)) => Ok(Some(
            resolve_or_create_site_named(db, system_id, label, now_ms)
                .await?
                .id,
        )),
        (None, None) => Ok(None),
    }
}

/// A unit heard within a call (rdio `sources[]`/`units[]`).
#[derive(Debug, Clone, Default)]
pub struct NewCallUnit {
    pub unit_ref: i64,
    pub label: Option<String>,
    pub offset_ms: Option<i64>,
    /// The alias the radio transmitted, where `label` is the one the recorder
    /// had configured (#42, TR's `tag_ota`; SDRTrunk's `talkerAlias`).
    pub tag_ota: Option<String>,
    pub emergency: bool,
    pub signal_system: Option<String>,
    /// Wall-clock start of this Unit's transmission, unix milliseconds.
    pub at_ms: Option<i64>,
}

impl NewCallUnit {
    /// What to call this radio — [`crate::call::unit_name`] over the two names
    /// this Call carries for it (#47, spec US 12).
    pub fn name(&self) -> Option<&str> {
        crate::call::unit_name(self.label.as_deref(), self.tag_ota.as_deref())
    }
}

/// A frequency sample within a call (rdio `frequencies[]`).
#[derive(Debug, Clone, Default)]
pub struct NewCallFrequency {
    pub freq: i64,
    pub pos_ms: Option<i64>,
    pub len_ms: Option<i64>,
    pub dbm: Option<f64>,
    pub error_count: Option<i32>,
    pub spike_count: Option<i32>,
    /// Wall-clock start of this segment, unix milliseconds.
    pub at_ms: Option<i64>,
}

/// **What a Recorder said** about one Call — Refs and labels, exactly as it
/// sent them.
///
/// Deliberately *only* that (#96). It carried the object key and the byte
/// length too, which are facts about our own store and are not knowable until
/// the audio has been written; [`StoredAudio`] carries those, and
/// [`insert_call`] takes both — so ADR-0001's write-order is a property of the
/// types rather than a comment somebody has to read.
///
/// **No `Default`**, for the same reason: a defaulted `NewCall` is a Call on
/// Talkgroup 0 of System 0 at the beginning of the epoch, which no Recorder
/// could send and which several tests were quietly seeding. [`NewCall::new`]
/// takes the three facts every upload has, and `..NewCall::new(…)` keeps
/// everything else as short as `..Default::default()` was.
#[derive(Debug, Clone)]
pub struct NewCall {
    pub system_ref: i64,
    pub system_label: Option<String>,
    pub talkgroup_ref: i64,
    pub talkgroup_label: Option<String>,
    pub talkgroup_name: Option<String>,
    pub talkgroup_tag: Option<String>,
    pub talkgroup_groups: Vec<String>,
    pub call_at_ms: i64,
    pub frequency: Option<i64>,
    pub audio_mime: Option<String>,
    pub audio_name: Option<String>,
    /// The recorder's own figure when it sent one; otherwise filled in by
    /// ingest from the audio's container header (#42).
    pub duration_ms: Option<i64>,
    pub stop_at_ms: Option<i64>,
    pub emergency: bool,
    /// No audio object is written for an encrypted Call — the row records that
    /// the activity happened, and there is nothing to hear (spec US 9).
    pub encrypted: bool,
    pub priority: Option<i32>,
    pub audio_type: Option<String>,
    /// The Site Ref the recorder named, resolved to (or created as) a `sites`
    /// row by [`insert_call`] — the auto-populate precedent (#8), because a
    /// multi-site System discovers its towers the same way it discovers its
    /// Talkgroups.
    pub site_ref: Option<i64>,
    /// What the recorder *called* that Site. **Mining** (#48) is the only
    /// source: SDRTrunk's ID3 names the tower a channel was configured for,
    /// and its rdio broadcaster sends no `site` field to pair it with — so on
    /// an SDRTrunk Call this arrives alone and a Ref is minted for it.
    pub site_label: Option<String>,
    /// When this Call's audio was looked inside — set by ingest's own mining
    /// pass, and `None` for a caller that stored a Call without one.
    ///
    /// Here rather than stamped by the writers because the column records that
    /// **Mining** happened, and mining is what fills it: a Call seeded straight
    /// into the Archive genuinely has not been read, which is the same thing as
    /// every Call stored before #48 and is what the sweep goes looking for.
    pub mined_at_ms: Option<i64>,
    pub patches: Vec<i64>,
    pub units: Vec<NewCallUnit>,
    pub frequencies: Vec<NewCallFrequency>,
}

/// What a Recorder said about the **channel** a Call was on — the labels an
/// unknown Talkgroup is auto-populated with (#8), and nothing else.
///
/// Its own type because a Talkgroup is also created where there is no Call at
/// all: unmerging one (#45) restores it from a member Ref and a label. That
/// path used to build a whole `NewCall` to carry four strings, which meant
/// inventing a Call on System 0 at the beginning of the epoch — exactly the
/// value #96 stopped `NewCall` from being able to express.
pub struct NewTalkgroup<'a> {
    pub talkgroup_ref: i64,
    pub label: Option<&'a str>,
    pub name: Option<&'a str>,
    pub tag: Option<&'a str>,
    pub groups: &'a [String],
}

impl NewCall {
    /// What this Call says about its own channel.
    pub fn talkgroup(&self) -> NewTalkgroup<'_> {
        NewTalkgroup {
            talkgroup_ref: self.talkgroup_ref,
            label: self.talkgroup_label.as_deref(),
            name: self.talkgroup_name.as_deref(),
            tag: self.talkgroup_tag.as_deref(),
            groups: &self.talkgroup_groups,
        }
    }

    /// How good a copy of its transmission this one is — what keep-best
    /// compares against the Calls already stored (#46).
    ///
    /// Decode errors are summed across the frequencies the recorder reported,
    /// counting only the entries that gave a number: a `freqList` where none
    /// did is a recorder that does not report signal health at all, which is
    /// silence rather than a clean decode ([`Quality::better_than`]).
    pub fn quality(&self) -> Quality {
        let mut decode_errors = None;
        for reported in self.frequencies.iter().filter_map(|f| f.error_count) {
            *decode_errors.get_or_insert(0) += i64::from(reported);
        }
        Quality {
            decode_errors,
            duration_ms: self.duration_ms,
        }
    }

    /// The three facts every upload carries — which System, which Talkgroup,
    /// and when — with everything a Recorder may or may not have said left
    /// empty. Fill the rest in with `..NewCall::new(system, talkgroup, at_ms)`.
    pub fn new(system_ref: i64, talkgroup_ref: i64, call_at_ms: i64) -> Self {
        NewCall {
            system_ref,
            system_label: None,
            talkgroup_ref,
            talkgroup_label: None,
            talkgroup_name: None,
            talkgroup_tag: None,
            talkgroup_groups: Vec::new(),
            call_at_ms,
            frequency: None,
            audio_mime: None,
            audio_name: None,
            duration_ms: None,
            stop_at_ms: None,
            emergency: false,
            encrypted: false,
            priority: None,
            audio_type: None,
            site_ref: None,
            site_label: None,
            mined_at_ms: None,
            patches: Vec::new(),
            units: Vec::new(),
            frequencies: Vec::new(),
        }
    }
}

/// Insert a Call and its child rows, into the `channel` already resolved for it
/// — creating the System or the Talkgroup where `resolved` says there is none.
/// Returns the stored Call.
///
/// `audio` is the object ingest wrote a moment ago, or `None` for an **encrypted
/// Call**, which is a row and nothing else (#42, spec US 9). It is an argument
/// rather than a field of `new` because only a completed write produces one,
/// which is how ADR-0001's ordering is expressed: the row cannot be inserted
/// until the object exists.
///
/// A brand-new Talkgroup is auto-populated (#8) with the recorder's labels,
/// falling back to rdio-scanner's defaults (numeric label, `Talkgroup <ref>`
/// name, `Untagged` Tag, `Unknown` Group). An **existing** Talkgroup is left
/// untouched — auto-populate fills unknowns, it never rewrites curated rows. The
/// `auto_populate` flag (the effective global-or-per-system value from
/// [`disposition`]) gates only the Unit roster, matching rdio.
///
/// Passing `&Resolved::unresolved()` asks for both to be resolved here, which is
/// what a caller seeding rows directly wants; ingest passes what it already
/// read, so the pair is resolved once per Call rather than twice (#96).
///
/// Not internally transactional — the caller (ingest) wraps this in one so the
/// resolve → insert sequence is atomic with the audio write (ADR-0001).
pub async fn insert_call<C: ConnectionTrait>(
    db: &C,
    new: &NewCall,
    audio: Option<StoredAudio>,
    resolved: &Resolved,
    auto_populate: bool,
    now_ms: i64,
) -> Result<call::Model, DbErr> {
    let sys = match resolved.system.clone() {
        Some(known) => known,
        // System gets a `System <ref>` default label when the recorder sent
        // none. The label is only applied on create; an existing System keeps
        // its own.
        None => {
            let label = new
                .system_label
                .clone()
                .or_else(|| Some(format!("System {}", new.system_ref)));
            resolve_or_create_system(db, new.system_ref, label, now_ms).await?
        }
    };

    let tg = match resolved.talkgroup.clone() {
        Some(known) => known,
        None => match resolve_talkgroup(db, sys.id, new.talkgroup_ref).await? {
            Some(existing) => existing,
            None => create_populated_talkgroup(db, sys.id, &new.talkgroup(), now_ms).await?,
        },
    };

    // A Site is discovered the way a Talkgroup is (#8): a multi-site System
    // learns its towers from the traffic, because a file naming them goes stale
    // the moment the operator adds one.
    let site_id = site_of(db, sys.id, new, now_ms).await?;

    let mut row = call::ActiveModel {
        system_id: Set(sys.id),
        talkgroup_id: Set(tg.id),
        // What the recorder said, beside what it resolved to (#45). Written on
        // every Call, not only the merged ones: a Ref becomes a member Ref long
        // after its Calls arrived, and a column filled in only once it mattered
        // would be empty in exactly the archive an operator wants to unfold.
        talkgroup_ref: Set(Some(new.talkgroup_ref)),
        call_at_ms: Set(new.call_at_ms),
        site_id: Set(site_id),
        created_at_ms: Set(now_ms),
        ..Default::default()
    };
    describe_transmission(&mut row, new, audio);
    let stored = row.insert(db).await?;

    // Patch members (#81). A patch ref is kept only when this System has a
    // Talkgroup for it. SDRTrunk builds one unseparated array as
    // `[<patchgroup>, <talkgroup>…, <radio>…]` — the patched *radio IDs* ride
    // behind the talkgroups with nothing marking the boundary
    // (`RdioScannerBroadcaster.java:546-574`) — and sends it under the same
    // field name Trunk Recorder's uploader uses, so the wire cannot say which
    // entries are Talkgroup Refs. Resolving against what the System knows is
    // what rdio-scanner does (`call.go:572-582`, `if !talkgroupId.Valid`), and
    // it is the only answer that never records a radio as a Talkgroup or fans a
    // Call out to a listener subscribed to that number. An unrecognised ref is
    // skipped rather than ending the list: order is SDRTrunk's alone, and Trunk
    // Recorder's `patched_talkgroups` carries no radios to cut at.
    //
    // One query for the whole array rather than one per ref: this runs inside
    // the transaction that already holds the audio write (ADR-0001), and on a
    // Postgres backend every extra statement is a round-trip a Pi pays for.
    //
    // Already resolved when ingest asked (#46 needs the answer to decide the
    // Call's Admission at all); resolved here for a caller that seeds rows
    // directly, exactly as the System and the Talkgroup above are.
    let patched = match resolved.patches.clone() {
        Some(known) => known,
        None => resolve_patches(db, &sys, &new.patches, new.talkgroup_ref).await?,
    };
    write_patches(db, stored.id, &patched).await?;
    write_signal_detail(db, stored.id, new).await?;
    roster_units(db, sys.id, new, auto_populate, now_ms).await?;

    Ok(stored)
}

/// What storing a better copy of an already-stored transmission did (#46).
///
/// Two arms because the Call being replaced can be gone by the time this runs
/// and the difference matters to everything downstream: a replacement is
/// deliberately silent on the live feed (the Listener already has this Call),
/// where a Call stored for the first time has to be published or nobody hears
/// it at all.
#[derive(Debug)]
pub enum Replacement {
    /// The stored Call now carries this copy — same id, same channel, same
    /// place in the Archive.
    Replaced(call::Model),
    /// The Call this was a better copy *of* is gone, so the copy became a Call
    /// of its own.
    Stored(call::Model),
}

/// Point an already-stored Call at a better copy of its transmission, **under
/// its own id** (#46, spec US 10) — the enhancement pipeline's swap precedent
/// ([`store_enhanced_audio`]) applied to a copy that arrived from a Recorder
/// rather than one this Instance produced.
///
/// **What changes is the transmission; what stays is the identity.** The audio
/// object, everything the Recorder said about the signal, and the per-frequency
/// and per-Unit detail are this copy's. The id, the System, the Talkgroup, the
/// instant and the created-at are the stored Call's, because a Listener may
/// already hold this Call in a queue, a Run or an open Archive page, and a Call
/// that changed channel or moved in time under them would be a worse bug than
/// the duplicate this exists to prevent.
///
/// **Patches are unioned, never replaced.** Each copy names the channels its own
/// recorder saw the patch reach, and the surviving row stands for all of them —
/// so a Listener subscribed to any member still receives it, whichever copy
/// happened to win.
///
/// **Enhancement is reset.** The levelled audio a previous pass produced
/// describes a copy that no longer exists, so the Call goes back to `none` and
/// ingest offers it again.
///
/// Returns [`Replacement::Stored`] when the Call is no longer there. Retention
/// is entitled to prune one between the moment the decision read it and the
/// moment this writes — the size cap prunes oldest-first regardless of age, and
/// a Recorder backfilling old Calls puts candidates right at that edge — and
/// the honest answer then is that the transmission is not in the Archive, so
/// this copy becomes it.
///
/// Not internally transactional: the caller wraps this with the audio write,
/// exactly as it does [`insert_call`].
pub async fn store_replacement<C: ConnectionTrait>(
    db: &C,
    call_id: CallId,
    new: &NewCall,
    audio: Option<StoredAudio>,
    resolved: &Resolved,
    auto_populate: bool,
    now_ms: i64,
) -> Result<Replacement, DbErr> {
    let Some(stored) = call::Entity::find_by_id(call_id).one(db).await? else {
        return Ok(Replacement::Stored(
            insert_call(db, new, audio, resolved, auto_populate, now_ms).await?,
        ));
    };

    let system_id = stored.system_id;
    let mut row: call::ActiveModel = stored.into();
    // Everything this copy's Recorder said about the transmission — the same
    // mapping the insert uses, so the two cannot describe a Call differently.
    describe_transmission(&mut row, new, audio);
    // A Site only when this copy named one: a multi-site System hears one
    // transmission on several towers, and the copy that won says which tower
    // the audio a Listener now gets came off. A copy that named none knows
    // nothing about towers and must not erase what the other copy knew. It is
    // outside `describe_transmission` because it is the one such column that
    // costs a query, and because that function must stay pure enough for the
    // insert to call it before the row exists.
    if let Some(site_id) = site_of(db, system_id, new, now_ms).await? {
        row.site_id = Set(Some(site_id));
    }
    // **`talkgroup_ref` is deliberately not touched.** It records the Ref the
    // recorder sent, and its one consumer is unmerge (#45): "which of this
    // channel's Calls arrived under the Ref being unfolded". The stored Call is
    // the one a Listener has always seen, on the channel it has always been on,
    // so unfolding must give it back to the Ref *it* arrived under — not to the
    // one a later copy happened to name. Every other column here describes the
    // audio, which is why every other column moves.
    let replaced = row.update(db).await?;

    // The signal detail belongs to the audio, so it is this copy's outright.
    call_frequency::Entity::delete_many()
        .filter(call_frequency::Column::CallId.eq(call_id))
        .exec(db)
        .await?;
    call_unit::Entity::delete_many()
        .filter(call_unit::Column::CallId.eq(call_id))
        .exec(db)
        .await?;
    write_signal_detail(db, call_id, new).await?;

    // The patch membership does not: it is what the transmission *reached*,
    // and the surviving Call stands for every copy of it.
    let already: Vec<i64> = call_patch::Entity::find()
        .filter(call_patch::Column::CallId.eq(call_id))
        .select_only()
        .column(call_patch::Column::TalkgroupRef)
        .into_tuple()
        .all(db)
        .await?;
    // Iterated rather than matched on: ingest resolves these *before* deciding
    // anything (the widened duplicate test is asked over them) and ingest is the
    // only thing that replaces a Call, so the unresolved case is a caller that
    // named no patches and the union simply adds nothing. An arm for it would be
    // one no production input can reach.
    let added: Vec<i64> = resolved
        .patches
        .iter()
        .flatten()
        .filter(|patch| !already.contains(patch))
        .copied()
        .collect();
    write_patches(db, call_id, &added).await?;

    roster_units(db, system_id, new, auto_populate, now_ms).await?;

    Ok(Replacement::Replaced(replaced))
}

/// Write everything a Recorder said about **this transmission** onto a Call
/// row: the audio object it produced, and every column that describes what was
/// heard rather than which channel it was on or when.
///
/// **One mapping, two writers** (#46). [`insert_call`] uses it for a Call
/// arriving for the first time and [`store_replacement`] for a better copy of
/// one already stored, and the two must agree exactly — a replacement that
/// carried the losing copy's `error_count` or `duration_ms` would leave the
/// Archive describing audio nobody holds any more. Written twice, a field added
/// by a later ticket would silently reach only one of them, and nothing about
/// either path would look wrong; that is the same failure the shipped Trunk
/// Recorder artifacts have a whole test to prevent (CLAUDE.md's two-artifact
/// rule), applied to the two ways a row is written here.
///
/// Deliberately *not* the identity columns — `system_id`, `talkgroup_id`,
/// `talkgroup_ref`, `call_at_ms`, `created_at_ms`, `site_id` — which the two
/// writers treat differently on purpose: a replacement keeps the stored Call's.
fn describe_transmission(row: &mut call::ActiveModel, new: &NewCall, audio: Option<StoredAudio>) {
    // An **Encrypted Call** has no object: the empty key is what the serve path
    // and the wire both read as "there is nothing here", and a `NULL` size is
    // what keeps retention's cap counting only what exists.
    row.object_key = Set(audio
        .as_ref()
        .map(|a| a.key().to_owned())
        .unwrap_or_default());
    row.audio_size = Set(audio.as_ref().map(StoredAudio::bytes));
    row.audio_mime = Set(new.audio_mime.clone());
    row.audio_name = Set(new.audio_name.clone());
    row.duration_ms = Set(new.duration_ms);
    row.stop_at_ms = Set(new.stop_at_ms);
    row.emergency = Set(new.emergency);
    row.encrypted = Set(new.encrypted);
    row.priority = Set(new.priority);
    row.audio_type = Set(new.audio_type.clone());
    row.frequency = Set(new.frequency);
    // Whether this copy's audio has been looked inside (#48). A **Replacement**
    // brings a different Recorder's file with its own tag, and ingest mined it
    // in the same pass it read its duration — so the stamp travels with every
    // other fact the winner's audio carries.
    row.mined_at_ms = Set(new.mined_at_ms);
    // Every Call arrives passthrough, and a replaced one goes back to it: the
    // levelled audio a previous pass produced describes a copy that no longer
    // exists. Set explicitly rather than left to a column default, because a
    // fresh database gets its `calls` table from the entity-derived DDL in
    // `m0001_init`, which carries no defaults — only an upgraded database goes
    // through `m0006`'s `ALTER`.
    row.enhancement = Set(call::EnhancementState::NONE.to_string());
}

/// The patch rows for a Call, given the canonical Refs [`resolve_patches`]
/// settled on.
async fn write_patches<C: ConnectionTrait>(
    db: &C,
    call_id: CallId,
    patched: &[i64],
) -> Result<(), DbErr> {
    for patch in patched {
        call_patch::ActiveModel {
            call_id: Set(call_id),
            talkgroup_ref: Set(*patch),
            ..Default::default()
        }
        .insert(db)
        .await?;
    }
    Ok(())
}

/// The per-Unit and per-frequency detail a Recorder sent about a Call.
///
/// Shared by [`insert_call`] and [`store_replacement`] because a better copy of
/// a transmission brings its own signal detail with it, and the two must write
/// it identically — a replacement whose `error_count` rows came from the losing
/// copy would leave the Archive describing audio nobody holds any more (#46).
async fn write_signal_detail<C: ConnectionTrait>(
    db: &C,
    call_id: CallId,
    new: &NewCall,
) -> Result<(), DbErr> {
    for u in &new.units {
        call_unit::ActiveModel {
            call_id: Set(call_id),
            unit_ref: Set(u.unit_ref),
            label: Set(u.label.clone()),
            offset_ms: Set(u.offset_ms),
            tag_ota: Set(u.tag_ota.clone()),
            emergency: Set(u.emergency),
            signal_system: Set(u.signal_system.clone()),
            at_ms: Set(u.at_ms),
            ..Default::default()
        }
        .insert(db)
        .await?;
    }
    for f in &new.frequencies {
        call_frequency::ActiveModel {
            call_id: Set(call_id),
            freq: Set(f.freq),
            pos_ms: Set(f.pos_ms),
            len_ms: Set(f.len_ms),
            dbm: Set(f.dbm),
            error_count: Set(f.error_count),
            spike_count: Set(f.spike_count),
            at_ms: Set(f.at_ms),
            ..Default::default()
        }
        .insert(db)
        .await?;
    }
    Ok(())
}

/// Unit roster (#8): a heard radio becomes a Unit entity only when the recorder
/// gave it an alias — rdio rosters units with a non-empty label
/// (`controller.go`), not every anonymous Ref. Gated on auto-populate like
/// rdio; the per-call `call_units` detail is always recorded regardless.
///
/// **Either alias names it** ([`NewCallUnit::name`], #47): the configured one,
/// or the one the radio broadcast about itself. Before #47 only the configured
/// one counted, so the zero-configuration install this roster exists for — a
/// Trunk Recorder with no unit file, an SDRTrunk sending `talkerAlias` on every
/// upload — never named a single radio, however loudly the radios named
/// themselves.
async fn roster_units<C: ConnectionTrait>(
    db: &C,
    system_id: i64,
    new: &NewCall,
    auto_populate: bool,
    now_ms: i64,
) -> Result<(), DbErr> {
    if auto_populate {
        for u in &new.units {
            if let Some(name) = u.name().filter(|_| u.unit_ref > 0) {
                let name = Some(name.to_string());
                resolve_or_create_unit(db, system_id, u.unit_ref, name, now_ms).await?;
            }
        }
    }
    Ok(())
}

/// Which of `refs` are Talkgroups this System has — the patch members (#81) —
/// each resolved to the **canonical** channel that owns it (#45), in the order
/// the recorder sent them.
///
/// The rest are dropped. A recorder's `patches` array is not a list of Talkgroup
/// Refs and cannot be read as one: SDRTrunk appends the patch group's radio IDs
/// behind its talkgroups in the same flat array, under the same field name Trunk
/// Recorder's uploader uses, with nothing between them
/// (`RdioScannerBroadcaster.java:546-574`). What the System knows is the only
/// discriminator there is, and it is the one rdio-scanner uses
/// (`call.go:572-582`).
///
/// **The resolved list is deduplicated, where the raw one was not.** This is the
/// array that produces rdio's issue #466 — a console minting a fresh TGID per
/// patch event floods the panel with duplicate buttons — so once two member Refs
/// of one channel both appear here, emitting that channel twice would put the
/// churn straight back on the wire that merging it was meant to take it off.
/// Two arrivals of the same *number* collapse for the same reason: a patches
/// array names the channels a Call also reaches, and reaching one twice is not a
/// fact about anything.
///
/// **The Call's own `talkgroup_ref` is a member whether or not a row exists for
/// it yet** (#46). The discriminator above exists to tell a Talkgroup Ref from a
/// radio ID in a flat array, and for this one number there is independent
/// evidence: the recorder named it in the `talkgroup` field, which is a
/// Talkgroup by definition. It matters because this is the Ref auto-populate is
/// *about to create* — resolving the array before the Talkgroup exists (which is
/// what #46 moved it to, so the duplicate decision can see it) would otherwise
/// drop a channel's own Ref from the first Call on it and keep it on every Call
/// after, which is the kind of difference nobody finds for months.
///
/// Three statements at worst and one at best: the primaries, then — only when
/// some ref was not one — the member rows and their owners, each in a single
/// query. An instance with no merges pays exactly what it did before, and the
/// empty array (almost every Call) still costs nothing at all.
async fn patch_members<C: ConnectionTrait>(
    db: &C,
    system_id: i64,
    refs: &[i64],
    talkgroup_ref: i64,
) -> Result<PatchMembers, DbErr> {
    if refs.is_empty() {
        return Ok(PatchMembers::default());
    }
    // Arriving Ref -> the primary Ref of the channel that owns it. A primary Ref
    // owns itself, which is why the first pass maps each hit to itself.
    let mut canonical: HashMap<i64, i64> = talkgroup::Entity::find()
        .filter(talkgroup::Column::SystemId.eq(system_id))
        .filter(talkgroup::Column::Ref.is_in(refs.iter().copied()))
        .select_only()
        .column(talkgroup::Column::Ref)
        .into_tuple::<i64>()
        .all(db)
        .await?
        .into_iter()
        .map(|primary| (primary, primary))
        .collect();

    // Only the Refs the first pass could not place. Asking about the rest would
    // be asking a question whose answer must be ignored — a member Ref never
    // overrides a primary one ([`resolve_talkgroup`]'s precedence) — and it
    // keeps the `IN` list to the Refs that actually need one.
    let unplaced: Vec<i64> = refs
        .iter()
        .copied()
        .filter(|r| !canonical.contains_key(r))
        .collect();
    if !unplaced.is_empty() {
        let members: Vec<(i64, i64)> = talkgroup_ref::Entity::find()
            .filter(talkgroup_ref::Column::SystemId.eq(system_id))
            .filter(talkgroup_ref::Column::Ref.is_in(unplaced))
            .select_only()
            .column(talkgroup_ref::Column::Ref)
            .column(talkgroup_ref::Column::TalkgroupId)
            .into_tuple()
            .all(db)
            .await?;
        // One statement for the owners, not one per member Ref: this runs
        // inside the transaction that already holds the audio write
        // (ADR-0001), and on a Postgres backend every extra statement is a
        // round-trip a Pi pays for.
        let owner_refs: std::collections::HashMap<i64, i64> = talkgroup::Entity::find()
            .filter(talkgroup::Column::Id.is_in(members.iter().map(|(_, id)| *id)))
            .select_only()
            .column(talkgroup::Column::Id)
            .column(talkgroup::Column::Ref)
            .into_tuple()
            .all(db)
            .await?
            .into_iter()
            .collect();
        for (member_ref, talkgroup_id) in members {
            if let Some(&owner_ref) = owner_refs.get(&talkgroup_id) {
                canonical.insert(member_ref, owner_ref);
            }
        }
    }

    // The Call's own Ref, as a **last resort** — after both queries have had
    // their say, so that a Ref which already resolves (to itself, or to the
    // channel that owns it as a member Ref) keeps the answer they gave. What
    // this adds is only the case where nothing owns it *yet*, which is the
    // Talkgroup auto-populate is about to create for this very Call.
    canonical.entry(talkgroup_ref).or_insert(talkgroup_ref);

    Ok(patch_members_of(refs, &canonical))
}

/// Which of `refs` this System claims, given the arriving-Ref → owning-Ref map
/// the queries above produced — the decision half of [`patch_members`] (#96).
///
/// Pure, so every shape a recorder can send is a value a table names: the radio
/// tail SDRTrunk appends, a member Ref resolving to its owner, and two Refs that
/// turn out to be one channel.
fn patch_members_of(refs: &[i64], canonical: &std::collections::HashMap<i64, i64>) -> PatchMembers {
    let mut found = PatchMembers::default();
    for arrived_as in refs {
        let Some(&owner) = canonical.get(arrived_as) else {
            found.dropped += 1;
            continue;
        };
        if found.members.contains(&owner) {
            found.collapsed += 1;
        } else {
            found.members.push(owner);
        }
    }
    found
}

/// What [`patch_members`] made of a recorder's `patches` array.
///
/// The two counts are separate facts and read as separate problems: `dropped`
/// says the System has never heard of a ref (SDRTrunk's radio-id tail, or a
/// patch to a channel this instance does not carry), while `collapsed` says two
/// refs named one channel — which is channel merge working, not a loss.
#[derive(Debug, Default)]
struct PatchMembers {
    /// The canonical primary Refs this Call is patched to, deduplicated, in the
    /// order the recorder first named each of them.
    members: Vec<i64>,
    /// Refs no Talkgroup on this System claims.
    dropped: usize,
    /// Refs that resolved to a channel already in `members`.
    collapsed: usize,
}

/// The Talkgroup a Ref belongs to on this System, primary Ref or member Ref
/// (#45). A Ref is unique only within its System.
///
/// **Primary before member**, and the order is load-bearing rather than
/// defensive. The two sets are kept disjoint on the way in — the importer
/// refuses a member Ref that some Talkgroup already holds as its primary — but a
/// database an operator has edited by hand is not bound by that, and the answer
/// that surprises nobody is the one where a channel's own number still means
/// itself. Resolving to the member owner instead would make a Talkgroup's Calls
/// silently land somewhere else.
///
/// The second query costs one statement and is only spent when the first found
/// nothing, which on an archive with no merges is exactly the case ingest was
/// already paying for a moment later in `create_populated_talkgroup`.
pub async fn resolve_talkgroup<C: ConnectionTrait>(
    db: &C,
    system_id: i64,
    ext_ref: i64,
) -> Result<Option<talkgroup::Model>, DbErr> {
    if let Some(primary) = talkgroup::Entity::find()
        .filter(talkgroup::Column::SystemId.eq(system_id))
        .filter(talkgroup::Column::Ref.eq(ext_ref))
        .one(db)
        .await?
    {
        return Ok(Some(primary));
    }
    let Some(member) = talkgroup_ref::Entity::find()
        .filter(talkgroup_ref::Column::SystemId.eq(system_id))
        .filter(talkgroup_ref::Column::Ref.eq(ext_ref))
        .one(db)
        .await?
    else {
        return Ok(None);
    };
    talkgroup::Entity::find_by_id(member.talkgroup_id)
        .one(db)
        .await
}

/// Create a Talkgroup, auto-populating (#8) any field the recorder left blank
/// with rdio-scanner's defaults: the numeric Ref as the label, `Talkgroup <ref>`
/// as the name, the `Untagged` Tag, and the `Unknown` Group. The Tag and Group
/// rows are created here (not for existing Talkgroups) so curated archives aren't
/// polluted with defaults on every subsequent call.
async fn create_populated_talkgroup<C: ConnectionTrait>(
    db: &C,
    system_id: i64,
    new: &NewTalkgroup<'_>,
    now_ms: i64,
) -> Result<talkgroup::Model, DbErr> {
    let tag_name = new.tag.unwrap_or(DEFAULT_TAG);
    let tag_id = resolve_or_create_tag(db, tag_name, now_ms).await?.id;
    let label = new
        .label
        .map(str::to_string)
        .unwrap_or_else(|| new.talkgroup_ref.to_string());
    let name = new
        .name
        .map(str::to_string)
        .unwrap_or_else(|| format!("Talkgroup {}", new.talkgroup_ref));

    let tg = talkgroup::ActiveModel {
        system_id: Set(system_id),
        r#ref: Set(new.talkgroup_ref),
        label: Set(Some(label)),
        name: Set(Some(name)),
        tag_id: Set(Some(tag_id)),
        // `led` is left unset (NULL) — LED colours are assigned by curation (#18).
        created_at_ms: Set(now_ms),
        ..Default::default()
    }
    .insert(db)
    .await?;

    let group_labels: Vec<&str> = if new.groups.is_empty() {
        vec![DEFAULT_GROUP]
    } else {
        new.groups.iter().map(String::as_str).collect()
    };
    for group_name in group_labels {
        let grp = resolve_or_create_group(db, group_name, now_ms).await?;
        link_talkgroup_group(db, tg.id, grp.id).await?;
    }

    Ok(tg)
}

// ---------------------------------------------------------------------------
// Channel merge (#45, spec US 15–18)
// ---------------------------------------------------------------------------

/// What changing a Talkgroup's member Refs actually moved.
///
/// Counts rather than a boolean because a fold is the one curation act that
/// touches an operator's *archive* and not just their configuration — "this will
/// re-point 1,412 Calls" is the sentence a preview has to be able to say (#18's
/// dry run, and #50's confirmation step).
///
/// And [`moved`](Self::moved) beside the counts, because "1 folded" is the same
/// sentence whichever channel went. #50 needs to name the row it is about to
/// destroy *before* it destroys it, and ADR-0011 wants the ids of what moved on
/// the line it leaves behind — one structure rather than two that could disagree
/// about what a fold did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MergeChange {
    /// Talkgroups that stopped existing in their own right.
    pub folded: u64,
    /// Member Refs that became Talkgroups again.
    pub unfolded: u64,
    /// Calls whose owning Talkgroup changed, in either direction.
    pub calls_repointed: u64,
    /// Every Ref that changed hands, in the order it was asked for.
    pub moved: Vec<Moved>,
}

impl MergeChange {
    /// Did anything happen? A row whose member Refs already read as the CSV says
    /// is `unchanged`, which is what makes re-importing a curated file a no-op.
    pub fn is_empty(&self) -> bool {
        *self == MergeChange::default()
    }

    fn add(&mut self, other: MergeChange) {
        self.folded += other.folded;
        self.unfolded += other.unfolded;
        self.calls_repointed += other.calls_repointed;
        self.moved.extend(other.moved);
    }

    /// **Write down what this merge moved** (#50, ADR-0011 rule 7).
    ///
    /// A merge is the one curation act that rewrites the **archive** rather than
    /// the configuration, so it leaves a line an Operator reading journald can
    /// find — the report only ever reaches whoever posted the CSV or clicked the
    /// button. INFO because nothing was rejected and no Call was destroyed: a
    /// Talkgroup row went, and unfolding brings it back.
    ///
    /// **One callsite for one message**, which is #92's rule applied a layer up.
    /// Two surfaces reach this (the CSV importer and the browser), and while they
    /// each had their own `tracing::info!` they had already drifted into
    /// different field sets under an identical message — so a log an Operator
    /// greps said different things depending on which door the merge came
    /// through, and a test matching on the message could not tell them apart.
    ///
    /// Silent when nothing moved: a form submitted twice, or a re-imported file
    /// whose merges all already applied, is not a merge action.
    pub fn record(&self, talkgroup_id: i64, talkgroup_ref: i64, dry_run: bool) {
        if self.is_empty() {
            return;
        }
        // Built here rather than inside the macro: a `tracing` field expression
        // is expanded into machinery that runs only when a subscriber is
        // interested — right for a hot path, and it makes these two lists look
        // unreachable to coverage.
        let refs = joined(self.moved.iter().map(|moved| Some(moved.r#ref)));
        let talkgroup_ids = joined(self.moved.iter().map(|moved| moved.talkgroup_id));
        tracing::info!(
            talkgroup_id,
            talkgroup_ref,
            folded = self.folded,
            unfolded = self.unfolded,
            calls_repointed = self.calls_repointed,
            %refs,
            %talkgroup_ids,
            dry_run,
            "talkgroup member Refs changed"
        );
    }
}

/// The ids on a merge's log line — comma-separated, and `-` where a Ref named no
/// channel.
///
/// Positional, so `refs` and `talkgroup_ids` line up entry by entry. Dropping the
/// empty ones would shift every id after them onto the wrong Ref, which is the
/// one way a line about a merge could mislead rather than merely omit.
fn joined(values: impl Iterator<Item = Option<i64>>) -> String {
    values
        .map(|value| match value {
            Some(id) => id.to_string(),
            None => String::from("-"),
        })
        .collect::<Vec<_>>()
        .join(",")
}

/// Which way one Ref went.
///
/// [`Recorded`](Movement::Recorded) is the arm the counts cannot express:
/// naming a Ref nothing has been heard on yet is not a fold — no channel was
/// absorbed and no Call moved — but it *is* a member Ref where there was none,
/// and it is what a Ref belonging to another **System** looks like from here.
/// Telling it apart from a fold is the whole reason a preview is worth showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Movement {
    /// A channel was absorbed and its Calls carried across.
    Folded,
    /// A Ref was written down, ready for traffic. Nothing existed to absorb.
    Recorded,
    /// A member Ref became a channel again, with the Calls that arrived under it.
    Unfolded,
}

/// One Ref that changed hands, and what came with it.
///
/// Deliberately **not** a wire shape — the data layer builds no view (#98), so
/// the JSON a browser reads is [`crate::curate::members::MovedRow`], made from
/// this.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Moved {
    pub r#ref: i64,
    pub movement: Movement,
    /// The Talkgroup this is about: the one absorbed by a fold, or the one an
    /// unfold restored. `None` for a [`Movement::Recorded`] Ref, which is
    /// exactly the fact worth showing about one.
    ///
    /// On a **dry run** a restored id is provisional — the row is rolled back
    /// with everything else — so it is the Ref, never this, that a preview
    /// renders.
    pub talkgroup_id: Option<i64>,
    /// What that channel called itself, so a confirmation reads "TAC 3" rather
    /// than a number the operator has to go and look up.
    pub label: Option<String>,
    /// Calls this Ref moved. The number the whole preview exists for.
    pub calls: u64,
    /// Member Refs the absorbed channel owned, which come across with it. The
    /// CSV importer refuses this outright so a file keeps describing what it
    /// made; a form has no file to round-trip, so it applies and says so.
    pub carried: Vec<i64>,
}

/// The Talkgroup that owns `ext_ref` as a **member** Ref on this System, if any.
///
/// Distinct from [`resolve_talkgroup`], which answers "which channel does this
/// Ref reach" and is happy to answer with the Ref's own Talkgroup. This asks the
/// narrower question the importer needs: *is this Ref spoken for by somebody
/// else's member list*, which a primary Ref never is.
pub async fn member_ref_owner<C: ConnectionTrait>(
    db: &C,
    system_id: i64,
    ext_ref: i64,
) -> Result<Option<talkgroup_ref::Model>, DbErr> {
    talkgroup_ref::Entity::find()
        .filter(talkgroup_ref::Column::SystemId.eq(system_id))
        .filter(talkgroup_ref::Column::Ref.eq(ext_ref))
        .one(db)
        .await
}

/// **Would taking `candidate` as a member Ref steal it from another channel?**
///
/// Stealing one would silently break that channel to fix this one, so both
/// surfaces refuse it — the CSV importer as `member-ref-owned-elsewhere` and the
/// browser as `talkgroup-ref-taken`. The *rule* is written here once because the
/// two are one merge policy wearing two error shapes, and a copy each is how
/// they come to disagree about what a fold may take (`curate::systems`'s
/// blacklist parser is the same argument).
///
/// Three things it is deliberately **not**:
///
/// - **A primary Ref is not a conflict.** Absorbing a channel is exactly what a
///   fold is; only somebody else's *member* list is spoken for.
/// - **The owner's own members are not a conflict**, which `owner_id` says.
/// - **A chain fold is not a conflict.** If the channel holding `candidate` is
///   itself named in `wanted`, the Ref arrives with it. What the two surfaces
///   then *do* differs — the importer refuses until the cell lists the carried
///   Refs so a file keeps describing what it made; a form has no file to
///   round-trip and applies it — but that is a policy above this predicate, not
///   inside it.
pub async fn member_ref_held_elsewhere<C: ConnectionTrait>(
    db: &C,
    system_id: i64,
    owner_id: Option<i64>,
    candidate: i64,
    wanted: &[i64],
) -> Result<bool, DbErr> {
    let Some(elsewhere) = member_ref_owner(db, system_id, candidate)
        .await?
        .filter(|member| Some(member.talkgroup_id) != owner_id)
    else {
        return Ok(false);
    };
    let holder = talkgroup::Entity::find_by_id(elsewhere.talkgroup_id)
        .one(db)
        .await?;

    Ok(!holder.is_some_and(|holder| wanted.contains(&holder.r#ref)))
}

/// The member Refs a Talkgroup answers to, in the operator's order.
pub async fn member_refs_of<C: ConnectionTrait>(
    db: &C,
    talkgroup_id: i64,
) -> Result<Vec<i64>, DbErr> {
    Ok(talkgroup_ref::Entity::find()
        .filter(talkgroup_ref::Column::TalkgroupId.eq(talkgroup_id))
        .order_by_asc(talkgroup_ref::Column::Position)
        .all(db)
        .await?
        .into_iter()
        .map(|member| member.r#ref)
        .collect())
}

/// Make `owner`'s member Refs exactly `wanted`, folding and unfolding as needed.
///
/// The set semantics are [`replace_groups`](crate::import)'s, deliberately: a
/// non-empty cell in a CSV **is** the whole truth for the row it names, so
/// removing a Ref from the list is how an operator unmerges. `owner`'s own
/// primary Ref is ignored if it appears — a channel answering to its own number
/// is true but not a *member*, and rejecting it would fail a file that is merely
/// redundant.
///
/// Unfolds run before folds, so a Ref moving from one owner to another inside a
/// single import cannot collide on the uniqueness index halfway through.
///
/// Not internally transactional: the caller owns the boundary, because an import
/// applies many rows together or not at all and a fold that half-happened would
/// leave Calls pointing at a Talkgroup that no longer exists.
pub async fn set_member_refs<C: ConnectionTrait>(
    db: &C,
    owner: &talkgroup::Model,
    wanted: &[i64],
    now_ms: i64,
) -> Result<MergeChange, DbErr> {
    let wanted: Vec<i64> = {
        let mut seen = Vec::new();
        for &r in wanted {
            if r != owner.r#ref && !seen.contains(&r) {
                seen.push(r);
            }
        }
        seen
    };
    let current: Vec<talkgroup_ref::Model> = talkgroup_ref::Entity::find()
        .filter(talkgroup_ref::Column::TalkgroupId.eq(owner.id))
        .order_by_asc(talkgroup_ref::Column::Position)
        .all(db)
        .await?;

    let mut change = MergeChange::default();
    for gone in current.iter().filter(|m| !wanted.contains(&m.r#ref)) {
        change.add(unfold_ref(db, owner, gone, now_ms).await?);
    }
    for (position, &arrived) in wanted.iter().enumerate() {
        let position = position as i32;
        match current.iter().find(|member| member.r#ref == arrived) {
            // Already a member: the CSV is still the authority on the *order*
            // an operator sees them in, so a re-ordered file re-orders the list.
            // Not counted as a change — order is presentation, and a file whose
            // merges all already applied should still report `unchanged`.
            Some(member) if member.position != position => {
                talkgroup_ref::ActiveModel {
                    id: Set(member.id),
                    position: Set(position),
                    ..Default::default()
                }
                .update(db)
                .await?;
            }
            Some(_) => {}
            None => change.add(fold_ref(db, owner, arrived, position, now_ms).await?),
        }
    }

    // **A chain fold is the one way a member arrives without being asked for**,
    // and it arrives carrying the position it held on the channel it came from —
    // which collides with a position this loop just assigned. Two rows sharing
    // one leaves the rendered order down to whatever the database returns first,
    // and the two dialects need not agree (ADR-0003). Harmless while the order
    // was only a CSV column's; #50 puts it on a screen.
    //
    // Guarded, so the steady-state cost is unchanged: nothing else can leave a
    // member outside `wanted`, since everything else there was got unfolded.
    if change.moved.iter().any(|moved| !moved.carried.is_empty()) {
        renumber_members(db, owner, &wanted).await?;
    }
    Ok(change)
}

/// Make the owner's member positions dense and unique: the Refs the operator
/// named, in their order, then whatever a fold carried in behind them.
///
/// Carried Refs keep their relative order (by the position they had, then by
/// Ref) rather than being interleaved, because the operator did not put them
/// there and a list that reshuffles what they *did* write is worse than one with
/// a tail they did not.
async fn renumber_members<C: ConnectionTrait>(
    db: &C,
    owner: &talkgroup::Model,
    wanted: &[i64],
) -> Result<(), DbErr> {
    let mut held: Vec<talkgroup_ref::Model> = talkgroup_ref::Entity::find()
        .filter(talkgroup_ref::Column::TalkgroupId.eq(owner.id))
        .all(db)
        .await?;
    held.sort_by_key(|member| {
        (
            wanted
                .iter()
                .position(|named| *named == member.r#ref)
                .unwrap_or(usize::MAX),
            member.position,
            member.r#ref,
        )
    });

    for (position, member) in held.iter().enumerate() {
        let position = position as i32;
        if member.position != position {
            talkgroup_ref::ActiveModel {
                id: Set(member.id),
                position: Set(position),
                ..Default::default()
            }
            .update(db)
            .await?;
        }
    }
    Ok(())
}

/// Give `owner` another Ref to answer to, absorbing the Talkgroup that Ref names
/// if there is one (spec US 17).
///
/// Two cases, and only one of them is a *fold*: naming a Ref nothing has been
/// heard on yet simply records it, ready for the traffic; naming a Ref that is
/// already an auto-populated channel carries that channel's history across and
/// removes it from the panel. The second is the case the feature exists for, and
/// the operator does not have to know which they are doing.
///
/// The absorbed Talkgroup's own member Refs come with it. A Ref left pointing at
/// a deleted Talkgroup would be a row resolution could never resolve, and
/// dropping them instead would silently un-merge somebody's earlier work.
async fn fold_ref<C: ConnectionTrait>(
    db: &C,
    owner: &talkgroup::Model,
    member_ref: i64,
    position: i32,
    now_ms: i64,
) -> Result<MergeChange, DbErr> {
    let source = talkgroup::Entity::find()
        .filter(talkgroup::Column::SystemId.eq(owner.system_id))
        .filter(talkgroup::Column::Ref.eq(member_ref))
        .one(db)
        .await?;

    let mut change = MergeChange::default();
    let mut label = None;
    let mut moved = Moved {
        r#ref: member_ref,
        movement: Movement::Recorded,
        talkgroup_id: None,
        label: None,
        calls: 0,
        carried: Vec::new(),
    };
    if let Some(source) = source {
        // Read before the fold moves them onto the owner, where they become
        // indistinguishable from the owner's own. One statement, and only on the
        // arm that absorbed a channel — a Ref merely being written down asks the
        // database nothing extra.
        moved.carried = member_refs_of(db, source.id).await?;
        // The Ref each Call arrived under, for the ones stored before the column
        // existed. Written *before* the move, while "which Talkgroup was this"
        // is still answerable — afterwards it is the owner's, and the fold would
        // be irreversible. Two plain statements rather than one `COALESCE`
        // update: both dialects agree about `IS NULL`, and ADR-0003's rule is to
        // spend a statement rather than a divergence.
        call::Entity::update_many()
            .col_expr(call::Column::TalkgroupRef, Expr::value(source.r#ref))
            .filter(call::Column::TalkgroupId.eq(source.id))
            .filter(call::Column::TalkgroupRef.is_null())
            .exec(db)
            .await?;
        change.calls_repointed = call::Entity::update_many()
            .col_expr(call::Column::TalkgroupId, Expr::value(owner.id))
            .filter(call::Column::TalkgroupId.eq(source.id))
            .exec(db)
            .await?
            .rows_affected;

        // The patch rows of Calls already archived name the folded number too,
        // and nothing resolves them on the way out (`stored_calls` reads them
        // verbatim). Left alone, every one of those Calls keeps serving a chip
        // for a channel the panel no longer offers — the duplicate-button
        // problem this feature exists to remove, preserved in the archive.
        //
        // Scoped by System through a subquery because `call_patches` carries no
        // System of its own and a Ref is only unique within one. Collisions go
        // first: a Call patched to *both* Refs — what a console re-broadcasting
        // through a minted TGID actually produces — would otherwise end up with
        // the same chip twice.
        let calls_of_system = || {
            sea_orm::sea_query::Query::select()
                .column(call::Column::Id)
                .from(call::Entity)
                .and_where(Expr::col(call::Column::SystemId).eq(owner.system_id))
                .to_owned()
        };
        call_patch::Entity::delete_many()
            .filter(call_patch::Column::TalkgroupRef.eq(source.r#ref))
            .filter(call_patch::Column::CallId.in_subquery(calls_of_system()))
            .filter(
                call_patch::Column::CallId.in_subquery(
                    sea_orm::sea_query::Query::select()
                        .column(call_patch::Column::CallId)
                        .from(call_patch::Entity)
                        .and_where(Expr::col(call_patch::Column::TalkgroupRef).eq(owner.r#ref))
                        .to_owned(),
                ),
            )
            .exec(db)
            .await?;
        call_patch::Entity::update_many()
            .col_expr(call_patch::Column::TalkgroupRef, Expr::value(owner.r#ref))
            .filter(call_patch::Column::TalkgroupRef.eq(source.r#ref))
            .filter(call_patch::Column::CallId.in_subquery(calls_of_system()))
            .exec(db)
            .await?;

        talkgroup_ref::Entity::update_many()
            .col_expr(talkgroup_ref::Column::TalkgroupId, Expr::value(owner.id))
            .filter(talkgroup_ref::Column::TalkgroupId.eq(source.id))
            .exec(db)
            .await?;
        talkgroup_group::Entity::delete_many()
            .filter(talkgroup_group::Column::TalkgroupId.eq(source.id))
            .exec(db)
            .await?;
        talkgroup::Entity::delete_by_id(source.id).exec(db).await?;

        moved.movement = Movement::Folded;
        moved.talkgroup_id = Some(source.id);
        moved.label = source.label.clone();
        moved.calls = change.calls_repointed;
        label = source.label;
        change.folded = 1;
    }
    change.moved.push(moved);

    // The Ref may already be a member — carried across a moment ago by the fold
    // of a channel that owned it (see the guard in `crate::import`, which is why
    // it is always one the caller also asked for). Then there is nothing to
    // insert and only its place in the operator's order to settle.
    //
    // Written unconditionally rather than behind an `if it moved`: this runs
    // once per Ref of one curation action, where `set_member_refs` compares
    // before writing because it walks the whole set on every re-import.
    if let Some(held) = talkgroup_ref::Entity::find()
        .filter(talkgroup_ref::Column::TalkgroupId.eq(owner.id))
        .filter(talkgroup_ref::Column::Ref.eq(member_ref))
        .one(db)
        .await?
    {
        talkgroup_ref::ActiveModel {
            id: Set(held.id),
            position: Set(position),
            ..Default::default()
        }
        .update(db)
        .await?;
        return Ok(change);
    }

    talkgroup_ref::ActiveModel {
        talkgroup_id: Set(owner.id),
        system_id: Set(owner.system_id),
        r#ref: Set(member_ref),
        position: Set(position),
        // What the channel called itself, so unfolding gives an operator back
        // the name they curated rather than the number a recorder sent. `NULL`
        // for a Ref that was never a Talkgroup of its own, which unfolds to the
        // auto-populate default exactly as a first sighting would.
        label: Set(label),
        created_at_ms: Set(now_ms),
        ..Default::default()
    }
    .insert(db)
    .await?;
    Ok(change)
}

/// Give a member Ref back its own Talkgroup, and its own Calls with it.
///
/// Takes the member row rather than looking it up: every caller has just read
/// the owner's whole set to decide what to unfold, so a second query would only
/// buy an unreachable "not found" arm to leave untested.
///
/// **Patch rows do not come back.** A `call_patches` row records a Ref with no
/// note of what it arrived as, so once a fold has rewritten one there is nothing
/// to tell it from a chip the recorder always sent. Calls are entities and get
/// their provenance column; a Patch is a property a Call carries (CONTEXT.md),
/// and one that churns by design — the honest trade, rather than a second
/// provenance column on a table that exists to be rewritten.
///
/// The Calls that come back are exactly the ones that *arrived* under this Ref —
/// which is the whole reason [`call::Model::talkgroup_ref`] is recorded. The
/// restored row is built the way auto-populate would build it, so an unfolded
/// channel is indistinguishable from one the recorder had just discovered: same
/// `Untagged` Tag, same `Unknown` Group, same `Talkgroup <ref>` name. Only the
/// label is carried across, because it is the field an operator is most likely
/// to have curated and the one a bare number replaces most painfully.
async fn unfold_ref<C: ConnectionTrait>(
    db: &C,
    owner: &talkgroup::Model,
    member: &talkgroup_ref::Model,
    now_ms: i64,
) -> Result<MergeChange, DbErr> {
    let restored = create_populated_talkgroup(
        db,
        owner.system_id,
        &NewTalkgroup {
            talkgroup_ref: member.r#ref,
            label: member.label.as_deref(),
            // Everything else falls back to rdio-scanner's defaults, as it did
            // when this Ref was first heard.
            name: None,
            tag: None,
            groups: &[],
        },
        now_ms,
    )
    .await?;

    let calls_repointed = call::Entity::update_many()
        .col_expr(call::Column::TalkgroupId, Expr::value(restored.id))
        .filter(call::Column::TalkgroupId.eq(owner.id))
        .filter(call::Column::TalkgroupRef.eq(member.r#ref))
        .exec(db)
        .await?
        .rows_affected;

    talkgroup_ref::Entity::delete_by_id(member.id)
        .exec(db)
        .await?;
    Ok(MergeChange {
        folded: 0,
        unfolded: 1,
        calls_repointed,
        moved: vec![Moved {
            r#ref: member.r#ref,
            movement: Movement::Unfolded,
            talkgroup_id: Some(restored.id),
            label: member.label.clone(),
            calls: calls_repointed,
            carried: Vec::new(),
        }],
    })
}

/// Give a Unit another span of Refs to answer to (#45, spec US 16), refusing one
/// that collides with a Range already owned on this System.
///
/// **The refusal is the point.** A Ref inside two Ranges belongs to whichever
/// row the query returns first, so one radio's Calls would attribute to two
/// different apparatus depending on the day — a bug an operator would debug by
/// staring at configuration that looks right. The uniqueness index that stops
/// this for member Refs cannot express it for spans (`idx_unit_refs_system_span`
/// is an access path, not a constraint), so the guarantee is this function, and
/// [`crate::merge`] holds the arithmetic it is made of.
///
/// The Unit's own primary Ref is not a Range and is not checked here: primary
/// beats member in [`resolve_unit`], so a span covering it is redundant rather
/// than ambiguous.
pub async fn add_unit_range<C: ConnectionTrait>(
    db: &C,
    unit: &unit::Model,
    range: crate::merge::Range,
    position: i32,
    now_ms: i64,
) -> Result<RangeAdded, DbErr> {
    let owned = spans_on_system(db, unit.system_id).await?;
    insert_range(db, unit, range, &owned, position, now_ms).await
}

/// **Make the Refs a Unit answers to exactly `wanted`** (#47, spec US 43) — the
/// whole set at once, which is how a CSV means its member-Refs cell.
///
/// One read of the System's spans rather than one per Range, which is the
/// difference between a 500-row fleet import and a thousand full table reads on
/// a Pi. The overlap guarantee is unchanged: every insert still goes through
/// [`insert_range`], against a set kept current as the removals and additions
/// happen — so a row that narrows a block and widens it in the same cell is not
/// refused by the version of itself it is replacing.
///
/// **Removals happen first**, for exactly that reason.
pub async fn set_unit_ranges<C: ConnectionTrait>(
    db: &C,
    unit: &unit::Model,
    wanted: &[crate::merge::Range],
    now_ms: i64,
) -> Result<RangesSet, DbErr> {
    let held = unit_ref::Entity::find()
        .filter(unit_ref::Column::SystemId.eq(unit.system_id))
        .all(db)
        .await?;

    let mut set = RangesSet::default();
    let mut owned: Vec<crate::merge::Range> = Vec::new();
    for row in &held {
        let span = crate::merge::Range::new(row.ref_from, row.ref_to);
        if row.unit_id == unit.id && !wanted.contains(&span) {
            unit_ref::Entity::delete_by_id(row.id).exec(db).await?;
            set.removed += 1;
            continue;
        }
        owned.push(span);
    }

    for (position, range) in wanted.iter().enumerate() {
        if owned.contains(range) {
            continue;
        }
        match insert_range(db, unit, *range, &owned, position as i32, now_ms).await? {
            RangeAdded::Added(_) => {
                owned.push(*range);
                set.added += 1;
            }
            RangeAdded::Overlaps(collision) => set.refused.push((*range, collision)),
        }
    }
    Ok(set)
}

/// What [`set_unit_ranges`] made of the set an operator asked for.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RangesSet {
    pub added: u64,
    pub removed: u64,
    /// The Ranges refused, each with the one already owned that is in the way —
    /// because "that overlaps something" is not an actionable sentence about a
    /// fleet with forty of them.
    pub refused: Vec<(crate::merge::Range, crate::merge::Range)>,
}

impl RangesSet {
    /// Did anything move? A re-import of an unchanged file must count as
    /// unchanged, not as an update.
    pub fn is_empty(&self) -> bool {
        self.added == 0 && self.removed == 0
    }
}

/// Every span owned on a System, whichever Unit owns it — what an overlap is
/// checked against.
async fn spans_on_system<C: ConnectionTrait>(
    db: &C,
    system_id: i64,
) -> Result<Vec<crate::merge::Range>, DbErr> {
    Ok(unit_ref::Entity::find()
        .filter(unit_ref::Column::SystemId.eq(system_id))
        .all(db)
        .await?
        .into_iter()
        .map(|owned| crate::merge::Range::new(owned.ref_from, owned.ref_to))
        .collect())
}

/// One span, inserted unless it collides with one of `owned` — the single place
/// the no-overlap guarantee is enforced, whether one Range is being added or a
/// whole cell is being replaced.
async fn insert_range<C: ConnectionTrait>(
    db: &C,
    unit: &unit::Model,
    range: crate::merge::Range,
    owned: &[crate::merge::Range],
    position: i32,
    now_ms: i64,
) -> Result<RangeAdded, DbErr> {
    if let Some(collision) = crate::merge::first_overlap(owned, &range) {
        return Ok(RangeAdded::Overlaps(collision));
    }
    Ok(RangeAdded::Added(
        unit_ref::ActiveModel {
            unit_id: Set(unit.id),
            system_id: Set(unit.system_id),
            ref_from: Set(range.from()),
            ref_to: Set(range.to()),
            position: Set(position),
            created_at_ms: Set(now_ms),
            ..Default::default()
        }
        .insert(db)
        .await?,
    ))
}

/// What [`add_unit_range`] made of a Range an operator asked for.
///
/// A named pair rather than a nested `Result`, which would make "that overlaps
/// something you already own" and "the database is broken" read identically at
/// the call site — one is the operator's to fix and the other is not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RangeAdded {
    Added(unit_ref::Model),
    /// Refused: the Range this one collides with.
    Overlaps(crate::merge::Range),
}

/// The group names a Talkgroup belongs to (assembled in Rust, not via DB-side
/// string aggregation — keeps the query dialect-agnostic).
pub async fn groups_for_talkgroup<C: ConnectionTrait>(
    db: &C,
    talkgroup_id: i64,
) -> Result<Vec<String>, DbErr> {
    let mut names: Vec<String> = group::Entity::find()
        .join(JoinType::InnerJoin, group::Relation::TalkgroupGroup.def())
        .filter(talkgroup_group::Column::TalkgroupId.eq(talkgroup_id))
        .all(db)
        .await?
        .into_iter()
        .map(|g| g.name)
        .collect();
    names.sort();
    Ok(names)
}

/// Record that `call_id` went out on the live feed as emission `seq`.
///
/// The one place a Call stops being merely stored and becomes emitted. Ingest
/// does it a breath after the insert; a **Delay** (#73) does it whenever its
/// policy releases the Call — and either way this is what a **Backfill** reads.
pub async fn emit_call<C: ConnectionTrait>(
    db: &C,
    call_id: CallId,
    seq: Emission,
) -> Result<(), DbErr> {
    call::Entity::update_many()
        .col_expr(call::Column::EmittedSeq, Expr::value(seq))
        .filter(call::Column::Id.eq(call_id))
        .exec(db)
        .await?;
    Ok(())
}

/// The highest emission this archive has recorded, or `0` for one that has never
/// emitted anything.
///
/// Read once at boot, to resume the sequence where the last process left it. A
/// sequence that restarted at `1` would hand newly emitted Calls numbers a
/// Listener's cursor is already past, so their next reconnect would backfill
/// nothing at all until the counter had climbed back over the archive.
///
/// **The `IS NOT NULL` is load-bearing, and it is a dialect difference** (#22).
/// A Call that is stored but not yet emitted has no emission, and the two
/// dialects disagree about where that sorts: SQLite treats `NULL` as smallest,
/// so a descending sort puts it last, while Postgres treats it as largest and
/// puts it *first*. Without the filter, one held Call — a **Delay** (#73), or
/// one whose emission could not be recorded — would make this answer `0` on
/// Postgres and the right number on SQLite, and every Listener's Backfill would
/// come back empty after the next restart.
pub async fn latest_emission<C: ConnectionTrait>(db: &C) -> Result<Emission, DbErr> {
    Ok(call::Entity::find()
        .filter(call::Column::EmittedSeq.is_not_null())
        .order_by_desc(call::Column::EmittedSeq)
        .one(db)
        .await?
        .and_then(|call| call.emitted_seq)
        .unwrap_or(0))
}

/// Calls that reach `talkgroup_ref` via a patch (full patch resolution for the
/// live feed is #9; this is the archive-side helper).
pub async fn calls_patched_to<C: ConnectionTrait>(
    db: &C,
    talkgroup_ref: i64,
) -> Result<Vec<call::Model>, DbErr> {
    call::Entity::find()
        .join(JoinType::InnerJoin, call::Relation::CallPatch.def())
        .filter(call_patch::Column::TalkgroupRef.eq(talkgroup_ref))
        .distinct()
        .order_by_desc(call::Column::CallAtMs)
        .all(db)
        .await
}

// ---------------------------------------------------------------------------
// Ingest auth (ADR-0008) and duplicate detection (ADR-0001) — ticket #5.
// ---------------------------------------------------------------------------

/// SHA-256 hex of an API key. Keys are high-entropy secrets, so a fast hash is
/// sufficient (no salt/KDF needed); admin passwords (#19) use argon2.
pub fn hash_key(raw: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hex = String::with_capacity(64);
    for byte in Sha256::digest(raw.as_bytes()) {
        use std::fmt::Write;
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

/// Store a new API key (hashed). `system_ref = None` grants all Systems.
pub async fn create_api_key<C: ConnectionTrait>(
    db: &C,
    raw_key: &str,
    system_ref: Option<i64>,
    label: Option<String>,
    now_ms: i64,
) -> Result<api_key::Model, DbErr> {
    api_key::ActiveModel {
        key_hash: Set(hash_key(raw_key)),
        label: Set(label),
        system_ref: Set(system_ref),
        disabled: Set(false),
        created_at_ms: Set(now_ms),
        ..Default::default()
    }
    .insert(db)
    .await
}

/// Number of API keys configured. First run generates one when this is zero.
pub async fn count_api_keys<C: ConnectionTrait>(db: &C) -> Result<u64, DbErr> {
    api_key::Entity::find().count(db).await
}

/// Whether `raw_key` is already in the roster, whatever state it is in.
///
/// Split out from [`ensure_api_key`] because boot has two questions rather than
/// one (#49): "is this key known?" decides whether there is anything to do at
/// all, and "does the roster already hold keys?" decides whether the environment
/// is still allowed to seed one. Asking them separately is what lets a leftover
/// variable be reported as leftover instead of as a key that failed to register.
pub async fn api_key_exists<C: ConnectionTrait>(db: &C, raw_key: &str) -> Result<bool, DbErr> {
    Ok(find_api_key(db, raw_key).await?.is_some())
}

/// Register `raw_key` unless it is already known. Returns whether it was added.
///
/// This is how a key configured out-of-band — `RADIO_SCOUT_API_KEY`, typically
/// from `.env` (ADR-0012 keeps it there, since first run *writes* it) — seeds an
/// Instance whose roster is empty, and keeps working across restarts without
/// stacking up a row per boot. A key an operator **disabled** counts as known
/// and stays disabled; re-registering must never quietly undo a revocation
/// (ADR-0008). Since #49 the *caller* decides whether the environment may seed
/// at all — see [`crate::startup::provision_ingest_key`].
pub async fn ensure_api_key<C: ConnectionTrait>(
    db: &C,
    raw_key: &str,
    system_ref: Option<i64>,
    now_ms: i64,
) -> Result<bool, DbErr> {
    let existing = api_key::Entity::find()
        .filter(api_key::Column::KeyHash.eq(hash_key(raw_key)))
        .one(db)
        .await?;
    if existing.is_some() {
        return Ok(false);
    }
    create_api_key(
        db,
        raw_key,
        system_ref,
        Some("configured (RADIO_SCOUT_API_KEY)".to_string()),
        now_ms,
    )
    .await?;
    Ok(true)
}

/// The API key row `raw_key` names, if there is one. The rule about it is
/// [`authorizes`] — a read here, a decision there (#96).
async fn find_api_key<C: ConnectionTrait>(
    db: &C,
    raw_key: &str,
) -> Result<Option<api_key::Model>, DbErr> {
    api_key::Entity::find()
        .filter(api_key::Column::KeyHash.eq(hash_key(raw_key)))
        .one(db)
        .await
}

/// Whether `raw_key` authorizes ingesting into `system_ref` — the read and the
/// rule, composed.
pub async fn authorize_ingest<C: ConnectionTrait>(
    db: &C,
    raw_key: &str,
    system_ref: i64,
) -> Result<bool, DbErr> {
    Ok(authorizes(
        find_api_key(db, raw_key).await?.as_ref(),
        system_ref,
    ))
}

/// Whether a key row authorizes ingesting into `system_ref` (ADR-0008:
/// recorders always require a valid per-system key). Denied when the key is
/// missing, disabled, or scoped to a different System.
pub fn authorizes(key: Option<&api_key::Model>, system_ref: i64) -> bool {
    let Some(key) = key else {
        return false;
    };
    if key.disabled {
        return false;
    }
    match key.system_ref {
        None => true,
        Some(scoped) => scoped == system_ref,
    }
}

/// The Calls already stored on this **System** inside `window`, as the rows a
/// duplicate decision is made over (ADR-0001, reshaped by #96, widened by #46).
///
/// Scoped to the System rather than to one channel, and that is the widening.
/// Keyed on the canonical Talkgroup, a merge already collapsed the traffic a
/// multi-site System uploads once per Ref (#45) — but a **patch** puts the same
/// transmission on channels that are genuinely different, and a console minting
/// a fresh TGID per patch event puts it on a Ref no Talkgroup owns at all.
/// Neither is reachable from a query keyed on the channel the arriving Call
/// resolved to. Which of these rows is the same transmission is a decision, and
/// it is [`crate::ingest::admit`]'s, made purely over what this returns.
///
/// **The System scope lives here**, which is why `tests/ingest.rs` proves it on
/// both dialects rather than a pure test proving it against itself: two Systems
/// that happen to number a Talkgroup the same way must not see each other's
/// Calls, and after this widening they are one `WHERE` clause apart.
///
/// `None` means no System owns that Ref yet, which is not a state a duplicate
/// can exist in: a Call is a row referencing a System row, so no System means
/// no Calls to be a duplicate of. It also costs no query.
///
/// Three statements at worst and one at best. The patch rows and the signal
/// health behind them are only asked for when the window turned something up —
/// so a Pi taking a Call every few seconds, where the window is empty every
/// time, pays exactly the one statement it paid before.
pub async fn calls_within<C: ConnectionTrait>(
    db: &C,
    system_id: Option<i64>,
    window: std::ops::RangeInclusive<i64>,
) -> Result<Vec<Candidate>, DbErr> {
    let Some(system_id) = system_id else {
        return Ok(Vec::new());
    };
    // The canonical Talkgroup **Ref**, not the `calls.talkgroup_ref` column —
    // that one records what the recorder said, which for a Call that arrived
    // under a member Ref is precisely not the channel it is on (#45).
    let found: Vec<(CallId, i64, i64, i64, Option<i64>, bool)> = call::Entity::find()
        .filter(call::Column::SystemId.eq(system_id))
        .filter(call::Column::CallAtMs.gte(*window.start()))
        .filter(call::Column::CallAtMs.lte(*window.end()))
        .inner_join(talkgroup::Entity)
        .select_only()
        .column(call::Column::Id)
        .column(call::Column::CallAtMs)
        .column(call::Column::CreatedAtMs)
        .column_as(talkgroup::Column::Ref, "talkgroup_ref")
        .column(call::Column::DurationMs)
        .column(call::Column::Encrypted)
        .into_tuple()
        .all(db)
        .await?;
    if found.is_empty() {
        return Ok(Vec::new());
    }

    let ids: Vec<CallId> = found.iter().map(|(id, ..)| *id).collect();
    let mut patches: HashMap<CallId, Vec<i64>> = HashMap::new();
    for (call_id, talkgroup_ref) in call_patch::Entity::find()
        .filter(call_patch::Column::CallId.is_in(ids.clone()))
        .select_only()
        .column(call_patch::Column::CallId)
        .column(call_patch::Column::TalkgroupRef)
        .into_tuple::<(CallId, i64)>()
        .all(db)
        .await?
    {
        patches.entry(call_id).or_default().push(talkgroup_ref);
    }

    // Decode errors, summed per Call. Summed in Rust rather than by a `GROUP
    // BY` so the fold is the same one [`NewCall::quality`] applies to the
    // arriving copy — one rule for what "how many errors did this copy have"
    // means, rather than one in SQL and one in Rust that can disagree about a
    // `NULL`.
    let mut errors: HashMap<CallId, Option<i64>> = HashMap::new();
    for (call_id, error_count) in call_frequency::Entity::find()
        .filter(call_frequency::Column::CallId.is_in(ids))
        .filter(call_frequency::Column::ErrorCount.is_not_null())
        .select_only()
        .column(call_frequency::Column::CallId)
        .column(call_frequency::Column::ErrorCount)
        .into_tuple::<(CallId, Option<i32>)>()
        .all(db)
        .await?
    {
        let total = errors.entry(call_id).or_default();
        *total.get_or_insert(0) += i64::from(error_count.unwrap_or_default());
    }

    Ok(found
        .into_iter()
        .map(
            |(id, call_at_ms, stored_ms, talkgroup, duration_ms, encrypted)| Candidate {
                id,
                call_at_ms,
                stored_ms,
                talkgroup,
                patches: patches.remove(&id).unwrap_or_default(),
                quality: Quality {
                    decode_errors: errors.remove(&id).flatten(),
                    duration_ms,
                },
                // Read from the flag rather than from an empty `object_key`:
                // the two agree by construction, and the flag is the one that
                // says *why* there is nothing to hear (spec US 9).
                has_audio: !encrypted,
            },
        )
        .collect())
}

// ---------------------------------------------------------------------------
// Auto-populate + blacklist policy (#8, ADR-0001).
// ---------------------------------------------------------------------------

/// Why an incoming Call was dropped by [`disposition`]. The two paths are
/// distinct behaviours worth telling apart — each is logged with its own
/// `reason` (ADR-0011 rule 3) — though the recorder gets the same HTTP 200
/// either way so it never retries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DropReason {
    /// The Talkgroup Ref is on the System's blacklist.
    Blacklisted,
    /// The System (or Talkgroup) is unknown and auto-populate is off, so there is
    /// nothing to attach the Call to.
    NotPopulated,
}

/// What to do with an incoming Call once the auto-populate + blacklist policy is
/// applied (#8).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Disposition {
    /// Persist the Call. `auto_populate` is the **effective** flag (global OR the
    /// System's per-system flag) that [`insert_call`] uses to gate the Unit roster.
    Store { auto_populate: bool },
    /// Drop it silently.
    Drop(DropReason),
}

/// **What an arriving Call's Refs resolve to** — the System, and the Talkgroup
/// that owns the Ref it named (#45).
///
/// Both are `Option` because either may not exist yet: an unknown System is
/// about to be created by auto-populate, or dropped by the policy. **Resolved
/// once per Call** (#96) and then carried — the disposition, the dedup window
/// and the insert all need the same two rows, and before this each looked them
/// up again.
///
/// Deliberately not called a *channel*: CONTEXT.md spends that word on
/// **Talkgroup** and puts it on its Avoid list, and this is a pair.
#[derive(Debug, Clone, Default)]
pub struct Resolved {
    pub system: Option<system::Model>,
    pub talkgroup: Option<talkgroup::Model>,
    /// The canonical Talkgroup Refs the Call's `patches` array named on this
    /// System (#81's rule, #45's canonicalization), or `None` when nobody has
    /// asked yet — in which case [`insert_call`] resolves them for itself,
    /// exactly as it does the System and the Talkgroup.
    ///
    /// Ingest fills this in during [`resolve_refs`] because the widened
    /// duplicate test (#46) is asked over these Refs, and resolving them again
    /// inside the insert would be the second lookup #96 removed.
    pub patches: Option<Vec<i64>>,
}

impl Resolved {
    /// Nothing looked up yet — what a caller seeding rows directly passes, and
    /// what makes [`insert_call`] resolve both for itself. Named rather than
    /// `default()`, because "no System" and "I did not ask" are different
    /// things and only one of them is this.
    pub fn unresolved() -> Self {
        Resolved::default()
    }

    /// The System's Id — what the dedup window is read across (#46), and
    /// `None` when no System owns the Ref yet, which is not a state a duplicate
    /// can exist in and costs no query.
    pub fn system_id(&self) -> Option<i64> {
        self.system.as_ref().map(|sys| sys.id)
    }

    /// The canonical Talkgroup's Id, which is what a Call is stored against.
    pub fn talkgroup_id(&self) -> Option<i64> {
        self.talkgroup.as_ref().map(|tg| tg.id)
    }

    /// The canonical Talkgroup's **primary Ref** — the channel a Call reaches,
    /// in the vocabulary the duplicate test is asked in (#46).
    ///
    /// Refs rather than Ids there because the other half of that test is a
    /// Call's `call_patches` rows, which store canonical Refs; comparing one
    /// against the other in Ids would mean resolving every patch row back to a
    /// Talkgroup for a question already answerable.
    pub fn talkgroup_ref(&self) -> Option<i64> {
        self.talkgroup.as_ref().map(|tg| tg.r#ref)
    }
}

/// Is `talkgroup_ref` on a System's comma-separated `blacklist`? Empty entries
/// and non-numeric junk are ignored (rdio-scanner stores the list as free text).
pub fn is_blacklisted(blacklist: Option<&str>, talkgroup_ref: i64) -> bool {
    let Some(list) = blacklist else {
        return false;
    };
    list.split(',')
        .filter_map(|entry| entry.trim().parse::<i64>().ok())
        .any(|ref_| ref_ == talkgroup_ref)
}

/// The System an arriving Call names, and the Talkgroup its Ref resolves to —
/// **the one read** either is looked up in (#96).
///
/// Two statements at worst and one at best, where ingest previously spent them
/// twice over: once to decide the disposition and again inside [`insert_call`],
/// which resolved both rows a second time from scratch.
pub async fn resolve_refs<C: ConnectionTrait>(
    db: &C,
    system_ref: i64,
    talkgroup_ref: i64,
    patches: &[i64],
) -> Result<Resolved, DbErr> {
    let Some(system) = system::Entity::find()
        .filter(system::Column::Ref.eq(system_ref))
        .one(db)
        .await?
    else {
        // No System means no Talkgroup and no patch members: a Ref is unique
        // only within its System, so there is nothing to look any of them up in
        // and no query to spend.
        return Ok(Resolved::unresolved());
    };
    let talkgroup = resolve_talkgroup(db, system.id, talkgroup_ref).await?;
    // Free on the overwhelming majority of Calls: an empty `patches` array
    // costs no statement at all.
    let patches = resolve_patches(db, &system, patches, talkgroup_ref).await?;
    Ok(Resolved {
        system: Some(system),
        talkgroup,
        patches: Some(patches),
    })
}

/// The canonical Talkgroup Refs a recorder's `patches` array names on this
/// System — [`patch_members`] plus the line that says what it made of them.
///
/// Its own function since #46, because the answer is now needed *before* the
/// insert (the widened duplicate test is asked over it) as well as inside one,
/// and resolving it twice would cost a Pi the round-trips #96 removed.
pub async fn resolve_patches<C: ConnectionTrait>(
    db: &C,
    system: &system::Model,
    refs: &[i64],
    talkgroup_ref: i64,
) -> Result<Vec<i64>, DbErr> {
    let patched = patch_members(db, system.id, refs, talkgroup_ref).await?;
    if patched.dropped > 0 || patched.collapsed > 0 {
        // Expected on every SDRTrunk patch — the radio tail is always dropped —
        // so DEBUG, not the WARN of rule 7: this is protocol detail, not
        // something an operator must act on. It is per-Call, which rule 8
        // allows, and it is what answers "why is my patch not fanning out?"
        // and, since #45, "why is my patch one chip and not three?".
        // Bound here rather than inside the macro: `tracing` evaluates a field
        // expression lazily, and a method call in that position is a coverage
        // region the instrumentation cannot see taken even when the line it
        // produces is asserted on.
        let kept = patched.members.len();
        let system_ref = system.r#ref;
        tracing::debug!(
            dropped = patched.dropped,
            collapsed = patched.collapsed,
            kept,
            system_ref,
            "patch refs resolved to this System's channels"
        );
    }
    Ok(patched.members)
}

/// Decide what to do with an incoming Call before any audio is written (#8) —
/// **purely**, over the channel [`resolve_channel`] read.
///
/// Mirrors rdio-scanner's `controller.go`: a brand-new System is auto-created
/// only when the **global** toggle is on; an unknown Talkgroup under a known
/// System is auto-created when either the global or that System's per-system flag
/// is on; and a blacklisted Talkgroup Ref is dropped regardless. Unlike rdio
/// (which only blacklist-checks already-known Talkgroups), the blacklist here
/// applies to a Talkgroup Ref even on its first sighting — "never ingest this"
/// should hold from the first call.
pub fn disposition(
    resolved: &Resolved,
    talkgroup_ref: i64,
    global_auto_populate: bool,
) -> Disposition {
    let Some(sys) = resolved.system.as_ref() else {
        // Unknown System: only auto-created when the global toggle is on.
        return match global_auto_populate {
            true => Disposition::Store {
                auto_populate: true,
            },
            false => Disposition::Drop(DropReason::NotPopulated),
        };
    };

    // **Both Refs are judged**: the one the recorder sent, and the primary Ref of
    // the channel it resolves to (#45). Blacklisting a channel has to reach every
    // Ref it answers to, or merging a Ref into a blacklisted channel would be a
    // way around the operator's own policy — that is the spec's "blacklists
    // operate on the canonical entity". Judging *only* the canonical one would
    // be the mirror-image bug: a Ref an operator refused yesterday would start
    // storing the day somebody merged it, with nothing said about the change.
    let arrived_as_blacklisted = is_blacklisted(sys.blacklist.as_deref(), talkgroup_ref);
    let owner_blacklisted = resolved
        .talkgroup
        .as_ref()
        .is_some_and(|tg| is_blacklisted(sys.blacklist.as_deref(), tg.r#ref));
    if arrived_as_blacklisted || owner_blacklisted {
        return Disposition::Drop(DropReason::Blacklisted);
    }

    let effective = global_auto_populate || sys.auto_populate;
    // A Call for an already-known Talkgroup is always stored; an unknown one needs
    // auto-populate to bring it into being. A member Ref counts as known — its
    // channel exists, and nothing is about to be created.
    let known_talkgroup = resolved.talkgroup.is_some();
    match known_talkgroup || effective {
        true => Disposition::Store {
            auto_populate: effective,
        },
        false => Disposition::Drop(DropReason::NotPopulated),
    }
}

/// The lowest positive Ref not yet used by any System (rdio-scanner's
/// `GetNewSystemRef`). Used to number a new System a recorder gave no numeric Ref
/// for (#8) — Trunk Recorder's native upload identifies systems by name only.
pub async fn lowest_free_system_ref<C: ConnectionTrait>(db: &C) -> Result<i64, DbErr> {
    let taken: std::collections::HashSet<i64> = system::Entity::find()
        .select_only()
        .column(system::Column::Ref)
        .into_tuple()
        .all(db)
        .await?
        .into_iter()
        .collect();
    let mut next = 1;
    while taken.contains(&next) {
        next += 1;
    }
    Ok(next)
}

// ---------------------------------------------------------------------------
// Retention (#10, ADR-0002 / spec US 41).
// ---------------------------------------------------------------------------

/// The minimum a Call needs for retention to prune it: which row to delete,
/// which object to delete after it, and how many bytes that reclaims.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrunableCall {
    pub id: CallId,
    pub object_key: String,
    /// Bytes the audio object occupies. Rows written before the `audio_size`
    /// column existed report 0 rather than blocking the sweep.
    pub audio_size: i64,
}

impl From<call::Model> for PrunableCall {
    fn from(call: call::Model) -> Self {
        PrunableCall {
            id: call.id,
            object_key: call.object_key,
            audio_size: call.audio_size.unwrap_or(0).max(0),
        }
    }
}

/// Up to `limit` Calls that happened before `cutoff_ms`, oldest first — one page
/// of the age-based prune. Paging (rather than one unbounded `DELETE`, as rdio
/// does) keeps each SQLite write-lock window short so a sweep over a large
/// archive never stalls ingest on a Pi.
pub async fn calls_older_than<C: ConnectionTrait>(
    db: &C,
    cutoff_ms: i64,
    limit: u64,
) -> Result<Vec<PrunableCall>, DbErr> {
    Ok(call::Entity::find()
        .filter(call::Column::CallAtMs.lt(cutoff_ms))
        .order_by_asc(call::Column::CallAtMs)
        .order_by_asc(call::Column::Id)
        .limit(limit)
        .all(db)
        .await?
        .into_iter()
        .map(PrunableCall::from)
        .collect())
}

/// Up to `limit` Calls, oldest first, regardless of age — one page of the
/// size-cap prune, which drops the oldest until the archive fits.
pub async fn oldest_calls<C: ConnectionTrait>(
    db: &C,
    limit: u64,
) -> Result<Vec<PrunableCall>, DbErr> {
    Ok(call::Entity::find()
        .order_by_asc(call::Column::CallAtMs)
        .order_by_asc(call::Column::Id)
        .limit(limit)
        .all(db)
        .await?
        .into_iter()
        .map(PrunableCall::from)
        .collect())
}

/// Total bytes of stored audio across the archive — what the size cap is
/// measured against.
///
/// The `SUM` is cast to `BIGINT` because Postgres widens `SUM(bigint)` to
/// `numeric` while SQLite keeps it an integer; the cast makes one query decode
/// on both dialects (ADR-0003). Rows with a `NULL` size contribute nothing.
pub async fn total_audio_bytes<C: ConnectionTrait>(db: &C) -> Result<u64, DbErr> {
    let total: Option<i64> = call::Entity::find()
        .select_only()
        .column_as(
            call::Column::AudioSize.sum().cast_as(Alias::new("BIGINT")),
            "total",
        )
        .into_tuple::<Option<i64>>()
        .one(db)
        .await?
        .flatten();
    Ok(total.unwrap_or(0).max(0) as u64)
}

/// Delete Calls and their child rows, returning how many Call rows went. Child
/// rows go first — the schema's foreign keys are `RESTRICT`, and SQLite enforces
/// them (`PRAGMA foreign_keys = ON`, see [`crate::db::connect`]).
pub async fn delete_calls<C: ConnectionTrait>(db: &C, ids: &[CallId]) -> Result<u64, DbErr> {
    if ids.is_empty() {
        return Ok(0);
    }
    call_frequency::Entity::delete_many()
        .filter(call_frequency::Column::CallId.is_in(ids.iter().copied()))
        .exec(db)
        .await?;
    call_unit::Entity::delete_many()
        .filter(call_unit::Column::CallId.is_in(ids.iter().copied()))
        .exec(db)
        .await?;
    call_patch::Entity::delete_many()
        .filter(call_patch::Column::CallId.is_in(ids.iter().copied()))
        .exec(db)
        .await?;
    Ok(call::Entity::delete_many()
        .filter(call::Column::Id.is_in(ids.iter().copied()))
        .exec(db)
        .await?
        .rows_affected)
}

/// Every object key a Call row still points at — the "keep" set for orphan-GC.
pub async fn referenced_object_keys<C: ConnectionTrait>(
    db: &C,
) -> Result<std::collections::HashSet<String>, DbErr> {
    Ok(call::Entity::find()
        .select_only()
        .column(call::Column::ObjectKey)
        .into_tuple::<String>()
        .all(db)
        .await?
        .into_iter()
        .collect())
}

/// A stored Call's own row, without the denormalizing joins — for the paths
/// that need a column the [`StoredCall`] view doesn't carry (the download's
/// `audio_name`).
pub async fn find_call<C: ConnectionTrait>(
    db: &C,
    id: CallId,
) -> Result<Option<call::Model>, DbErr> {
    call::Entity::find_by_id(id).one(db).await
}

/// The object key + mime for a call's audio (the serve path — lightweight).
pub async fn get_call_audio<C: ConnectionTrait>(
    db: &C,
    id: CallId,
) -> Result<Option<CallAudio>, DbErr> {
    Ok(call::Entity::find_by_id(id)
        .one(db)
        .await?
        .map(|c| CallAudio {
            object_key: c.object_key,
            mime: c.audio_mime,
            enhancement: c.enhancement,
            created_at_ms: c.created_at_ms,
        }))
}

/// What serving a Call's audio needs to know: where the bytes are, what to call
/// them, and whether they are about to be replaced (#20).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallAudio {
    pub object_key: String,
    pub mime: Option<String>,
    /// One of [`call::EnhancementState`]'s values. `pending` is why this is here at
    /// all: audio that is queued for enhancement must not be cached as
    /// immutable, because the object behind this id is going to change.
    pub enhancement: String,
    /// When the row was stored — the other reason its object may still change
    /// (#46): a Call inside its dedup window can be replaced by a better copy
    /// of the same transmission, and is cached exactly as a pending one is.
    pub created_at_ms: i64,
}

/// The System's and Talkgroup's enhancement flags for a Call, in that order.
///
/// Read *after* the Call is inserted rather than before, because auto-populate
/// may have created either row a moment ago — and a row that has just been
/// created has `NULL`, which is the value that inherits.
pub async fn enhancement_scope<C: ConnectionTrait>(
    db: &C,
    id: CallId,
) -> Result<Option<crate::enhance::Scope>, DbErr> {
    let Some(call) = call::Entity::find_by_id(id).one(db).await? else {
        return Ok(None);
    };
    let system = system::Entity::find_by_id(call.system_id).one(db).await?;
    let talkgroup = talkgroup::Entity::find_by_id(call.talkgroup_id)
        .one(db)
        .await?;
    Ok(Some(crate::enhance::Scope {
        system: system.and_then(|s| s.enhancement),
        talkgroup: talkgroup.and_then(|t| t.enhancement),
    }))
}

/// Move a Call to an enhancement state, leaving its audio alone.
pub async fn mark_enhancement<C: ConnectionTrait>(
    db: &C,
    id: CallId,
    state: &str,
) -> Result<(), DbErr> {
    call::Entity::update_many()
        .col_expr(call::Column::Enhancement, Expr::value(state))
        .filter(call::Column::Id.eq(id))
        .exec(db)
        .await?;
    Ok(())
}

/// Point a Call at its enhanced audio.
///
/// One statement, so a Call is never briefly `done` while still naming the old
/// object — a reader between the two writes would serve audio that orphan-GC is
/// entitled to delete. The old object is left behind deliberately: #10's sweep
/// reclaims it once its grace period is up, which is also what makes this safe
/// to do while somebody is mid-download of the original.
pub async fn store_enhanced_audio<C: ConnectionTrait>(
    db: &C,
    id: CallId,
    audio: EnhancedAudio<'_>,
) -> Result<(), DbErr> {
    call::Entity::update_many()
        .col_expr(call::Column::ObjectKey, Expr::value(audio.object_key))
        .col_expr(call::Column::AudioMime, Expr::value(audio.mime))
        .col_expr(call::Column::AudioName, Expr::value(audio.name))
        .col_expr(call::Column::AudioSize, Expr::value(audio.bytes))
        .col_expr(call::Column::DurationMs, Expr::value(audio.duration_ms))
        .col_expr(
            call::Column::Enhancement,
            Expr::value(call::EnhancementState::DONE),
        )
        .filter(call::Column::Id.eq(id))
        .exec(db)
        .await?;
    Ok(())
}

/// The result of enhancing a Call, as the row records it.
#[derive(Debug, Clone)]
pub struct EnhancedAudio<'a> {
    pub object_key: &'a str,
    pub mime: &'a str,
    pub name: String,
    pub bytes: i64,
    /// Measured while decoding — the first time this column is ever anything
    /// but `NULL`, since ingest only ever sees bytes.
    pub duration_ms: i64,
}

/// Calls a restart interrupted: queued or in flight when the process went away.
///
/// Deliberately only `pending`. Calls marked `none` were ingested while
/// enhancement was off, and re-queueing those would mean switching enhancement
/// on silently rewrote an operator's whole archive on the next boot.
pub async fn calls_pending_enhancement<C: ConnectionTrait>(db: &C) -> Result<Vec<CallId>, DbErr> {
    Ok(call::Entity::find()
        .filter(call::Column::Enhancement.eq(call::EnhancementState::PENDING))
        .order_by_asc(call::Column::Id)
        .all(db)
        .await?
        .into_iter()
        .map(|c| c.id)
        .collect())
}

/// The System Ref for a Trunk Recorder `short_name` (which carries no numeric
/// ref). If a System already has that label, reuse its Ref so TR and generic
/// uploads converge; otherwise assign the lowest-free Ref (#8, rdio-scanner's
/// `GetNewSystemRef`). Read-only — the System row is created (if new) by the
/// ingest pipeline with this Ref and the `short_name` as its label.
pub async fn system_ref_for_short_name<C: ConnectionTrait>(
    db: &C,
    short_name: &str,
) -> Result<i64, DbErr> {
    if let Some(sys) = system::Entity::find()
        .filter(system::Column::Label.eq(short_name))
        .one(db)
        .await?
    {
        return Ok(sys.r#ref);
    }
    lowest_free_system_ref(db).await
}

// -- Web Push subscriptions (#16) -------------------------------------------

/// Store a browser's push subscription, or update the one already registered
/// for the same endpoint. Returns the token that device proves itself with.
///
/// An endpoint **is** the device: a browser that re-subscribes — because the
/// listener changed their Selection, or because the push service rotated the
/// URL and the client subscribed again — must not leave a second row behind
/// notifying the same phone twice. Its token survives that, so a socket already
/// holding one keeps working.
///
/// `selection` is optional because re-subscribing has two reasons: syncing a
/// changed Selection (which passes one) and a reload learning its token back
/// (which must not overwrite what the listener chose on another visit).
pub async fn upsert_push_subscription<C: ConnectionTrait>(
    db: &C,
    endpoint: &str,
    p256dh: &str,
    auth: &str,
    selection: Option<&str>,
    now_ms: i64,
) -> Result<String, DbErr> {
    let existing = push_subscription::Entity::find()
        .filter(push_subscription::Column::Endpoint.eq(endpoint))
        .one(db)
        .await?;

    if let Some(existing) = existing {
        let token = existing.token.clone();
        let mut update: push_subscription::ActiveModel = existing.into();
        update.p256dh = Set(p256dh.to_string());
        update.auth = Set(auth.to_string());
        if let Some(selection) = selection {
            update.selection = Set(selection.to_string());
        }
        update.updated_at_ms = Set(now_ms);
        update.update(db).await?;
        return Ok(token);
    }

    let token = uuid::Uuid::new_v4().simple().to_string();
    push_subscription::ActiveModel {
        endpoint: Set(endpoint.to_string()),
        token: Set(token.clone()),
        p256dh: Set(p256dh.to_string()),
        auth: Set(auth.to_string()),
        // A subscription nobody has said anything about hears nothing, rather
        // than everything: the safe direction for a row whose Selection was
        // lost.
        selection: Set(selection.unwrap_or("{}").to_string()),
        created_at_ms: Set(now_ms),
        updated_at_ms: Set(now_ms),
        ..Default::default()
    }
    .insert(db)
    .await?;
    Ok(token)
}

/// Forget the subscription holding `token` — the listener turned notifications
/// off.
///
/// Deliberately says nothing about whether there *was* one: unsubscribing is
/// idempotent by design (a browser that unsubscribes twice, or whose row a
/// `410 Gone` already removed, is not an error), so a caller has nothing to do
/// with the count.
pub async fn delete_push_subscription<C: ConnectionTrait>(
    db: &C,
    token: &str,
) -> Result<(), DbErr> {
    push_subscription::Entity::delete_many()
        .filter(push_subscription::Column::Token.eq(token))
        .exec(db)
        .await?;
    Ok(())
}

/// The subscription a token belongs to, by **Id** — what the live-feed socket
/// presents to say its listener is listening (#16).
pub async fn push_subscription_id<C: ConnectionTrait>(
    db: &C,
    token: &str,
) -> Result<Option<i64>, DbErr> {
    Ok(push_subscription::Entity::find()
        .filter(push_subscription::Column::Token.eq(token))
        .one(db)
        .await?
        .map(|row| row.id))
}

/// Forget one subscription by Id — what a push service's `410 Gone` means.
pub async fn delete_push_subscription_by_id<C: ConnectionTrait>(
    db: &C,
    id: i64,
) -> Result<(), DbErr> {
    push_subscription::Entity::delete_by_id(id).exec(db).await?;
    Ok(())
}

/// Every registered push subscription, oldest first.
pub async fn push_subscriptions<C: ConnectionTrait>(
    db: &C,
) -> Result<Vec<push_subscription::Model>, DbErr> {
    push_subscription::Entity::find()
        .order_by_asc(push_subscription::Column::Id)
        .all(db)
        .await
}

// ---------------------------------------------------------------------------
// The operator log surface (#30)
// ---------------------------------------------------------------------------

/// Store a batch of log events ([`crate::logsink`]).
///
/// One statement per batch, **parameterised** — which is the whole difference
/// from rdio-scanner, whose `log.go` builds its insert with an interpolated
/// `fmt.Sprintf`, so a log message containing an apostrophe corrupts the row it
/// was supposed to become. Nothing here escapes anything: the values travel as
/// bind parameters, so a message is stored as whatever bytes it was.
pub async fn insert_log_events<C: ConnectionTrait>(
    db: &C,
    events: &[NewLogEvent],
) -> Result<(), DbErr> {
    if events.is_empty() {
        return Ok(());
    }
    log_event::Entity::insert_many(events.iter().map(|event| log_event::ActiveModel {
        at_ms: Set(event.at_ms),
        level: Set(event.level.clone()),
        target: Set(event.target.clone()),
        message: Set(event.message.clone()),
        fields: Set(event.fields.clone()),
        request_id: Set(event.request_id.clone()),
        ..Default::default()
    }))
    .exec(db)
    .await?;
    Ok(())
}

/// What the Logs view is asking for (#30).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LogSearch {
    /// Only events at or after this instant.
    pub after_ms: Option<i64>,
    /// Only events strictly before it.
    pub before_ms: Option<i64>,
    /// The stored level names to include — a severity floor, already expanded
    /// by [`crate::logview`]. `None` is every level.
    pub levels: Option<Vec<String>>,
    pub limit: u64,
    pub offset: u64,
}

/// One page of stored log events, **newest first** — the order an operator
/// opens the page wanting.
pub async fn search_log_events<C: ConnectionTrait>(
    db: &C,
    search: &LogSearch,
) -> Result<Vec<log_event::Model>, DbErr> {
    filtered_logs(search)
        // By id as well as time: events written in the same millisecond — a
        // burst is exactly that — would otherwise page unstably, showing one
        // twice and another never.
        .order_by_desc(log_event::Column::AtMs)
        .order_by_desc(log_event::Column::Id)
        .limit(search.limit)
        .offset(search.offset)
        .all(db)
        .await
}

/// How many events match, ignoring the page window.
pub async fn count_log_events<C: ConnectionTrait>(
    db: &C,
    search: &LogSearch,
) -> Result<u64, DbErr> {
    filtered_logs(search).count(db).await
}

/// The filters both of the above share, so a page and its total can never
/// disagree about what was asked for.
fn filtered_logs(search: &LogSearch) -> sea_orm::Select<log_event::Entity> {
    let mut query = log_event::Entity::find();
    if let Some(after_ms) = search.after_ms {
        query = query.filter(log_event::Column::AtMs.gte(after_ms));
    }
    if let Some(before_ms) = search.before_ms {
        query = query.filter(log_event::Column::AtMs.lt(before_ms));
    }
    if let Some(levels) = &search.levels {
        query = query.filter(log_event::Column::Level.is_in(levels.iter().map(String::as_str)));
    }
    query
}

/// Delete up to `limit` stored log events older than `cutoff_ms`, returning how
/// many went — one page of retention's log prune (#30).
///
/// Paged for the same reason the archive's prune is: an instance that has been
/// logging for a month has a lot of rows, and one unbounded `DELETE` holds a
/// SQLite write lock for all of them. `limit = 0` deletes nothing and reports
/// nothing, which is what stops a mis-typed `batch_size` from spinning.
pub async fn delete_logs_older_than<C: ConnectionTrait>(
    db: &C,
    cutoff_ms: i64,
    limit: u64,
) -> Result<u64, DbErr> {
    let ids: Vec<i64> = log_event::Entity::find()
        .select_only()
        .column(log_event::Column::Id)
        .filter(log_event::Column::AtMs.lt(cutoff_ms))
        .order_by_asc(log_event::Column::Id)
        .limit(limit)
        .into_tuple::<i64>()
        .all(db)
        .await?;
    if ids.is_empty() {
        return Ok(0);
    }
    Ok(log_event::Entity::delete_many()
        .filter(log_event::Column::Id.is_in(ids))
        .exec(db)
        .await?
        .rows_affected)
}

/// One event on its way to the `logs` table.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct NewLogEvent {
    pub at_ms: i64,
    pub level: String,
    pub target: String,
    pub message: String,
    pub fields: Option<String>,
    pub request_id: Option<String>,
}

// ---------------------------------------------------------------------------
// Mining a Call already in the Archive (#48, spec US 13).
// ---------------------------------------------------------------------------

/// Fold what was mined out of a stored Call's audio into the Call. `true` if
/// anything was added.
///
/// **Does not stamp the Call looked-at.** That is the sweep's, in one statement
/// for the whole batch — so there is exactly one place a Call is marked read,
/// and no arm here can be the one that forgot. A crash between this and the
/// stamp leaves an enriched-but-unstamped Call, which the next sweep re-reads
/// and finds nothing left to fill: idempotent, and the safe direction.
///
/// The counterpart of [`crate::mining::apply`], which does the same job to a
/// Call an ingest has not written yet. Two writers because there are two
/// genuinely different situations — a value being assembled, and a row that has
/// existed for a year — and `tests/mining.rs` holds them to landing the
/// identical Call, the way #44 holds the upload script and the plugin together.
///
/// Every rule they share is the same rule: **fill and never overwrite**, and a
/// name goes only on a radio this Call actually heard.
pub async fn apply_mined<C: ConnectionTrait>(
    db: &C,
    call: &call::Model,
    mined: &crate::mining::Mined,
    global_auto_populate: bool,
    now_ms: i64,
) -> Result<bool, DbErr> {
    let mut row: call::ActiveModel = call.clone().into();
    let mut applied = false;
    let mut frequency = call.frequency;
    let mut audio_type = call.audio_type.clone();
    if crate::mining::fill(&mut frequency, mined.frequency) {
        row.frequency = Set(frequency);
        applied = true;
    }
    if crate::mining::fill(&mut audio_type, mined.decoder.clone()) {
        row.audio_type = Set(audio_type);
        applied = true;
    }
    // **A Ref identifies and a name names**, exactly as it does at ingest
    // ([`site_of`]): a Call that already points at a tower was told *which* by
    // its Recorder and keeps that tower, but the tower still gains the name if
    // it has none. A Call pointing at no tower gets one minted for the name.
    //
    // The `Some` arm is why this is written out rather than left as "only when
    // the Call has no Site": without it the two writers disagree, and the
    // equality test cannot see it — SDRTrunk sends no `site`, so no Call that
    // goes down both paths ever has one.
    if let Some(label) = mined.site.as_deref() {
        match call.site_id {
            Some(site_id) => {
                if let Some(site) = site::Entity::find_by_id(site_id).one(db).await?
                    && site.label.is_none()
                {
                    name_site(db, site, Some(label)).await?;
                    applied = true;
                }
            }
            None => {
                let site = resolve_or_create_site_named(db, call.system_id, label, now_ms).await?;
                row.site_id = Set(Some(site.id));
                applied = true;
            }
        }
    }
    // Only where there is something to write: the steady state of a sweep is a
    // Call that gives up nothing, and an `UPDATE` setting every column to what
    // it already held would be a write per Call bought for no change at all.
    if applied {
        row.update(db).await?;
    }

    Ok(applied | name_heard_unit(db, call, mined, global_auto_populate, now_ms).await?)
}

/// Give the radio this Call heard the name its audio carried — on the Call's
/// own `call_units` row, and on the **Unit** the roster keeps.
///
/// Both, because they answer different questions: the row records what *this*
/// Call's Recorder called the radio, and the Unit is what an apparatus is named
/// across the Archive ([`crate::call::unit_name`], #47). Filling only one would
/// make a Listener reading a Call and searching for that radio disagree.
async fn name_heard_unit<C: ConnectionTrait>(
    db: &C,
    call: &call::Model,
    mined: &crate::mining::Mined,
    global_auto_populate: bool,
    now_ms: i64,
) -> Result<bool, DbErr> {
    let Some(named) = &mined.unit else {
        return Ok(false);
    };
    // Only a radio the Call lists. The tag names one radio at one moment, and
    // hanging that name on whichever Unit the Call happens to carry would put
    // an apparatus's name on a different apparatus.
    let Some(heard) = call_unit::Entity::find()
        .filter(call_unit::Column::CallId.eq(call.id))
        .filter(call_unit::Column::UnitRef.eq(named.unit_ref))
        .one(db)
        .await?
    else {
        return Ok(false);
    };
    if heard.label.is_some() {
        return Ok(false);
    }
    let mut row: call_unit::ActiveModel = heard.into();
    row.label = Set(Some(named.label.clone()));
    row.update(db).await?;

    // The roster is gated on auto-populate exactly as it is at ingest (#8): an
    // Operator who turned it off did so to stop unknown entities appearing, and
    // a sweep is not an exception to that. **The same effective flag**,
    // instance-wide OR the System's own — [`disposition`]'s rule, and reading
    // only the column here would silently roster nothing on the shipped
    // default, where the instance says yes and every System row says nothing.
    let effective = global_auto_populate
        || system::Entity::find_by_id(call.system_id)
            .one(db)
            .await?
            .is_some_and(|system| system.auto_populate);
    if effective {
        resolve_or_create_unit(
            db,
            call.system_id,
            named.unit_ref,
            Some(named.label.clone()),
            now_ms,
        )
        .await?;
    }
    Ok(true)
}

/// A page of Calls nothing has looked inside yet, newest first (#48).
///
/// Newest first because that is the half of an Archive somebody is most likely
/// to be listening to, so the enrichment shows up where it is noticed rather
/// than a week later.
pub async fn unmined_calls<C: ConnectionTrait>(
    db: &C,
    limit: u64,
) -> Result<Vec<call::Model>, DbErr> {
    call::Entity::find()
        .filter(call::Column::MinedAtMs.is_null())
        .order_by_desc(call::Column::Id)
        .limit(limit)
        .all(db)
        .await
}

/// Stamp these Calls looked-at.
///
/// One statement for a whole page, not one per Call: the steady state of a
/// sweep over a Trunk Recorder archive is a batch in which *nothing* is
/// mined, and paying a round-trip per Call for that would make the sweep's cost
/// the thing an Operator notices about it.
///
/// It is also the **only** place a Call is stamped by the sweep, whatever the
/// sweep found in it — so "looked at" cannot be set on one path and forgotten
/// on another.
pub async fn mark_mined<C: ConnectionTrait>(
    db: &C,
    call_ids: &[CallId],
    now_ms: i64,
) -> Result<(), DbErr> {
    if call_ids.is_empty() {
        return Ok(());
    }
    call::Entity::update_many()
        .col_expr(call::Column::MinedAtMs, Expr::value(now_ms))
        .filter(call::Column::Id.is_in(call_ids.iter().copied()))
        .exec(db)
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    /// An empty batch is a no-op, not a statement. [`crate::logsink`]'s writer
    /// never sends one — it batches at least the event it woke up for — but an
    /// `INSERT` with no rows is a SQL error rather than an empty result, so
    /// the guard is the difference between "nothing to do" and a failed sink.
    #[tokio::test]
    async fn storing_no_log_events_is_not_a_statement() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let db = crate::db::connect(&crate::testing::sqlite_url(&tmp))
            .await
            .expect("db");

        insert_log_events(&db, &[]).await.expect("an empty batch");

        assert_eq!(
            log_event::Entity::find()
                .count(&db)
                .await
                .expect("count log events"),
            0
        );
    }

    // -- What a merge moved (#50) -------------------------------------------

    /// A System with two channels and a Call on the second — the shape every
    /// merge test below folds.
    async fn a_system_with_two_channels() -> (tempfile::TempDir, crate::db::Db, talkgroup::Model) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let db = crate::db::connect(&crate::testing::sqlite_url(&tmp))
            .await
            .expect("db");
        let system = system::ActiveModel {
            r#ref: Set(11),
            auto_populate: Set(true),
            created_at_ms: Set(0),
            ..Default::default()
        }
        .insert(&db)
        .await
        .expect("a system");
        let owner = talkgroup::ActiveModel {
            system_id: Set(system.id),
            r#ref: Set(100),
            label: Set(Some(String::from("Fire Dispatch"))),
            created_at_ms: Set(0),
            ..Default::default()
        }
        .insert(&db)
        .await
        .expect("the owner");
        let churn = talkgroup::ActiveModel {
            system_id: Set(system.id),
            r#ref: Set(1201),
            label: Set(Some(String::from("TAC 3"))),
            created_at_ms: Set(0),
            ..Default::default()
        }
        .insert(&db)
        .await
        .expect("the churn row");
        for at_ms in [1_000, 2_000] {
            call::ActiveModel {
                system_id: Set(system.id),
                talkgroup_id: Set(churn.id),
                talkgroup_ref: Set(Some(1201)),
                call_at_ms: Set(at_ms),
                object_key: Set(format!("k{at_ms}.wav")),
                emergency: Set(false),
                encrypted: Set(false),
                enhancement: Set(String::from(call::EnhancementState::NONE)),
                created_at_ms: Set(at_ms),
                ..Default::default()
            }
            .insert(&db)
            .await
            .expect("a call");
        }
        (tmp, db, owner)
    }

    /// **A fold says what it took, one Ref at a time.**
    ///
    /// The counts alone cannot: "1 folded, 2 calls re-pointed" is the same
    /// sentence whichever channel went, and #50's confirmation step has to be
    /// able to name the row it is about to destroy. The same detail is what the
    /// log line's ids are made of, which is why it is one structure and not two.
    #[tokio::test]
    async fn a_fold_reports_the_channel_it_absorbed() {
        let (_tmp, db, owner) = a_system_with_two_channels().await;

        let change = set_member_refs(&db, &owner, &[1201], 3_000)
            .await
            .expect("the fold");

        assert_eq!(change.folded, 1);
        assert_eq!(change.calls_repointed, 2);
        let moved = match change.moved.as_slice() {
            [only] => only,
            other => panic!("expected one move, got {other:?}"),
        };
        assert_eq!(moved.r#ref, 1201);
        assert_eq!(moved.movement, Movement::Folded);
        assert_eq!(moved.label.as_deref(), Some("TAC 3"));
        assert_eq!(moved.calls, 2, "the Calls this Ref brought with it");
        assert!(moved.talkgroup_id.is_some(), "the channel that went");
        assert!(moved.carried.is_empty(), "it owned no members of its own");
    }

    /// **A Ref nothing has been heard on is `Recorded`, not `Folded`.**
    ///
    /// The two are indistinguishable in the counts — both leave a member Ref
    /// behind and neither is an error — but only one of them destroyed a row.
    /// This is also the shape a Ref selected from *another System* arrives in
    /// (a Ref is unique only within one), which is the case the preview exists
    /// to make visible rather than plausible.
    #[tokio::test]
    async fn a_ref_no_channel_answers_to_is_recorded_rather_than_folded() {
        let (_tmp, db, owner) = a_system_with_two_channels().await;

        let change = set_member_refs(&db, &owner, &[8123], 3_000)
            .await
            .expect("the record");

        assert_eq!(change.folded, 0, "nothing was absorbed");
        assert_eq!(change.calls_repointed, 0);
        assert_eq!(
            change
                .moved
                .iter()
                .map(|moved| (moved.r#ref, moved.movement))
                .collect::<Vec<_>>(),
            [(8123, Movement::Recorded)]
        );
        assert!(change.moved[0].talkgroup_id.is_none());
    }

    /// An unfold reports the Calls it gave back, so the same confirmation works
    /// in the direction that *creates* a channel.
    #[tokio::test]
    async fn an_unfold_reports_the_calls_it_gave_back() {
        let (_tmp, db, owner) = a_system_with_two_channels().await;
        set_member_refs(&db, &owner, &[1201], 3_000)
            .await
            .expect("the fold");

        let change = set_member_refs(&db, &owner, &[], 4_000)
            .await
            .expect("the unfold");

        assert_eq!(change.unfolded, 1);
        let moved = match change.moved.as_slice() {
            [only] => only,
            other => panic!("expected one move, got {other:?}"),
        };
        assert_eq!(moved.r#ref, 1201);
        assert_eq!(moved.movement, Movement::Unfolded);
        assert_eq!(moved.calls, 2, "exactly the Calls that arrived under it");
        assert_eq!(
            moved.label.as_deref(),
            Some("TAC 3"),
            "the name the operator curated comes back with it"
        );
    }

    /// **A chain fold says what came with it.** Folding a channel that owns
    /// member Refs of its own carries them across silently otherwise, and the
    /// operator finds out by noticing a number in a list later. The CSV
    /// importer refuses this outright so a file keeps describing what it made;
    /// a form has no file to round-trip, so it applies and reports.
    #[tokio::test]
    async fn a_fold_reports_the_members_that_came_with_the_channel() {
        let (_tmp, db, owner) = a_system_with_two_channels().await;
        let churn = talkgroup::Entity::find()
            .filter(talkgroup::Column::Ref.eq(1201))
            .one(&db)
            .await
            .expect("read")
            .expect("the churn row");
        set_member_refs(&db, &churn, &[8123, 8124], 2_500)
            .await
            .expect("its own members");

        let change = set_member_refs(&db, &owner, &[1201], 3_000)
            .await
            .expect("the fold");

        assert_eq!(change.moved.len(), 1, "one Ref was named");
        assert_eq!(change.moved[0].carried, [8123, 8124]);
    }

    /// **A chain fold must not leave two members sharing a position.**
    ///
    /// A carried Ref arrives holding the position it had on the channel it came
    /// from, which is one this call just handed out. Two rows at position 0
    /// leave the order down to whatever the database returns first, and the two
    /// dialects need not agree — invisible while the order was only a CSV
    /// column's, and a list that renders differently per install once #50 puts
    /// it on a screen.
    #[tokio::test]
    async fn a_carried_member_gets_a_position_of_its_own() {
        let (_tmp, db, owner) = a_system_with_two_channels().await;
        let churn = talkgroup::Entity::find()
            .filter(talkgroup::Column::Ref.eq(1201))
            .one(&db)
            .await
            .expect("read")
            .expect("the churn row");
        set_member_refs(&db, &churn, &[8123], 2_500)
            .await
            .expect("its own member");

        set_member_refs(&db, &owner, &[1201], 3_000)
            .await
            .expect("the fold");

        let mut held: Vec<(i32, i64)> = talkgroup_ref::Entity::find()
            .filter(talkgroup_ref::Column::TalkgroupId.eq(owner.id))
            .all(&db)
            .await
            .expect("read members")
            .into_iter()
            .map(|member| (member.position, member.r#ref))
            .collect();
        held.sort();

        assert_eq!(
            held,
            [(0, 1201), (1, 8123)],
            "the named Ref keeps the operator's order and the carried one follows"
        );
    }

    /// Nothing moved is an empty report — which is what makes a re-import, and
    /// a form submitted twice, a no-op rather than a merge action.
    #[tokio::test]
    async fn a_fold_that_changes_nothing_reports_nothing() {
        let (_tmp, db, owner) = a_system_with_two_channels().await;
        set_member_refs(&db, &owner, &[1201], 3_000)
            .await
            .expect("the fold");

        let again = set_member_refs(&db, &owner, &[1201], 4_000)
            .await
            .expect("the same fold");

        assert!(again.is_empty(), "{again:?}");
        assert!(again.moved.is_empty());
    }

    // -- Mining a Call already in the Archive (#48) --------------------------

    /// **Either half alone is an application.** [`apply_mined`] folds in two
    /// independent things — what a Call knows about itself, and what its radio
    /// is called — and the answer it hands back decides whether the sweep
    /// reports the Call as `mined` or as `nothing-new`.
    ///
    /// So each half is asserted *on its own*: with both folded together, an
    /// `&` in place of the `|` would still report `true` whenever both fired,
    /// and every test that mines a complete tag would pass while a Call that
    /// only gained a tower was reported as having gained nothing.
    #[rstest]
    #[case::only_a_tower(Some("Downtown"), None, true)]
    #[case::only_a_radios_name(None, Some("Engine 1"), true)]
    #[case::both(Some("Downtown"), Some("Engine 1"), true)]
    #[case::neither(None, None, false)]
    #[tokio::test]
    async fn either_half_of_a_mining_is_an_application(
        #[case] site: Option<&str>,
        #[case] radio: Option<&str>,
        #[case] expected: bool,
    ) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let db = crate::db::connect(&crate::testing::sqlite_url(&tmp))
            .await
            .expect("db");
        let stored = insert_call(
            &db,
            &NewCall {
                units: vec![NewCallUnit {
                    unit_ref: 1234567,
                    ..Default::default()
                }],
                ..NewCall::new(11, 54241, 1_000)
            },
            crate::blob::StoredAudio::written("a/b.mp3".into(), 1).into(),
            &Resolved::unresolved(),
            true,
            0,
        )
        .await
        .expect("the stored Call");

        let applied = apply_mined(
            &db,
            &stored,
            &crate::mining::Mined {
                unit: radio.map(|label| crate::mining::MinedUnit {
                    unit_ref: 1234567,
                    label: label.to_string(),
                }),
                site: site.map(str::to_string),
                decoder: None,
                frequency: None,
            },
            true,
            2_000,
        )
        .await
        .expect("apply what was mined");

        assert_eq!(applied, expected);
    }

    // -- The decisions ingest makes, as values (#96) -------------------------
    //
    // Each of these used to be reachable only through a socket and a multipart
    // body, because the read and the rule were one function. Resolve reads;
    // these decide; a table names every arm.

    /// An API key row as the database holds one.
    fn a_key(disabled: bool, system_ref: Option<i64>) -> api_key::Model {
        api_key::Model {
            id: 1,
            key_hash: "unused — the row was found by its hash".into(),
            label: None,
            system_ref,
            disabled,
            created_at_ms: 0,
        }
    }

    /// **Authorization is a rule about a row, not a query** (ADR-0008). Every
    /// arm is a recorder that would otherwise be silently refused, or silently
    /// let into a System it was never scoped to.
    #[rstest]
    #[case::no_such_key(None, false)]
    #[case::a_key_for_every_system(Some(a_key(false, None)), true)]
    #[case::a_key_for_this_system(Some(a_key(false, Some(11))), true)]
    #[case::a_key_for_another_system(Some(a_key(false, Some(22))), false)]
    #[case::a_disabled_key_for_every_system(Some(a_key(true, None)), false)]
    #[case::a_disabled_key_for_this_system(Some(a_key(true, Some(11))), false)]
    fn a_key_authorizes_only_the_system_it_is_scoped_to(
        #[case] key: Option<api_key::Model>,
        #[case] authorized: bool,
    ) {
        assert_eq!(authorizes(key.as_ref(), 11), authorized);
    }

    fn a_system(auto_populate: bool, blacklist: Option<&str>) -> system::Model {
        system::Model {
            id: 1,
            r#ref: 11,
            label: None,
            auto_populate,
            blacklist: blacklist.map(str::to_string),
            enhancement: None,
            created_at_ms: 0,
        }
    }

    fn a_talkgroup(primary_ref: i64) -> talkgroup::Model {
        talkgroup::Model {
            id: 7,
            system_id: 1,
            r#ref: primary_ref,
            label: None,
            name: None,
            tag_id: None,
            led: None,
            enhancement: None,
            created_at_ms: 0,
        }
    }

    /// The auto-populate + blacklist policy (#8), over what was resolved.
    ///
    /// The last two cases are #45's rule and its mirror image: blacklisting a
    /// channel reaches every Ref it answers to, or merging a Ref into a
    /// blacklisted channel would be a way around the operator's own policy —
    /// and a Ref they refused yesterday must not start storing the day somebody
    /// merged it.
    #[rstest]
    #[case::an_unknown_system_with_auto_populate_on(None, None, true, Disposition::Store { auto_populate: true })]
    #[case::an_unknown_system_with_auto_populate_off(
        None,
        None,
        false,
        Disposition::Drop(DropReason::NotPopulated)
    )]
    #[case::an_unknown_talkgroup_with_auto_populate_on(Some(a_system(false, None)), None, true, Disposition::Store { auto_populate: true })]
    #[case::an_unknown_talkgroup_with_auto_populate_off(
        Some(a_system(false, None)),
        None,
        false,
        Disposition::Drop(DropReason::NotPopulated)
    )]
    #[case::a_known_talkgroup_needs_no_auto_populate(Some(a_system(false, None)), Some(a_talkgroup(54241)), false, Disposition::Store { auto_populate: false })]
    #[case::the_systems_own_flag_overrides_a_global_off(Some(a_system(true, None)), None, false, Disposition::Store { auto_populate: true })]
    #[case::the_ref_that_arrived_is_blacklisted(
        Some(a_system(true, Some("54241"))),
        None,
        true,
        Disposition::Drop(DropReason::Blacklisted)
    )]
    #[case::the_channel_it_resolves_to_is_blacklisted(
        Some(a_system(true, Some("100"))),
        Some(a_talkgroup(100)),
        true,
        Disposition::Drop(DropReason::Blacklisted)
    )]
    fn the_policy_decides_over_what_was_resolved(
        #[case] system: Option<system::Model>,
        #[case] talkgroup: Option<talkgroup::Model>,
        #[case] global_auto_populate: bool,
        #[case] expected: Disposition,
    ) {
        let resolved = Resolved {
            system,
            talkgroup,
            ..Resolved::unresolved()
        };

        assert_eq!(
            disposition(&resolved, 54241, global_auto_populate),
            expected
        );
    }

    /// A recorder's `patches` array, resolved against what the System knows
    /// (#81, #45) — the pure half, over a map the two queries produced.
    ///
    /// `dropped` and `collapsed` are separate facts and read as separate
    /// problems: the first says the System has never heard of a ref (SDRTrunk's
    /// radio-id tail), the second says two refs named one channel, which is
    /// channel merge working.
    #[rstest]
    #[case::nothing_sent(&[], &[], &[], 0, 0)]
    #[case::every_ref_is_a_primary(&[100, 200], &[(100, 100), (200, 200)], &[100, 200], 0, 0)]
    #[case::the_radio_tail_is_dropped(&[100, 4424001], &[(100, 100)], &[100], 1, 0)]
    #[case::a_member_ref_resolves_to_its_owner(&[555], &[(555, 100)], &[100], 0, 0)]
    #[case::two_refs_for_one_channel_collapse(&[100, 555], &[(100, 100), (555, 100)], &[100], 0, 1)]
    #[case::the_same_ref_twice_collapses(&[100, 100], &[(100, 100)], &[100], 0, 1)]
    #[case::order_is_the_order_the_recorder_sent(&[200, 100], &[(100, 100), (200, 200)], &[200, 100], 0, 0)]
    fn a_patches_array_resolves_to_this_systems_channels(
        #[case] refs: &[i64],
        #[case] canonical: &[(i64, i64)],
        #[case] members: &[i64],
        #[case] dropped: usize,
        #[case] collapsed: usize,
    ) {
        let canonical: std::collections::HashMap<i64, i64> = canonical.iter().copied().collect();

        let found = patch_members_of(refs, &canonical);

        assert_eq!(found.members, members);
        assert_eq!(found.dropped, dropped, "refs no Talkgroup claims");
        assert_eq!(found.collapsed, collapsed, "refs that named one channel");
    }

    #[rstest]
    // Present in the list (whitespace tolerated).
    #[case(Some("54241"), 54241, true)]
    #[case(Some("100,54241,200"), 54241, true)]
    #[case(Some(" 100 , 54241 , 200 "), 54241, true)]
    // Absent / empty / junk.
    #[case(Some("100,200"), 54241, false)]
    #[case(Some(""), 54241, false)]
    #[case(Some(",,"), 54241, false)]
    #[case(Some("abc,54241x"), 54241, false)] // non-numeric junk never matches
    #[case(None, 54241, false)]
    // A prefix/substring must not match: "5424" is not "54241".
    #[case(Some("5424"), 54241, false)]
    fn blacklist_membership(
        #[case] blacklist: Option<&str>,
        #[case] talkgroup_ref: i64,
        #[case] expected: bool,
    ) {
        assert_eq!(is_blacklisted(blacklist, talkgroup_ref), expected);
    }
}
