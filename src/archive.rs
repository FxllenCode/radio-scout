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
use crate::access::{AccessScope, Viewer};
use crate::activity::{Axis, Grain, Series};
use crate::call::{CallDetail, CallId, Emission, StoredCall};
use crate::db::entities::{
    call, call_frequency, call_patch, call_unit, group, site, system, tag, talkgroup,
    talkgroup_group,
};
use crate::db::repo::distinct;
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
#[derive(Debug, Clone)]
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
    /// Only Calls a given radio was heard on (#47, spec US 44) — and, when a
    /// **Unit** owns that Ref, every other Ref the apparatus answers to.
    ///
    /// Deliberately *not* one of the cascading filter dimensions: a county has
    /// tens of thousands of radios, and offering them as a dropdown would put an
    /// unbounded list in every filter response. It is reached by tapping a unit
    /// label or typing a radio id, which is how somebody arrives at the question.
    pub unit_ref: Option<i64>,
    /// Only Calls carrying this **Mark** (#42, #55, spec US 20) — the emergency
    /// bit a radio set, or a **Tone profile** this Instance heard paged.
    ///
    /// One filter over the whole closed vocabulary rather than a boolean per
    /// mark, because that is what a Mark *is*: `MARKS` is the list, so a mark
    /// added later is filterable without a wire change or a migration. Single-
    /// valued on purpose — every other filter here combines with AND, and a list
    /// would have to mean OR, which is a second rule on one query string.
    ///
    /// Deliberately **not** a cascading dimension. The cascade exists to stop a
    /// filter offering choices that would return nothing, which needs a query
    /// per dimension per request; a vocabulary of two whose meaning does not
    /// depend on the others is a toggle, and buying two more facet queries on
    /// every filter request to tell an Operator that nothing was ever an
    /// emergency would be paying a real price for that. `minDuration` and `unit`
    /// are out for the same reason.
    pub mark: Option<crate::webhook::Mark>,
    /// Only Calls a **Selection** reaches (#63, spec US 39) — the **DVR**'s
    /// scope, and the one it has.
    ///
    /// The DVR's Talkgroup picker mints a one-entry Selection rather than
    /// setting `talkgroup_ref`, so the surface has a single scoping rule
    /// instead of two that differ on **Patch**es: this one answers
    /// [`Selection::reaches_channels`]'s *question* — the live feed's own —
    /// where `talkgroup_ref` above compares the Call's canonical channel and
    /// stops. A Listener rewinding a channel through a patch would otherwise
    /// find it silent exactly when the county was busiest.
    ///
    /// It answers that question with a **second implementation** ([`within`]),
    /// because a predicate over one Call cannot be a `WHERE` clause. The two
    /// are held together by
    /// `tests/archive.rs::the_sql_filter_answers_what_the_selection_itself_would`,
    /// which enumerates the matrix exhaustively — #62's bucket arithmetic is
    /// the same shape, and its lesson is that either implementation alone
    /// answers confidently and wrongly.
    ///
    /// Deliberately not a cascading dimension, for `mark`'s reason: it is the
    /// whole scanner rather than one axis of a form, and no dropdown offers it.
    pub selection: Option<crate::selection::Selection>,
    /// Only Calls a **Listener** starred (#66, spec US 37).
    ///
    /// A boolean rather than an `Option<bool>`, because there is no third
    /// question: "show me the starred ones" and "show me everything" are the
    /// two a Listener asks, and *only the unstarred ones* is not a thing
    /// anybody wants — it would answer with the whole Archive minus a handful
    /// and read as an unfiltered page. So `starred=false` and no parameter at
    /// all mean the same thing, which is also what a checkbox produces when it
    /// is cleared.
    ///
    /// Deliberately not a cascading dimension, for [`CallSearch::mark`]'s
    /// reason: a two-valued toggle whose meaning does not depend on the other
    /// filters would buy a facet query per request to tell a Listener that
    /// nothing has been starred.
    pub starred: bool,
    /// Only Calls with audio behind them (#65) — an **Encrypted Call** is a row
    /// and no object at all (#42, spec US 9).
    ///
    /// The **stitched export**'s, and nothing else's: every other surface shows
    /// an encrypted Call precisely so that the activity is visible. It is also
    /// not a query-string filter — nobody types this — which is why it is set by
    /// the one caller that needs it rather than read by [`parse_search`].
    pub with_audio: bool,
    /// What the **Listener** asking may hear (#68, spec US 52) — every
    /// unrestricted channel, plus whatever **Access code** they presented opens.
    ///
    /// [`CallSearch::with_audio`]'s shape and for a sharper reason: nobody types
    /// this, and a query string that *could* would be a way to ask for somebody
    /// else's channels. [`crate::query::Params`] never reads it; every handler
    /// sets it from the [`crate::access::Viewer`] it was extracted with, which is
    /// what makes forgetting one a compile-time-visible omission rather than a
    /// search that quietly answers with the whole Archive.
    ///
    /// The default is [`AccessScope::All`] rather than open listening, because
    /// this type is also how the **Downstream** sender, the **Mining** sweep and
    /// every worker read the Archive — none of which is a Listener, and all of
    /// which must see every Call there is.
    pub scope: AccessScope,
    pub sort: CallSort,
    pub limit: u64,
    pub offset: u64,
}

impl Default for CallSearch {
    /// An unfiltered search that reaches **every** Call there is.
    ///
    /// Hand-written rather than derived so that [`CallSearch::scope`] can be
    /// [`AccessScope::All`] while [`AccessScope`]'s own default is open
    /// listening. The two are opposite on purpose: a scope assembled by mistake
    /// must grant the least, and a `CallSearch` assembled by hand belongs to a
    /// **Worker** rather than to a Listener — the **Downstream** sender, an
    /// export walking a range it has already been allowed, a sweep. Every
    /// Listener-facing search comes through [`parse_search`], which cannot be
    /// called without a [`Viewer`].
    fn default() -> Self {
        CallSearch {
            after_ms: None,
            before_ms: None,
            system_ref: None,
            talkgroup_ref: None,
            group_name: None,
            tag_name: None,
            min_duration_ms: None,
            unit_ref: None,
            mark: None,
            selection: None,
            starred: false,
            with_audio: false,
            scope: AccessScope::All,
            sort: CallSort::Newest,
            limit: 0,
            offset: 0,
        }
    }
}

/// A [`CallSearch`] with everything the database had to be asked **before** its
/// query could be built (#47).
///
/// One thing needs it today: a unit filter searches for the apparatus rather
/// than for the number typed, and which Refs that is lives in the `units` and
/// `unit_refs` tables. Resolving it once per request — rather than once per
/// query, of which [`options`] issues five — is why this is a value and not a
/// lookup inside [`CallQuery`].
///
/// It is also why it is a *type*. Every query below is built from one of these
/// and there is one way to make one, so a read that forgot to resolve the scope
/// and silently searched the bare Ref is not expressible.
struct Filters {
    search: CallSearch,
    /// Which Refs the unit filter reaches, and on which System each counts.
    /// `None` when there is no unit filter.
    unit: Option<crate::merge::UnitScope>,
}

impl Filters {
    async fn resolve<C: ConnectionTrait>(db: &C, search: &CallSearch) -> Result<Self, DbErr> {
        let unit = match search.unit_ref {
            Some(unit_ref) => Some(crate::db::repo::unit_scope(db, unit_ref).await?),
            None => None,
        };
        Ok(Filters {
            search: search.clone(),
            unit,
        })
    }

    /// The same filters with one dimension cleared — the cascade's semantics
    /// (see [`options`]).
    ///
    /// The resolved scope survives untouched, and can: a [`crate::merge::UnitScope`]
    /// carries the System each of its Ranges counts on, so it is a fact about
    /// the Ref rather than about the search that asked. Clearing the System
    /// filter — which the System facet does — therefore cannot leave behind a
    /// scope that means something else than it did.
    fn without(&self, mutate: fn(&mut CallSearch)) -> Filters {
        let mut search = self.search.clone();
        mutate(&mut search);
        Filters {
            search,
            unit: self.unit.clone(),
        }
    }
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
    fn filtered_by(mut self, filters: &Filters) -> Self {
        let search = &filters.search;
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
        if search.with_audio {
            self = self.and_where(call::Column::Encrypted.eq(false));
        }
        if search.starred {
            // A column, so this is a filter and never a join — `carrying`'s
            // rule, and the reason a Star is not a child table (#66).
            self = self.and_where(call::Column::StarredAtMs.is_not_null());
        }
        if let Some(mark) = search.mark {
            self = self.and_where(carrying(mark));
        }
        if let Some(scope) = &filters.unit {
            self = self.and_where(heard_by(scope));
        }
        if let Some(reached) = search.selection.as_ref().and_then(within) {
            self = self.join_system().join_talkgroup().and_where(reached);
        }
        // Last, and structurally last: every filter above narrows what a
        // Listener asked for, and this narrows what they are allowed to have
        // asked. `None` is the whole cost of this feature on an Instance that
        // gates nothing — no clause, and no joins made for one.
        if let Some(permitted) = gate(&search.scope) {
            self = self.join_system().join_talkgroup().and_where(permitted);
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

    /// The query as a projection the caller will **group**, which is the other
    /// way to collapse rows and wants no `DISTINCT` on top of it (#47's per-Unit
    /// history). Its own method rather than a caller reaching past `rows()` into
    /// the select: which of the three shapes a query is asked for is exactly the
    /// bookkeeping [`CallQuery`] exists to hold.
    fn grouped(self) -> Select<call::Entity> {
        self.select.select_only()
    }
}

/// The Calls carrying one **Mark** (#42, #55).
///
/// Both arms are a column on the Call row, which is what makes this a filter and
/// not a join: `calls.emergency` is what the recorder sent, and `calls.tone` is
/// where detection got to — the denormalized answer #55 keeps beside the
/// `call_tones` rows precisely so that reading it costs no statement here or in
/// [`stored_calls`]. Adding a mark that is *not* a column would owe a subquery,
/// on [`heard_by`]'s terms and never a join.
fn carrying(mark: crate::webhook::Mark) -> sea_orm::sea_query::SimpleExpr {
    use crate::webhook::Mark;

    match mark {
        Mark::Emergency => call::Column::Emergency.eq(true),
        Mark::Tone => call::Column::Tone.eq(call::ToneState::MATCHED),
    }
}

/// The Calls a **UnitScope** reaches — the unit filter (#47), as **subqueries**
/// rather than a join.
///
/// `call_units` is the first genuinely to-many table a filter here reaches: a
/// Recorder lists a radio once per stretch it keys, so Trunk Recorder routinely
/// sends the same `src` twice on one Call. Joined, that Call would come back
/// twice and be counted twice — the case [`CallQuery::rows`] says would owe a
/// `DISTINCT`. A subquery owes nothing: no row is multiplied, the join
/// bookkeeping is untouched, and the covering index `idx_call_units_unit_ref`
/// answers it without visiting the table.
///
/// **The Ref asked about matches on any System; the rest of the apparatus only
/// on its own.** A Ref is unique within a System, so one System's fleet block
/// must never reach another's radios — and the alternative to saying that here
/// is saying it in every caller, which is the unwritten rule #98 deleted. One
/// extra subquery per System an apparatus belongs to, which in practice is one.
fn heard_by(scope: &crate::merge::UnitScope) -> sea_orm::Condition {
    let mut matching =
        sea_orm::Condition::any().add(call::Column::Id.in_subquery(calls_heard_by(&[
            crate::merge::Range::new(scope.asked, scope.asked),
        ])));
    for (system_id, spans) in &scope.owned {
        matching = matching.add(
            call::Column::SystemId
                .eq(*system_id)
                .and(call::Column::Id.in_subquery(calls_heard_by(spans))),
        );
    }
    matching
}

/// Which Calls a **Selection** reaches (#63) — `None` when it reaches every
/// Call there is, so a DVR of an un-narrowed scanner costs no condition and no
/// joins at all.
///
/// **A second implementation of [`Selection::reaches_channels`]**, and there is
/// no way around that: a predicate over one Call in hand cannot be a `WHERE`
/// clause over a table. `the_sql_filter_answers_what_the_selection_itself_would`
/// is what stops the two drifting, by running every shape of matrix through
/// both.
///
/// The matrix resolves, per System, to one of two shapes and never to a third:
/// its default is [`Selection::selects`]'s fallback chain (the System's
/// wildcard, else the global `all`), and every entry that *agrees* with that
/// default says nothing. So a System is either **everything except a list** or
/// **exactly a list**, which is what makes this expressible as SQL rather than
/// as a `CASE` per row.
fn within(selection: &crate::selection::Selection) -> Option<sea_orm::Condition> {
    let scopes = system_scopes(selection);

    // A System whose default is on with nothing excepted admits every Call on
    // it, which is what `all` already says — so a scanner made only of those is
    // an unfiltered search. Asked of the Selection rather than of the reduction,
    // so that this and [`crate::access::AccessScope::granting`] — which
    // normalizes a scope reaching everything to "no gate at all" — cannot come
    // to disagree about which matrices are unfiltered.
    if selection.reaches_everything() {
        return None;
    }

    let mut arms: Vec<sea_orm::Condition> = Vec::new();
    for scope in &scopes {
        let named = system::Column::Ref.eq(scope.system_ref);
        match (scope.default_on, scope.exceptions.is_empty()) {
            // Everything on this System.
            (true, true) => arms.push(sea_orm::Condition::all().add(named)),
            // Everything except the exceptions.
            (true, false) => arms.push(
                sea_orm::Condition::all()
                    .add(named)
                    .add(reaching_other_than(&scope.exceptions)),
            ),
            // Exactly the exceptions.
            (false, false) => arms.push(
                sea_orm::Condition::all()
                    .add(named)
                    .add(reaching(&scope.exceptions)),
            ),
            // Nothing at all on this System — no arm, rather than an arm that
            // can never be true.
            (false, true) => {}
        }
    }

    // Systems the matrix says nothing about fall through to the global
    // default. Only reachable with `all` set, since the everything case
    // returned above.
    if selection.all {
        arms.push(
            sea_orm::Condition::all().add(
                system::Column::Ref.is_not_in(
                    scopes
                        .iter()
                        .map(|scope| scope.system_ref)
                        .collect::<Vec<_>>(),
                ),
            ),
        );
    }

    // Every arm dropped out: a scanner with nothing on. An honest empty page,
    // not an error — switching everything off is an ordinary thing to have
    // said.
    if arms.is_empty() {
        return Some(never());
    }
    Some(
        arms.into_iter()
            .fold(sea_orm::Condition::any(), sea_orm::Condition::add),
    )
}

/// Which Calls an [`AccessScope`] may see — `None` when it may see every Call
/// there is, so an Instance that gates nothing pays no clause and no joins.
///
/// Two arms, `OR`ed, and the order they are written in is the feature's own
/// sentence: **an unrestricted channel is open to everybody**, and a restricted
/// one is reachable only where the grant reaches it. The second half is
/// [`within`] — the Selection SQL a **DVR** already filters on — so an Access
/// code scoped to a channel reaches a **Patch** onto that channel, which is the
/// half rdio-scanner's own scope check gets wrong one feature along
/// (`downstream.go:99`).
///
/// The restriction is read off the Call's **own** channel, which is the same
/// fact [`crate::call::StoredCall::restricted`] carries and therefore the same
/// answer [`AccessScope::permits`] gives a live frame. A Call addressed to a
/// gated channel stays gated however it was patched; one addressed to an open
/// channel stays open, which hides nothing — that audio is reachable under the
/// open channel's own name by anybody.
pub(crate) fn gate(scope: &AccessScope) -> Option<sea_orm::Condition> {
    match scope {
        AccessScope::All => None,
        AccessScope::Granted(granted) => Some(
            sea_orm::Condition::any()
                .add(unrestricted())
                .add(within(granted).unwrap_or_else(never)),
        ),
    }
}

/// A Call whose own channel is open to everybody.
///
/// `COALESCE` because `talkgroups.restricted` is nullable and `NULL` **inherits
/// its System** (#68) — which is what an auto-populated channel carries, so a
/// Ref a recorder discovers on a gated System is gated by this expression
/// without anybody having curated it. `systems.restricted` is not null, so the
/// coalesce always resolves and there is no third state for the comparison to
/// fall through.
fn unrestricted() -> sea_orm::sea_query::SimpleExpr {
    use sea_orm::sea_query::{Expr, Func};

    Expr::expr(Func::coalesce([
        Expr::col((talkgroup::Entity, talkgroup::Column::Restricted)).into(),
        Expr::col((system::Entity, system::Column::Restricted)).into(),
    ]))
    .eq(false)
}

/// One System of a Selection, reduced to a default and the Refs that differ
/// from it.
struct SystemScope {
    system_ref: i64,
    default_on: bool,
    /// The Refs whose state is *not* `default_on`. Refs agreeing with it are
    /// dropped: they change nothing, and carrying them would put a county's
    /// worth of ticked boxes into an `IN` list for no effect.
    exceptions: Vec<i64>,
}

fn system_scopes(selection: &crate::selection::Selection) -> Vec<SystemScope> {
    let mut scopes: Vec<SystemScope> = selection
        .sel
        .iter()
        .filter_map(|(system_ref, talkgroups)| {
            let system_ref = system_ref.parse::<i64>().ok()?;
            let default_on = talkgroups
                .get(crate::selection::TALKGROUP_WILDCARD)
                .copied()
                .unwrap_or(selection.all);
            let mut exceptions: Vec<i64> = talkgroups
                .iter()
                .filter(|(_, on)| **on != default_on)
                .filter_map(|(ref_, _)| ref_.parse::<i64>().ok())
                .collect();
            // Ordered, so the SQL a given Selection produces is one statement
            // rather than one per hash iteration — which is what lets a
            // prepared statement be reused and a test read.
            exceptions.sort_unstable();
            Some(SystemScope {
                system_ref,
                default_on,
                exceptions,
            })
        })
        .collect();
    scopes.sort_unstable_by_key(|scope| scope.system_ref);
    scopes
}

/// A Call reaching any of `refs` — its own channel, or one it was **patched**
/// onto. The System is the caller's to pin, which is why this says nothing
/// about one: a Ref means something only inside a System, and `call_patches`
/// carries no System of its own.
fn reaching(refs: &[i64]) -> sea_orm::Condition {
    sea_orm::Condition::any()
        .add(talkgroup::Column::Ref.is_in(refs.to_vec()))
        .add(call::Column::Id.in_subquery(calls_patched_onto(
            call_patch::Column::TalkgroupRef.is_in(refs.to_vec()),
        )))
}

/// A Call reaching any channel that is *not* one of `refs` — the other reading
/// of the same question, for a System whose default is on.
fn reaching_other_than(refs: &[i64]) -> sea_orm::Condition {
    sea_orm::Condition::any()
        .add(talkgroup::Column::Ref.is_not_in(refs.to_vec()))
        .add(call::Column::Id.in_subquery(calls_patched_onto(
            call_patch::Column::TalkgroupRef.is_not_in(refs.to_vec()),
        )))
}

/// The Call ids carrying a patch row that `matching` accepts.
fn calls_patched_onto(
    matching: sea_orm::sea_query::SimpleExpr,
) -> sea_orm::sea_query::SelectStatement {
    use sea_orm::sea_query::Query as SeaQuery;

    SeaQuery::select()
        .column(call_patch::Column::CallId)
        .from(call_patch::Entity)
        .cond_where(matching)
        .to_owned()
}

/// Matches no Call at all — what a scanner with everything switched off asks
/// for. Written as an expression rather than an empty `Condition::any()`,
/// which sea-query renders as no condition and therefore as *every* Call: the
/// exact inversion of what a Listener said.
fn never() -> sea_orm::Condition {
    use sea_orm::sea_query::Expr;
    sea_orm::Condition::all().add(Expr::val(1).eq(0))
}

/// The Call ids any of `spans` was heard on.
fn calls_heard_by(spans: &[crate::merge::Range]) -> sea_orm::sea_query::SelectStatement {
    use sea_orm::sea_query::Query as SeaQuery;

    let matching = spans.iter().fold(sea_orm::Condition::any(), |any, span| {
        any.add(call_unit::Column::UnitRef.between(span.from(), span.to()))
    });
    SeaQuery::select()
        .column(call_unit::Column::CallId)
        .from(call_unit::Entity)
        .cond_where(matching)
        .to_owned()
}

/// The filtered Call query, without ordering or paging.
fn filtered(filters: &Filters) -> Select<call::Entity> {
    CallQuery::new().filtered_by(filters).rows()
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
    let filters = Filters::resolve(db, search).await?;
    let rows = search_rows(db, &filters).await?;
    let results = stored_calls(db, &rows).await?;
    let count = count(db, &filters).await?;

    Ok(Page::new(results, count, search.limit, search.offset))
}

/// The rows one window holds: filtered, ordered, and paged.
async fn search_rows<C: ConnectionTrait>(
    db: &C,
    filters: &Filters,
) -> Result<Vec<call::Model>, DbErr> {
    let search = &filters.search;
    let mut query = filtered(filters);

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
async fn count<C: ConnectionTrait>(db: &C, filters: &Filters) -> Result<u64, DbErr> {
    filtered(filters).count(db).await
}

// ---------------------------------------------------------------------------
// How busy the Archive was (#62, spec US 34–35)
// ---------------------------------------------------------------------------

/// The range a series covers when the filters match nothing at all and the
/// caller named no bounds of their own.
///
/// A day, because that is what a Listener means by "recently" — the same
/// reading [`crate::catalog::ACTIVITY_WINDOW_MS`] takes, kept separate because
/// that one bounds a *query* and this one only decides what an empty chart is
/// labelled. Answering with an axis rather than with nothing is deliberate:
/// every consumer would otherwise need an arm for a series with no buckets in
/// it, to draw the same flat nothing this draws.
const EMPTY_SPAN_MS: i64 = 24 * 60 * 60 * 1000;

/// One bucket of a grouped activity query, as the database answers it.
#[derive(Debug, sea_orm::FromQueryResult)]
struct ActivityBucket {
    bucket: i64,
    calls: i64,
}

/// How far a filtered search reaches, when a bound has to be discovered rather
/// than given.
#[derive(Debug, sea_orm::FromQueryResult)]
struct Bounds {
    first_ms: Option<i64>,
    last_ms: Option<i64>,
}

/// **How busy the Archive was, bucket by bucket** (#62, spec US 34–35) — the
/// read behind `GET /api/calls/activity`.
///
/// The same [`Filters`] the page and its total are built from, answered as
/// counts instead of rows. That is the whole design: a density ribbon drawn
/// over search results has to describe *those* results, and the only way to be
/// sure of it is for the two to be one filter rather than two that agree today.
/// It is also what makes this the per-Talkgroup activity chart — a chart of one
/// channel is this with a Talkgroup filter set — and what #63's DVR will read
/// its timeline from.
///
/// **The axis is resolved before the aggregate, and only then.** A search that
/// names both bounds has said where its axis is, and costs one statement; a
/// search that names one or neither costs a second to ask the Archive how far
/// it reaches under those filters. Deriving it that way rather than always
/// charting "the last day" is what makes the ribbon describe the search instead
/// of describing the clock.
pub async fn call_activity<C: ConnectionTrait>(
    db: &C,
    search: &CallSearch,
    grain: Grain,
    now_ms: i64,
) -> Result<Series, DbErr> {
    let filters = Filters::resolve(db, search).await?;
    let axis = axis_over(db, &filters, grain, now_ms).await?;

    // Bounded to the axis explicitly, rather than trusting the search's own
    // `after`/`before` to have done it. Two things rest on it: no row can
    // produce a bucket index off the end of the array, and every offset the
    // division sees is non-negative — which is what makes truncating and
    // flooring the same operation, and so what makes the two dialects agree
    // (`crate::activity`).
    let rows = CallQuery::new()
        .filtered_by(&filters)
        .and_where(call::Column::CallAtMs.gte(axis.from_ms()))
        .and_where(call::Column::CallAtMs.lt(axis.to_ms()))
        .grouped()
        .column_as(
            crate::activity::bucket_expr(call::Column::CallAtMs, &axis),
            crate::activity::BUCKET,
        )
        .column_as(call::Column::Id.count(), "calls")
        .group_by(crate::activity::bucket_group())
        .into_model::<ActivityBucket>()
        .all(db)
        .await?;

    Ok(axis.series(rows.into_iter().map(|row| (row.bucket, row.calls))))
}

/// Where a series' axis sits: what the search asked for, else how far the
/// Archive reaches under it, else the last day.
///
/// The second statement is issued **only when a bound is missing**, which is
/// why this is a function and not two lines inside the aggregate: the dated
/// search — a preset, a heatmap, a tapped bucket — is the common one, and it
/// should not pay to be told what it already said.
async fn axis_over<C: ConnectionTrait>(
    db: &C,
    filters: &Filters,
    grain: Grain,
    now_ms: i64,
) -> Result<Axis, DbErr> {
    let search = &filters.search;
    if let (Some(after), Some(before)) = (search.after_ms, search.before_ms) {
        return Ok(Axis::over(after, before, grain));
    }

    let extent = CallQuery::new()
        .filtered_by(filters)
        .grouped()
        .column_as(call::Column::CallAtMs.min(), "first_ms")
        .column_as(call::Column::CallAtMs.max(), "last_ms")
        .into_model::<Bounds>()
        .one(db)
        .await?
        .and_then(|extent| Some((extent.first_ms?, extent.last_ms?)));

    let (first_ms, last_ms) = extent.unwrap_or((now_ms - EMPTY_SPAN_MS + 1, now_ms));
    Ok(Axis::over(
        search.after_ms.unwrap_or(first_ms),
        search.before_ms.unwrap_or(last_ms),
        grain,
    ))
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
    let heard = call_unit::Entity::find()
        .filter(call_unit::Column::CallId.eq(id))
        .order_by_asc(call_unit::Column::Id)
        .all(db)
        .await?;
    // Every radio on the timeline, resolved to the **Unit** that owns it, so a
    // name an Operator curated reaches the one endpoint that shows every radio
    // heard (#47) — and not only the first, which is all `StoredCall` carries.
    // Batched, so a Call with forty sources costs the same two statements as one
    // with a single source.
    let owners = crate::db::repo::units_owning(
        db,
        &heard
            .iter()
            .map(|unit| (row.system_id, unit.unit_ref))
            .collect::<Vec<_>>(),
    )
    .await?;
    let units = heard
        .into_iter()
        .map(|u| crate::call::CallUnitDetail {
            unit_label: owners
                .get(&(row.system_id, u.unit_ref))
                .and_then(|owner| owner.label.clone()),
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

// ---------------------------------------------------------------------------
// What an export is about to be (#65, spec US 33)
// ---------------------------------------------------------------------------

/// One Call as an **export** writes it: the manifest row, the object behind it,
/// and the name it takes inside the archive (#65).
#[derive(Debug, Clone)]
pub struct Exported {
    /// The Call as a Listener sees it — the manifest's row, and the same wire
    /// shape a search page answers with, so a script reading a manifest and a
    /// script reading the API are reading one thing.
    pub call: StoredCall,
    /// What this Call is called inside the zip, and what the manifest says it
    /// is called. Empty for an **Encrypted Call**, which has no file.
    pub filename: String,
    /// Where the audio is. Empty for an **Encrypted Call**.
    pub object_key: String,
}

/// What an export is about to be, asked **before a byte of it is written**
/// (#65).
///
/// One statement, and everything a refusal or a header needs is in it: how many
/// Calls (the cap), how many bytes of audio (a ZIP's 32-bit offsets), how long
/// altogether (the stitched file's declared timeline), and when the range
/// starts (what the download is named after).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Extent {
    pub calls: u64,
    /// Stored audio, summed. Rows whose size was never written down contribute
    /// nothing, exactly as they do to the retention cap.
    pub bytes: u64,
    /// Measured length, summed — what [`crate::export::stitch`] declares.
    pub duration_ms: i64,
    /// The instant the range opens at, or `None` when it holds no Calls.
    pub first_ms: Option<i64>,
}

/// One aggregate row, as the two dialects hand it back.
#[derive(Debug, sea_orm::FromQueryResult)]
struct ExtentRow {
    calls: i64,
    bytes: Option<i64>,
    duration_ms: Option<i64>,
    first_ms: Option<i64>,
}

/// **How big this export would be**, in one statement over the same filters the
/// export itself will walk (#65).
///
/// The two `SUM`s are cast to `BIGINT` for [`crate::db::repo::total_audio_bytes`]'s
/// reason: Postgres widens `SUM(bigint)` to `numeric` where SQLite keeps it an
/// integer, and the cast is what makes one query decode on both dialects.
pub async fn extent<C: ConnectionTrait>(db: &C, search: &CallSearch) -> Result<Extent, DbErr> {
    use sea_orm::sea_query::Alias;

    let filters = Filters::resolve(db, search).await?;
    let row = CallQuery::new()
        .filtered_by(&filters)
        .rows()
        .select_only()
        .column_as(call::Column::Id.count(), "calls")
        .column_as(
            call::Column::AudioSize.sum().cast_as(Alias::new("BIGINT")),
            "bytes",
        )
        .column_as(
            call::Column::DurationMs.sum().cast_as(Alias::new("BIGINT")),
            "duration_ms",
        )
        .column_as(call::Column::CallAtMs.min(), "first_ms")
        .into_model::<ExtentRow>()
        .one(db)
        .await?
        .unwrap_or(ExtentRow {
            calls: 0,
            bytes: None,
            duration_ms: None,
            first_ms: None,
        });

    Ok(Extent {
        calls: row.calls.max(0) as u64,
        bytes: row.bytes.unwrap_or(0).max(0) as u64,
        duration_ms: row.duration_ms.unwrap_or(0).max(0),
        // An aggregate over no rows answers `NULL` here, which is exactly the
        // reading: a range with no Calls in it starts nowhere.
        first_ms: row.first_ms,
    })
}

/// **One page of an export**, in the order it will be written (#65).
///
/// [`page`]'s reads plus the two columns a file needs and a view does not — the
/// object to fetch, and what to call it. It lives here rather than in
/// [`crate::export`] because the Archive is read by one module (#98), and it is
/// batched for [`stored_calls`]'s reason: an export of a county's night is the
/// last place to issue a statement per Call.
///
/// **The name a Call takes is a function of that Call alone** — never of its
/// position in the answer, and never of anything discovered while writing. That
/// is what lets the zip's manifest be written *first*, from a pass that reads no
/// audio at all, and still name every file correctly: the two passes are two
/// reads of a live Archive, and a name derived from a row's *position* would
/// silently slide by one for every Call retention pruned between them — leaving
/// a manifest whose `file` named a real file belonging to a different Call.
pub async fn exportable<C: ConnectionTrait>(
    db: &C,
    search: &CallSearch,
) -> Result<Vec<Exported>, DbErr> {
    let filters = Filters::resolve(db, search).await?;
    let rows = search_rows(db, &filters).await?;
    let views = stored_calls(db, &rows).await?;

    Ok(rows
        .iter()
        .zip(views)
        .map(|(row, call)| Exported {
            filename: match row.object_key.is_empty() {
                true => String::new(),
                false => export_filename(&call, row.audio_name.as_deref()),
            },
            object_key: row.object_key.clone(),
            call,
        })
        .collect())
}

/// What a Call is called inside an export's zip: **when it was**, then the name
/// a single download would have taken, then its Id.
///
/// Three jobs, and only the first is obvious. The stamp leads so that a folder
/// of extracted files sorts into the order the incident happened, whatever a
/// filesystem thinks of the labels — fixed width, so it sorts as text. It is
/// **UTC**, because the Instance's timezone is not the recipient's and a
/// filename carries no zone to say which was meant. And the Id ends it, so two
/// Calls a recorder stamped at the same millisecond on the same channel are two
/// files rather than one.
pub(crate) fn export_filename(call: &StoredCall, audio_name: Option<&str>) -> String {
    let at_ms = call.timestamp.unwrap_or_default();
    let stamp = time::OffsetDateTime::from_unix_timestamp(at_ms.div_euclid(1_000))
        .ok()
        .and_then(|at| {
            at.format(&time::macros::format_description!(
                "[year][month][day]-[hour][minute][second]"
            ))
            .ok()
        })
        .unwrap_or_else(|| "00000000-000000".to_string());
    let named = download_filename(call, audio_name);
    let (stem, extension) = named.rsplit_once('.').unwrap_or((named.as_str(), "bin"));

    format!("{stamp}-{stem}-{}.{extension}", call.id)
}

/// Which **Tone profiles** each of these Calls paged (#55), by Call id.
///
/// **Two properties, and both are load-bearing.** It is *batched*, so a page of
/// five hundred costs one statement rather than five hundred (#86); and it is
/// **skipped entirely** when nothing in the page carries the mark, so the
/// overwhelmingly common search page — and every live frame of an ordinary Call
/// — costs no statement at all to be told there is nothing there. The Call row
/// already knows, which is what `calls.tone` is for.
async fn tone_pages<C: ConnectionTrait>(
    db: &C,
    calls: &[call::Model],
) -> Result<HashMap<CallId, Vec<crate::call::TonePage>>, DbErr> {
    let marked: Vec<CallId> = calls
        .iter()
        .filter(|call| call.tone_matched())
        .map(|call| call.id)
        .collect();
    if marked.is_empty() {
        return Ok(HashMap::new());
    }
    let mut paged: HashMap<CallId, Vec<crate::call::TonePage>> = HashMap::new();
    for row in crate::db::repo::tone_matches_for(db, &marked).await? {
        paged
            .entry(row.call_id)
            .or_default()
            .push(crate::call::TonePage::from_row(&row));
    }
    Ok(paged)
}

/// One Call as a **Webhook** is about to be told about it (#54).
///
/// Deliberately the [`StoredCall`] a **Listener** would see rather than
/// [`forwardable`]'s recorder-facing view, and the difference is the whole
/// distinction between the two sinks: a peer stores the Call as if it were the
/// recorder, so it needs the recorder's own words; a webhook is read by a person
/// in a chat room or by a script, so it gets what the app shows — curated
/// labels, the resolved **Unit**, the audio URL.
///
/// Which means this is [`stored_calls`] over one row, plus the marks, and
/// nothing else. It lives here because the Archive is read by one module (#98).
pub async fn deliverable<C: ConnectionTrait>(
    db: &C,
    id: CallId,
) -> Result<Option<crate::webhook::sender::Deliverable>, DbErr> {
    let Some(row) = call::Entity::find_by_id(id).one(db).await? else {
        return Ok(None);
    };
    // **The marks are read off the row now, not remembered from when it was
    // queued.** A Call that has been replaced by a better copy (#46) since is
    // sent as it now stands, which is the same rule the forwarding path follows
    // and the reason neither queue stores a rendered payload.
    let marks = crate::webhook::Marks::on_call(row.emergency, row.tone_matched());
    // Which stations were paged rides on the Call itself, so there is nothing
    // extra to read and nothing to keep in step: what the webhook is told is
    // exactly what a Listener would be shown.
    Ok(one_view(db, &row)
        .await?
        .map(|call| crate::webhook::sender::Deliverable { call, marks }))
}

/// One Call as a **Downstream** peer is about to be told about it (#52).
///
/// A third single-Call read rather than a reuse of [`call_detail`], and the
/// difference is the point: a detail view is what a *Listener* is shown — the
/// first radio resolved to its curated **Unit**, one Group, no filename — where
/// a forward has to carry **what the Recorder said**, because the peer is going
/// to store it as if it were the recorder. So this reads the Talkgroup's whole
/// Group list, every radio with the alias that arrived on the Call rather than
/// the one an Operator has since written down, and the audio's own name.
///
/// It lives here because the Archive is read by one module (#98), and it is
/// eight statements for one Call — deliberately unbatched, because it runs on
/// the sender's Worker where #86's N+1 argument does not apply: there is no page,
/// only ever one Call at a time, and the object read behind it costs more than
/// all eight.
pub async fn forwardable<C: ConnectionTrait>(
    db: &C,
    id: CallId,
) -> Result<Option<crate::downstream::dialect::Forwardable>, DbErr> {
    use crate::downstream::dialect::{ForwardFrequency, ForwardUnit, Forwardable, seconds};

    let Some(row) = call::Entity::find_by_id(id).one(db).await? else {
        return Ok(None);
    };
    // A Call whose System or Talkgroup has been deleted underneath it cannot be
    // described in a dialect whose two mandatory fields are their Refs. Treated
    // as gone rather than forwarded with a zero, which a peer would happily
    // auto-populate into a System nobody has.
    let (Some(system), Some(talkgroup)) = (
        system::Entity::find_by_id(row.system_id).one(db).await?,
        talkgroup::Entity::find_by_id(row.talkgroup_id)
            .one(db)
            .await?,
    ) else {
        return Ok(None);
    };

    let tag = match talkgroup.tag_id {
        Some(tag_id) => tag::Entity::find_by_id(tag_id).one(db).await?,
        None => None,
    };
    let talkgroup_groups = talkgroup_group::Entity::find()
        .select_only()
        .column(group::Column::Name)
        .join(JoinType::InnerJoin, talkgroup_group::Relation::Group.def())
        .filter(talkgroup_group::Column::TalkgroupId.eq(talkgroup.id))
        .order_by_asc(group::Column::Name)
        .into_tuple::<String>()
        .all(db)
        .await?;
    let site = match row.site_id {
        Some(site_id) => site::Entity::find_by_id(site_id).one(db).await?,
        None => None,
    };
    let units = call_unit::Entity::find()
        .filter(call_unit::Column::CallId.eq(id))
        .order_by_asc(call_unit::Column::Id)
        .all(db)
        .await?
        .into_iter()
        .map(|unit| ForwardUnit {
            id: unit.unit_ref,
            // The alias this Call arrived with — the configured one, else the
            // **OTA alias** — by `call::unit_name`, the one precedence (#47).
            // Deliberately not the owning Unit's curated name: a peer running
            // its own curation would have that overwritten by ours, and
            // **Curation always wins over discovery** on the instance that did
            // it, not on somebody else's.
            label: crate::call::unit_name(unit.label.as_deref(), unit.tag_ota.as_deref())
                .map(str::to_string),
            offset: seconds(unit.offset_ms.unwrap_or_default()),
        })
        .collect();
    let frequencies = call_frequency::Entity::find()
        .filter(call_frequency::Column::CallId.eq(id))
        .order_by_asc(call_frequency::Column::Id)
        .all(db)
        .await?
        .into_iter()
        .map(|freq| ForwardFrequency {
            freq: freq.freq,
            pos: seconds(freq.pos_ms.unwrap_or_default()),
            len: freq.len_ms.map(seconds),
            dbm: freq.dbm,
            error_count: freq.error_count,
            spike_count: freq.spike_count,
        })
        .collect();
    let patches = call_patch::Entity::find()
        .select_only()
        .column(call_patch::Column::TalkgroupRef)
        .filter(call_patch::Column::CallId.eq(id))
        .order_by_asc(call_patch::Column::Id)
        .into_tuple::<i64>()
        .all(db)
        .await?;

    Ok(Some(Forwardable {
        system_ref: system.r#ref,
        system_label: system.label,
        // The **canonical** Ref, not `calls.talkgroup_ref` (#45): a Call that
        // arrived on a member Ref is one this Instance has decided belongs to
        // this channel, and forwarding the number it arrived under would ask the
        // peer to re-derive a merge it knows nothing about.
        talkgroup_ref: talkgroup.r#ref,
        talkgroup_label: talkgroup.label,
        talkgroup_name: talkgroup.name,
        talkgroup_tag: tag.map(|tag| tag.name),
        talkgroup_groups,
        call_at_ms: row.call_at_ms,
        frequency: row.frequency,
        site_ref: site.map(|site| site.r#ref),
        audio_name: row.audio_name,
        audio_mime: row.audio_mime,
        patches,
        units,
        frequencies,
        object_key: row.object_key,
    }))
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

/// A **Site** as a Call is shown under it — the Ref a recorder would recognise,
/// and the name a Listener reads.
///
/// A named pair rather than a tuple because both halves are optional-ish and
/// only one of them is a number: read positionally, a swap is silent and every
/// test still passes.
struct Tower {
    site_ref: i64,
    label: Option<String>,
}

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

    // Skipped entirely unless something in this page carries the mark, which is
    // what keeps an ordinary page — and every ordinary live frame — at exactly
    // the statement count it had before #55.
    let mut paged = tone_pages(db, calls).await?;

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
    // like every other id a client sees; the column is an internal Id. Since
    // #48 it carries the tower's *name* too, which is the only thing a
    // Listener can read — a Ref alone distinguishes towers without describing
    // one, and on an SDRTrunk Call the Ref is this Instance's own numbering.
    let sites: HashMap<i64, Tower> = site::Entity::find()
        .filter(site::Column::Id.is_in(distinct(calls.iter().filter_map(|c| c.site_id))))
        .all(db)
        .await?
        .into_iter()
        .map(|s| {
            (
                s.id,
                Tower {
                    site_ref: s.r#ref,
                    label: s.label,
                },
            )
        })
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

    // The radio each Call is shown under (#47, spec US 42): the first one heard,
    // in the order the recorder listed them — which is the order the ids were
    // inserted in, and the order `call_detail` reads its timeline out in.
    let mut heard_first: HashMap<CallId, call_unit::Model> = HashMap::new();
    for unit in call_unit::Entity::find()
        .filter(call_unit::Column::CallId.is_in(calls.iter().map(|c| c.id)))
        .order_by_asc(call_unit::Column::Id)
        .all(db)
        .await?
    {
        heard_first.entry(unit.call_id).or_insert(unit);
    }
    // ...resolved to the **Unit** that owns that radio id, so a Call keyed by one
    // portable of an apparatus is shown as the apparatus (#45's member Refs and
    // Ranges). Two statements for the page, none at all when nobody was heard.
    let owners = crate::db::repo::units_owning(
        db,
        &calls
            .iter()
            .filter_map(|call| Some((call.system_id, heard_first.get(&call.id)?.unit_ref)))
            .collect::<Vec<_>>(),
    )
    .await?;

    Ok(calls
        .iter()
        .map(|call| {
            let system = systems.get(&call.system_id);
            let talkgroup = talkgroups.get(&call.talkgroup_id);
            let heard = heard_first.get(&call.id);
            let owner = heard.and_then(|unit| owners.get(&(call.system_id, unit.unit_ref)));
            StoredCall {
                id: call.id,
                // A Call always has a System and a Talkgroup (`RESTRICT` foreign
                // keys), so these fallbacks are belt-and-braces, not a real case.
                system_ref: system.map_or(0, |s| s.r#ref),
                system_label: system.and_then(|s| s.label.clone()),
                talkgroup_ref: talkgroup.map_or(0, |t| t.r#ref),
                // `NULL` on the channel inherits the System, which is what an
                // auto-populated Ref carries (#68) — the same coalesce the
                // Archive's own `unrestricted` predicate makes in SQL, so a
                // live frame and a search page cannot disagree about which
                // Calls are gated. A Call with neither row is the belt-and-
                // braces case below and reads as open, which is what an
                // Instance that gates nothing wants.
                restricted: talkgroup
                    .and_then(|t| t.restricted)
                    .unwrap_or_else(|| system.is_some_and(|s| s.restricted)),
                talkgroup_label: talkgroup.and_then(|t| t.label.clone()),
                talkgroup_group: group_of.get(&call.talkgroup_id).cloned(),
                talkgroup_tag: talkgroup
                    .and_then(|t| t.tag_id)
                    .and_then(|tag_id| tags.get(&tag_id).cloned()),
                led: talkgroup.and_then(|t| t.led.clone()),
                patches: patches_of.remove(&call.id).unwrap_or_default(),
                frequency: call.frequency,
                // The **canonical** Ref where one is owned, so a fleet's
                // portable and its mobile read as one apparatus; the arriving
                // id where no Unit claims it, which is every uncurated archive.
                unit_ref: owner
                    .map(|unit| unit.r#ref)
                    .or_else(|| heard.map(|unit| unit.unit_ref)),
                unit_label: owner.and_then(|unit| unit.label.clone()).or_else(|| {
                    heard
                        .and_then(|unit| {
                            crate::call::unit_name(unit.label.as_deref(), unit.tag_ota.as_deref())
                        })
                        .map(str::to_owned)
                }),
                timestamp: Some(call.call_at_ms),
                audio_mime: call.audio_mime.clone(),
                duration_ms: call.duration_ms,
                emergency: call.emergency,
                encrypted: call.encrypted,
                // The mark is read off the Call row, so an unmarked Call costs
                // this denormalizer nothing at all; the stations are the child
                // rows, read once for the whole page and only when one of them
                // says there is something to read.
                tone: call.tone_matched(),
                tones: paged.remove(&call.id).unwrap_or_default(),
                // Off the Call row too, and unpacked rather than joined — the
                // spans are a column precisely so this costs no statement
                // (#59). Most Calls carry `NULL` here and serialize no key.
                quiet: call
                    .quiet
                    .as_deref()
                    .map(crate::quiet::unpack)
                    .unwrap_or_default()
                    .into_iter()
                    .map(<[i64; 2]>::from)
                    .collect(),
                // Off the Call row, like the marks above it: a Star is one
                // column, which is what keeps it out of `delete_calls`'
                // reckoning and off this page's statement count (#66).
                starred: call.starred_at_ms.is_some(),
                site_ref: call
                    .site_id
                    .and_then(|id| sites.get(&id))
                    .map(|tower| tower.site_ref),
                site_label: call
                    .site_id
                    .and_then(|id| sites.get(&id))
                    .and_then(|tower| tower.label.clone()),
                object_key: call.object_key.clone(),
                // An empty key means no object was ever written — an encrypted
                // Call (#42). Offering a URL for it would be offering a 404.
                audio_url: (!call.object_key.is_empty())
                    .then(|| format!("/api/call/{}/audio", call.id)),
            }
        })
        .collect())
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
async fn facet_rows<C: ConnectionTrait>(db: &C, filters: &Filters) -> Result<Vec<FacetRow>, DbErr> {
    CallQuery::new()
        .join_system()
        .join_talkgroup()
        .join_tag()
        .filtered_by(filters)
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
async fn group_facets<C: ConnectionTrait>(db: &C, filters: &Filters) -> Result<Vec<String>, DbErr> {
    CallQuery::new()
        .join_group()
        .filtered_by(filters)
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
    filters: &Filters,
) -> Result<(Option<i64>, Option<i64>), DbErr> {
    // MIN/MAX stay `BIGINT` on both dialects (unlike SUM, which Postgres widens
    // to `numeric` — see `repo::total_audio_bytes`), so one decode works for
    // both.
    Ok(filtered(filters)
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
    let filters = Filters::resolve(db, search).await?;

    // A facet row is one (System, Talkgroup, Tag) combination, so each dimension
    // is that row set collapsed onto its own key. The rows arrive ordered by
    // (System Ref, Talkgroup Ref), and `dedup_by_key` preserves that order.
    let systems = dedup_by_key(
        facet_rows(db, &filters.without(|s| s.system_ref = None)).await?,
        |row| row.system_ref,
        |row| SystemOption {
            r#ref: row.system_ref,
            label: row.system_label,
        },
    );

    let talkgroups = dedup_by_key(
        facet_rows(db, &filters.without(|s| s.talkgroup_ref = None)).await?,
        |row| (row.system_ref, row.talkgroup_ref),
        |row| TalkgroupOption {
            system_ref: row.system_ref,
            r#ref: row.talkgroup_ref,
            label: row.talkgroup_label,
            tag: row.tag_name,
        },
    );

    let mut tags: Vec<String> = facet_rows(db, &filters.without(|s| s.tag_name = None))
        .await?
        .into_iter()
        .filter_map(|row| row.tag_name)
        .collect();
    tags.sort();
    tags.dedup();

    let groups = group_facets(db, &filters.without(|s| s.group_name = None)).await?;

    let (date_start_ms, date_stop_ms) = call_time_bounds(
        db,
        &filters.without(|s| {
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

// ---------------------------------------------------------------------------
// One radio's history
// ---------------------------------------------------------------------------

/// One Talkgroup a radio has been heard on, straight out of the aggregate.
#[derive(Debug, sea_orm::FromQueryResult)]
struct UnitTalkgroupRow {
    talkgroup_ref: i64,
    talkgroup_label: Option<String>,
    calls: i64,
    first_heard_ms: i64,
    last_heard_ms: i64,
}

/// **What one radio has been up to** (#47, spec US 44) — the read behind
/// `GET /api/unit/{systemRef}/{ref}`.
///
/// `None` when the System is unknown, or when nothing at all is known about the
/// radio: no **Unit** row and no Call it was ever heard on. A curated Unit that
/// has never keyed *is* found, with an empty history — an Operator who wrote an
/// apparatus down should be able to open it and see that it has been quiet,
/// which is a different fact from a mistyped Ref.
///
/// Four statements: the System, the Unit, the Refs the apparatus answers to
/// ([`crate::db::repo::unit_scope`], which is two or three of its own), and one
/// aggregate over the Archive. The last is `GROUP BY` rather than a page walked
/// in Rust because a busy radio has thousands of Calls and this view is a
/// *summary* — the Calls themselves are an ordinary `?unit=` search, which pages.
pub async fn unit_history<C: ConnectionTrait>(
    db: &C,
    system_ref: i64,
    unit_ref: i64,
    scope: &AccessScope,
) -> Result<Option<crate::call::UnitHistory>, DbErr> {
    use crate::call::{RefSpan, UnitHistory, UnitTalkgroup};

    let Some(system) = system::Entity::find()
        .filter(system::Column::Ref.eq(system_ref))
        .one(db)
        .await?
    else {
        return Ok(None);
    };
    let unit = crate::db::repo::resolve_unit(db, system.id, unit_ref).await?;
    let heard = crate::db::repo::unit_scope(db, unit_ref).await?;

    let mut query = CallQuery::new()
        .join_talkgroup()
        .and_where(call::Column::SystemId.eq(system.id))
        .and_where(heard_by(&heard));
    // The summary counts what this Listener may hear and nothing else (#68).
    // Without it a gated channel is absent from `?unit=`'s Calls and present in
    // the tally above them — a number that says a radio was somewhere its
    // Calls do not appear, which is worse than either answer alone.
    if let Some(permitted) = gate(scope) {
        query = query.join_system().and_where(permitted);
    }
    let mut rows = query
        .grouped()
        .column_as(talkgroup::Column::Ref, "talkgroup_ref")
        .column_as(talkgroup::Column::Label, "talkgroup_label")
        .column_as(call::Column::Id.count(), "calls")
        .column_as(call::Column::CallAtMs.min(), "first_heard_ms")
        .column_as(call::Column::CallAtMs.max(), "last_heard_ms")
        .group_by(talkgroup::Column::Ref)
        .group_by(talkgroup::Column::Label)
        .into_model::<UnitTalkgroupRow>()
        .all(db)
        .await?;
    // Busiest first, and the Ref breaking ties so a page does not reshuffle
    // between two equally busy channels. Ordered here rather than in SQL: the
    // set is one row per Talkgroup a single radio has touched, which is a
    // handful, and `ORDER BY COUNT(*)` is one more thing to keep dialect-safe.
    rows.sort_by_key(|row| (std::cmp::Reverse(row.calls), row.talkgroup_ref));

    if unit.is_none() && rows.is_empty() {
        return Ok(None);
    }

    let owned = match &unit {
        Some(unit) => crate::db::repo::member_spans(db, unit.id).await?,
        None => Vec::new(),
    };

    Ok(Some(UnitHistory {
        system_ref: system.r#ref,
        system_label: system.label,
        // The canonical Ref where a Unit owns this radio, so the view a Listener
        // lands on names the apparatus rather than the portable they tapped.
        r#ref: unit.as_ref().map_or(unit_ref, |unit| unit.r#ref),
        label: unit.and_then(|unit| unit.label),
        member_refs: owned
            .into_iter()
            .map(|span| RefSpan {
                from: span.from(),
                to: span.to(),
            })
            .collect(),
        call_count: rows.iter().map(|row| row.calls as u64).sum(),
        first_heard_ms: rows.iter().map(|row| row.first_heard_ms).min(),
        last_heard_ms: rows.iter().map(|row| row.last_heard_ms).max(),
        talkgroups: rows
            .into_iter()
            .map(|row| UnitTalkgroup {
                r#ref: row.talkgroup_ref,
                label: row.talkgroup_label,
                calls: row.calls as u64,
                last_heard_ms: row.last_heard_ms,
            })
            .collect(),
    }))
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
///
/// **The [`Viewer`] is a parameter rather than something a handler remembers to
/// set afterwards** (#68). A `CallSearch` built from a query string is by
/// definition one somebody asked for over HTTP, and what such a search may
/// *reach* is not theirs to say — so there is no way to parse one without
/// saying whose it is. The worker-facing searches keep [`CallSearch::default`],
/// which reaches every Call there is, because none of them is a Listener.
pub(crate) fn parse_search(
    params: &HashMap<String, String>,
    viewer: &Viewer,
) -> Filtered<CallSearch> {
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

    // Named rather than silently ignored, the way `sort` is: a client asking
    // for a mark this release has never heard of is a client that will render an
    // unfiltered page believing it filtered one, and a Listener reading it has
    // no way to tell.
    let mark = match params.raw("mark") {
        None => None,
        Some(raw) => Some(crate::webhook::Mark::from_slug(raw).ok_or_else(|| {
            bad(format!(
                "mark must be one of {} (got {raw:?})",
                crate::webhook::MARKS
                    .map(crate::webhook::Mark::slug)
                    .join(", ")
            ))
        })?),
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
        unit_ref: params.number("unit")?,
        mark,
        // All or nothing, the way the client's own reader takes it: a link is
        // one statement made by somebody else, and applying half of it would
        // answer with a scanner nobody asked for.
        selection: params
            .raw("sel")
            .map(|raw| {
                crate::selection::Selection::decode(raw)
                    .ok_or_else(|| bad("sel must be a selection, as the share link spells one"))
            })
            .transpose()?,
        // Absent and `false` are one answer (#66): a Listener who has never
        // touched the control and one who has cleared it are asking the same
        // question, and a checkbox spells the second.
        starred: params.flag("starred")?.unwrap_or(false),
        // ...nor this one, and it is not even the caller's to choose: it is
        // whatever the grant this request arrived with opens (#68).
        scope: viewer.scope.clone(),
        // Nobody types this one: it is the **stitched export**'s (#65), set by
        // the one caller that needs it.
        with_audio: false,
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
    viewer: Viewer,
) -> Result<SearchPage, Failure> {
    let search = parse_search(&params, &viewer)?;

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
    viewer: Viewer,
) -> Result<FilterOptions, Failure> {
    let search = parse_search(&params, &viewer)?;

    options(&state.db, &search)
        .await
        .map_err(Stage::LoadFilterOptions.failed())
}

/// `GET /api/calls/quiet?ids=1,2,3` — where those Calls are quiet (#59, spec
/// US 23).
///
/// **This endpoint exists because a live frame cannot carry the answer.** The
/// frame is published at ingest, before anything has looked at the audio, and
/// nothing republishes one (#46) — so a Call sitting in a Listener's queue has
/// no spans on it however long ago the scanner finished with it. Catch-up asks
/// about the window of the queue it is about to play. The Archive needs none of
/// this: a Call read back from a search page carries its spans on itself.
///
/// One statement whatever the window's size, and Calls with nothing to trim are
/// absent rather than empty — see [`crate::db::repo::quiet_spans_for`].
pub async fn quiet(
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
    viewer: Viewer,
) -> Result<crate::quiet::QuietWindow, Failure> {
    let ids = parse_ids(params.get("ids").map(String::as_str))?;

    crate::db::repo::quiet_spans_for(&state.db, &ids, &viewer.scope)
        .await
        .map(crate::quiet::QuietWindow::of)
        .map_err(Stage::LoadQuietSpans.failed())
}

/// The `ids` of a quiet-span request, or the named rejection every other bad
/// parameter here gets.
///
/// **An absent or empty list is refused rather than answered `{}`.** A caller
/// with nothing to ask about does not send this request, so the only way to get
/// here empty is a bug — and a mistyped parameter name that answered "no Call
/// has any gaps" would be a Catch-up that silently stopped trimming and looked
/// exactly like an Archive full of continuous speech.
fn parse_ids(raw: Option<&str>) -> Result<Vec<CallId>, Reason> {
    let Some(raw) = raw.map(str::trim).filter(|raw| !raw.is_empty()) else {
        return Err(bad("ids must name at least one Call, e.g. ids=1,2,3"));
    };
    let ids = raw
        .split(',')
        .map(|id| {
            id.trim()
                .parse::<CallId>()
                .map_err(|_| bad(format!("ids must be Call ids; {id:?} is not one")))
        })
        .collect::<Result<Vec<_>, _>>()?;
    if ids.len() > crate::quiet::MAX_WINDOW {
        return Err(bad(format!(
            "ids may name at most {} Calls",
            crate::quiet::MAX_WINDOW
        )));
    }
    Ok(ids)
}

/// `GET /api/calls/activity` — how busy the Archive was under this search
/// (#62, spec US 34–35).
///
/// Every filter `GET /api/calls` takes, read by the same parser, so the ribbon
/// a Listener sees above their results is a picture of *those* results and not
/// of a search that merely resembles them. `sort`, `limit` and `offset` are
/// read and ignored: a window into a page says nothing about how many Calls
/// there were, and refusing them would mean the client stripping three keys off
/// the object it already has in hand.
///
/// The grain rides beside them — `bucketMs` for a caller that needs an exact
/// width (the hour-by-day heatmap), `buckets` for one that only knows how wide
/// its chart is (the ribbon).
pub async fn activity(
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
    viewer: Viewer,
) -> Result<Series, Failure> {
    let search = parse_search(&params, &viewer)?;
    let grain = crate::activity::parse_grain(&params)?;

    call_activity(&state.db, &search, grain, state.clock.now_ms())
        .await
        .map_err(Stage::LoadActivity.failed())
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
    viewer: Viewer,
) -> Result<CallDetail, Failure> {
    call_detail(&state.db, id)
        .await
        .map_err(Stage::LoadCallDetail.failed())?
        .filter(|detail| reachable(&viewer.scope, &detail.call))
        .ok_or(Reason::CallNotFound.into())
}

/// Whether a viewer may have this Call at all (#68).
///
/// **The single-Call form of [`gate`]**, and the answer is deliberately the same
/// one a Call that is not there gets: [`Reason::CallNotFound`]. An Instance that
/// told a stranger "that Call exists and you may not have it" would publish, one
/// id at a time, exactly what an Operator gated the channel to keep quiet — and
/// a `403` on a hand-typed id is a working oracle. The log tells the two apart,
/// which is where the Operator looks.
pub(crate) fn reachable(scope: &AccessScope, call: &StoredCall) -> bool {
    scope.permits(call.system_ref, call.talkgroup_ref, call.restricted)
}

/// `GET /api/unit/{system}/{ref}` — one radio's history (#47, spec US 44).
///
/// A Ref is unique only within its System, so both ride the path. What this
/// carries is the summary a search cannot give without reading the whole
/// archive; the Calls themselves are `GET /api/calls?system=…&unit=…`, which
/// pages and plays like any other search.
pub async fn unit(
    State(state): State<AppState>,
    Path((system_ref, unit_ref)): Path<(i64, i64)>,
    viewer: Viewer,
) -> Result<crate::call::UnitHistory, Failure> {
    unit_history(&state.db, system_ref, unit_ref, &viewer.scope)
        .await
        .map_err(Stage::LoadUnitHistory.failed())?
        .ok_or(Reason::UnitNotFound.into())
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
    viewer: Viewer,
) -> Result<Attachment, Failure> {
    if !crate::access::reaches_call(&state.db, &viewer.scope, id)
        .await
        .map_err(Stage::Access.failed())?
    {
        return Err(Reason::CallNotFound.into());
    }
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
///
/// `pub(crate)` for one caller outside: an **Event**'s frozen copy is named after
/// what is in it (#67), which is the same question asked of the same two facts.
/// It had a second copy of this match for about an hour and the copy had already
/// drifted — `audio/x-flac` was in one and not the other — which is exactly what
/// "written once" exists to stop.
pub(crate) fn download_extension(audio_name: Option<&str>, mime: Option<&str>) -> String {
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
    slug_named(raw, "call")
}

/// ...with the caller's own word for a label that survives none of it.
///
/// `pub(crate)` for one caller outside: an **Event**'s export is named after the
/// Event (#67), and that name is an Operator's free text heading for the same
/// header this already guards. It needs its own fallback because this one's is
/// `call` — which is the right word for the file a download names and an
/// actively wrong one for an incident, and a file called `radio-scout-call.zip`
/// holding four hundred of them is a small lie told to whoever was sent it.
pub(crate) fn slug_named(raw: &str, fallback: &str) -> String {
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
        fallback.to_string()
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

        let filters = Filters::resolve(&db, &search).await.expect("resolve");
        let rows = CallQuery::new()
            .join_system()
            .join_talkgroup()
            .join_tag()
            .join_group()
            .filtered_by(&filters)
            .rows()
            .all(&db)
            .await;
        assert!(rows.is_ok(), "a Call query joined twice: {rows:?}");

        // ...and through the facet query, which is where the pre-join is real.
        assert!(
            group_facets(&db, &filters).await.is_ok(),
            "the Group facet query, with a Group filter on it"
        );
        assert!(facet_rows(&db, &filters).await.is_ok());
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
        let search = parse_search(&query(&[]), &Viewer::unrestricted()).unwrap();
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
        let search = parse_search(
            &query(&[
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
            ]),
            &Viewer::unrestricted(),
        )
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
        let search = parse_search(
            &query(&[
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
            ]),
            &Viewer::unrestricted(),
        )
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
            parse_search(&query(&[("sort", raw)]), &Viewer::unrestricted())
                .unwrap()
                .sort,
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
            parse_search(&query(&[("limit", raw)]), &Viewer::unrestricted())
                .unwrap()
                .limit,
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
        let result = parse_search(&query(&[(key, value)]), &Viewer::unrestricted());
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
            restricted: false,
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
            unit_ref: None,
            unit_label: None,
            timestamp: Some(1_669_740_338_000),
            audio_mime: Some("audio/mp4".into()),
            duration_ms: None,
            emergency: false,
            encrypted: false,
            tone: false,
            tones: Vec::new(),
            quiet: Vec::new(),
            starred: false,
            site_ref: None,
            site_label: None,
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
