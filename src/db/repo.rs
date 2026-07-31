//! Repository functions over the domain entities.
//!
//! **Auto-populate** (#8, ADR-0001) is the policy that keeps the archive usable
//! with zero manual config: an unknown System/Talkgroup/Unit is created the first
//! time a Call for it is ingested, using the recorder's labels and falling back
//! to rdio-scanner's defaults (`Untagged` Tag, `Unknown` Group, numeric Talkgroup
//! label, `Talkgroup <ref>` name, lowest-free Ref for new Systems). Two toggles
//! gate it — a global one ([`IngestConfig`](crate::ingest::IngestConfig)) and a
//! per-system one ([`system::Model::auto_populate`]) — and a per-system blacklist
//! drops chosen Talkgroups outright. [`ingest_disposition`] is the single place
//! that decision is made; [`insert_call`] applies the defaults on create.
//!
//! The archive-search query filters via joins + `DISTINCT` (portable across
//! SQLite/Postgres). It deliberately does **no** DB-side list aggregation
//! (`GROUP_CONCAT`/`STRING_AGG` diverge by dialect, ADR-0003) — a call's groups
//! are loaded separately and assembled in Rust.

use sea_orm::sea_query::{Alias, Expr};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DbErr, EntityTrait, JoinType, PaginatorTrait,
    QueryFilter, QueryOrder, QuerySelect, RelationTrait, Set,
};

use crate::call::{CallId, FilterOptions, StoredCall, SystemOption, TalkgroupOption};
use crate::catalog::{Catalog, CatalogSystem, CatalogTalkgroup, display_order};
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

/// Find the Unit a radio id belongs to, creating one if no Unit owns it. A Ref
/// is unique only within its System. `label` (a radio alias) is recorded only on
/// create — an existing Unit keeps its curated alias (#8 auto-populate), and a
/// Unit reached through a Range keeps it for the same reason: an apparatus is
/// named once, not renamed by whichever of its portables keyed last.
pub async fn resolve_or_create_unit<C: ConnectionTrait>(
    db: &C,
    system_id: i64,
    ext_ref: i64,
    label: Option<String>,
    now_ms: i64,
) -> Result<unit::Model, DbErr> {
    if let Some(found) = resolve_unit(db, system_id, ext_ref).await? {
        return Ok(found);
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

/// Find a Site by (System, Ref), creating it if absent (#42, spec US 11).
///
/// Unlike a Unit this is never gated on the auto-populate flag. A Site Ref is a
/// fact about the System's own infrastructure, not a radio somebody keyed — an
/// operator who turned auto-populate off did so to stop unknown Talkgroups
/// filling their panel, and unnamed towers are what makes simulcast coverage
/// legible rather than clutter. It is created without a label: #48 fills those
/// in from SDRTrunk's ID3 tags, where a site *is* a name.
pub async fn resolve_or_create_site<C: ConnectionTrait>(
    db: &C,
    system_id: i64,
    ext_ref: i64,
    now_ms: i64,
) -> Result<site::Model, DbErr> {
    if let Some(found) = site::Entity::find()
        .filter(site::Column::SystemId.eq(system_id))
        .filter(site::Column::Ref.eq(ext_ref))
        .one(db)
        .await?
    {
        return Ok(found);
    }
    site::ActiveModel {
        system_id: Set(system_id),
        r#ref: Set(ext_ref),
        label: Set(None),
        created_at_ms: Set(now_ms),
        ..Default::default()
    }
    .insert(db)
    .await
}

/// A unit heard within a call (rdio `sources[]`/`units[]`).
#[derive(Debug, Clone, Default)]
pub struct NewCallUnit {
    pub unit_ref: i64,
    pub label: Option<String>,
    pub offset_ms: Option<i64>,
    /// The alias the radio transmitted, where `label` is the one the recorder
    /// had configured (#42, TR's `tag_ota`).
    pub tag_ota: Option<String>,
    pub emergency: bool,
    pub signal_system: Option<String>,
    /// Wall-clock start of this Unit's transmission, unix milliseconds.
    pub at_ms: Option<i64>,
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

/// A Call to persist, described by Refs/labels as a recorder sends it.
#[derive(Debug, Clone, Default)]
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
    pub source_ref: Option<i64>,
    pub object_key: String,
    pub audio_mime: Option<String>,
    pub audio_name: Option<String>,
    /// Byte length of the audio object, filled in by ingest when it writes the
    /// object (#10 — retention's size cap sums it).
    pub audio_size: Option<i64>,
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
    pub patches: Vec<i64>,
    pub units: Vec<NewCallUnit>,
    pub frequencies: Vec<NewCallFrequency>,
}

/// Resolve the Call's System/Talkgroup/Tag/Groups by Ref (creating as needed with
/// auto-populate defaults), then insert the Call and its child rows. Returns the
/// stored Call.
///
/// A brand-new Talkgroup is auto-populated (#8) with the recorder's labels,
/// falling back to rdio-scanner's defaults (numeric label, `Talkgroup <ref>`
/// name, `Untagged` Tag, `Unknown` Group). An **existing** Talkgroup is left
/// untouched — auto-populate fills unknowns, it never rewrites curated rows. The
/// `auto_populate` flag (the effective global-or-per-system value from
/// [`ingest_disposition`]) gates only the Unit roster, matching rdio.
///
/// Not internally transactional — the caller (ingest) wraps this in one so the
/// resolve → insert sequence is atomic with the audio write (ADR-0001).
pub async fn insert_call<C: ConnectionTrait>(
    db: &C,
    new: &NewCall,
    auto_populate: bool,
    now_ms: i64,
) -> Result<call::Model, DbErr> {
    // System gets a `System <ref>` default label when the recorder sent none.
    // The label is only applied on create; an existing System keeps its own.
    let system_label = new
        .system_label
        .clone()
        .or_else(|| Some(format!("System {}", new.system_ref)));
    let sys = resolve_or_create_system(db, new.system_ref, system_label, now_ms).await?;

    let tg = match resolve_talkgroup(db, sys.id, new.talkgroup_ref).await? {
        Some(existing) => existing,
        None => create_populated_talkgroup(db, sys.id, new, now_ms).await?,
    };

    // A Site is discovered the way a Talkgroup is (#8): a multi-site System
    // learns its towers from the traffic, because a file naming them goes stale
    // the moment the operator adds one.
    let site_id = match new.site_ref {
        Some(site_ref) => Some(
            resolve_or_create_site(db, sys.id, site_ref, now_ms)
                .await?
                .id,
        ),
        None => None,
    };

    let stored = call::ActiveModel {
        system_id: Set(sys.id),
        talkgroup_id: Set(tg.id),
        // What the recorder said, beside what it resolved to (#45). Written on
        // every Call, not only the merged ones: a Ref becomes a member Ref long
        // after its Calls arrived, and a column filled in only once it mattered
        // would be empty in exactly the archive an operator wants to unfold.
        talkgroup_ref: Set(Some(new.talkgroup_ref)),
        call_at_ms: Set(new.call_at_ms),
        frequency: Set(new.frequency),
        source_ref: Set(new.source_ref),
        object_key: Set(new.object_key.clone()),
        audio_mime: Set(new.audio_mime.clone()),
        audio_name: Set(new.audio_name.clone()),
        audio_size: Set(new.audio_size),
        duration_ms: Set(new.duration_ms),
        stop_at_ms: Set(new.stop_at_ms),
        emergency: Set(new.emergency),
        encrypted: Set(new.encrypted),
        priority: Set(new.priority),
        audio_type: Set(new.audio_type.clone()),
        site_id: Set(site_id),
        // Every Call arrives passthrough. Set explicitly rather than left to a
        // column default, because a fresh database gets its `calls` table from
        // the entity-derived DDL in `m0001_init`, which carries no defaults —
        // only an upgraded database goes through `m0006`'s `ALTER`.
        enhancement: Set(call::EnhancementState::NONE.to_string()),
        created_at_ms: Set(now_ms),
        ..Default::default()
    }
    .insert(db)
    .await?;

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
    let patched = patch_members(db, sys.id, &new.patches).await?;
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
        tracing::debug!(
            dropped = patched.dropped,
            collapsed = patched.collapsed,
            kept,
            %new.system_ref,
            "patch refs resolved to this System's channels"
        );
    }
    for patch in patched.members {
        call_patch::ActiveModel {
            call_id: Set(stored.id),
            talkgroup_ref: Set(patch),
            ..Default::default()
        }
        .insert(db)
        .await?;
    }
    for u in &new.units {
        call_unit::ActiveModel {
            call_id: Set(stored.id),
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
            call_id: Set(stored.id),
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

    // Unit roster (#8): a heard radio becomes a Unit entity only when the recorder
    // gave it an alias — rdio rosters units with a non-empty label (`controller.go`),
    // not every anonymous Ref. Gated on auto-populate like rdio; the per-call
    // `call_units` detail above is always recorded regardless.
    if auto_populate {
        for u in &new.units {
            if u.unit_ref > 0 && u.label.is_some() {
                resolve_or_create_unit(db, sys.id, u.unit_ref, u.label.clone(), now_ms).await?;
            }
        }
    }

    Ok(stored)
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
/// Three statements at worst and one at best: the primaries, then — only when
/// some ref was not one — the member rows and their owners, each in a single
/// query. An instance with no merges pays exactly what it did before, and the
/// empty array (almost every Call) still costs nothing at all.
async fn patch_members<C: ConnectionTrait>(
    db: &C,
    system_id: i64,
    refs: &[i64],
) -> Result<PatchMembers, DbErr> {
    if refs.is_empty() {
        return Ok(PatchMembers::default());
    }
    // Arriving Ref -> the primary Ref of the channel that owns it. A primary Ref
    // owns itself, which is why the first pass maps each hit to itself.
    let mut canonical: std::collections::HashMap<i64, i64> = talkgroup::Entity::find()
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
    Ok(found)
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
    new: &NewCall,
    now_ms: i64,
) -> Result<talkgroup::Model, DbErr> {
    let tag_name = new.talkgroup_tag.as_deref().unwrap_or(DEFAULT_TAG);
    let tag_id = resolve_or_create_tag(db, tag_name, now_ms).await?.id;
    let label = new
        .talkgroup_label
        .clone()
        .unwrap_or_else(|| new.talkgroup_ref.to_string());
    let name = new
        .talkgroup_name
        .clone()
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

    let group_labels: Vec<&str> = if new.talkgroup_groups.is_empty() {
        vec![DEFAULT_GROUP]
    } else {
        new.talkgroup_groups.iter().map(String::as_str).collect()
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
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MergeChange {
    /// Talkgroups that stopped existing in their own right.
    pub folded: u64,
    /// Member Refs that became Talkgroups again.
    pub unfolded: u64,
    /// Calls whose owning Talkgroup changed, in either direction.
    pub calls_repointed: u64,
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
    }
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
    Ok(change)
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
    if let Some(source) = source {
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

        label = source.label;
        change.folded = 1;
    }

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
        &NewCall {
            talkgroup_ref: member.r#ref,
            talkgroup_label: member.label.clone(),
            ..Default::default()
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
    let existing: Vec<crate::merge::Range> = unit_ref::Entity::find()
        .filter(unit_ref::Column::SystemId.eq(unit.system_id))
        .all(db)
        .await?
        .into_iter()
        .map(|owned| crate::merge::Range::new(owned.ref_from, owned.ref_to))
        .collect();
    if let Some(collision) = crate::merge::first_overlap(&existing, &range) {
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

/// Which end of the archive a search walks from.
///
/// Newest-first is the scanner default ("what just happened"); oldest-first is
/// what **playback mode** needs, because catching up on history means walking
/// forwards in time through the filtered results (#13, spec US 25).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CallSort {
    #[default]
    Newest,
    Oldest,
}

/// Cascading archive-search filters. All are optional and combine with AND;
/// `limit == 0` means unlimited.
#[derive(Debug, Clone, Default)]
pub struct CallSearch {
    pub after_ms: Option<i64>,
    pub before_ms: Option<i64>,
    pub system_ref: Option<i64>,
    pub talkgroup_ref: Option<i64>,
    pub group_name: Option<String>,
    pub tag_name: Option<String>,
    /// Hide anything shorter than this many milliseconds (#42, spec US 8) — the
    /// kerchunk filter. A Call whose length was never measured never matches:
    /// a threshold cannot be tested against an unknown, and admitting unknowns
    /// would make the filter quietly not filter the part of an upgraded archive
    /// that predates the duration column.
    pub min_duration_ms: Option<i64>,
    pub sort: CallSort,
    pub limit: u64,
    pub offset: u64,
}

/// Relations a caller has already joined onto a Call query, so [`apply_filters`]
/// doesn't join them a second time (which SQL would reject as a duplicate
/// alias). The search path joins nothing up front and lets the filters pull in
/// only what they need; the facet queries join System/Talkgroup/Tag eagerly
/// because they *select* from those tables.
#[derive(Debug, Clone, Copy, Default)]
struct Joined {
    system: bool,
    talkgroup: bool,
    tag: bool,
}

/// Apply the archive-search filters to a Call query — the single definition of
/// what each filter means, shared by the result page, the total count, and the
/// cascading filter options.
fn apply_filters(
    mut query: sea_orm::Select<call::Entity>,
    search: &CallSearch,
    joined: Joined,
) -> sea_orm::Select<call::Entity> {
    if let Some(after) = search.after_ms {
        query = query.filter(call::Column::CallAtMs.gte(after));
    }
    if let Some(before) = search.before_ms {
        query = query.filter(call::Column::CallAtMs.lte(before));
    }
    if let Some(system_ref) = search.system_ref {
        if !joined.system {
            query = query.join(JoinType::InnerJoin, call::Relation::System.def());
        }
        query = query.filter(system::Column::Ref.eq(system_ref));
    }
    if let Some(min_duration_ms) = search.min_duration_ms {
        // `>=` on a nullable column is already false for `NULL` in both
        // dialects, so the unmeasured Calls fall out here without a second
        // clause — but the behaviour is load-bearing enough to be tested
        // (`a_call_with_no_known_duration_does_not_match_a_duration_filter`)
        // rather than left to SQL's three-valued logic being remembered.
        query = query.filter(call::Column::DurationMs.gte(min_duration_ms));
    }

    // Talkgroup is the hinge: the Talkgroup, Tag, and Group filters all reach
    // the Call through it.
    let needs_talkgroup =
        search.talkgroup_ref.is_some() || search.tag_name.is_some() || search.group_name.is_some();
    if needs_talkgroup && !joined.talkgroup {
        query = query.join(JoinType::InnerJoin, call::Relation::Talkgroup.def());
    }
    if let Some(talkgroup_ref) = search.talkgroup_ref {
        query = query.filter(talkgroup::Column::Ref.eq(talkgroup_ref));
    }
    if let Some(tag_name) = &search.tag_name {
        if !joined.tag {
            query = query.join(JoinType::InnerJoin, talkgroup::Relation::Tag.def());
        }
        query = query.filter(tag::Column::Name.eq(tag_name.clone()));
    }
    if let Some(group_name) = &search.group_name {
        query = query
            .join(
                JoinType::InnerJoin,
                talkgroup::Relation::TalkgroupGroup.def(),
            )
            .join(JoinType::InnerJoin, talkgroup_group::Relation::Group.def())
            .filter(group::Column::Name.eq(group_name.clone()));
    }

    query
}

/// The filtered Call query, without ordering or paging.
///
/// `DISTINCT` is applied only when the Group filter is in play — that join is
/// the one that can multiply a Call across rows (a Talkgroup may be in several
/// Groups). Every other join is many-to-one, so deduplicating would just cost a
/// sort buffer on every plain search: real money on a Pi.
fn filtered_calls(search: &CallSearch) -> sea_orm::Select<call::Entity> {
    let query = apply_filters(call::Entity::find(), search, Joined::default());
    if search.group_name.is_some() {
        query.distinct()
    } else {
        query
    }
}

/// Search calls, filtered by date range / System / Talkgroup / Group / Tag and
/// ordered by [`CallSearch::sort`]. Distinct calls only, even when a talkgroup
/// is in several groups.
pub async fn search_calls<C: ConnectionTrait>(
    db: &C,
    search: &CallSearch,
) -> Result<Vec<call::Model>, DbErr> {
    let mut query = filtered_calls(search);

    // `id` breaks ties in the same direction as the timestamp, so paging is
    // stable even when a recorder stamps two calls at the same millisecond.
    query = match search.sort {
        CallSort::Newest => query
            .order_by_desc(call::Column::CallAtMs)
            .order_by_desc(call::Column::Id),
        CallSort::Oldest => query
            .order_by_asc(call::Column::CallAtMs)
            .order_by_asc(call::Column::Id),
    };

    // SQLite rejects OFFSET without LIMIT, so an offset with no explicit limit
    // gets an effectively-unbounded one; a zero offset emits no OFFSET at all.
    let effective_limit = match (search.limit, search.offset) {
        (0, 0) => None,
        (0, _) => Some(i64::MAX as u64),
        (limit, _) => Some(limit),
    };
    if let Some(limit) = effective_limit {
        query = query.limit(limit);
    }
    if search.offset > 0 {
        query = query.offset(search.offset);
    }

    query.all(db).await
}

/// How many Calls match `search`, ignoring its page window — the total a
/// paginator reports and the client uses to size its page controls.
pub async fn count_calls<C: ConnectionTrait>(db: &C, search: &CallSearch) -> Result<u64, DbErr> {
    filtered_calls(search).count(db).await
}

/// One distinct (System, Talkgroup, Tag) combination that has Calls matching a
/// search — the raw material the cascading filter options are built from.
#[derive(Debug, Clone, sea_orm::FromQueryResult)]
struct FacetRow {
    system_ref: i64,
    system_label: Option<String>,
    talkgroup_ref: i64,
    talkgroup_label: Option<String>,
    tag_name: Option<String>,
}

/// The distinct System/Talkgroup/Tag combinations reachable under `search`.
async fn facet_rows<C: ConnectionTrait>(
    db: &C,
    search: &CallSearch,
) -> Result<Vec<FacetRow>, DbErr> {
    let query = call::Entity::find()
        .select_only()
        .column_as(system::Column::Ref, "system_ref")
        .column_as(system::Column::Label, "system_label")
        .column_as(talkgroup::Column::Ref, "talkgroup_ref")
        .column_as(talkgroup::Column::Label, "talkgroup_label")
        .column_as(tag::Column::Name, "tag_name")
        .join(JoinType::InnerJoin, call::Relation::System.def())
        .join(JoinType::InnerJoin, call::Relation::Talkgroup.def())
        // A Talkgroup may carry no Tag, so this one can't be an inner join.
        .join(JoinType::LeftJoin, talkgroup::Relation::Tag.def());

    apply_filters(
        query,
        search,
        Joined {
            system: true,
            talkgroup: true,
            tag: true,
        },
    )
    .distinct()
    .order_by_asc(system::Column::Ref)
    .order_by_asc(talkgroup::Column::Ref)
    .into_model::<FacetRow>()
    .all(db)
    .await
}

/// The distinct Group names reachable under `search`.
async fn group_facets<C: ConnectionTrait>(
    db: &C,
    search: &CallSearch,
) -> Result<Vec<String>, DbErr> {
    let query = call::Entity::find()
        .select_only()
        .column_as(group::Column::Name, "name")
        .join(JoinType::InnerJoin, call::Relation::Talkgroup.def())
        .join(
            JoinType::InnerJoin,
            talkgroup::Relation::TalkgroupGroup.def(),
        )
        .join(JoinType::InnerJoin, talkgroup_group::Relation::Group.def());

    // The caller always clears `group_name` before asking for Group options, so
    // `apply_filters` never adds a second copy of the two joins above.
    apply_filters(
        query,
        search,
        Joined {
            talkgroup: true,
            ..Default::default()
        },
    )
    .distinct()
    .order_by_asc(group::Column::Name)
    .into_tuple::<String>()
    .all(db)
    .await
}

/// Oldest and newest Call time (unix ms) reachable under `search`, or
/// `(None, None)` when nothing matches.
async fn call_time_bounds<C: ConnectionTrait>(
    db: &C,
    search: &CallSearch,
) -> Result<(Option<i64>, Option<i64>), DbErr> {
    // MIN/MAX stay `BIGINT` on both dialects (unlike SUM, which Postgres widens
    // to `numeric` — see `total_audio_bytes`), so one decode works for both.
    Ok(filtered_calls(search)
        .select_only()
        .column_as(call::Column::CallAtMs.min(), "start")
        .column_as(call::Column::CallAtMs.max(), "stop")
        .into_tuple::<(Option<i64>, Option<i64>)>()
        .one(db)
        .await?
        .unwrap_or((None, None)))
}

/// The values each filter can usefully take given the *others* already chosen
/// (#13, spec US 24).
///
/// Every dimension is computed with its **own** filter cleared: picking System
/// 100 narrows the Talkgroup list but leaves the System list complete, so a
/// choice is always reversible. Only values backed by real Calls are offered —
/// rdio-scanner populates the same dropdowns from its whole Talkgroup config and
/// so offers options that search to nothing.
pub async fn filter_options<C: ConnectionTrait>(
    db: &C,
    search: &CallSearch,
) -> Result<FilterOptions, DbErr> {
    let without = |mutate: fn(&mut CallSearch)| {
        let mut cleared = search.clone();
        mutate(&mut cleared);
        cleared
    };

    // A facet row is one (System, Talkgroup, Tag) combination, so each dimension
    // is that row set collapsed onto its own key. The rows arrive ordered by
    // (System Ref, Talkgroup Ref), and `dedup_by_key` preserves that order.
    let systems = dedup_by_key(
        facet_rows(db, &without(|s| s.system_ref = None)).await?,
        |row| row.system_ref,
        |row| SystemOption {
            r#ref: row.system_ref,
            label: row.system_label,
        },
    );

    let talkgroups = dedup_by_key(
        facet_rows(db, &without(|s| s.talkgroup_ref = None)).await?,
        |row| (row.system_ref, row.talkgroup_ref),
        |row| TalkgroupOption {
            system_ref: row.system_ref,
            r#ref: row.talkgroup_ref,
            label: row.talkgroup_label,
            tag: row.tag_name,
        },
    );

    let mut tags: Vec<String> = facet_rows(db, &without(|s| s.tag_name = None))
        .await?
        .into_iter()
        .filter_map(|row| row.tag_name)
        .collect();
    tags.sort();
    tags.dedup();

    let groups = group_facets(db, &without(|s| s.group_name = None)).await?;

    let (date_start_ms, date_stop_ms) = call_time_bounds(
        db,
        &without(|s| {
            s.after_ms = None;
            s.before_ms = None;
        }),
    )
    .await?;

    Ok(FilterOptions {
        systems,
        talkgroups,
        groups,
        tags,
        date_start_ms,
        date_stop_ms,
    })
}

/// Keep the first row for each distinct `key`, in input order, mapped to the
/// option it describes.
fn dedup_by_key<Row, Key, Option_>(
    rows: Vec<Row>,
    key: impl Fn(&Row) -> Key,
    build: impl Fn(Row) -> Option_,
) -> Vec<Option_>
where
    Key: std::hash::Hash + Eq,
{
    let mut seen = std::collections::HashSet::new();
    rows.into_iter()
        .filter(|row| seen.insert(key(row)))
        .map(build)
        .collect()
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

/// Every System and Talkgroup a listener can select (#12, spec US 19–21).
///
/// Three flat queries — systems, talkgroups (with their Tag), and the
/// Talkgroup→Group links — assembled in Rust. Constant in the number of
/// round trips rather than one per Talkgroup, which is what makes this
/// affordable on a Pi with a few hundred Talkgroups, and it keeps the
/// dialect-divergent list aggregation out of SQL (ADR-0003), like the archive
/// search above.
pub async fn catalog<C: ConnectionTrait>(db: &C) -> Result<Catalog, DbErr> {
    let mut groups_by_talkgroup: std::collections::HashMap<i64, Vec<String>> =
        std::collections::HashMap::new();
    for (link, group) in talkgroup_group::Entity::find()
        .find_also_related(group::Entity)
        .all(db)
        .await?
    {
        if let Some(group) = group {
            groups_by_talkgroup
                .entry(link.talkgroup_id)
                .or_default()
                .push(group.name);
        }
    }

    let mut talkgroups_by_system: std::collections::HashMap<i64, Vec<CatalogTalkgroup>> =
        std::collections::HashMap::new();
    for (talkgroup, tag) in talkgroup::Entity::find()
        .find_also_related(tag::Entity)
        .all(db)
        .await?
    {
        let mut groups = groups_by_talkgroup
            .remove(&talkgroup.id)
            .unwrap_or_default();
        groups.sort();
        talkgroups_by_system
            .entry(talkgroup.system_id)
            .or_default()
            .push(CatalogTalkgroup {
                r#ref: talkgroup.r#ref,
                label: talkgroup.label,
                name: talkgroup.name,
                tag: tag.map(|tag| tag.name),
                groups,
                led: talkgroup.led,
            });
    }

    let mut systems: Vec<CatalogSystem> = system::Entity::find()
        .all(db)
        .await?
        .into_iter()
        .map(|system| {
            let mut talkgroups = talkgroups_by_system.remove(&system.id).unwrap_or_default();
            talkgroups.sort_by_key(|tg| display_order(tg.label.as_deref(), tg.r#ref));
            CatalogSystem {
                r#ref: system.r#ref,
                label: system.label,
                talkgroups,
            }
        })
        .collect();
    systems.sort_by_key(|system| display_order(system.label.as_deref(), system.r#ref));

    Ok(Catalog { systems })
}

/// The most-recent up-to-`limit` Calls with `id > since_id`, returned in
/// ascending id order (ready to enqueue oldest-first).
///
/// This backs the live feed's **reconnect catch-up** (#9, an improvement over
/// rdio, which drops any Call that arrives while a listener is briefly
/// disconnected). A reconnecting client sends the last Call id it saw as `since`;
/// the server backfills what it missed, bounded to `limit` so a client returning
/// after a long gap replays a recent slice (not the whole archive) and falls back
/// to archive search (#13) for more. Ordering by `id` (monotonic insert order),
/// not `call_at_ms`, keeps "since the last one I saw" exact even if recorder
/// timestamps are out of order. The caller filters the result through the
/// connection's subscription + access scope, so this deliberately does no
/// matrix filtering of its own.
pub async fn recent_calls_since<C: ConnectionTrait>(
    db: &C,
    since_id: CallId,
    limit: u64,
) -> Result<Vec<call::Model>, DbErr> {
    let mut newest = call::Entity::find()
        .filter(call::Column::Id.gt(since_id))
        .order_by_desc(call::Column::Id)
        .limit(limit)
        .all(db)
        .await?;
    newest.reverse(); // newest-first query -> ascending for oldest-first delivery
    Ok(newest)
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

/// Register `raw_key` unless it is already known. Returns whether it was added.
///
/// This is how a key configured out-of-band — `RADIO_SCOUT_API_KEY`, typically
/// from `.env` (ADR-0012 keeps it there, since first run *writes* it) —
/// survives restarts: the recorder's
/// configured secret keeps working across boots without stacking up a row per
/// boot. A key an operator **disabled** counts as known and stays disabled;
/// re-registering must never quietly undo a revocation (ADR-0008).
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

/// Whether `raw_key` is a valid, enabled key scoped to `system_ref`. Denied when
/// the key is missing, disabled, or scoped to a different System (ADR-0008:
/// recorders always require a valid per-system key).
pub async fn authorize_ingest<C: ConnectionTrait>(
    db: &C,
    raw_key: &str,
    system_ref: i64,
) -> Result<bool, DbErr> {
    let Some(key) = api_key::Entity::find()
        .filter(api_key::Column::KeyHash.eq(hash_key(raw_key)))
        .one(db)
        .await?
    else {
        return Ok(false);
    };
    if key.disabled {
        return Ok(false);
    }
    Ok(match key.system_ref {
        None => true,
        Some(scoped) => scoped == system_ref,
    })
}

/// Is there already a Call on this Talkgroup within `±window_ms` of
/// `call_at_ms`? (ADR-0001 duplicate detection.)
///
/// Keyed on the **canonical** Talkgroup's Id rather than the Ref a recorder
/// sent (#45), which is what makes a merge collapse the traffic and not merely
/// the panel: a multi-site System uploading one transmission twice, once under
/// each of its Refs, is the plainest reason to merge two channels, and a merge
/// that left the Call playing twice would have fixed nothing a listener hears.
/// Recognising the same transmission across *different* channels, and keeping
/// the better copy of it, is #46's widening of this predicate.
///
/// `None` means no Talkgroup owns that Ref yet, which is not a state a duplicate
/// can exist in: a Call is a row referencing a Talkgroup row, so no Talkgroup
/// means no Calls to be a duplicate of. It also costs no query, where the
/// previous shape spent one joining through two tables to reach the same
/// certainty — and both Systems and Talkgroups have now been resolved by
/// [`ingest_disposition`] a moment earlier anyway.
pub async fn is_duplicate_call<C: ConnectionTrait>(
    db: &C,
    talkgroup_id: Option<i64>,
    call_at_ms: i64,
    window_ms: i64,
) -> Result<bool, DbErr> {
    let Some(talkgroup_id) = talkgroup_id else {
        return Ok(false);
    };
    let count = call::Entity::find()
        .filter(call::Column::TalkgroupId.eq(talkgroup_id))
        .filter(call::Column::CallAtMs.gte(call_at_ms - window_ms))
        .filter(call::Column::CallAtMs.lte(call_at_ms + window_ms))
        .count(db)
        .await?;
    Ok(count > 0)
}

// ---------------------------------------------------------------------------
// Auto-populate + blacklist policy (#8, ADR-0001).
// ---------------------------------------------------------------------------

/// Why an incoming Call was dropped by [`ingest_disposition`]. The two paths are
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
    ///
    /// `talkgroup_id` is the **canonical** channel the Ref resolved to (#45) —
    /// `None` when nothing owns it yet and auto-populate is about to create it.
    /// Carried rather than rediscovered because deciding the disposition already
    /// required resolving it, and dedup needs the same answer two statements
    /// later: on a Pi taking a Call a second, a query saved is a query saved.
    Store {
        auto_populate: bool,
        talkgroup_id: Option<i64>,
    },
    /// Drop it silently.
    Drop(DropReason),
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

/// Decide what to do with an incoming Call before any audio is written (#8).
///
/// Mirrors rdio-scanner's `controller.go`: a brand-new System is auto-created
/// only when the **global** toggle is on; an unknown Talkgroup under a known
/// System is auto-created when either the global or that System's per-system flag
/// is on; and a blacklisted Talkgroup Ref is dropped regardless. Unlike rdio
/// (which only blacklist-checks already-known Talkgroups), the blacklist here
/// applies to a Talkgroup Ref even on its first sighting — "never ingest this"
/// should hold from the first call.
pub async fn ingest_disposition<C: ConnectionTrait>(
    db: &C,
    system_ref: i64,
    talkgroup_ref: i64,
    global_auto_populate: bool,
) -> Result<Disposition, DbErr> {
    let Some(sys) = system::Entity::find()
        .filter(system::Column::Ref.eq(system_ref))
        .one(db)
        .await?
    else {
        // Unknown System: only auto-created when the global toggle is on. No
        // System means no Talkgroup, so there is nothing to have resolved.
        return Ok(if global_auto_populate {
            Disposition::Store {
                auto_populate: true,
                talkgroup_id: None,
            }
        } else {
            Disposition::Drop(DropReason::NotPopulated)
        });
    };

    // Resolved once, and used for both of the decisions below (#45).
    let owner = resolve_talkgroup(db, sys.id, talkgroup_ref).await?;

    // **Both Refs are judged**: the one the recorder sent, and the primary Ref of
    // the channel it resolves to (#45). Blacklisting a channel has to reach every
    // Ref it answers to, or merging a Ref into a blacklisted channel would be a
    // way around the operator's own policy — that is the spec's "blacklists
    // operate on the canonical entity". Judging *only* the canonical one would
    // be the mirror-image bug: a Ref an operator refused yesterday would start
    // storing the day somebody merged it, with nothing said about the change.
    let arrived_as_blacklisted = is_blacklisted(sys.blacklist.as_deref(), talkgroup_ref);
    let owner_blacklisted = owner
        .as_ref()
        .is_some_and(|tg| is_blacklisted(sys.blacklist.as_deref(), tg.r#ref));
    if arrived_as_blacklisted || owner_blacklisted {
        return Ok(Disposition::Drop(DropReason::Blacklisted));
    }

    let effective = global_auto_populate || sys.auto_populate;
    // A Call for an already-known Talkgroup is always stored; an unknown one needs
    // auto-populate to bring it into being. A member Ref counts as known — its
    // channel exists, and nothing is about to be created.
    let known_talkgroup = owner.is_some();
    Ok(if known_talkgroup || effective {
        Disposition::Store {
            auto_populate: effective,
            talkgroup_id: owner.map(|tg| tg.id),
        }
    } else {
        Disposition::Drop(DropReason::NotPopulated)
    })
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

/// Build the denormalized `StoredCall` view (the live-feed / serve DTO) for a
/// stored call by joining its System, Talkgroup, Tag, and Groups.
pub async fn stored_call<C: ConnectionTrait>(
    db: &C,
    id: CallId,
) -> Result<Option<StoredCall>, DbErr> {
    let Some(call) = call::Entity::find_by_id(id).one(db).await? else {
        return Ok(None);
    };
    Ok(stored_calls(db, std::slice::from_ref(&call)).await?.pop())
}

/// Denormalize a whole page of Calls in a fixed number of queries — five, no
/// matter how many Calls — rather than five *per Call*.
///
/// This is what lets `GET /api/calls` answer with ready-to-render, ready-to-play
/// results. rdio-scanner instead returns bare ids and has the client re-request
/// every Call it wants to display, which is an N+1 over its WebSocket and the
/// single worst part of its archive UX on a Pi. Results come back in the order
/// they were given.
pub async fn stored_calls<C: ConnectionTrait>(
    db: &C,
    calls: &[call::Model],
) -> Result<Vec<StoredCall>, DbErr> {
    use std::collections::HashMap;

    if calls.is_empty() {
        return Ok(Vec::new());
    }

    let system_ids = distinct(calls.iter().map(|c| c.system_id));
    let systems: HashMap<i64, system::Model> = system::Entity::find()
        .filter(system::Column::Id.is_in(system_ids))
        .all(db)
        .await?
        .into_iter()
        .map(|s| (s.id, s))
        .collect();

    let talkgroup_ids = distinct(calls.iter().map(|c| c.talkgroup_id));
    let talkgroups: HashMap<i64, talkgroup::Model> = talkgroup::Entity::find()
        .filter(talkgroup::Column::Id.is_in(talkgroup_ids.clone()))
        .all(db)
        .await?
        .into_iter()
        .map(|t| (t.id, t))
        .collect();

    let tag_ids = distinct(talkgroups.values().filter_map(|t| t.tag_id));
    let tags: HashMap<i64, String> = tag::Entity::find()
        .filter(tag::Column::Id.is_in(tag_ids))
        .all(db)
        .await?
        .into_iter()
        .map(|t| (t.id, t.name))
        .collect();

    // A Talkgroup may be in several Groups; the view carries one, and the
    // alphabetically-first is the stable pick. Ordering in SQL means "first
    // wins" here.
    let mut group_of: HashMap<i64, String> = HashMap::new();
    for (talkgroup_id, name) in talkgroup_group::Entity::find()
        .select_only()
        .column(talkgroup_group::Column::TalkgroupId)
        .column_as(group::Column::Name, "name")
        .join(JoinType::InnerJoin, talkgroup_group::Relation::Group.def())
        .filter(talkgroup_group::Column::TalkgroupId.is_in(talkgroup_ids))
        .order_by_asc(group::Column::Name)
        .into_tuple::<(i64, String)>()
        .all(db)
        .await?
    {
        group_of.entry(talkgroup_id).or_insert(name);
    }

    // Sites (#42, spec US 11). The wire carries the recorder-facing **Ref**,
    // like every other id a client sees; the column is an internal Id.
    let site_refs: HashMap<i64, i64> = site::Entity::find()
        .filter(site::Column::Id.is_in(distinct(calls.iter().filter_map(|c| c.site_id))))
        .all(db)
        .await?
        .into_iter()
        .map(|s| (s.id, s.r#ref))
        .collect();

    // Patched Talkgroup Refs (rdio `patches[]`): carried on the wire and used for
    // live-feed patch fanout (#9). Ordered for a stable payload.
    let mut patches_of: HashMap<CallId, Vec<i64>> = HashMap::new();
    for patch in call_patch::Entity::find()
        .filter(call_patch::Column::CallId.is_in(calls.iter().map(|c| c.id)))
        .order_by_asc(call_patch::Column::TalkgroupRef)
        .all(db)
        .await?
    {
        patches_of
            .entry(patch.call_id)
            .or_default()
            .push(patch.talkgroup_ref);
    }

    Ok(calls
        .iter()
        .map(|call| {
            let system = systems.get(&call.system_id);
            let talkgroup = talkgroups.get(&call.talkgroup_id);
            StoredCall {
                id: call.id,
                // A Call always has a System and a Talkgroup (`RESTRICT` foreign
                // keys), so these fallbacks are belt-and-braces, not a real case.
                system_ref: system.map_or(0, |s| s.r#ref),
                system_label: system.and_then(|s| s.label.clone()),
                talkgroup_ref: talkgroup.map_or(0, |t| t.r#ref),
                talkgroup_label: talkgroup.and_then(|t| t.label.clone()),
                talkgroup_group: group_of.get(&call.talkgroup_id).cloned(),
                talkgroup_tag: talkgroup
                    .and_then(|t| t.tag_id)
                    .and_then(|tag_id| tags.get(&tag_id).cloned()),
                led: talkgroup.and_then(|t| t.led.clone()),
                patches: patches_of.remove(&call.id).unwrap_or_default(),
                frequency: call.frequency,
                source: call.source_ref,
                date_time: None,
                timestamp: Some(call.call_at_ms),
                audio_mime: call.audio_mime.clone(),
                duration_ms: call.duration_ms,
                emergency: call.emergency,
                encrypted: call.encrypted,
                site_ref: call.site_id.and_then(|id| site_refs.get(&id).copied()),
                object_key: call.object_key.clone(),
                // An empty key means no object was ever written — an encrypted
                // Call (#42). Offering a URL for it would be offering a 404.
                audio_url: (!call.object_key.is_empty())
                    .then(|| format!("/api/call/{}/audio", call.id)),
            }
        })
        .collect())
}

/// One Call with everything the recorder said about it (#42) — the query behind
/// `GET /api/call/{id}`.
///
/// Three statements past [`stored_call`]'s, and only ever for one Call: this is
/// the detail deliberately kept off the live-feed frame and the search page, so
/// it is paid for exactly when somebody opens a Call.
pub async fn call_detail<C: ConnectionTrait>(
    db: &C,
    id: CallId,
) -> Result<Option<crate::call::CallDetail>, DbErr> {
    let Some(row) = call::Entity::find_by_id(id).one(db).await? else {
        return Ok(None);
    };

    // Ordered by the id they were inserted under, which is the order the
    // recorder listed them — a timeline read out of order is not a timeline.
    let frequencies = call_frequency::Entity::find()
        .filter(call_frequency::Column::CallId.eq(id))
        .order_by_asc(call_frequency::Column::Id)
        .all(db)
        .await?
        .into_iter()
        .map(|f| crate::call::CallFrequencyDetail {
            freq: f.freq,
            pos_ms: f.pos_ms,
            len_ms: f.len_ms,
            dbm: f.dbm,
            error_count: f.error_count,
            spike_count: f.spike_count,
            at_ms: f.at_ms,
        })
        .collect();
    let units = call_unit::Entity::find()
        .filter(call_unit::Column::CallId.eq(id))
        .order_by_asc(call_unit::Column::Id)
        .all(db)
        .await?
        .into_iter()
        .map(|u| crate::call::CallUnitDetail {
            r#ref: u.unit_ref,
            label: u.label,
            tag_ota: u.tag_ota,
            offset_ms: u.offset_ms,
            emergency: u.emergency,
            signal_system: u.signal_system,
            at_ms: u.at_ms,
        })
        .collect();

    // `.map` rather than a second `let … else`: one row in gives one view out,
    // so a `None` here is a case that cannot happen — and writing the branch
    // anyway would mean either an untestable line or a panic on a read path.
    // [`crate::archive::load_call`] resolves the same shape the same way.
    Ok(stored_calls(db, std::slice::from_ref(&row))
        .await?
        .pop()
        .map(|call| crate::call::CallDetail {
            call,
            stop_ms: row.stop_at_ms,
            priority: row.priority,
            audio_type: row.audio_type,
            frequencies,
            units,
        }))
}

/// The distinct values of an iterator, order-insensitive — the `IN (…)` list for
/// a batched lookup.
fn distinct(ids: impl Iterator<Item = i64>) -> Vec<i64> {
    ids.collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect()
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
