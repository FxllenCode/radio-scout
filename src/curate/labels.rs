//! **Groups and Tags** — the two name-only entities (#49).
//!
//! They are written once, over a [`Table`] value, rather than twice: everything
//! an Operator can do to a Group they can do identically to a Tag, and the only
//! places the two differ are which table a statement names and how a Talkgroup
//! comes to belong to one. Spelling that twice would be two chances for the
//! trimming, the uniqueness check or the not-found arm to drift — and the pair
//! that drifted would be the pair nobody looked at.
//!
//! **What they are is not the same, and the deletes prove it.** A Group is a
//! cross-system *category* a Talkgroup belongs to (many-to-many), so deleting one
//! removes the links and leaves the channels; a Tag is the *single* service label
//! on a Talkgroup, so deleting one un-tags its channels and leaves them. Either
//! way the Talkgroup survives — losing a way of looking at a county's channels
//! must never lose the channels.

use axum::extract::{Path, State};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DbErr, EntityTrait, IntoActiveModel,
    QueryFilter, QuerySelect, Set,
};
use serde::{Deserialize, Serialize};

use super::{Created, Listing, Rejected, Removed, What, required_name};
use crate::AppState;
use crate::db::entities::{group, tag, talkgroup, talkgroup_group};
use crate::failure::{Failure, Stage};

/// Which name-only entity a request is about.
///
/// A value rather than a generic parameter because SeaORM's entities are
/// distinct types with distinct `Model`s: a `match` that normalises each arm
/// into the shared row is shorter, and far more readable, than the trait bounds
/// it would take to make one function accept both.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Table {
    Group,
    Tag,
}

impl Table {
    fn what(self) -> What {
        match self {
            Table::Group => What::Group,
            Table::Tag => What::Tag,
        }
    }
}

/// One Group or Tag, as the curation screen lists it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LabelRow {
    pub id: i64,
    pub name: String,
    /// How many Talkgroups are behind it — what an Operator about to delete one
    /// actually needs to know, and **one query for the whole list** rather than
    /// one per row (#86's N+1, which the denormalizer already paid for once).
    pub talkgroups: u64,
    pub created_at_ms: i64,
}

crate::answers_json!(LabelRow);

/// What a create or a rename carries. One field, and no way to send another:
/// `deny_unknown_fields` is what makes "nothing silently ignored" true of a
/// typo'd key rather than only of a typo'd value.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LabelBody {
    pub name: String,
}

// -- The routes, one pair of thin wrappers per table ------------------------

pub async fn list_groups(state: State<AppState>) -> Result<Listing<LabelRow>, Failure> {
    list(Table::Group, state).await
}

pub async fn list_tags(state: State<AppState>) -> Result<Listing<LabelRow>, Failure> {
    list(Table::Tag, state).await
}

pub async fn create_group(
    state: State<AppState>,
    body: axum::Json<LabelBody>,
) -> Result<Created<LabelRow>, Failure> {
    create(Table::Group, state, body).await
}

pub async fn create_tag(
    state: State<AppState>,
    body: axum::Json<LabelBody>,
) -> Result<Created<LabelRow>, Failure> {
    create(Table::Tag, state, body).await
}

pub async fn update_group(
    state: State<AppState>,
    id: Path<i64>,
    body: axum::Json<LabelBody>,
) -> Result<LabelRow, Failure> {
    update(Table::Group, state, id, body).await
}

pub async fn update_tag(
    state: State<AppState>,
    id: Path<i64>,
    body: axum::Json<LabelBody>,
) -> Result<LabelRow, Failure> {
    update(Table::Tag, state, id, body).await
}

pub async fn remove_group(state: State<AppState>, id: Path<i64>) -> Result<Removed, Failure> {
    remove(Table::Group, state, id).await
}

pub async fn remove_tag(state: State<AppState>, id: Path<i64>) -> Result<Removed, Failure> {
    remove(Table::Tag, state, id).await
}

// -- What they all do -------------------------------------------------------

/// `GET /api/admin/{groups,tags}` — every row, with its Talkgroup count.
async fn list(table: Table, State(state): State<AppState>) -> Result<Listing<LabelRow>, Failure> {
    read_all(&state.db, table)
        .await
        .map(Listing::new)
        .map_err(Stage::Curate.failed())
}

/// `POST /api/admin/{groups,tags}` — one new row.
async fn create(
    table: Table,
    State(state): State<AppState>,
    axum::Json(body): axum::Json<LabelBody>,
) -> Result<Created<LabelRow>, Failure> {
    let name = required_name(&body.name, "name")?;
    let db = &state.db;
    if find_by_name(db, table, &name)
        .await
        .map_err(Stage::Curate.failed())?
        .is_some()
    {
        return Err(Rejected::NameTaken { name }.into());
    }

    let now_ms = state.clock.now_ms();
    let id = insert(db, table, &name, now_ms)
        .await
        .map_err(Stage::Curate.failed())?;

    Ok(Created(LabelRow {
        id,
        name,
        talkgroups: 0,
        created_at_ms: now_ms,
    }))
}

/// `PATCH /api/admin/{groups,tags}/{id}` — a rename.
async fn update(
    table: Table,
    State(state): State<AppState>,
    Path(id): Path<i64>,
    axum::Json(body): axum::Json<LabelBody>,
) -> Result<LabelRow, Failure> {
    let name = required_name(&body.name, "name")?;
    let db = &state.db;
    let existing = find_by_id(db, table, id)
        .await
        .map_err(Stage::Curate.failed())?
        .ok_or(Rejected::NotFound(table.what()))?;

    // A row renamed to the name it already has is not a collision with itself —
    // which a form re-submitted unchanged does on every save.
    if let Some(other) = find_by_name(db, table, &name)
        .await
        .map_err(Stage::Curate.failed())?
        && other != id
    {
        return Err(Rejected::NameTaken { name }.into());
    }

    rename(db, table, id, &name)
        .await
        .map_err(Stage::Curate.failed())?;

    Ok(LabelRow {
        id,
        name,
        talkgroups: count_of(db, table, id)
            .await
            .map_err(Stage::Curate.failed())?,
        created_at_ms: existing.created_at_ms,
    })
}

/// `DELETE /api/admin/{groups,tags}/{id}` — the row, and the way its Talkgroups
/// pointed at it. Never the Talkgroups.
async fn remove(
    table: Table,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Removed, Failure> {
    let db = &state.db;
    find_by_id(db, table, id)
        .await
        .map_err(Stage::Curate.failed())?
        .ok_or(Rejected::NotFound(table.what()))?;

    // One transaction, because a Talkgroup left pointing at a Tag that is gone
    // is a channel whose Tag renders as nothing and whose row says otherwise.
    let txn = db.begin().await.map_err(Stage::Curate.failed())?;
    detach(&txn, table, id)
        .await
        .map_err(Stage::Curate.failed())?;
    match table {
        Table::Group => group::Entity::delete_by_id(id).exec(&txn).await,
        Table::Tag => tag::Entity::delete_by_id(id).exec(&txn).await,
    }
    .map_err(Stage::Curate.failed())?;
    txn.commit().await.map_err(Stage::Curate.failed())?;

    Ok(Removed)
}

// -- The statements ---------------------------------------------------------

/// What a rename has to carry over from the stored row: when it was made.
///
/// The name is what the caller is about to change and the id is what they named
/// it by, so this is the whole of what neither of them supplies.
struct Existing {
    created_at_ms: i64,
}

/// Every row, ordered the way the panel orders anything an Operator reads: by
/// name, case-insensitively, **in Rust**. Collation is exactly where SQLite and
/// Postgres disagree (ADR-0003), and a list that reshuffles when an Operator
/// moves database is a bug the design can simply not have.
async fn read_all<C: ConnectionTrait>(db: &C, table: Table) -> Result<Vec<LabelRow>, DbErr> {
    let counts = counts(db, table).await?;
    let mut rows: Vec<LabelRow> = match table {
        Table::Group => group::Entity::find()
            .all(db)
            .await?
            .into_iter()
            .map(|row| (row.id, row.name, row.created_at_ms))
            .collect(),
        Table::Tag => tag::Entity::find()
            .all(db)
            .await?
            .into_iter()
            .map(|row| (row.id, row.name, row.created_at_ms))
            .collect::<Vec<_>>(),
    }
    .into_iter()
    .map(|(id, name, created_at_ms)| LabelRow {
        id,
        name,
        talkgroups: counts.get(&id).copied().unwrap_or(0),
        created_at_ms,
    })
    .collect();
    rows.sort_by(|a, b| {
        a.name
            .to_lowercase()
            .cmp(&b.name.to_lowercase())
            .then_with(|| a.id.cmp(&b.id))
    });
    Ok(rows)
}

/// How many Talkgroups belong to each row, in **one** grouped statement.
///
/// A Group counts its join-table links; a Tag counts the Talkgroups pointing at
/// it. Rows with none are simply absent from the map, which is what
/// `unwrap_or(0)` above reads them as.
async fn counts<C: ConnectionTrait>(
    db: &C,
    table: Table,
) -> Result<std::collections::HashMap<i64, u64>, DbErr> {
    let pairs: Vec<(Option<i64>, i64)> = match table {
        Table::Group => talkgroup_group::Entity::find()
            .select_only()
            .column(talkgroup_group::Column::GroupId)
            .column_as(talkgroup_group::Column::TalkgroupId.count(), "held")
            .group_by(talkgroup_group::Column::GroupId)
            .into_tuple::<(i64, i64)>()
            .all(db)
            .await?
            .into_iter()
            .map(|(id, held)| (Some(id), held))
            .collect(),
        Table::Tag => {
            talkgroup::Entity::find()
                .select_only()
                .column(talkgroup::Column::TagId)
                .column_as(talkgroup::Column::Id.count(), "held")
                .group_by(talkgroup::Column::TagId)
                .into_tuple::<(Option<i64>, i64)>()
                .all(db)
                .await?
        }
    };
    Ok(pairs
        .into_iter()
        // An untagged Talkgroup groups under `NULL`, which is a count of
        // nothing's Talkgroups rather than a row's.
        .filter_map(|(id, held)| id.map(|id| (id, held.max(0) as u64)))
        .collect())
}

/// One row's Talkgroup count, for the rename that has to answer with it.
async fn count_of<C: ConnectionTrait>(db: &C, table: Table, id: i64) -> Result<u64, DbErr> {
    Ok(counts(db, table).await?.get(&id).copied().unwrap_or(0))
}

async fn find_by_id<C: ConnectionTrait>(
    db: &C,
    table: Table,
    id: i64,
) -> Result<Option<Existing>, DbErr> {
    Ok(match table {
        Table::Group => group::Entity::find_by_id(id)
            .one(db)
            .await?
            .map(|row| Existing {
                created_at_ms: row.created_at_ms,
            }),
        Table::Tag => tag::Entity::find_by_id(id)
            .one(db)
            .await?
            .map(|row| Existing {
                created_at_ms: row.created_at_ms,
            }),
    })
}

/// The id of the row with this exact name, if any.
async fn find_by_name<C: ConnectionTrait>(
    db: &C,
    table: Table,
    name: &str,
) -> Result<Option<i64>, DbErr> {
    Ok(match table {
        Table::Group => group::Entity::find()
            .filter(group::Column::Name.eq(name))
            .one(db)
            .await?
            .map(|row| row.id),
        Table::Tag => tag::Entity::find()
            .filter(tag::Column::Name.eq(name))
            .one(db)
            .await?
            .map(|row| row.id),
    })
}

async fn insert<C: ConnectionTrait>(
    db: &C,
    table: Table,
    name: &str,
    now_ms: i64,
) -> Result<i64, DbErr> {
    Ok(match table {
        Table::Group => {
            group::ActiveModel {
                name: Set(name.to_owned()),
                created_at_ms: Set(now_ms),
                ..Default::default()
            }
            .insert(db)
            .await?
            .id
        }
        Table::Tag => {
            tag::ActiveModel {
                name: Set(name.to_owned()),
                created_at_ms: Set(now_ms),
                ..Default::default()
            }
            .insert(db)
            .await?
            .id
        }
    })
}

async fn rename<C: ConnectionTrait>(
    db: &C,
    table: Table,
    id: i64,
    name: &str,
) -> Result<(), DbErr> {
    match table {
        Table::Group => {
            let mut row = group::Entity::find_by_id(id)
                .one(db)
                .await?
                .ok_or_else(|| DbErr::RecordNotFound(format!("group {id}")))?
                .into_active_model();
            row.name = Set(name.to_owned());
            row.update(db).await?;
        }
        Table::Tag => {
            let mut row = tag::Entity::find_by_id(id)
                .one(db)
                .await?
                .ok_or_else(|| DbErr::RecordNotFound(format!("tag {id}")))?
                .into_active_model();
            row.name = Set(name.to_owned());
            row.update(db).await?;
        }
    }
    Ok(())
}

/// Cut every Talkgroup loose from the row about to be deleted — the links for a
/// Group, the pointer for a Tag.
async fn detach<C: ConnectionTrait>(db: &C, table: Table, id: i64) -> Result<(), DbErr> {
    match table {
        Table::Group => {
            talkgroup_group::Entity::delete_many()
                .filter(talkgroup_group::Column::GroupId.eq(id))
                .exec(db)
                .await?;
        }
        Table::Tag => {
            talkgroup::Entity::update_many()
                .col_expr(
                    talkgroup::Column::TagId,
                    sea_orm::sea_query::Expr::value(Option::<i64>::None),
                )
                .filter(talkgroup::Column::TagId.eq(id))
                .exec(db)
                .await?;
        }
    }
    Ok(())
}
