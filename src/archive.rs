//! The **Archive**, read: one module for every filtered window of it (#13, #98,
//! spec US 24–27).
//!
//! CONTEXT.md: the Archive is every Call an Instance currently holds. Four
//! surfaces read it — archive search, the cascading filter options, one Call by
//! id (rendered, or downloaded as a file), and the live feed's **Backfill** — and
//! before #98 each composed its own answer out of [`crate::db::repo`]. Three of
//! them therefore had to independently know that a page and its total must come
//! from the same filter, that a page of views comes back in the order its rows
//! went in, and that denormalizing is batched rather than per Call. #63's DVR
//! would have been the fifth to know it.
//!
//! So the composition lives here instead, and a caller asks for a *window*:
//!
//! - [`page`] — filtered, ordered, windowed, with the total behind it
//! - [`options`] — what each filter can usefully take, given the others
//! - [`emitted_since`] — the emission window a Backfill is read from
//! - [`call_detail`] / [`call_download`] — one Call, rendered or saved
//! - [`stored_calls`] — the denormalizer all of them share, and the only one
//!
//! # Improving on rdio-scanner
//!
//! rdio searches over its proprietary WebSocket and answers with bare
//! `{id, system, talkgroup, dateTime}` rows, so the client fetches every Call it
//! wants to show *again*, one at a time. Radio-Scout searches over plain HTTP
//! (`GET /api/calls`) and answers with the same denormalized [`StoredCall`] the
//! live feed delivers — a page renders and plays with no follow-up round trip,
//! and the query is cacheable, linkable, and reachable by anything that speaks
//! HTTP.
//!
//! The wire contract is [`SearchPage`] plus [`FilterOptions`]; the entities
//! behind them are [`crate::db::entities`], and everything that *writes* the
//! Archive stays in [`crate::db::repo`].

use std::collections::HashMap;

use axum::extract::{Path, Query, State};
use axum::http::{HeaderValue, header};
use axum::response::{IntoResponse, Response};
use sea_orm::{
    ColumnTrait, ConnectionTrait, DbErr, EntityTrait, JoinType, PaginatorTrait, QueryFilter,
    QueryOrder, QuerySelect, RelationTrait, Select,
};

use crate::AppState;
use crate::call::{CallDetail, CallId, Emission, StoredCall};
use crate::db::entities::{
    call, call_frequency, call_patch, call_unit, group, site, system, tag, talkgroup,
    talkgroup_group,
};
use crate::failure::{Failure, Reason, Stage};
use crate::query::{Filtered, Page, Params, bad};

// The cascading filter-option view types live in `crate::call` beside
// `StoredCall`, so anything may build them without depending on this module.
pub use crate::call::{FilterOptions, SystemOption, TalkgroupOption};

/// Page size when the client asks for none. Big enough that a phone screen
/// scrolls for a while, small enough to stay snappy on a Pi.
const DEFAULT_LIMIT: u64 = 100;
/// Hard ceiling on a page, so one request can't ask the Pi to denormalize the
/// whole archive. Requests above it are clamped, not refused, and the response
/// reports the limit actually applied.
const MAX_LIMIT: u64 = 500;

/// One page of archive-search results: the Calls, fully denormalized (no N+1
/// follow-up fetches), and the window they came from.
pub type SearchPage = Page<StoredCall>;

// ---------------------------------------------------------------------------
// What a read asks for
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// Building the query
// ---------------------------------------------------------------------------

/// A Call query under construction, which remembers the relations it has
/// already joined.
///
/// **What this replaces is a hand-maintained protocol** (#98). The filters used
/// to be applied by a function taking a `Joined { system, talkgroup, tag }` flag
/// set that every caller filled in by hand to describe the joins it had made
/// itself — plus one rule that was never a flag at all: the Group facet query
/// joins Talkgroup→Group in order to *select* from it, so its caller had to
/// clear the Group filter first or the filter would join the same tables again.
/// Getting any of that wrong is not a wrong answer that a test might notice; it
/// is `ambiguous column name: groups.name` at runtime, on whichever combination
/// of filters nobody happened to try. Nobody had tried a Group filter.
///
/// Here a join is **asked for, and asking twice is asking once**. Two things
/// stop being anybody's to remember: which joins a filter needs, and that Tag
/// and Group only reach a Call *through* its Talkgroup, so asking for either
/// asks for that too.
struct CallQuery {
    select: Select<call::Entity>,
    system: bool,
    talkgroup: bool,
    tag: bool,
    group: bool,
}

impl CallQuery {
    fn new() -> Self {
        CallQuery {
            select: call::Entity::find(),
            system: false,
            talkgroup: false,
            tag: false,
            group: false,
        }
    }

    fn join_system(mut self) -> Self {
        if !self.system {
            self.system = true;
            self.select = self
                .select
                .join(JoinType::InnerJoin, call::Relation::System.def());
        }
        self
    }

    fn join_talkgroup(mut self) -> Self {
        if !self.talkgroup {
            self.talkgroup = true;
            self.select = self
                .select
                .join(JoinType::InnerJoin, call::Relation::Talkgroup.def());
        }
        self
    }

    /// **Left**, not inner: the facet query selects the Tag of every reachable
    /// Talkgroup and a Talkgroup may carry none. A filter on `tag.name` narrows
    /// a left join to an inner one anyway, so one kind serves both readings and
    /// there is no "which join did the other caller make" left to get wrong.
    fn join_tag(mut self) -> Self {
        self = self.join_talkgroup();
        if !self.tag {
            self.tag = true;
            self.select = self
                .select
                .join(JoinType::LeftJoin, talkgroup::Relation::Tag.def());
        }
        self
    }

    fn join_group(mut self) -> Self {
        self = self.join_talkgroup();
        if !self.group {
            self.group = true;
            self.select = self
                .select
                .join(
                    JoinType::InnerJoin,
                    talkgroup::Relation::TalkgroupGroup.def(),
                )
                .join(JoinType::InnerJoin, talkgroup_group::Relation::Group.def());
        }
        self
    }

    fn and_where(mut self, condition: impl sea_orm::sea_query::IntoCondition) -> Self {
        self.select = self.select.filter(condition);
        self
    }

    /// Narrow by everything `search` asks for, joining whatever that needs —
    /// the single definition of what each filter *means*, shared by the result
    /// page, the total behind it, and every cascading filter option.
    fn filtered_by(mut self, search: &CallSearch) -> Self {
        if let Some(after) = search.after_ms {
            self = self.and_where(call::Column::CallAtMs.gte(after));
        }
        if let Some(before) = search.before_ms {
            self = self.and_where(call::Column::CallAtMs.lte(before));
        }
        if let Some(min_duration_ms) = search.min_duration_ms {
            // `>=` on a nullable column is already false for `NULL` in both
            // dialects, so the unmeasured Calls fall out here without a second
            // clause — but the behaviour is load-bearing enough to be tested
            // (`a_call_with_no_known_duration_does_not_match_a_duration_filter`)
            // rather than left to SQL's three-valued logic being remembered.
            self = self.and_where(call::Column::DurationMs.gte(min_duration_ms));
        }
        if let Some(system_ref) = search.system_ref {
            self = self
                .join_system()
                .and_where(system::Column::Ref.eq(system_ref));
        }
        if let Some(talkgroup_ref) = search.talkgroup_ref {
            self = self
                .join_talkgroup()
                .and_where(talkgroup::Column::Ref.eq(talkgroup_ref));
        }
        if let Some(tag_name) = &search.tag_name {
            self = self
                .join_tag()
                .and_where(tag::Column::Name.eq(tag_name.clone()));
        }
        if let Some(group_name) = &search.group_name {
            self = self
                .join_group()
                .and_where(group::Column::Name.eq(group_name.clone()));
        }
        self
    }

    /// The query, as Calls — and **not deduplicated**, because none of these
    /// joins can multiply one.
    ///
    /// This carried a `DISTINCT` under the Group filter until #98, on the
    /// reasoning that Talkgroup→Group is many-to-many and a Talkgroup in several
    /// Groups would come back once per Group. That is true of the *relation* and
    /// false of the *query*, and the schema is what settles it: `groups.name` is
    /// `UNIQUE`, so a name matches at most one Group, and `talkgroup_groups` is
    /// keyed `(talkgroup_id, group_id)`, so at most one link joins that Group to
    /// that Talkgroup. One row per Call, always. Every other join here is
    /// many-to-one.
    ///
    /// Removing it failed no test, which is the point: it was an *unkillable*
    /// mutation sitting on the search path, buying a sort buffer per group
    /// filter — real money on a Pi — for a case SQL could not produce. The two
    /// constraints that make it so are asserted in
    /// `tests/db.rs::a_group_filter_cannot_multiply_a_call`, so the day either
    /// is relaxed, something fails here rather than an operator counting their
    /// results twice. **A filter joining a genuinely to-many relation would owe
    /// a `DISTINCT` again** — that is what the join flags are here to make
    /// answerable.
    fn rows(self) -> Select<call::Entity> {
        self.select
    }

    /// The query as a **projection** — a facet, which selects columns off the
    /// joined tables rather than Calls, and is therefore always deduplicated:
    /// its whole purpose is the distinct values reachable under a filter.
    fn facets(self) -> Select<call::Entity> {
        self.select.select_only().distinct()
    }
}

/// The filtered Call query, without ordering or paging.
fn filtered(search: &CallSearch) -> Select<call::Entity> {
    CallQuery::new().filtered_by(search).rows()
}

// ---------------------------------------------------------------------------
// Reading a window of the Archive
// ---------------------------------------------------------------------------

/// **One filtered window of the Archive**: the Calls it holds, denormalized,
/// and the total behind them (#98).
///
/// Three things are this function's rather than a caller's, because three
/// callers each used to have to know them:
///
/// 1. **The page and its total come from the same filter.** [`filtered`] is
///    applied to both, and the window — limit and offset — is applied to the
///    page *only*, so a total cannot silently describe a different search than
///    the rows above it.
/// 2. **Ordering is total.** `call_at_ms` with `id` breaking ties in the same
///    direction, so paging is stable even when a recorder stamps two Calls at
///    the same millisecond.
/// 3. **Denormalizing is batched.** [`stored_calls`] costs the same handful of
///    lookups for five hundred Calls as for one, and returns them in the order
///    it was given.
pub async fn page<C: ConnectionTrait>(db: &C, search: &CallSearch) -> Result<SearchPage, DbErr> {
    let rows = search_rows(db, search).await?;
    let results = stored_calls(db, &rows).await?;
    let count = count(db, search).await?;

    Ok(Page::new(results, count, search.limit, search.offset))
}

/// The rows one window holds: filtered, ordered, and paged.
async fn search_rows<C: ConnectionTrait>(
    db: &C,
    search: &CallSearch,
) -> Result<Vec<call::Model>, DbErr> {
    let mut query = filtered(search);

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

/// How many Calls match `search`, **ignoring its window** — the total a
/// paginator reports and the client uses to size its page controls.
async fn count<C: ConnectionTrait>(db: &C, search: &CallSearch) -> Result<u64, DbErr> {
    filtered(search).count(db).await
}

/// The Calls a reconnecting Listener missed — everything emitted after `since`,
/// bounded by `limit`, oldest emission first, each paired with the emission it
/// went out as.
///
/// This backs the live feed's **Backfill** (#9, an improvement over rdio, which
/// drops any Call that arrives while a listener is briefly disconnected). A
/// reconnecting Listener sends the last emission they saw as `since`; the server
/// backfills what it missed, bounded so a client returning after a long gap
/// replays a recent slice (not the whole archive) and falls back to archive
/// search (#13) for more.
///
/// **Emission order, not storage order and not `call_at_ms`** (#94). A recorder
/// timestamp can arrive out of order, so `call_at_ms` was never a cursor; `id`
/// was one only for as long as every Call is emitted the instant it is stored.
/// A Call still being held — a **Delay** (#73), or one whose emission could not
/// be recorded — has no emission yet and is not backfilled, which is the honest
/// answer: it has not gone out.
///
/// The caller filters the result through the connection's Selection and access
/// scope, so this deliberately does no matrix filtering of its own.
pub async fn emitted_since<C: ConnectionTrait>(
    db: &C,
    since: Emission,
    limit: u64,
) -> Result<EmissionWindow, DbErr> {
    let mut rows = call::Entity::find()
        .filter(call::Column::EmittedSeq.gt(since))
        .order_by_desc(call::Column::EmittedSeq)
        .limit(limit)
        .all(db)
        .await?;
    rows.reverse(); // newest-first query -> ascending for oldest-first delivery

    // Read off the page rather than off what a caller ends up sending, which is
    // filtered by a Selection this knows nothing about.
    //
    // `limit > 0` because an empty page under a zero limit is not a gap in
    // anyone's history — it is a caller who asked for nothing. The live feed
    // never does, but this is the window #63's DVR reads too, and "truncated"
    // is a claim a Listener is shown.
    let truncated = limit > 0 && rows.len() as u64 == limit;
    let views = stored_calls(db, &rows).await?;

    Ok(EmissionWindow {
        // The rows and their views are in the same order, so each Call is paired
        // back with the emission it went out as — the cursor the Listener hands
        // back next time. It has to be the *emission*: a window carrying a
        // Delayed Call would otherwise move that cursor backwards.
        calls: rows
            .iter()
            // Every row here matched `emitted_seq > since`, so none of them can
            // be missing one; the fallback is what a `NULL` would have to mean
            // and not a case the query can produce.
            .map(|row| row.emitted_seq.unwrap_or_default())
            .zip(views)
            .collect(),
        truncated,
    })
}

/// A window of the Archive read in **emission** order, and whether it reached
/// its bound.
///
/// `truncated` is a fact about the Listener's history rather than about this
/// query: it means there is older traffic the window could not reach back far
/// enough to carry, and only archive search (#13) can fill it. A Backfill that
/// quietly returned a short page would be indistinguishable from a Listener who
/// missed nothing.
#[derive(Debug)]
pub struct EmissionWindow {
    /// Each Call and the emission it went out as, oldest first.
    pub calls: Vec<(Emission, StoredCall)>,
    pub truncated: bool,
}

/// One Call with everything the recorder said about it (#42) — the read behind
/// `GET /api/call/{id}`.
///
/// Three statements past [`stored_calls`]'s, and only ever for one Call: this is
/// the detail deliberately kept off the live-feed frame and the search page, so
/// it is paid for exactly when somebody opens a Call.
pub async fn call_detail<C: ConnectionTrait>(
    db: &C,
    id: CallId,
) -> Result<Option<CallDetail>, DbErr> {
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
    Ok(one_view(db, &row).await?.map(|call| CallDetail {
        call,
        stop_ms: row.stop_at_ms,
        priority: row.priority,
        audio_type: row.audio_type,
        frequencies,
        units,
    }))
}

/// One Call's audio, and **what to call the file** — the read behind
/// `GET /api/call/{id}/download`.
///
/// The filename is decided here rather than at the handler because this is
/// where the fields it derives from are all in hand at once (#98): the
/// recorder's own `audio_name`, which the [`StoredCall`] view deliberately
/// omits, and the System and Talkgroup labels, which only the view carries. The
/// handler used to be given both — a view plus one loose column — and asked to
/// put them together, which meant a second read of the same Call to fetch the
/// column back out of the row the first read had already discarded.
pub async fn call_download<C: ConnectionTrait>(
    db: &C,
    id: CallId,
) -> Result<Option<Download>, DbErr> {
    let Some(row) = call::Entity::find_by_id(id).one(db).await? else {
        return Ok(None);
    };
    Ok(one_view(db, &row).await?.map(|view| Download {
        filename: download_filename(&view, row.audio_name.as_deref()),
        mime: view
            .audio_mime
            .unwrap_or_else(|| "application/octet-stream".to_string()),
        object_key: view.object_key,
    }))
}

/// Where one Call's audio lives, what it is, and what a browser should save it
/// as.
pub struct Download {
    /// Empty for an **encrypted Call**, which is a row and no object at all
    /// (#42, spec US 9).
    pub object_key: String,
    pub mime: String,
    pub filename: String,
}

/// The denormalized view of a row already read.
///
/// One row in gives one view out, so the `Option` is the shape [`stored_calls`]
/// answers in rather than a case either caller has to handle twice.
async fn one_view<C: ConnectionTrait>(
    db: &C,
    row: &call::Model,
) -> Result<Option<StoredCall>, DbErr> {
    Ok(stored_calls(db, std::slice::from_ref(row)).await?.pop())
}

// ---------------------------------------------------------------------------
// Denormalizing
// ---------------------------------------------------------------------------

/// Denormalize a whole page of Calls in a fixed number of queries — six, no
/// matter how many Calls — rather than six *per Call*.
///
/// This is what lets `GET /api/calls` answer with ready-to-render, ready-to-play
/// results. rdio-scanner instead returns bare ids and has the client re-request
/// every Call it wants to display, which is an N+1 over its WebSocket and the
/// single worst part of its archive UX on a Pi. Results come back in the order
/// they were given.
///
/// **The only denormalizer** (#86). A single-Call form used to sit beside it,
/// and every caller of it already held the row it re-fetched — so what it really
/// bought was the chance to write a loop, which the live-feed Backfill did:
/// seven round-trips per Call, up to seven hundred per reconnect on a Pi. A
/// caller with one Call passes `std::slice::from_ref(&call)`.
pub async fn stored_calls<C: ConnectionTrait>(
    db: &C,
    calls: &[call::Model],
) -> Result<Vec<StoredCall>, DbErr> {
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

/// The distinct values of an iterator, order-insensitive — the `IN (…)` list for
/// a batched lookup.
fn distinct(ids: impl Iterator<Item = i64>) -> Vec<i64> {
    ids.collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect()
}

// ---------------------------------------------------------------------------
// Cascading filter options
// ---------------------------------------------------------------------------

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
    CallQuery::new()
        .join_system()
        .join_talkgroup()
        .join_tag()
        .filtered_by(search)
        .facets()
        .column_as(system::Column::Ref, "system_ref")
        .column_as(system::Column::Label, "system_label")
        .column_as(talkgroup::Column::Ref, "talkgroup_ref")
        .column_as(talkgroup::Column::Label, "talkgroup_label")
        .column_as(tag::Column::Name, "tag_name")
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
    CallQuery::new()
        .join_group()
        .filtered_by(search)
        .facets()
        .column_as(group::Column::Name, "name")
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
    // to `numeric` — see `repo::total_audio_bytes`), so one decode works for
    // both.
    Ok(filtered(search)
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
///
/// Clearing the dimension's own filter is now **only** the cascade's semantics.
/// It used to be load-bearing twice over: the Group query would have joined
/// Talkgroup→Group a second time if a Group filter had ever reached it, and no
/// test set one. [`CallQuery`] is why that is no longer possible.
pub async fn options<C: ConnectionTrait>(
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
fn dedup_by_key<Row, Key, Out>(
    rows: Vec<Row>,
    key: impl Fn(&Row) -> Key,
    build: impl Fn(Row) -> Out,
) -> Vec<Out>
where
    Key: std::hash::Hash + Eq,
{
    let mut seen = std::collections::HashSet::new();
    rows.into_iter()
        .filter(|row| seen.insert(key(row)))
        .map(build)
        .collect()
}

// ---------------------------------------------------------------------------
// Query parsing
// ---------------------------------------------------------------------------

/// Read the archive-search filters out of a query string, or say which
/// parameter was wrong. Blank is absent and bad input is named — see
/// [`crate::query`], which both read surfaces share.
fn parse_search(params: &HashMap<String, String>) -> Filtered<CallSearch> {
    let params = Params::new(params);

    let sort = match params.raw("sort") {
        None | Some("newest") | Some("desc") => CallSort::Newest,
        Some("oldest") | Some("asc") => CallSort::Oldest,
        Some(other) => {
            return Err(bad(format!(
                "sort must be one of newest, oldest, desc, asc (got {other:?})"
            )));
        }
    };

    Ok(CallSearch {
        after_ms: params.time("after")?,
        before_ms: params.time("before")?,
        system_ref: params.number("system")?,
        talkgroup_ref: params.number("talkgroup")?,
        group_name: params.raw("group").map(str::to_owned),
        tag_name: params.raw("tag").map(str::to_owned),
        // Whole **seconds** on the wire, milliseconds in the column. Seconds is
        // the unit a listener thinks in and the unit the control offers
        // (1 s / 3 s / 5 s); every other time value here is milliseconds
        // because it is an *instant*, and a duration is not one.
        //
        // `checked_mul` because `number` yields an unbounded `i64`, and the
        // conversion is the one place in this function where a value that
        // *parsed* can still not fit — an unchecked `* 1000` would panic in
        // debug and silently wrap in release, turning a hostile query string
        // into a search that quietly matched the wrong Calls.
        min_duration_ms: params
            .number("minDuration")?
            .map(seconds_to_ms)
            .transpose()?,
        sort,
        limit: params.limit(DEFAULT_LIMIT, MAX_LIMIT)?,
        offset: params.offset()?,
    })
}

/// A whole-second duration from a query string, as the milliseconds the column
/// stores — or the same named-parameter rejection every other bad value here
/// gets. Refuses a negative value too: a Call cannot be shorter than no time at
/// all, and `-1` would otherwise match everything with a duration.
fn seconds_to_ms(seconds: i64) -> Filtered<i64> {
    seconds
        .checked_mul(1000)
        .filter(|ms| *ms >= 0)
        .ok_or_else(|| bad("minDuration must be a duration in whole seconds"))
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// `GET /api/calls` — one page of archive-search results (spec US 24/25).
pub async fn search(
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<SearchPage, Failure> {
    let search = parse_search(&params)?;

    page(&state.db, &search)
        .await
        .map_err(Stage::SearchCalls.failed())
}

/// `GET /api/calls/filters` — the cascading filter options for the filters
/// already chosen (spec US 24). `sort`/`limit`/`offset` are accepted and
/// ignored, so a client can reuse one query string for both endpoints.
pub async fn filters(
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<FilterOptions, Failure> {
    let search = parse_search(&params)?;

    options(&state.db, &search)
        .await
        .map_err(Stage::LoadFilterOptions.failed())
}

/// `GET /api/call/{id}` — one Call, with everything the recorder said about it
/// (#42, spec US 5).
///
/// The home of the per-frequency and per-source detail: the search page and the
/// live feed carry what a *list* needs, and this carries what one Call is. See
/// [`crate::call::CallDetail`] for why the split is where it is.
pub async fn detail(
    State(state): State<AppState>,
    Path(id): Path<CallId>,
) -> Result<CallDetail, Failure> {
    call_detail(&state.db, id)
        .await
        .map_err(Stage::LoadCallDetail.failed())?
        .ok_or(Reason::CallNotFound.into())
}

/// `GET /api/call/{id}/download` — the Call's audio as a named file attachment
/// (spec US 27).
///
/// Unlike [`crate::serve::audio`], this always proxies the bytes, even on an S3
/// backend that could redirect to a presigned URL: the browser would then save
/// the file under the opaque object key, which is precisely what this endpoint
/// exists to avoid. Downloads are occasional and a Call is seconds of audio, so
/// the proxy costs little.
pub async fn download(
    State(state): State<AppState>,
    Path(id): Path<CallId>,
) -> Result<Attachment, Failure> {
    let call = call_download(&state.db, id)
        .await
        .map_err(Stage::LookUpCall.failed())?
        .ok_or(Reason::CallNotFound)?;

    // An encrypted Call has no object behind it (#42, spec US 9) — the same
    // answer the streaming path gives, for the same reason.
    if call.object_key.is_empty() {
        return Err(Reason::CallHasNoAudio.into());
    }

    let bytes = state
        .audio
        .get(&call.object_key)
        .await
        .map_err(Stage::ReadAudio.failed())?
        .ok_or(Reason::AudioNotFound)?;

    Ok(Attachment {
        filename: call.filename,
        mime: call.mime,
        bytes,
    })
}

/// A Call's audio as a named file a browser saves rather than plays.
pub struct Attachment {
    filename: String,
    mime: String,
    bytes: bytes::Bytes,
}

impl IntoResponse for Attachment {
    fn into_response(self) -> Response {
        (
            [
                (
                    header::CONTENT_TYPE,
                    header_value(&self.mime, "application/octet-stream"),
                ),
                (
                    header::CONTENT_DISPOSITION,
                    header_value(
                        &format!("attachment; filename=\"{}\"", self.filename),
                        "attachment",
                    ),
                ),
            ],
            self.bytes,
        )
            .into_response()
    }
}

/// A header value from runtime text, falling back when the text can't be one.
/// Recorder-supplied MIME types reach this unvalidated, so the fallback is a
/// real path, not a formality.
fn header_value(raw: &str, fallback: &'static str) -> HeaderValue {
    HeaderValue::from_str(raw).unwrap_or(HeaderValue::from_static(fallback))
}

// ---------------------------------------------------------------------------
// Download filenames
// ---------------------------------------------------------------------------

/// The longest stem a download filename gets, so a pathological label can't
/// produce a name a filesystem rejects.
const MAX_STEM: usize = 120;

/// Name a downloaded Call after what it *is*: `System-Talkgroup-time.ext`.
///
/// rdio-scanner hands back the recorder's own filename, which names neither the
/// System nor the Talkgroup — a folder of them is unreadable. The recorder's
/// name is still consulted for the file *extension*, since it knows the real
/// container; the MIME type is the fallback.
fn download_filename(call: &StoredCall, audio_name: Option<&str>) -> String {
    let system = call
        .system_label
        .clone()
        .unwrap_or_else(|| call.system_ref.to_string());
    let talkgroup = call
        .talkgroup_label
        .clone()
        .unwrap_or_else(|| call.talkgroup_ref.to_string());
    let at_ms = call.timestamp.unwrap_or_default();
    let extension = download_extension(audio_name, call.audio_mime.as_deref());

    format!(
        "{}.{extension}",
        slug(&format!("{system}-{talkgroup}-{at_ms}"))
    )
}

/// The container extension for a download: the recorder's filename knows best,
/// the MIME type is the fallback, and `bin` is the last resort.
fn download_extension(audio_name: Option<&str>, mime: Option<&str>) -> String {
    let from_name = audio_name
        .and_then(|name| name.rsplit_once('.'))
        .map(|(_, ext)| ext.trim().to_ascii_lowercase())
        .filter(|ext| {
            (1..=8).contains(&ext.len()) && ext.chars().all(|c| c.is_ascii_alphanumeric())
        });
    if let Some(ext) = from_name {
        return ext;
    }

    // Strip any `; codecs=…` parameter before matching.
    let mime = mime
        .map(|mime| mime.split(';').next().unwrap_or_default().trim())
        .unwrap_or_default()
        .to_ascii_lowercase();
    match mime.as_str() {
        "audio/mpeg" | "audio/mp3" => "mp3",
        "audio/mp4" | "audio/m4a" | "audio/x-m4a" => "m4a",
        "audio/aac" => "aac",
        "audio/wav" | "audio/x-wav" | "audio/wave" | "audio/vnd.wave" => "wav",
        "audio/ogg" | "audio/opus" => "ogg",
        "audio/flac" | "audio/x-flac" => "flac",
        _ => "bin",
    }
    .to_string()
}

/// Reduce a label to something safe in a `Content-Disposition` header and on
/// every filesystem: ASCII word characters, dots, dashes and underscores, with
/// everything else collapsed to a single dash.
fn slug(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len().min(MAX_STEM));
    for ch in raw.chars() {
        let keep = ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_');
        if keep {
            out.push(ch);
        } else if !out.ends_with('-') {
            out.push('-');
        }
        if out.len() >= MAX_STEM {
            break;
        }
    }
    let trimmed = out.trim_matches(['-', '.', '_']);
    if trimmed.is_empty() {
        "call".to_string()
    } else {
        trimmed.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::blob::StoredAudio;
    use crate::db::repo::{NewCall, Resolved, emit_call, insert_call};
    use proptest::prelude::*;
    use rstest::rstest;

    /// A database with the schema on it, and nothing in it.
    async fn db(dir: &tempfile::TempDir) -> crate::db::Db {
        crate::db::connect(&crate::testing::sqlite_url(dir))
            .await
            .expect("db")
    }

    /// The least a Call needs to be storable, and the object it points at.
    fn a_call(talkgroup_ref: i64) -> (NewCall, Option<StoredAudio>) {
        (
            NewCall::new(11, talkgroup_ref, 1_000 + talkgroup_ref),
            Some(StoredAudio::written(format!("k{talkgroup_ref}"), 0)),
        )
    }

    /// **Asking for a join twice is asking for it once** — the property that
    /// replaced the `Joined` flag set, asserted on every relation and especially
    /// on the one that never had a flag.
    ///
    /// The Group join is what the old protocol got wrong: [`group_facets`] joins
    /// Talkgroup→Group in order to *select* from it, and a Group **filter**
    /// joins the same two tables to narrow by name. Together they were
    /// `ambiguous column name: groups.name` — and nothing ever asked for both,
    /// because [`options`] clears each dimension's own filter before reading it.
    /// Here they are asked for together on purpose, which is the only way to
    /// state that the protocol is gone rather than merely unexercised.
    #[tokio::test]
    async fn a_join_asked_for_twice_is_made_once() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let db = db(&tmp).await;
        // Every filter that joins something, so every guard is asked twice.
        let search = CallSearch {
            system_ref: Some(11),
            talkgroup_ref: Some(100),
            tag_name: Some("Fire".into()),
            group_name: Some("Emergency".into()),
            ..CallSearch::default()
        };

        let rows = CallQuery::new()
            .join_system()
            .join_talkgroup()
            .join_tag()
            .join_group()
            .filtered_by(&search)
            .rows()
            .all(&db)
            .await;
        assert!(rows.is_ok(), "a Call query joined twice: {rows:?}");

        // ...and through the facet query, which is where the pre-join is real.
        assert!(
            group_facets(&db, &search).await.is_ok(),
            "the Group facet query, with a Group filter on it"
        );
        assert!(facet_rows(&db, &search).await.is_ok());
    }

    /// A **Backfill** is ordered by *emission*, never by storage (CONTEXT.md's
    /// **Backfill**). #73's **Delay** stores a Call on arrival and emits it
    /// later, so a Call stored first can be emitted second — and a cursor over
    /// storage order steps straight past it, silently, for the one Listener who
    /// was away when it went out.
    ///
    /// Each Call comes back **paired with the emission it went out as**, which
    /// is what the Listener hands back as their next cursor. Pairing them here
    /// rather than at the socket is why the two can no longer be zipped up
    /// wrongly by a second caller: there is only one place that zips.
    #[tokio::test]
    async fn a_window_reads_emission_order_not_storage_order() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let db = db(&tmp).await;
        let (call, audio) = a_call(100);
        let early = insert_call(&db, &call, audio, &Resolved::unresolved(), true, 0)
            .await
            .expect("early");
        let (call, audio) = a_call(200);
        let late = insert_call(&db, &call, audio, &Resolved::unresolved(), true, 0)
            .await
            .expect("late");

        // Emitted in the other order: the Call stored second goes out first, and
        // the one stored first is held back the way a Delay holds one.
        emit_call(&db, late.id, 1).await.expect("emit the late one");
        emit_call(&db, early.id, 2)
            .await
            .expect("emit the early one");

        let window = emitted_since(&db, 0, 10).await.expect("a window");

        assert_eq!(
            window
                .calls
                .iter()
                .map(|(seq, call)| (*seq, call.id))
                .collect::<Vec<_>>(),
            vec![(1, late.id), (2, early.id)],
            "emission order, each Call carrying the emission it went out as"
        );
        assert!(!window.truncated, "ten asked for, two found");
    }

    /// Reaching the bound is a fact about the **Listener's history**, not about
    /// the query: it says there is older traffic this window could not carry, so
    /// the connection can say so rather than let a gap pass for "you missed
    /// nothing" (#86's `gap` frame).
    #[tokio::test]
    async fn a_window_that_reaches_its_bound_says_so() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let db = db(&tmp).await;
        for talkgroup_ref in 1..=3 {
            let (call, audio) = a_call(talkgroup_ref);
            let stored = insert_call(&db, &call, audio, &Resolved::unresolved(), true, 0)
                .await
                .expect("stored");
            emit_call(&db, stored.id, talkgroup_ref)
                .await
                .expect("emit");
        }

        let full = emitted_since(&db, 0, 2).await.expect("a bounded window");
        assert_eq!(full.calls.len(), 2);
        assert!(full.truncated, "two of three is a truncated window");
        assert_eq!(
            full.calls.iter().map(|(seq, _)| *seq).collect::<Vec<_>>(),
            vec![2, 3],
            "the bound keeps the *newest* emissions, ascending"
        );

        let rest = emitted_since(&db, 3, 2).await.expect("a caught-up window");
        assert!(rest.calls.is_empty());
        assert!(!rest.truncated, "nothing to carry is not a gap");

        // A caller who asked for nothing was not truncated either. The live feed
        // never asks, but a Listener is *shown* this claim, and #63's DVR reads
        // the same window.
        let none = emitted_since(&db, 0, 0).await.expect("a zero window");
        assert!(none.calls.is_empty());
        assert!(
            !none.truncated,
            "asking for no Calls is not a gap in history"
        );
    }

    fn query(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    #[test]
    fn an_empty_query_is_the_default_page_newest_first() {
        let search = parse_search(&query(&[])).unwrap();
        assert_eq!(search.sort, CallSort::Newest);
        assert_eq!(search.limit, DEFAULT_LIMIT);
        assert_eq!(search.offset, 0);
        assert_eq!(search.after_ms, None);
        assert_eq!(search.before_ms, None);
        assert_eq!(search.system_ref, None);
        assert_eq!(search.talkgroup_ref, None);
        assert_eq!(search.group_name, None);
        assert_eq!(search.tag_name, None);
        assert_eq!(search.min_duration_ms, None);
    }

    #[test]
    fn every_filter_is_read_and_whitespace_trimmed() {
        let search = parse_search(&query(&[
            ("after", " 1000 "),
            ("before", "2000"),
            ("system", "11"),
            ("talkgroup", "54241"),
            ("group", " Fire "),
            ("tag", "Fire Dispatch"),
            ("minDuration", " 5 "),
            ("sort", "oldest"),
            ("limit", "25"),
            ("offset", "50"),
        ]))
        .unwrap();

        assert_eq!(search.after_ms, Some(1000));
        assert_eq!(search.before_ms, Some(2000));
        assert_eq!(search.system_ref, Some(11));
        assert_eq!(search.talkgroup_ref, Some(54241));
        assert_eq!(search.group_name.as_deref(), Some("Fire"));
        assert_eq!(search.tag_name.as_deref(), Some("Fire Dispatch"));
        // Seconds on the wire, milliseconds in the column — asserted on a value
        // where the conversion is the only arithmetic that reaches it (#83).
        assert_eq!(search.min_duration_ms, Some(5_000));
        assert_eq!(search.sort, CallSort::Oldest);
        assert_eq!(search.limit, 25);
        assert_eq!(search.offset, 50);
    }

    /// The client's form state renders "no filter" as an empty value; blank and
    /// whitespace-only must read as absent, not as a filter matching nothing.
    #[rstest]
    #[case("")]
    #[case("   ")]
    fn blank_values_are_absent(#[case] blank: &str) {
        let search = parse_search(&query(&[
            ("system", blank),
            ("talkgroup", blank),
            ("group", blank),
            ("tag", blank),
            ("after", blank),
            ("before", blank),
            ("minDuration", blank),
            ("sort", blank),
            ("limit", blank),
            ("offset", blank),
        ]))
        .unwrap();

        assert_eq!(search.system_ref, None);
        assert_eq!(search.talkgroup_ref, None);
        assert_eq!(search.group_name, None);
        assert_eq!(search.tag_name, None);
        assert_eq!(search.after_ms, None);
        assert_eq!(search.before_ms, None);
        assert_eq!(search.min_duration_ms, None);
        assert_eq!(search.sort, CallSort::Newest);
        assert_eq!(search.limit, DEFAULT_LIMIT);
        assert_eq!(search.offset, 0);
    }

    #[rstest]
    #[case("newest", CallSort::Newest)]
    #[case("desc", CallSort::Newest)]
    #[case("oldest", CallSort::Oldest)]
    #[case("asc", CallSort::Oldest)]
    fn sort_spellings(#[case] raw: &str, #[case] expected: CallSort) {
        assert_eq!(
            parse_search(&query(&[("sort", raw)])).unwrap().sort,
            expected
        );
    }

    #[rstest]
    #[case("0", DEFAULT_LIMIT)] // zero means "unspecified", not "no rows"
    #[case("1", 1)]
    #[case("500", MAX_LIMIT)]
    #[case("501", MAX_LIMIT)] // clamped, not refused
    #[case("99999", MAX_LIMIT)]
    fn limit_defaults_and_clamps(#[case] raw: &str, #[case] expected: u64) {
        assert_eq!(
            parse_search(&query(&[("limit", raw)])).unwrap().limit,
            expected
        );
    }

    /// Every rejection names the parameter at fault — rdio-scanner silently
    /// ignores what it can't parse, so a typo returns plausible wrong results.
    #[rstest]
    #[case("system", "abc")]
    #[case("system", "1.5")]
    #[case("talkgroup", "twelve")]
    #[case("after", "yesterday")]
    #[case("after", "2026-13-45T00:00:00Z")]
    #[case("before", "")] // (blank is fine — see below; this case is overridden)
    #[case("minDuration", "a while")]
    #[case("minDuration", "2.5")]
    // The conversion to milliseconds is where a value that parsed can still
    // not fit — an unchecked multiply would panic in debug and wrap in release.
    #[case("minDuration", "9223372036854775807")]
    #[case("minDuration", "-1")]
    #[case("sort", "sideways")]
    #[case("limit", "-1")]
    #[case("offset", "-1")]
    #[case("offset", "1e3")]
    fn malformed_values_are_named_in_the_error(#[case] key: &str, #[case] value: &str) {
        let result = parse_search(&query(&[(key, value)]));
        if value.is_empty() {
            assert!(result.is_ok(), "blank {key} is absent, not malformed");
            return;
        }
        let told = result.expect_err("should reject").told();
        assert!(told.contains(key), "refusal {told:?} should name {key:?}");
    }

    // `parse_time_ms`'s own cases live with it in `crate::query`, which both
    // read surfaces share.

    fn call() -> StoredCall {
        StoredCall {
            id: 42,
            system_ref: 11,
            system_label: Some("Butler County".into()),
            talkgroup_ref: 54241,
            talkgroup_label: Some("TDB A1".into()),
            talkgroup_group: None,
            talkgroup_tag: None,
            led: None,
            patches: vec![],
            frequency: None,
            source: None,
            timestamp: Some(1_669_740_338_000),
            audio_mime: Some("audio/mp4".into()),
            duration_ms: None,
            emergency: false,
            encrypted: false,
            site_ref: None,
            object_key: "ab/opaque-key.m4a".into(),
            audio_url: Some("/api/call/42/audio".into()),
        }
    }

    /// The name says what the Call *is* — System, Talkgroup, when — instead of
    /// the recorder's or the object store's opaque key.
    #[test]
    fn download_filename_describes_the_call() {
        assert_eq!(
            download_filename(&call(), Some("54241-1669740338_774031250.m4a")),
            "Butler-County-TDB-A1-1669740338000.m4a"
        );
    }

    /// With no labels curated yet, the Refs carry the identity.
    #[test]
    fn download_filename_falls_back_to_refs_and_the_mime_type() {
        let bare = StoredCall {
            system_label: None,
            talkgroup_label: None,
            audio_mime: Some("audio/x-wav".into()),
            ..call()
        };
        assert_eq!(download_filename(&bare, None), "11-54241-1669740338000.wav");
    }

    /// A Call whose labels are blank or unprintable still gets a safe name —
    /// no empty stem, no leading dot, no missing extension.
    #[test]
    fn download_filename_survives_a_call_with_no_usable_metadata() {
        let empty = StoredCall {
            system_ref: 0,
            system_label: Some(String::new()),
            talkgroup_label: Some("///".into()),
            timestamp: None,
            audio_mime: None,
            ..call()
        };
        // Only the (zero) call time survives the slug.
        assert_eq!(download_filename(&empty, None), "0.bin");
    }

    #[rstest]
    // The recorder's own filename knows the real container.
    #[case(Some("call.m4a"), Some("audio/x-wav"), "m4a")]
    #[case(Some("CALL.WAV"), None, "wav")]
    #[case(Some("a.b.opus"), None, "opus")]
    // Unusable names fall through to the MIME type...
    #[case(Some("noextension"), Some("audio/mpeg"), "mp3")]
    #[case(Some("call."), Some("audio/mp4"), "m4a")]
    #[case(Some("call.tar.gz!"), Some("audio/aac"), "aac")]
    #[case(Some("call.waaaaaaaaaay"), Some("audio/ogg"), "ogg")]
    #[case(None, Some("audio/flac"), "flac")]
    #[case(None, Some("audio/mp4; codecs=\"mp4a.40.2\""), "m4a")]
    #[case(None, Some("AUDIO/WAV"), "wav")]
    // ...and an unknown MIME type to a neutral last resort.
    #[case(None, Some("application/octet-stream"), "bin")]
    #[case(None, None, "bin")]
    fn extension_prefers_the_recorder_then_the_mime_type(
        #[case] name: Option<&str>,
        #[case] mime: Option<&str>,
        #[case] expected: &str,
    ) {
        assert_eq!(download_extension(name, mime), expected);
    }

    #[rstest]
    #[case("Butler County", "Butler-County")]
    #[case("Fire/EMS", "Fire-EMS")]
    #[case("a//b", "a-b")] // runs collapse to one dash
    #[case("../../etc/passwd", "etc-passwd")] // no traversal survives
    #[case("  spaced  ", "spaced")]
    #[case("Pompiers Sûreté", "Pompiers-S-ret")] // non-ASCII is replaced
    #[case("...", "call")] // nothing usable -> a name, never ""
    #[case("", "call")]
    #[case("_under_", "under")]
    fn slug_cases(#[case] raw: &str, #[case] expected: &str) {
        assert_eq!(slug(raw), expected);
    }

    /// A pathological label can't grow a filename past what a filesystem takes.
    #[test]
    fn slug_truncates_a_very_long_label() {
        assert_eq!(slug(&"a".repeat(500)).len(), MAX_STEM);
    }

    proptest! {
        /// Whatever a recorder or an operator put in a label, the slug is always
        /// a safe, non-empty, bounded filename component.
        #[test]
        fn slug_is_always_a_safe_filename_component(raw in ".{0,300}") {
            let slug = slug(&raw);
            prop_assert!(!slug.is_empty());
            prop_assert!(slug.len() <= MAX_STEM);
            prop_assert!(
                slug.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_')),
                "unsafe characters in {slug:?}"
            );
            // A dot inside the stem is harmless ("Engine.1"); what must never
            // happen is a leading one — that's both a hidden file and the only
            // way `.` or `..` could become the whole component.
            prop_assert!(!slug.starts_with('.'), "hidden or traversal name {slug:?}");
            prop_assert!(!slug.ends_with('.'), "trailing dot in {slug:?}");
        }

        /// A download filename is always header-safe: printable ASCII with no
        /// quote to break out of `filename="…"`, and always extended.
        #[test]
        fn download_filename_is_always_header_safe(
            system in ".{0,40}",
            talkgroup in ".{0,40}",
            name in ".{0,40}",
            at_ms in any::<i64>(),
        ) {
            let filename = download_filename(
                &StoredCall {
                    system_label: Some(system),
                    talkgroup_label: Some(talkgroup),
                    timestamp: Some(at_ms),
                    ..call()
                },
                Some(&name),
            );
            let header = format!("attachment; filename=\"{filename}\"");
            prop_assert!(HeaderValue::from_str(&header).is_ok());
            prop_assert!(!filename.contains('"'));
            prop_assert!(!filename.contains('/') && !filename.contains('\\'));
            prop_assert!(filename.contains('.'), "no extension in {filename:?}");
        }
    }
}
