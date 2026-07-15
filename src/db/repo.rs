//! Repository functions over the domain entities.
//!
//! Resolve-or-create-by-Ref is the minimal population needed to persist a Call
//! under the Ref-vs-Id model; the richer auto-populate semantics (blacklist,
//! default Group/Tag, lowest-free-Ref, label reconciliation) are #8, and wiring
//! this into the ingest pipeline inside a transaction is #5.
//!
//! The archive-search query filters via joins + `DISTINCT` (portable across
//! SQLite/Postgres). It deliberately does **no** DB-side list aggregation
//! (`GROUP_CONCAT`/`STRING_AGG` diverge by dialect, ADR-0003) — a call's groups
//! are loaded separately and assembled in Rust.

use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DbErr, EntityTrait, JoinType, QueryFilter,
    QueryOrder, QuerySelect, RelationTrait, Set,
};

use crate::db::entities::{
    call, call_frequency, call_patch, call_unit, group, system, tag, talkgroup, talkgroup_group,
};

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

/// Find a Talkgroup by (System, Ref), creating it if absent. A Ref is unique
/// only within its System.
pub async fn resolve_or_create_talkgroup<C: ConnectionTrait>(
    db: &C,
    system_id: i64,
    ext_ref: i64,
    label: Option<String>,
    tag_id: Option<i64>,
    now_ms: i64,
) -> Result<talkgroup::Model, DbErr> {
    if let Some(found) = talkgroup::Entity::find()
        .filter(talkgroup::Column::SystemId.eq(system_id))
        .filter(talkgroup::Column::Ref.eq(ext_ref))
        .one(db)
        .await?
    {
        return Ok(found);
    }
    talkgroup::ActiveModel {
        system_id: Set(system_id),
        r#ref: Set(ext_ref),
        label: Set(label),
        name: Set(None),
        tag_id: Set(tag_id),
        led: Set(None),
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

/// Resolve the Call's System/Talkgroup/Tag/Groups by Ref (creating as needed),
/// then insert the Call and its child rows. Returns the stored Call.
///
/// Not internally transactional — the caller (ingest, #5) wraps this in one so
/// the resolve → insert sequence is atomic with the audio write (ADR-0001).
pub async fn insert_call<C: ConnectionTrait>(
    db: &C,
    new: &NewCall,
    now_ms: i64,
) -> Result<call::Model, DbErr> {
    let sys =
        resolve_or_create_system(db, new.system_ref, new.system_label.clone(), now_ms).await?;

    let tag_id = match &new.talkgroup_tag {
        Some(name) => Some(resolve_or_create_tag(db, name, now_ms).await?.id),
        None => None,
    };

    let tg = resolve_or_create_talkgroup(
        db,
        sys.id,
        new.talkgroup_ref,
        new.talkgroup_label.clone(),
        tag_id,
        now_ms,
    )
    .await?;

    for group_name in &new.talkgroup_groups {
        let grp = resolve_or_create_group(db, group_name, now_ms).await?;
        link_talkgroup_group(db, tg.id, grp.id).await?;
    }

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

    Ok(stored)
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
