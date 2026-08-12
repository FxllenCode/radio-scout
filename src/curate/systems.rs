//! **Systems** — the radio networks an Instance receives Calls from (#49).
//!
//! A System is the one entity everything else hangs off: Talkgroups and Units
//! are unique only *within* one, and an API key is scoped to one. So it is also
//! the entity whose delete is worth being careful about, and the only place in
//! Radio-Scout where a single click could take a night of Archive with it —
//! which is why it cannot, unless it is asked to ([`super::Force`]).
//!
//! # The blacklist, in two shapes on purpose
//!
//! `systems.blacklist` is rdio's own storage: a comma-separated list of
//! Talkgroup Refs, free text, junk silently ignored (`repo::is_blacklisted`).
//! Two affordances read and write it, because Operators reach for it two ways:
//!
//! - **the list on this form**, for the Ref of a channel that does not exist yet
//!   — a patch-minted TGID an Operator wants refused *before* its first sighting,
//!   which nothing else can express; and
//! - **a toggle on the Talkgroup row** ([`super::talkgroups`]), for the ordinary
//!   case, where an Operator is thinking "stop ingesting this channel" and not
//!   "edit a comma-separated string on its parent".
//!
//! One storage, one parser, one canonical rendering — so the two cannot disagree
//! about what is blacklisted. What is deliberately *lost* on the first save is
//! rdio's junk: a `blacklist` of `"100, fire, 200"` reads back as `100,200`,
//! because the entry that never matched anything was never a policy.

use axum::extract::{Path, Query, State};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DbErr, EntityTrait, IntoActiveModel,
    PaginatorTrait, QueryFilter, QuerySelect, Set,
};
use serde::{Deserialize, Serialize};

use super::{
    CallScope, Created, Force, Listing, Rejected, Removed, What, cleared, nullable, optional_text,
};
use crate::AppState;
use crate::db::entities::{
    call, site, system, talkgroup, talkgroup_group, talkgroup_ref, unit, unit_ref,
};
use crate::db::repo;
use crate::failure::{Failure, Stage};

/// One System, as the curation screen lists it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SystemRow {
    pub id: i64,
    pub r#ref: i64,
    pub label: Option<String>,
    /// The per-System auto-populate flag (#8) — unknown Talkgroups and Units
    /// under this System are created on first sighting even when the instance
    /// toggle is off.
    pub auto_populate: bool,
    /// Talkgroup Refs never ingested for this System, canonical and sorted.
    pub blacklist: Vec<i64>,
    /// Whether Calls here are enhanced (#20). `null` inherits the instance's
    /// `[enhancement] mode` — a plain boolean has no way to say "follow the
    /// instance", which is exactly what makes this column nullable.
    pub enhancement: Option<bool>,
    pub talkgroups: u64,
    pub units: u64,
    /// Calls in the Archive under this System — what a delete would take.
    pub calls: u64,
    pub created_at_ms: i64,
}

crate::answers_json!(SystemRow);

/// What a create carries. Every field is optional: an Operator curating ahead of
/// their first Call knows the label and nothing else.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NewSystem {
    /// The radio network's own id. Absent **mints the lowest free one**, the
    /// same answer #8 gives a recorder that identified a System by name alone.
    pub r#ref: Option<i64>,
    pub label: Option<String>,
    #[serde(default)]
    pub auto_populate: bool,
    #[serde(default)]
    pub blacklist: Vec<i64>,
    pub enhancement: Option<bool>,
}

/// What an edit carries. Absent means **leave alone**; `null` on a nullable
/// field means **clear** (see [`super::nullable`]).
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SystemPatch {
    pub r#ref: Option<i64>,
    #[serde(default, deserialize_with = "nullable")]
    pub label: Option<Option<String>>,
    pub auto_populate: Option<bool>,
    pub blacklist: Option<Vec<i64>>,
    #[serde(default, deserialize_with = "nullable")]
    pub enhancement: Option<Option<bool>>,
}

/// `GET /api/admin/systems` — every System, with what hangs off it.
pub async fn list(State(state): State<AppState>) -> Result<Listing<SystemRow>, Failure> {
    read_all(&state.db)
        .await
        .map(Listing::new)
        .map_err(Stage::Curate.failed())
}

/// `POST /api/admin/systems` — one new System.
pub async fn create(
    State(state): State<AppState>,
    axum::Json(body): axum::Json<NewSystem>,
) -> Result<Created<SystemRow>, Failure> {
    let db = &state.db;
    let r#ref = match body.r#ref {
        Some(r#ref) => r#ref,
        None => repo::lowest_free_system_ref(db)
            .await
            .map_err(Stage::Curate.failed())?,
    };
    if find_by_ref(db, r#ref)
        .await
        .map_err(Stage::Curate.failed())?
        .is_some()
    {
        return Err(Rejected::RefTaken {
            what: What::System,
            taken: r#ref,
        }
        .into());
    }

    let now_ms = state.clock.now_ms();
    let row = system::ActiveModel {
        r#ref: Set(r#ref),
        label: Set(optional_text(body.label)),
        auto_populate: Set(body.auto_populate),
        blacklist: Set(blacklist_text(&body.blacklist)),
        enhancement: Set(body.enhancement),
        created_at_ms: Set(now_ms),
        ..Default::default()
    }
    .insert(db)
    .await
    .map_err(Stage::Curate.failed())?;

    Ok(Created(SystemRow {
        id: row.id,
        r#ref: row.r#ref,
        label: row.label,
        auto_populate: row.auto_populate,
        blacklist: blacklist_of(row.blacklist.as_deref()),
        enhancement: row.enhancement,
        talkgroups: 0,
        units: 0,
        calls: 0,
        created_at_ms: row.created_at_ms,
    }))
}

/// `PATCH /api/admin/systems/{id}` — change what was named, leave the rest.
pub async fn update(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    axum::Json(body): axum::Json<SystemPatch>,
) -> Result<SystemRow, Failure> {
    let db = &state.db;
    let existing = system::Entity::find_by_id(id)
        .one(db)
        .await
        .map_err(Stage::Curate.failed())?
        .ok_or(Rejected::NotFound(What::System))?;

    // A System renumbered onto another's Ref would make two rows answer to one
    // number, which `idx_systems_ref` would refuse as a 500 rather than as an
    // answer. Renumbering onto its *own* Ref is a form re-saved unchanged.
    if let Some(wanted) = body.r#ref
        && wanted != existing.r#ref
        && find_by_ref(db, wanted)
            .await
            .map_err(Stage::Curate.failed())?
            .is_some()
    {
        return Err(Rejected::RefTaken {
            what: What::System,
            taken: wanted,
        }
        .into());
    }

    let mut row = existing.into_active_model();
    if let Some(wanted) = body.r#ref {
        row.r#ref = Set(wanted);
    }
    if let Some(label) = body.label {
        row.label = Set(cleared(label));
    }
    if let Some(auto_populate) = body.auto_populate {
        row.auto_populate = Set(auto_populate);
    }
    if let Some(blacklist) = body.blacklist {
        row.blacklist = Set(blacklist_text(&blacklist));
    }
    if let Some(enhancement) = body.enhancement {
        row.enhancement = Set(enhancement);
    }
    row.update(db).await.map_err(Stage::Curate.failed())?;

    one(db, id).await.map_err(Stage::Curate.failed())
}

/// `DELETE /api/admin/systems/{id}` — the System and everything under it.
///
/// Refused while Calls remain, unless `?force=true`. **Retention owns removing
/// Calls** — the row, then the audio object, with orphan-GC behind it — so this
/// borrows that pass rather than growing a second one; what it adds is the
/// configuration underneath, which retention has no reason to touch.
pub async fn remove(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Query(force): Query<Force>,
) -> Result<Removed, Failure> {
    let db = &state.db;
    system::Entity::find_by_id(id)
        .one(db)
        .await
        .map_err(Stage::Curate.failed())?
        .ok_or(Rejected::NotFound(What::System))?;

    let calls = call::Entity::find()
        .filter(call::Column::SystemId.eq(id))
        .count(db)
        .await
        .map_err(Stage::Curate.failed())?;
    if calls > 0 && !force.force {
        return Err(Rejected::HasCalls {
            what: What::System,
            calls,
        }
        .into());
    }
    if calls > 0 {
        super::purge_calls(&state, CallScope::System(id)).await?;
    }

    let txn = db.begin().await.map_err(Stage::Curate.failed())?;
    let talkgroups: Vec<i64> = talkgroup::Entity::find()
        .filter(talkgroup::Column::SystemId.eq(id))
        .select_only()
        .column(talkgroup::Column::Id)
        .into_tuple()
        .all(&txn)
        .await
        .map_err(Stage::Curate.failed())?;
    if !talkgroups.is_empty() {
        talkgroup_group::Entity::delete_many()
            .filter(talkgroup_group::Column::TalkgroupId.is_in(talkgroups.clone()))
            .exec(&txn)
            .await
            .map_err(Stage::Curate.failed())?;
    }
    // Everything keyed on the System, member Refs and Ranges included (#45) —
    // a member Ref outliving its System is a row resolution could never resolve.
    for result in [
        talkgroup_ref::Entity::delete_many()
            .filter(talkgroup_ref::Column::SystemId.eq(id))
            .exec(&txn)
            .await,
        unit_ref::Entity::delete_many()
            .filter(unit_ref::Column::SystemId.eq(id))
            .exec(&txn)
            .await,
        talkgroup::Entity::delete_many()
            .filter(talkgroup::Column::SystemId.eq(id))
            .exec(&txn)
            .await,
        unit::Entity::delete_many()
            .filter(unit::Column::SystemId.eq(id))
            .exec(&txn)
            .await,
        site::Entity::delete_many()
            .filter(site::Column::SystemId.eq(id))
            .exec(&txn)
            .await,
        system::Entity::delete_by_id(id).exec(&txn).await,
    ] {
        result.map_err(Stage::Curate.failed())?;
    }
    txn.commit().await.map_err(Stage::Curate.failed())?;

    Ok(Removed)
}

// -- Reading ----------------------------------------------------------------

/// Every System, ordered the way a listener's panel orders them (#12) — by what
/// an Operator reads, then by Ref, and **in Rust**, because collation is where
/// the dialects disagree (ADR-0003).
pub async fn read_all<C: ConnectionTrait>(db: &C) -> Result<Vec<SystemRow>, DbErr> {
    let talkgroups = tally(db, talkgroup::Entity::find(), talkgroup::Column::SystemId).await?;
    let units = tally(db, unit::Entity::find(), unit::Column::SystemId).await?;
    let calls = tally(db, call::Entity::find(), call::Column::SystemId).await?;

    let mut rows: Vec<SystemRow> = system::Entity::find()
        .all(db)
        .await?
        .into_iter()
        .map(|row| SystemRow {
            talkgroups: talkgroups.get(&row.id).copied().unwrap_or(0),
            units: units.get(&row.id).copied().unwrap_or(0),
            calls: calls.get(&row.id).copied().unwrap_or(0),
            id: row.id,
            r#ref: row.r#ref,
            label: row.label.clone(),
            auto_populate: row.auto_populate,
            blacklist: blacklist_of(row.blacklist.as_deref()),
            enhancement: row.enhancement,
            created_at_ms: row.created_at_ms,
        })
        .collect();
    rows.sort_by_key(|row| crate::catalog::display_order(row.label.as_deref(), row.r#ref));
    Ok(rows)
}

/// One System's row, re-read after a write so the answer is the stored truth
/// rather than what the request hoped for.
async fn one<C: ConnectionTrait>(db: &C, id: i64) -> Result<SystemRow, DbErr> {
    read_all(db)
        .await?
        .into_iter()
        .find(|row| row.id == id)
        .ok_or_else(|| DbErr::RecordNotFound(format!("system {id}")))
}

/// How many rows of `find` there are per System — one grouped statement, not one
/// per System.
async fn tally<C, E>(
    db: &C,
    find: sea_orm::Select<E>,
    column: E::Column,
) -> Result<std::collections::HashMap<i64, u64>, DbErr>
where
    C: ConnectionTrait,
    E: EntityTrait,
{
    Ok(find
        .select_only()
        .column(column)
        .column_as(column.count(), "held")
        .group_by(column)
        .into_tuple::<(i64, i64)>()
        .all(db)
        .await?
        .into_iter()
        .map(|(id, held)| (id, held.max(0) as u64))
        .collect())
}

async fn find_by_ref<C: ConnectionTrait>(db: &C, r#ref: i64) -> Result<Option<i64>, DbErr> {
    Ok(system::Entity::find()
        .filter(system::Column::Ref.eq(r#ref))
        .one(db)
        .await?
        .map(|row| row.id))
}

// -- The blacklist ----------------------------------------------------------

/// The Refs a stored blacklist actually refuses — sorted, de-duplicated, and
/// with rdio's free-text junk dropped.
///
/// [`repo::is_blacklisted`] is the reader ingest uses and it ignores anything
/// non-numeric; this is the same rule, said once more where a form has to show
/// the list back. An entry that never matched anything is not shown as policy.
pub fn blacklist_of(raw: Option<&str>) -> Vec<i64> {
    let mut refs: Vec<i64> = raw
        .unwrap_or_default()
        .split(',')
        .filter_map(|entry| entry.trim().parse::<i64>().ok())
        .collect();
    refs.sort_unstable();
    refs.dedup();
    refs
}

/// ...and back to the column. `None` for an empty list rather than `""`, so
/// "nothing is blacklisted" has one spelling in the database instead of two.
pub fn blacklist_text(refs: &[i64]) -> Option<String> {
    let mut refs = refs.to_vec();
    refs.sort_unstable();
    refs.dedup();
    match refs.is_empty() {
        true => None,
        false => Some(
            refs.iter()
                .map(i64::to_string)
                .collect::<Vec<_>>()
                .join(","),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use rstest::rstest;

    /// A stored list reads back canonical: sorted, de-duplicated, junk gone.
    #[rstest]
    #[case(None, &[])]
    #[case(Some(""), &[])]
    #[case(Some("100"), &[100])]
    #[case(Some("200,100"), &[100, 200])]
    #[case(Some(" 200 , 100 "), &[100, 200])]
    #[case(Some("100,100"), &[100])]
    // rdio stores this as free text and ignores what it cannot parse; so do we,
    // and saying so is the point — an entry that refuses nothing is not policy.
    #[case(Some("100,fire,200"), &[100, 200])]
    #[case(Some(",,"), &[])]
    fn a_blacklist_reads_back_canonical(#[case] raw: Option<&str>, #[case] expected: &[i64]) {
        assert_eq!(blacklist_of(raw), expected, "{raw:?}");
    }

    /// An empty list is `NULL`, never `""` — one spelling for "nothing is
    /// blacklisted", so no query has to know both.
    #[rstest]
    #[case(&[], None)]
    #[case(&[100], Some("100"))]
    #[case(&[200, 100], Some("100,200"))]
    #[case(&[100, 100], Some("100"))]
    fn an_empty_blacklist_is_stored_as_nothing(
        #[case] refs: &[i64],
        #[case] expected: Option<&str>,
    ) {
        assert_eq!(blacklist_text(refs).as_deref(), expected);
    }

    proptest! {
        /// Whatever an Operator saves, saving it again changes nothing — and
        /// what ingest refuses is exactly what the form shows.
        #[test]
        fn a_blacklist_round_trips_and_agrees_with_ingest(refs in prop::collection::vec(-5i64..50, 0..8)) {
            let stored = blacklist_text(&refs);
            let shown = blacklist_of(stored.as_deref());

            prop_assert_eq!(blacklist_text(&shown), stored.clone());
            for candidate in -5i64..50 {
                prop_assert_eq!(
                    repo::is_blacklisted(stored.as_deref(), candidate),
                    shown.contains(&candidate),
                    "ref {}", candidate
                );
            }
        }
    }
}
