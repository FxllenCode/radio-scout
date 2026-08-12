//! **Units** — the radios heard on a System (#49, spec US 42–44).
//!
//! An Instance rosters these itself: every Call names the radios that keyed on
//! it, and #47 made either alias enough to create one, so a zero-configuration
//! install already has a Unit per radio it has ever heard. What curation adds is
//! the half a recorder can never supply — an apparatus's *name*, and a Unit that
//! exists before anything has heard it.
//!
//! Like Talkgroups these page and filter server-side: a county fleet is
//! thousands of radios, and a form that renders all of them is a form nobody can
//! use on a phone.
//!
//! **Ranges are not here.** A Unit's `1200-1299` is a member span (#45),
//! curated by CSV today (#47) and by #50's merge screen next; carrying it on
//! this row would be one query per Unit for a field this form cannot edit. A
//! Unit's own Ref is what this screen owns.

use std::collections::HashMap;

use axum::extract::{Path, Query, State};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, Condition, ConnectionTrait, DbErr, EntityTrait, IntoActiveModel,
    QueryFilter, QueryOrder, QuerySelect, Set,
};
use serde::{Deserialize, Serialize};

use super::{Created, Rejected, Removed, What, cleared, nullable, optional_text};
use crate::AppState;
use crate::db::entities::{system, unit, unit_ref};
use crate::failure::{Failure, Stage};
use crate::query::{Filtered, Page, Params};

const DEFAULT_LIMIT: u64 = 100;
const MAX_LIMIT: u64 = 500;

/// One Unit, as the curation screen lists it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UnitRow {
    pub id: i64,
    pub system_id: i64,
    pub system_ref: i64,
    pub system_label: Option<String>,
    pub r#ref: i64,
    /// The curated name. A Unit that has one keeps it — auto-populate fills
    /// blanks and never rewrites curation (#8, CONTEXT.md) — so this field is
    /// the only thing that can overwrite what a radio called itself.
    pub label: Option<String>,
    pub created_at_ms: i64,
}

crate::answers_json!(UnitRow);

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NewUnit {
    pub system_id: i64,
    pub r#ref: i64,
    pub label: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UnitPatch {
    pub r#ref: Option<i64>,
    #[serde(default, deserialize_with = "nullable")]
    pub label: Option<Option<String>>,
}

/// `GET /api/admin/units` — one filtered, paged screenful.
pub async fn list(
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Page<UnitRow>, Failure> {
    let search = parse_search(&params)?;

    read_page(&state.db, &search)
        .await
        .map_err(Stage::Curate.failed())
}

/// `POST /api/admin/units` — a radio an Operator knows about before it keys.
pub async fn create(
    State(state): State<AppState>,
    axum::Json(body): axum::Json<NewUnit>,
) -> Result<Created<UnitRow>, Failure> {
    let db = &state.db;
    system::Entity::find_by_id(body.system_id)
        .one(db)
        .await
        .map_err(Stage::Curate.failed())?
        .ok_or(Rejected::NoSuchSystem {
            system_id: body.system_id,
        })?;
    ensure_ref_free(db, body.system_id, body.r#ref, None).await?;

    let now_ms = state.clock.now_ms();
    let row = unit::ActiveModel {
        system_id: Set(body.system_id),
        r#ref: Set(body.r#ref),
        label: Set(optional_text(body.label)),
        created_at_ms: Set(now_ms),
        ..Default::default()
    }
    .insert(db)
    .await
    .map_err(Stage::Curate.failed())?;

    Ok(Created(
        one(db, row.id).await.map_err(Stage::Curate.failed())?,
    ))
}

/// `PATCH /api/admin/units/{id}` — name an apparatus, or renumber it.
pub async fn update(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    axum::Json(body): axum::Json<UnitPatch>,
) -> Result<UnitRow, Failure> {
    let db = &state.db;
    let existing = unit::Entity::find_by_id(id)
        .one(db)
        .await
        .map_err(Stage::Curate.failed())?
        .ok_or(Rejected::NotFound(What::Unit))?;
    if let Some(wanted) = body.r#ref
        && wanted != existing.r#ref
    {
        ensure_ref_free(db, existing.system_id, wanted, Some(id)).await?;
    }

    let mut row = existing.into_active_model();
    if let Some(wanted) = body.r#ref {
        row.r#ref = Set(wanted);
    }
    if let Some(label) = body.label {
        row.label = Set(cleared(label));
    }
    row.update(db).await.map_err(Stage::Curate.failed())?;

    one(db, id).await.map_err(Stage::Curate.failed())
}

/// `DELETE /api/admin/units/{id}` — the roster entry and the spans it owned.
///
/// **No `force` here, and nothing is refused.** A Call names the radios it heard
/// by *Ref* (`call_units`), never by a Unit's Id, so deleting a Unit takes no
/// Call with it — it un-names the radio, and the archive goes on showing the
/// bare number the way it did before anybody curated one (#47). That is why this
/// is the one delete on this surface with nothing to warn about.
pub async fn remove(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Removed, Failure> {
    let db = &state.db;
    unit::Entity::find_by_id(id)
        .one(db)
        .await
        .map_err(Stage::Curate.failed())?
        .ok_or(Rejected::NotFound(What::Unit))?;

    let txn = db.begin().await.map_err(Stage::Curate.failed())?;
    for result in [
        unit_ref::Entity::delete_many()
            .filter(unit_ref::Column::UnitId.eq(id))
            .exec(&txn)
            .await,
        unit::Entity::delete_by_id(id).exec(&txn).await,
    ] {
        result.map_err(Stage::Curate.failed())?;
    }
    txn.commit().await.map_err(Stage::Curate.failed())?;

    Ok(Removed)
}

// -- Reading ----------------------------------------------------------------

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UnitSearch {
    pub system_ref: Option<i64>,
    /// Free text over the label and the Ref as written.
    pub text: Option<String>,
    /// Only radios nobody has named — the working set for an Operator sitting
    /// down to name a fleet, and unreachable from a plain text search.
    pub unnamed: Option<bool>,
    pub limit: u64,
    pub offset: u64,
}

fn parse_search(params: &HashMap<String, String>) -> Filtered<UnitSearch> {
    let params = Params::new(params);
    Ok(UnitSearch {
        system_ref: params.number("system")?,
        text: params.raw("q").map(str::to_owned),
        unnamed: params.flag("unnamed")?,
        limit: params.limit(DEFAULT_LIMIT, MAX_LIMIT)?,
        offset: params.offset()?,
    })
}

/// One page of Units and the total behind it — ordered by System then Ref, in
/// SQL, for the reason the Talkgroup listing gives.
async fn read_page<C: ConnectionTrait>(
    db: &C,
    search: &UnitSearch,
) -> Result<Page<UnitRow>, DbErr> {
    let systems: HashMap<i64, system::Model> = system::Entity::find()
        .all(db)
        .await?
        .into_iter()
        .map(|row| (row.id, row))
        .collect();

    let mut query = unit::Entity::find();
    if let Some(system_ref) = search.system_ref {
        let ids: Vec<i64> = systems
            .values()
            .filter(|system| system.r#ref == system_ref)
            .map(|system| system.id)
            .collect();
        query = query.filter(unit::Column::SystemId.is_in(ids));
    }
    if let Some(text) = &search.text {
        use sea_orm::sea_query::{Expr, Func};
        let pattern = format!("%{}%", text.to_lowercase());
        let mut any = Condition::any().add(
            Expr::expr(Func::lower(Expr::col((unit::Entity, unit::Column::Label)))).like(pattern),
        );
        if let Ok(as_ref) = text.trim().parse::<i64>() {
            any = any.add(unit::Column::Ref.eq(as_ref));
        }
        query = query.filter(any);
    }
    if let Some(unnamed) = search.unnamed {
        query = query.filter(match unnamed {
            true => unit::Column::Label.is_null(),
            false => unit::Column::Label.is_not_null(),
        });
    }

    let query = query
        .order_by_asc(unit::Column::SystemId)
        .order_by_asc(unit::Column::Ref);
    let count = sea_orm::PaginatorTrait::count(query.clone(), db).await?;
    let page = query
        .offset(search.offset)
        .limit(search.limit)
        .all(db)
        .await?;

    Ok(Page::new(
        page.into_iter().map(|row| row_of(row, &systems)).collect(),
        count,
        search.limit,
        search.offset,
    ))
}

fn row_of(row: unit::Model, systems: &HashMap<i64, system::Model>) -> UnitRow {
    let system = systems.get(&row.system_id);
    UnitRow {
        system_ref: system.map(|s| s.r#ref).unwrap_or_default(),
        system_label: system.and_then(|s| s.label.clone()),
        id: row.id,
        system_id: row.system_id,
        r#ref: row.r#ref,
        label: row.label,
        created_at_ms: row.created_at_ms,
    }
}

async fn one<C: ConnectionTrait>(db: &C, id: i64) -> Result<UnitRow, DbErr> {
    let systems: HashMap<i64, system::Model> = system::Entity::find()
        .all(db)
        .await?
        .into_iter()
        .map(|row| (row.id, row))
        .collect();
    let row = unit::Entity::find_by_id(id)
        .one(db)
        .await?
        .ok_or_else(|| DbErr::RecordNotFound(format!("unit {id}")))?;

    Ok(row_of(row, &systems))
}

/// Refuse a radio id another Unit on this System already answers to — its own
/// Ref, or one inside a **Range** it owns (#45). A Ref owned twice is a Call
/// whose radio could resolve to either apparatus.
async fn ensure_ref_free<C: ConnectionTrait>(
    db: &C,
    system_id: i64,
    wanted: i64,
    except: Option<i64>,
) -> Result<(), Failure> {
    let owner = unit::Entity::find()
        .filter(unit::Column::SystemId.eq(system_id))
        .filter(unit::Column::Ref.eq(wanted))
        .one(db)
        .await
        .map_err(Stage::Curate.failed())?
        .map(|row| row.id);
    let spanned = unit_ref::Entity::find()
        .filter(unit_ref::Column::SystemId.eq(system_id))
        .filter(unit_ref::Column::RefFrom.lte(wanted))
        .filter(unit_ref::Column::RefTo.gte(wanted))
        .one(db)
        .await
        .map_err(Stage::Curate.failed())?
        .filter(|span| Some(span.unit_id) != except)
        .is_some();

    match owner.filter(|id| Some(*id) != except).is_some() || spanned {
        true => Err(Rejected::RefTaken {
            what: What::Unit,
            taken: wanted,
        }
        .into()),
        false => Ok(()),
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

    #[test]
    fn blank_filters_are_no_filter_at_all() {
        let search = parse_search(&params("system=&q=&unnamed=")).expect("parse");

        assert_eq!(
            search,
            UnitSearch {
                limit: DEFAULT_LIMIT,
                ..UnitSearch::default()
            }
        );
    }

    /// Both spellings of the unnamed filter, because an Operator naming a fleet
    /// wants one half and then the other.
    #[rstest]
    #[case("unnamed=true", Some(true))]
    #[case("unnamed=1", Some(true))]
    #[case("unnamed=false", Some(false))]
    #[case("unnamed=no", Some(false))]
    #[case("", None)]
    fn the_unnamed_filter_reads_both_ways(#[case] query: &str, #[case] expected: Option<bool>) {
        assert_eq!(
            parse_search(&params(query)).expect("parse").unnamed,
            expected
        );
    }

    #[rstest]
    #[case("system=eleven", "system")]
    #[case("unnamed=perhaps", "unnamed")]
    fn a_bad_filter_names_itself(#[case] query: &str, #[case] parameter: &str) {
        let told = parse_search(&params(query)).expect_err("refused").told();

        assert!(told.contains(parameter), "{told}");
    }
}
