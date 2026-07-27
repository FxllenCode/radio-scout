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
    api_key, call, call_frequency, call_patch, call_unit, group, push_subscription, system, tag,
    talkgroup, talkgroup_group, unit,
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
    /// Byte length of the audio object, filled in by ingest when it writes the
    /// object (#10 — retention's size cap sums it).
    pub audio_size: Option<i64>,
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
        audio_size: Set(new.audio_size),
        duration_ms: Set(new.duration_ms),
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
                object_key: call.object_key.clone(),
                audio_url: format!("/api/call/{}/audio", call.id),
            }
        })
        .collect())
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
