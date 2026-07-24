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

use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DbErr, EntityTrait, JoinType, PaginatorTrait,
    QueryFilter, QueryOrder, QuerySelect, RelationTrait, Set,
};

use crate::call::{CallId, StoredCall};
use crate::db::entities::{
    api_key, call, call_frequency, call_patch, call_unit, group, system, tag, talkgroup,
    talkgroup_group, unit,
};

/// Default Tag label for an auto-populated Talkgroup the recorder sent no tag for
/// (rdio-scanner `controller.go`).
const DEFAULT_TAG: &str = "Untagged";
/// Default Group label for an auto-populated Talkgroup the recorder sent no group
/// for (rdio-scanner `controller.go`).
const DEFAULT_GROUP: &str = "Unknown";

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

/// Find a Unit by (System, Ref), creating it if absent. A Ref is unique only
/// within its System. `label` (a radio alias) is recorded only on create — an
/// existing Unit keeps its curated alias (#8 auto-populate).
pub async fn resolve_or_create_unit<C: ConnectionTrait>(
    db: &C,
    system_id: i64,
    ext_ref: i64,
    label: Option<String>,
    now_ms: i64,
) -> Result<unit::Model, DbErr> {
    if let Some(found) = unit::Entity::find()
        .filter(unit::Column::SystemId.eq(system_id))
        .filter(unit::Column::Ref.eq(ext_ref))
        .one(db)
        .await?
    {
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

/// A unit heard within a call (rdio `sources[]`/`units[]`).
#[derive(Debug, Clone, Default)]
pub struct NewCallUnit {
    pub unit_ref: i64,
    pub label: Option<String>,
    pub offset_ms: Option<i64>,
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
    pub duration_ms: Option<i64>,
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

    let tg = match find_talkgroup(db, sys.id, new.talkgroup_ref).await? {
        Some(existing) => existing,
        None => create_populated_talkgroup(db, sys.id, new, now_ms).await?,
    };

    let stored = call::ActiveModel {
        system_id: Set(sys.id),
        talkgroup_id: Set(tg.id),
        call_at_ms: Set(new.call_at_ms),
        frequency: Set(new.frequency),
        source_ref: Set(new.source_ref),
        object_key: Set(new.object_key.clone()),
        audio_mime: Set(new.audio_mime.clone()),
        audio_name: Set(new.audio_name.clone()),
        duration_ms: Set(new.duration_ms),
        created_at_ms: Set(now_ms),
        ..Default::default()
    }
    .insert(db)
    .await?;

    for patch in &new.patches {
        call_patch::ActiveModel {
            call_id: Set(stored.id),
            talkgroup_ref: Set(*patch),
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

/// Find a Talkgroup by (System, Ref). A Ref is unique only within its System.
async fn find_talkgroup<C: ConnectionTrait>(
    db: &C,
    system_id: i64,
    ext_ref: i64,
) -> Result<Option<talkgroup::Model>, DbErr> {
    talkgroup::Entity::find()
        .filter(talkgroup::Column::SystemId.eq(system_id))
        .filter(talkgroup::Column::Ref.eq(ext_ref))
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
    pub limit: u64,
    pub offset: u64,
}

/// Search calls newest-first, filtered by date range / System / Talkgroup /
/// Group / Tag. Distinct calls only, even when a talkgroup is in several groups.
pub async fn search_calls<C: ConnectionTrait>(
    db: &C,
    search: &CallSearch,
) -> Result<Vec<call::Model>, DbErr> {
    let mut query = call::Entity::find();

    if let Some(after) = search.after_ms {
        query = query.filter(call::Column::CallAtMs.gte(after));
    }
    if let Some(before) = search.before_ms {
        query = query.filter(call::Column::CallAtMs.lte(before));
    }
    if let Some(system_ref) = search.system_ref {
        query = query
            .join(JoinType::InnerJoin, call::Relation::System.def())
            .filter(system::Column::Ref.eq(system_ref));
    }

    let needs_talkgroup =
        search.talkgroup_ref.is_some() || search.tag_name.is_some() || search.group_name.is_some();
    if needs_talkgroup {
        query = query.join(JoinType::InnerJoin, call::Relation::Talkgroup.def());
    }
    if let Some(talkgroup_ref) = search.talkgroup_ref {
        query = query.filter(talkgroup::Column::Ref.eq(talkgroup_ref));
    }
    if let Some(tag_name) = &search.tag_name {
        query = query
            .join(JoinType::InnerJoin, talkgroup::Relation::Tag.def())
            .filter(tag::Column::Name.eq(tag_name.clone()));
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

    query = query
        .distinct()
        .order_by_desc(call::Column::CallAtMs)
        .order_by_desc(call::Column::Id);

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

/// Is there already a call for this System+Talkgroup within `±window_ms` of
/// `call_at_ms`? (ADR-0001 duplicate detection.)
pub async fn is_duplicate_call<C: ConnectionTrait>(
    db: &C,
    system_ref: i64,
    talkgroup_ref: i64,
    call_at_ms: i64,
    window_ms: i64,
) -> Result<bool, DbErr> {
    let count = call::Entity::find()
        .join(JoinType::InnerJoin, call::Relation::System.def())
        .join(JoinType::InnerJoin, call::Relation::Talkgroup.def())
        .filter(system::Column::Ref.eq(system_ref))
        .filter(talkgroup::Column::Ref.eq(talkgroup_ref))
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
/// distinct behaviours worth telling apart (in tests today, in operator logs once
/// #17 lands), though the recorder gets the same HTTP 200 either way so it never
/// retries.
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
        // Unknown System: only auto-created when the global toggle is on.
        return Ok(if global_auto_populate {
            Disposition::Store {
                auto_populate: true,
            }
        } else {
            Disposition::Drop(DropReason::NotPopulated)
        });
    };

    if is_blacklisted(sys.blacklist.as_deref(), talkgroup_ref) {
        return Ok(Disposition::Drop(DropReason::Blacklisted));
    }

    let effective = global_auto_populate || sys.auto_populate;
    // A Call for an already-known Talkgroup is always stored; an unknown one needs
    // auto-populate to bring it into being.
    let known_talkgroup = find_talkgroup(db, sys.id, talkgroup_ref).await?.is_some();
    Ok(if known_talkgroup || effective {
        Disposition::Store {
            auto_populate: effective,
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

/// The object key + mime for a call's audio (the serve path — lightweight).
pub async fn get_call_audio<C: ConnectionTrait>(
    db: &C,
    id: CallId,
) -> Result<Option<(String, Option<String>)>, DbErr> {
    Ok(call::Entity::find_by_id(id)
        .one(db)
        .await?
        .map(|c| (c.object_key, c.audio_mime)))
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

    let (system_ref, system_label) =
        match system::Entity::find_by_id(call.system_id).one(db).await? {
            Some(s) => (s.r#ref, s.label),
            None => (0, None),
        };
    let (talkgroup_ref, talkgroup_label, tag_id) =
        match talkgroup::Entity::find_by_id(call.talkgroup_id)
            .one(db)
            .await?
        {
            Some(t) => (t.r#ref, t.label, t.tag_id),
            None => (0, None, None),
        };
    let talkgroup_tag = match tag_id {
        Some(tid) => tag::Entity::find_by_id(tid).one(db).await?.map(|t| t.name),
        None => None,
    };
    let talkgroup_group = groups_for_talkgroup(db, call.talkgroup_id)
        .await?
        .into_iter()
        .next();

    // Patched talkgroup Refs (rdio `patches[]`): carried on the wire and used for
    // live-feed patch fanout (#9). Ordered for a stable payload.
    let patches = call_patch::Entity::find()
        .filter(call_patch::Column::CallId.eq(call.id))
        .order_by_asc(call_patch::Column::TalkgroupRef)
        .all(db)
        .await?
        .into_iter()
        .map(|p| p.talkgroup_ref)
        .collect();

    Ok(Some(StoredCall {
        id: call.id,
        system_ref,
        system_label,
        talkgroup_ref,
        talkgroup_label,
        talkgroup_group,
        talkgroup_tag,
        patches,
        frequency: call.frequency,
        source: call.source_ref,
        date_time: None,
        timestamp: Some(call.call_at_ms),
        audio_mime: call.audio_mime,
        object_key: call.object_key,
        audio_url: format!("/api/call/{}/audio", call.id),
    }))
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

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

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
