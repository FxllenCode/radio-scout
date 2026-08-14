//! **Talkgroups** — the channels Calls are addressed to (#49, spec US 45–46).
//!
//! The one curation screen that has to survive a county: hundreds of channels,
//! most of them auto-populated from traffic (#8) and named after a number. So
//! this is the only entity here that **pages and filters server-side**, and the
//! only one with a bulk write — [`assign`], which is spec US 46's whole point.
//!
//! # Bulk assignment, and why it is one request
//!
//! "Categorizing a county doesn't take an afternoon" means selecting eighty rows
//! and giving them a Group. Done a row at a time that is eighty round trips and
//! eighty chances to end up half-applied; done as rdio does it, it is a `PUT` of
//! the entire configuration document with eighty rows changed inside it, where a
//! dropped connection loses whatever else was in the document. [`assign`] is one
//! statement set over one id list, in one transaction: it applies wholly or not
//! at all, and it touches nothing it was not given.
//!
//! Groups and Tags named here are **created if they do not exist**, exactly as
//! the CSV importer creates them (#18) — an Operator typing "Fire" into a bulk
//! action means the Group, whether or not they made it first.
//!
//! # The blacklist toggle
//!
//! `blacklisted` is not a column: it is this Talkgroup's Ref being present in
//! its System's list ([`super::systems`]). Exposing it as a per-row boolean is
//! the improvement over rdio, where the only affordance is a free-text comma
//! field on the parent System — an Operator thinking "stop ingesting this
//! channel" should not have to think about where that fact is stored. Both
//! spellings write the same column through the same parser, so they cannot
//! disagree.

use std::collections::{HashMap, HashSet};

use axum::extract::{Path, Query, State};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, Condition, ConnectionTrait, DbErr, EntityTrait, IntoActiveModel,
    PaginatorTrait, QueryFilter, QueryOrder, QuerySelect, Set,
};
use serde::{Deserialize, Serialize};

use super::systems::{blacklist_of, blacklist_text};
use super::{
    CallScope, Created, Force, Rejected, Removed, What, checked_led, cleared, nullable,
    optional_text,
};
use crate::AppState;
use crate::db::entities::{call, group, system, tag, talkgroup, talkgroup_group, talkgroup_ref};
use crate::db::repo;
use crate::failure::{Failure, Stage};
use crate::query::{Filtered, Page, Params};

/// Talkgroups per page when the client asks for none.
const DEFAULT_LIMIT: u64 = 100;

/// Hard ceiling on a page, so one request cannot ask a Pi to serialize a whole
/// county's catalog with its Groups resolved.
const MAX_LIMIT: u64 = 500;

/// One Talkgroup, as the curation screen lists it.
///
/// Deliberately **without member Refs** (#45): those are #50's screen, and
/// carrying them here would be one more query per page for a column this form
/// cannot edit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TalkgroupRow {
    pub id: i64,
    pub system_id: i64,
    pub system_ref: i64,
    pub system_label: Option<String>,
    pub r#ref: i64,
    pub label: Option<String>,
    pub name: Option<String>,
    /// The single service label a Talkgroup carries (CONTEXT.md).
    pub tag: Option<String>,
    /// Every Group it belongs to, sorted — a Talkgroup may belong to several.
    pub groups: Vec<String>,
    pub led: Option<String>,
    /// `null` inherits the System, which inherits the instance (#20).
    pub enhancement: Option<bool>,
    /// Whether this Ref is on its System's blacklist — a derived fact, not a
    /// column.
    pub blacklisted: bool,
    pub calls: u64,
    pub created_at_ms: i64,
}

crate::answers_json!(TalkgroupRow);

/// What a create carries.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NewTalkgroup {
    /// The **Id** of the System this channel belongs to — the value the systems
    /// listing just handed the form, rather than a Ref it would have to resolve
    /// a second time.
    pub system_id: i64,
    pub r#ref: i64,
    pub label: Option<String>,
    pub name: Option<String>,
    pub tag: Option<String>,
    #[serde(default)]
    pub groups: Vec<String>,
    pub led: Option<String>,
    pub enhancement: Option<bool>,
    #[serde(default)]
    pub blacklisted: bool,
}

/// What an edit carries. Absent leaves alone; `null` clears.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TalkgroupPatch {
    pub r#ref: Option<i64>,
    #[serde(default, deserialize_with = "nullable")]
    pub label: Option<Option<String>>,
    #[serde(default, deserialize_with = "nullable")]
    pub name: Option<Option<String>>,
    #[serde(default, deserialize_with = "nullable")]
    pub tag: Option<Option<String>>,
    /// **Replaces** the set, the CSV importer's rule (#18) — a set-valued field
    /// with no way to spell "none" could add a Group and never remove one.
    pub groups: Option<Vec<String>>,
    #[serde(default, deserialize_with = "nullable")]
    pub led: Option<Option<String>>,
    #[serde(default, deserialize_with = "nullable")]
    pub enhancement: Option<Option<bool>>,
    pub blacklisted: Option<bool>,
}

/// What one bulk action does to every row it names (spec US 46).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Assignment {
    /// The Talkgroup **Ids** the Operator selected.
    pub ids: Vec<i64>,
    /// Groups to add. Created if they do not exist yet.
    #[serde(default)]
    pub add_groups: Vec<String>,
    /// Groups to remove. Absent from a row is not an error — a bulk action over
    /// a mixed selection is exactly where that happens.
    #[serde(default)]
    pub remove_groups: Vec<String>,
    /// The Tag to set on every row; `null` clears it, absent leaves it.
    #[serde(default, deserialize_with = "nullable")]
    pub tag: Option<Option<String>>,
}

/// What a bulk action changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Assigned {
    /// Rows the action reached. Fewer than `ids` means some named no row — which
    /// is worth showing rather than swallowing, since a stale selection is how
    /// an Operator finds out their page is out of date.
    pub changed: u64,
}

crate::answers_json!(Assigned);

/// `GET /api/admin/talkgroups` — one filtered, paged screenful.
pub async fn list(
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Page<TalkgroupRow>, Failure> {
    let search = parse_search(&params)?;

    read_page(&state.db, &search)
        .await
        .map_err(Stage::Curate.failed())
}

/// `POST /api/admin/talkgroups` — one new channel.
pub async fn create(
    State(state): State<AppState>,
    axum::Json(body): axum::Json<NewTalkgroup>,
) -> Result<Created<TalkgroupRow>, Failure> {
    let db = &state.db;
    let led = checked_led(body.led)?;
    let system = system::Entity::find_by_id(body.system_id)
        .one(db)
        .await
        .map_err(Stage::Curate.failed())?
        .ok_or(Rejected::NoSuchSystem {
            system_id: body.system_id,
        })?;
    ensure_ref_free(db, system.id, body.r#ref, None).await?;

    let now_ms = state.clock.now_ms();
    let txn = db.begin().await.map_err(Stage::Curate.failed())?;
    let tag_id = match optional_text(body.tag) {
        Some(name) => Some(
            repo::resolve_or_create_tag(&txn, &name, now_ms)
                .await
                .map_err(Stage::Curate.failed())?
                .id,
        ),
        None => None,
    };
    let row = talkgroup::ActiveModel {
        system_id: Set(system.id),
        r#ref: Set(body.r#ref),
        label: Set(optional_text(body.label)),
        name: Set(optional_text(body.name)),
        tag_id: Set(tag_id),
        led: Set(led),
        enhancement: Set(body.enhancement),
        created_at_ms: Set(now_ms),
        ..Default::default()
    }
    .insert(&txn)
    .await
    .map_err(Stage::Curate.failed())?;
    set_groups(&txn, row.id, &body.groups, now_ms)
        .await
        .map_err(Stage::Curate.failed())?;
    set_blacklisted(&txn, &system, body.r#ref, body.blacklisted)
        .await
        .map_err(Stage::Curate.failed())?;
    txn.commit().await.map_err(Stage::Curate.failed())?;

    Ok(Created(
        one(db, row.id).await.map_err(Stage::Curate.failed())?,
    ))
}

/// `PATCH /api/admin/talkgroups/{id}` — change what was named.
pub async fn update(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    axum::Json(body): axum::Json<TalkgroupPatch>,
) -> Result<TalkgroupRow, Failure> {
    let db = &state.db;
    let existing = talkgroup::Entity::find_by_id(id)
        .one(db)
        .await
        .map_err(Stage::Curate.failed())?
        .ok_or(Rejected::NotFound(What::Talkgroup))?;
    let system = owning_system(db, existing.system_id).await?;
    let led = match body.led {
        Some(led) => Some(checked_led(led)?),
        None => None,
    };
    if let Some(wanted) = body.r#ref
        && wanted != existing.r#ref
    {
        ensure_ref_free(db, existing.system_id, wanted, Some(id)).await?;
    }

    let now_ms = state.clock.now_ms();
    let previous_ref = existing.r#ref;
    let txn = db.begin().await.map_err(Stage::Curate.failed())?;
    let tag_id = match &body.tag {
        Some(Some(name)) => match optional_text(Some(name.clone())) {
            Some(name) => Some(Some(
                repo::resolve_or_create_tag(&txn, &name, now_ms)
                    .await
                    .map_err(Stage::Curate.failed())?
                    .id,
            )),
            None => Some(None),
        },
        Some(None) => Some(None),
        None => None,
    };

    let mut row = existing.into_active_model();
    if let Some(wanted) = body.r#ref {
        row.r#ref = Set(wanted);
    }
    if let Some(label) = body.label {
        row.label = Set(cleared(label));
    }
    if let Some(name) = body.name {
        row.name = Set(cleared(name));
    }
    if let Some(tag_id) = tag_id {
        row.tag_id = Set(tag_id);
    }
    if let Some(led) = led {
        row.led = Set(led);
    }
    if let Some(enhancement) = body.enhancement {
        row.enhancement = Set(enhancement);
    }
    let stored = row.update(&txn).await.map_err(Stage::Curate.failed())?;
    if let Some(groups) = &body.groups {
        set_groups(&txn, id, groups, now_ms)
            .await
            .map_err(Stage::Curate.failed())?;
    }
    // A renumbered channel takes its blacklisting with it: the list holds Refs,
    // so leaving the old one behind would refuse a Ref nobody owns and let the
    // new one through — the policy silently inverted by an edit that never
    // mentioned it.
    if previous_ref != stored.r#ref {
        let was = blacklist_of(system.blacklist.as_deref()).contains(&previous_ref);
        set_blacklisted(&txn, &system, previous_ref, false)
            .await
            .map_err(Stage::Curate.failed())?;
        let system = owning_system(&txn, system.id).await?;
        set_blacklisted(&txn, &system, stored.r#ref, was)
            .await
            .map_err(Stage::Curate.failed())?;
    }
    if let Some(blacklisted) = body.blacklisted {
        let system = owning_system(&txn, system.id).await?;
        set_blacklisted(&txn, &system, stored.r#ref, blacklisted)
            .await
            .map_err(Stage::Curate.failed())?;
    }
    txn.commit().await.map_err(Stage::Curate.failed())?;

    one(db, id).await.map_err(Stage::Curate.failed())
}

/// `POST /api/admin/talkgroups/assign` — one action over many rows.
pub async fn assign(
    State(state): State<AppState>,
    axum::Json(body): axum::Json<Assignment>,
) -> Result<Assigned, Failure> {
    let db = &state.db;
    let now_ms = state.clock.now_ms();

    let txn = db.begin().await.map_err(Stage::Curate.failed())?;
    // Only rows that exist, so a stale selection reports what it really reached
    // rather than claiming to have changed a Talkgroup somebody else deleted.
    let ids: Vec<i64> = talkgroup::Entity::find()
        .filter(talkgroup::Column::Id.is_in(body.ids.clone()))
        .select_only()
        .column(talkgroup::Column::Id)
        .into_tuple()
        .all(&txn)
        .await
        .map_err(Stage::Curate.failed())?;

    if !ids.is_empty() {
        for name in body.add_groups.iter().filter_map(|name| trimmed(name)) {
            let group_id = repo::resolve_or_create_group(&txn, &name, now_ms)
                .await
                .map_err(Stage::Curate.failed())?
                .id;
            link_all(&txn, &ids, group_id)
                .await
                .map_err(Stage::Curate.failed())?;
        }
        for name in body.remove_groups.iter().filter_map(|name| trimmed(name)) {
            // A Group nobody has is nothing to remove — not an error, because a
            // bulk action over a mixed selection is where that happens.
            if let Some(group) = group::Entity::find()
                .filter(group::Column::Name.eq(name))
                .one(&txn)
                .await
                .map_err(Stage::Curate.failed())?
            {
                talkgroup_group::Entity::delete_many()
                    .filter(talkgroup_group::Column::GroupId.eq(group.id))
                    .filter(talkgroup_group::Column::TalkgroupId.is_in(ids.clone()))
                    .exec(&txn)
                    .await
                    .map_err(Stage::Curate.failed())?;
            }
        }
        if let Some(tag) = &body.tag {
            let tag_id = match tag.as_deref().and_then(trimmed) {
                Some(name) => Some(
                    repo::resolve_or_create_tag(&txn, &name, now_ms)
                        .await
                        .map_err(Stage::Curate.failed())?
                        .id,
                ),
                None => None,
            };
            talkgroup::Entity::update_many()
                .col_expr(
                    talkgroup::Column::TagId,
                    sea_orm::sea_query::Expr::value(tag_id),
                )
                .filter(talkgroup::Column::Id.is_in(ids.clone()))
                .exec(&txn)
                .await
                .map_err(Stage::Curate.failed())?;
        }
    }
    txn.commit().await.map_err(Stage::Curate.failed())?;

    Ok(Assigned {
        changed: ids.len() as u64,
    })
}

/// `DELETE /api/admin/talkgroups/{id}` — refused while it still has Calls,
/// unless `?force=true`.
pub async fn remove(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Query(force): Query<Force>,
) -> Result<Removed, Failure> {
    let db = &state.db;
    let existing = talkgroup::Entity::find_by_id(id)
        .one(db)
        .await
        .map_err(Stage::Curate.failed())?
        .ok_or(Rejected::NotFound(What::Talkgroup))?;

    let calls = call::Entity::find()
        .filter(call::Column::TalkgroupId.eq(id))
        .count(db)
        .await
        .map_err(Stage::Curate.failed())?;
    if calls > 0 && !force.force {
        return Err(Rejected::HasCalls {
            what: What::Talkgroup,
            calls,
        }
        .into());
    }
    if calls > 0 {
        super::purge_calls(&state, CallScope::Talkgroup(id)).await?;
    }

    let txn = db.begin().await.map_err(Stage::Curate.failed())?;
    for result in [
        talkgroup_group::Entity::delete_many()
            .filter(talkgroup_group::Column::TalkgroupId.eq(id))
            .exec(&txn)
            .await,
        talkgroup_ref::Entity::delete_many()
            .filter(talkgroup_ref::Column::TalkgroupId.eq(id))
            .exec(&txn)
            .await,
        talkgroup::Entity::delete_by_id(id).exec(&txn).await,
    ] {
        result.map_err(Stage::Curate.failed())?;
    }
    // The blacklist holds Refs, so a deleted channel's entry would go on
    // refusing a Ref that no longer names anything — and would silently reappear
    // as a blacklisted channel the moment auto-populate recreated it.
    //
    // The System is *required* rather than skipped-if-absent, the way `update`
    // requires it: a Talkgroup whose System has gone is a row nothing can
    // resolve, and quietly leaving its blacklisting behind would be the
    // silent half of an already-broken state.
    let system = owning_system(&txn, existing.system_id).await?;
    set_blacklisted(&txn, &system, existing.r#ref, false)
        .await
        .map_err(Stage::Curate.failed())?;
    txn.commit().await.map_err(Stage::Curate.failed())?;

    Ok(Removed)
}

// -- Reading ----------------------------------------------------------------

/// The filters this listing understands.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TalkgroupSearch {
    /// A System **Ref** — what an Operator reads on the screen, and what a
    /// hand-written URL would carry.
    pub system_ref: Option<i64>,
    /// Free text over the label, the name, and the Ref as written.
    pub text: Option<String>,
    pub group: Option<String>,
    pub tag: Option<String>,
    pub blacklisted: Option<bool>,
    pub limit: u64,
    pub offset: u64,
}

fn parse_search(params: &HashMap<String, String>) -> Filtered<TalkgroupSearch> {
    let params = Params::new(params);
    Ok(TalkgroupSearch {
        system_ref: params.number("system")?,
        text: params.raw("q").map(str::to_owned),
        group: params.raw("group").map(str::to_owned),
        tag: params.raw("tag").map(str::to_owned),
        blacklisted: params.flag("blacklisted")?,
        limit: params.limit(DEFAULT_LIMIT, MAX_LIMIT)?,
        offset: params.offset()?,
    })
}

/// One page of Talkgroups and the total behind it.
///
/// **Ordered by System Id then Ref**, in SQL, and deliberately not by label:
/// the window has to be applied by the database, and text collation is exactly
/// where SQLite and Postgres disagree (ADR-0003) — a page boundary that moves
/// when an Operator changes database would drop rows out of a listing silently.
/// Numeric order is also what every RadioReference export reads in.
///
/// **Every filter reaches SQL, the blacklist included.** It is not a column —
/// it is a Ref's membership of a comma-separated list on the parent — but the
/// Systems are already in hand, so the list becomes an `OR` of
/// `(system_id = …, ref IN …)` rather than a reason to read the whole table and
/// filter in Rust. That distinction is this screen's whole point: a county is
/// hundreds of channels and this runs on a Pi, so `MAX_LIMIT` has to bound what
/// the *database* returns and not merely what is serialized afterwards.
async fn read_page<C: ConnectionTrait>(
    db: &C,
    search: &TalkgroupSearch,
) -> Result<Page<TalkgroupRow>, DbErr> {
    let systems: HashMap<i64, system::Model> = system::Entity::find()
        .all(db)
        .await?
        .into_iter()
        .map(|row| (row.id, row))
        .collect();

    let query = base_query(search, &systems);
    // The window is applied to the rows and nowhere else, so the total keeps
    // describing the whole match while the page walks it (#98's rule).
    let count = PaginatorTrait::count(query.clone(), db).await?;
    let page = query
        .offset(search.offset)
        .limit(search.limit)
        .all(db)
        .await?;

    Ok(Page::new(
        denormalize(db, page, &systems).await?,
        count,
        search.limit,
        search.offset,
    ))
}

/// Everything the filters can express as SQL, ordered.
fn base_query(
    search: &TalkgroupSearch,
    systems: &HashMap<i64, system::Model>,
) -> sea_orm::Select<talkgroup::Entity> {
    let mut query = talkgroup::Entity::find();

    if let Some(system_ref) = search.system_ref {
        // Resolved from the Systems already in hand rather than by a join: a Ref
        // nobody has matches no System, and `is_in([])` is the honest empty page.
        let ids: Vec<i64> = systems
            .values()
            .filter(|system| system.r#ref == system_ref)
            .map(|system| system.id)
            .collect();
        query = query.filter(talkgroup::Column::SystemId.is_in(ids));
    }
    if let Some(text) = &search.text {
        let pattern = format!("%{}%", text.to_lowercase());
        let mut any = Condition::any()
            .add(lowered(talkgroup::Column::Label).like(pattern.clone()))
            .add(lowered(talkgroup::Column::Name).like(pattern));
        // A number an Operator typed is very often the Ref itself.
        if let Ok(as_ref) = text.trim().parse::<i64>() {
            any = any.add(talkgroup::Column::Ref.eq(as_ref));
        }
        query = query.filter(any);
    }
    if let Some(group) = &search.group {
        query = query.filter(
            talkgroup::Column::Id.in_subquery(
                sea_orm::QuerySelect::query(
                    &mut talkgroup_group::Entity::find()
                        .select_only()
                        .column(talkgroup_group::Column::TalkgroupId)
                        .inner_join(group::Entity)
                        .filter(group::Column::Name.eq(group.clone())),
                )
                .take(),
            ),
        );
    }
    if let Some(tag) = &search.tag {
        query = query.filter(
            talkgroup::Column::TagId.in_subquery(
                sea_orm::QuerySelect::query(
                    &mut tag::Entity::find()
                        .select_only()
                        .column(tag::Column::Id)
                        .filter(tag::Column::Name.eq(tag.clone())),
                )
                .take(),
            ),
        );
    }
    if let Some(wanted) = search.blacklisted {
        query = query.filter(blacklisted_where(systems, wanted));
    }

    query
        .order_by_asc(talkgroup::Column::SystemId)
        .order_by_asc(talkgroup::Column::Ref)
}

/// "This Ref is (or is not) on its System's blacklist", as a `WHERE`.
///
/// The lists are already read, so this needs no join and no second statement:
/// one `OR` per System that blacklists anything, negated for the other half.
/// Both columns are `NOT NULL`, so the negation is a plain complement rather
/// than three-valued logic quietly dropping rows.
///
/// With nothing blacklisted anywhere the `OR` would be empty, which renders as
/// *no filter at all* — the exact opposite of what `blacklisted=true` asked
/// for. So that case is spelled out: an impossible id for "show me the
/// blacklisted ones" (`id` is a positive autoincrement, so `0` is a comparison
/// no dialect can disagree about), and no filter at all for its complement.
fn blacklisted_where(systems: &HashMap<i64, system::Model>, wanted: bool) -> Condition {
    let listed: Vec<(i64, Vec<i64>)> = systems
        .values()
        .map(|system| (system.id, blacklist_of(system.blacklist.as_deref())))
        .filter(|(_, refs)| !refs.is_empty())
        .collect();

    if listed.is_empty() {
        return match wanted {
            true => Condition::all().add(talkgroup::Column::Id.eq(0)),
            false => Condition::all(),
        };
    }

    let mut any = Condition::any();
    for (system_id, refs) in listed {
        any = any.add(
            Condition::all()
                .add(talkgroup::Column::SystemId.eq(system_id))
                .add(talkgroup::Column::Ref.is_in(refs)),
        );
    }
    match wanted {
        true => any,
        false => any.not(),
    }
}

/// `LOWER(column)`, so free-text matching is case-insensitive on **both**
/// dialects: SQLite's `LIKE` folds ASCII case and Postgres' does not, which
/// would otherwise make the same search box behave differently per install.
fn lowered(column: talkgroup::Column) -> sea_orm::sea_query::Expr {
    use sea_orm::sea_query::{Expr, Func};
    Expr::expr(Func::lower(Expr::col((talkgroup::Entity, column))))
}

/// Fill one page's rows out with their Tag, Groups, System and Call count — four
/// statements for the page, never four per row (#86).
async fn denormalize<C: ConnectionTrait>(
    db: &C,
    page: Vec<talkgroup::Model>,
    systems: &HashMap<i64, system::Model>,
) -> Result<Vec<TalkgroupRow>, DbErr> {
    if page.is_empty() {
        return Ok(Vec::new());
    }
    let ids: Vec<i64> = page.iter().map(|row| row.id).collect();

    let mut groups: HashMap<i64, Vec<String>> = HashMap::new();
    for (link, group) in talkgroup_group::Entity::find()
        .filter(talkgroup_group::Column::TalkgroupId.is_in(ids.clone()))
        .find_also_related(group::Entity)
        .all(db)
        .await?
    {
        if let Some(group) = group {
            groups
                .entry(link.talkgroup_id)
                .or_default()
                .push(group.name);
        }
    }
    let tags: HashMap<i64, String> = tag::Entity::find()
        // Bounded by the *page*, never by how many Tags an Operator has
        // written down — the rule the denormalizer behind every search page
        // already follows (#86).
        .filter(tag::Column::Id.is_in(distinct(page.iter().filter_map(|row| row.tag_id))))
        .all(db)
        .await?
        .into_iter()
        .map(|row| (row.id, row.name))
        .collect();
    let calls: HashMap<i64, u64> = call::Entity::find()
        .filter(call::Column::TalkgroupId.is_in(ids))
        .select_only()
        .column(call::Column::TalkgroupId)
        .column_as(call::Column::Id.count(), "held")
        .group_by(call::Column::TalkgroupId)
        .into_tuple::<(i64, i64)>()
        .all(db)
        .await?
        .into_iter()
        .map(|(id, held)| (id, held.max(0) as u64))
        .collect();

    Ok(page
        .into_iter()
        .map(|row| {
            let system = systems.get(&row.system_id);
            let mut belongs = groups.remove(&row.id).unwrap_or_default();
            belongs.sort();
            TalkgroupRow {
                system_ref: system.map(|s| s.r#ref).unwrap_or_default(),
                system_label: system.and_then(|s| s.label.clone()),
                blacklisted: system
                    .map(|s| blacklist_of(s.blacklist.as_deref()).contains(&row.r#ref))
                    .unwrap_or(false),
                calls: calls.get(&row.id).copied().unwrap_or(0),
                tag: row.tag_id.and_then(|id| tags.get(&id).cloned()),
                groups: belongs,
                id: row.id,
                system_id: row.system_id,
                r#ref: row.r#ref,
                label: row.label,
                name: row.name,
                led: row.led,
                enhancement: row.enhancement,
                created_at_ms: row.created_at_ms,
            }
        })
        .collect())
}

/// One Talkgroup's row, re-read after a write.
async fn one<C: ConnectionTrait>(db: &C, id: i64) -> Result<TalkgroupRow, DbErr> {
    let systems: HashMap<i64, system::Model> = system::Entity::find()
        .all(db)
        .await?
        .into_iter()
        .map(|row| (row.id, row))
        .collect();
    let row = talkgroup::Entity::find_by_id(id)
        .one(db)
        .await?
        .ok_or_else(|| DbErr::RecordNotFound(format!("talkgroup {id}")))?;

    denormalize(db, vec![row], &systems)
        .await?
        .pop()
        .ok_or_else(|| DbErr::RecordNotFound(format!("talkgroup {id}")))
}

// -- Writing ----------------------------------------------------------------

/// The System a stored Talkgroup belongs to.
///
/// Its absence is a **500, not a 400**: the caller named a Talkgroup that
/// exists, and a Talkgroup whose System has gone is our own broken state rather
/// than anything they did wrong (#92 — a `Reason` is what the *caller* got
/// wrong). `NoSuchSystem` stays for the one case that really is the caller's:
/// a create whose body names a System that is not there.
async fn owning_system<C: ConnectionTrait>(
    db: &C,
    system_id: i64,
) -> Result<system::Model, Failure> {
    system::Entity::find_by_id(system_id)
        .one(db)
        .await
        .map_err(Stage::Curate.failed())?
        .ok_or_else(|| {
            Failure::broke(
                Stage::Curate,
                format!("talkgroup belongs to system {system_id}, which is not there"),
            )
        })
}

/// Refuse a Ref another channel in this System already answers to — its own, or
/// one it holds as a **member Ref** (#45), because both are resolved to a
/// channel at ingest and two owners is a Call that could land on either.
async fn ensure_ref_free<C: ConnectionTrait>(
    db: &C,
    system_id: i64,
    wanted: i64,
    except: Option<i64>,
) -> Result<(), Failure> {
    let owner = talkgroup::Entity::find()
        .filter(talkgroup::Column::SystemId.eq(system_id))
        .filter(talkgroup::Column::Ref.eq(wanted))
        .one(db)
        .await
        .map_err(Stage::Curate.failed())?
        .map(|row| row.id);
    let member = talkgroup_ref::Entity::find()
        .filter(talkgroup_ref::Column::SystemId.eq(system_id))
        .filter(talkgroup_ref::Column::Ref.eq(wanted))
        .one(db)
        .await
        .map_err(Stage::Curate.failed())?
        .is_some();

    match owner.filter(|id| Some(*id) != except).is_some() || member {
        true => Err(Rejected::RefTaken {
            what: What::Talkgroup,
            taken: wanted,
        }
        .into()),
        false => Ok(()),
    }
}

/// Put every Talkgroup in `ids` into one Group — **two statements, whatever the
/// selection**, where a link-at-a-time loop would be one per row.
///
/// Eighty rows is what US 46 is about, and eighty round trips is the afternoon
/// this feature exists to save. The read is what keeps it idempotent: a row
/// already in the Group is left alone rather than colliding with the join
/// table's own primary key.
async fn link_all<C: ConnectionTrait>(db: &C, ids: &[i64], group_id: i64) -> Result<(), DbErr> {
    let linked: HashSet<i64> = talkgroup_group::Entity::find()
        .filter(talkgroup_group::Column::GroupId.eq(group_id))
        .filter(talkgroup_group::Column::TalkgroupId.is_in(ids.to_vec()))
        .all(db)
        .await?
        .into_iter()
        .map(|link| link.talkgroup_id)
        .collect();
    let missing: Vec<talkgroup_group::ActiveModel> = ids
        .iter()
        .filter(|id| !linked.contains(id))
        .map(|id| talkgroup_group::ActiveModel {
            talkgroup_id: Set(*id),
            group_id: Set(group_id),
        })
        .collect();
    if !missing.is_empty() {
        talkgroup_group::Entity::insert_many(missing)
            .exec(db)
            .await?;
    }
    Ok(())
}

/// The distinct values of an iterator — the `IN (…)` list for a batched lookup.
fn distinct(values: impl IntoIterator<Item = i64>) -> Vec<i64> {
    values
        .into_iter()
        .collect::<HashSet<i64>>()
        .into_iter()
        .collect()
}

/// Make this Talkgroup's Groups exactly `names`, creating any that are new.
///
/// The write is [`repo::set_talkgroup_groups`], shared with the configuration
/// document (#51) so the two surfaces cannot come to disagree about what
/// replacing a set means — in particular about the stale-link delete, whose
/// absence in one copy would leave a channel in a Group its own row says it
/// left. What is this module's is the *trimming*: a stray comma in a form is
/// not a Group called "".
async fn set_groups<C: ConnectionTrait>(
    db: &C,
    talkgroup_id: i64,
    names: &[String],
    now_ms: i64,
) -> Result<(), DbErr> {
    let wanted: Vec<String> = names.iter().filter_map(|name| trimmed(name)).collect();
    repo::set_talkgroup_groups(db, talkgroup_id, &wanted, now_ms).await?;
    Ok(())
}

/// Put this Ref on its System's blacklist, or take it off — the toggle behind
/// [`TalkgroupRow::blacklisted`], writing the one column
/// [`super::systems`]'s list editor writes.
async fn set_blacklisted<C: ConnectionTrait>(
    db: &C,
    system: &system::Model,
    talkgroup_ref: i64,
    blacklisted: bool,
) -> Result<(), DbErr> {
    let mut refs = blacklist_of(system.blacklist.as_deref());
    let listed = refs.contains(&talkgroup_ref);
    if listed == blacklisted {
        return Ok(());
    }
    match blacklisted {
        true => refs.push(talkgroup_ref),
        false => refs.retain(|entry| *entry != talkgroup_ref),
    }

    let mut row = system.clone().into_active_model();
    row.blacklist = Set(blacklist_text(&refs));
    row.update(db).await?;
    Ok(())
}

/// A name with its whitespace gone, or nothing at all — a bulk action's list is
/// typed by hand and an empty entry is a stray comma, never a Group called "".
fn trimmed(name: &str) -> Option<String> {
    let name = name.trim();
    match name.is_empty() {
        true => None,
        false => Some(name.to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    fn params(query: &str) -> HashMap<String, String> {
        query
            .split('&')
            .filter(|pair| !pair.is_empty())
            .map(|pair| {
                let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
                (key.to_owned(), value.to_owned())
            })
            .collect()
    }

    /// Blank is absent — the client builds this query from form state, where "no
    /// filter" is the empty string.
    #[test]
    fn blank_filters_are_no_filter_at_all() {
        let search = parse_search(&params("system=&q=&group=&tag=&blacklisted=")).expect("parse");

        assert_eq!(
            search,
            TalkgroupSearch {
                limit: DEFAULT_LIMIT,
                ..TalkgroupSearch::default()
            }
        );
    }

    /// A filter that cannot be read names itself, rather than being coerced into
    /// a plausible wrong answer the way rdio's does.
    #[rstest]
    #[case("system=eleven", "system")]
    #[case("blacklisted=maybe", "blacklisted")]
    #[case("limit=lots", "limit")]
    #[case("offset=-1", "offset")]
    fn a_bad_filter_names_itself(#[case] query: &str, #[case] parameter: &str) {
        let told = parse_search(&params(query)).expect_err("refused").told();

        assert!(told.contains(parameter), "{told}");
    }

    /// A page is bounded whatever was asked for.
    #[rstest]
    #[case("", DEFAULT_LIMIT)]
    #[case("limit=10", 10)]
    #[case("limit=0", DEFAULT_LIMIT)]
    #[case("limit=99999", MAX_LIMIT)]
    fn a_page_is_bounded(#[case] query: &str, #[case] expected: u64) {
        assert_eq!(parse_search(&params(query)).expect("parse").limit, expected);
    }

    /// **An instance where nothing is blacklisted is where this gets it wrong.**
    ///
    /// The filter is an `OR` per System that blacklists something, and with none
    /// the `OR` is empty — which sea-query renders as *no condition at all*, so
    /// `blacklisted=true` would answer with every channel there is. Spelled out
    /// rather than left to the query builder, and pinned here.
    #[test]
    fn a_blacklist_filter_over_nothing_asks_for_nothing() {
        let empty = HashMap::new();

        // "Show me the blacklisted ones" has to match no row...
        let wanted = blacklisted_where(&empty, true);
        assert!(
            !wanted.is_empty(),
            "an empty condition would match everything"
        );

        // ...and its complement has nothing to exclude, so it filters nothing.
        assert!(blacklisted_where(&empty, false).is_empty());
    }

    /// With a System that blacklists something, both halves are real conditions.
    #[test]
    fn a_blacklist_filter_names_the_systems_that_have_one() {
        let mut systems = HashMap::new();
        systems.insert(
            1,
            system::Model {
                id: 1,
                r#ref: 11,
                label: None,
                auto_populate: false,
                blacklist: Some(String::from("100,200")),
                enhancement: None,
                created_at_ms: 0,
            },
        );
        // A second System with nothing listed contributes no term, rather than
        // an always-false one that would exclude its channels from the
        // complement.
        systems.insert(
            2,
            system::Model {
                id: 2,
                r#ref: 12,
                label: None,
                auto_populate: false,
                blacklist: None,
                enhancement: None,
                created_at_ms: 0,
            },
        );

        assert!(!blacklisted_where(&systems, true).is_empty());
        assert!(!blacklisted_where(&systems, false).is_empty());
    }

    /// A Talkgroup whose System has gone is **our** broken state, so it is a
    /// 500 with a stage rather than a 400 telling the caller they got something
    /// wrong (#92). Unreachable through a handler — the System is deleted with
    /// its channels — so it is driven here, over a real database, the way
    /// `src/ingest.rs` drives its own unreachable arms.
    #[tokio::test]
    async fn a_talkgroup_whose_system_has_gone_is_a_failure_not_a_refusal() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let db = crate::db::connect(&crate::testing::sqlite_url(&tmp))
            .await
            .expect("a database");

        let broke = owning_system(&db, 404).await.expect_err("no such system");

        // **500, not 404.** A `Reason` would render its own status and body
        // here; a break renders an empty 500 and hands its cause to the request
        // middleware, which is the only thing that knows the request id (#29).
        // That the line then names `stage=curate` is `tests/curate.rs`'s to
        // prove, over a real request.
        let response = axum::response::IntoResponse::into_response(broke);
        assert_eq!(response.status(), 500);
    }

    /// A stray comma in a bulk action's list is not a Group called "".
    #[rstest]
    #[case("Fire", Some("Fire"))]
    #[case("  Fire  ", Some("Fire"))]
    #[case("", None)]
    #[case("   ", None)]
    fn a_blank_group_name_is_dropped(#[case] raw: &str, #[case] expected: Option<&str>) {
        assert_eq!(trimmed(raw).as_deref(), expected);
    }
}
