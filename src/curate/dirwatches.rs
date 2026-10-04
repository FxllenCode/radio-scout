//! **Dirwatch** entries, curated from the browser (#72, spec US 14 and 45).
//!
//! One row per watched folder: where it is, which Recorder writes it, how long
//! to leave a file alone before reading it, and whether to delete it after.
//! Every write re-arms the Worker on the same request, so a watch added here is
//! watching — and its folder has been looked in — by the time `settle()`
//! returns.
//!
//! # The folder is bounded by the TOML
//!
//! A watch reads every matching file in its folder and, with delete-after,
//! removes it; an unbounded path field would make an admin session the service
//! user's whole filesystem, which rdio's is. So the folder must resolve —
//! symlinks and `..` followed — to somewhere inside one of `[dirwatch] roots`,
//! which only a shell can set (ADR-0021). The listing carries the roots, so the
//! screen can say where a watch may go, and say how to turn Dirwatch on when
//! there are none, rather than offering a form that would be refused.
//!
//! # What a write is held to
//!
//! The **whole row** after the write, not just the fields a `PATCH` names — so
//! no sequence of edits reaches a combination a create would refuse:
//!
//! - inside a root, and not overlapping another watch's folder (two watches on
//!   one file would race each other's delete-after);
//! - a format this release reads, and an extension that is audio;
//! - for a mask watch, a mask that compiles — refused **here**, on the form,
//!   where rdio compiles it at the first file and panics its server;
//! - for a mask watch, something that says which Talkgroup and which System —
//!   the watch itself, or a token in the mask.
//!
//! Changing the folder or the format resets the watermark to now: what the old
//! one had seen says nothing about the new one, and a watch pointed somewhere
//! new should not import that folder's history any more than a new watch does.

use axum::extract::{Path, State};
use sea_orm::{ActiveModelTrait, EntityTrait, IntoActiveModel, QueryOrder, Set};
use serde::{Deserialize, Serialize};

use super::{Created, Rejected, Removed, What, nullable, optional_text, required_name};
use crate::AppState;
use crate::db::entities::dirwatch;
use crate::dirwatch::mask::{Mask, Token};
use crate::dirwatch::{Bound, Format, Health, audio_mime, within_roots};
use crate::failure::{Failure, Stage};

/// The delay a watch waits when the form names none — rdio's floor.
const DEFAULT_DELAY_MS: i64 = 2_000;

/// The longest delay worth having: past ten minutes a Recorder is not still
/// writing, it has stopped.
const MAX_DELAY_MS: i64 = 600_000;

/// One watch, as the screen lists it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DirwatchRow {
    pub id: i64,
    pub label: Option<String>,
    pub directory: String,
    pub format: String,
    /// The stored extension, or `null` for the format's own default.
    pub extension: Option<String>,
    pub mask: Option<String>,
    pub system_ref: Option<i64>,
    pub talkgroup_ref: Option<i64>,
    pub frequency: Option<i64>,
    pub delay_ms: i64,
    pub delete_after: bool,
    pub poll: bool,
    pub disabled: bool,
    pub created_at_ms: i64,
    /// What the Worker says it is doing with it.
    pub health: Health,
}

crate::answers_json!(DirwatchRow);

/// Every watch, and where a watch may be.
#[derive(Debug, Clone, Serialize)]
pub struct DirwatchListing {
    pub results: Vec<DirwatchRow>,
    /// `[dirwatch] roots`, as configured. Empty means Dirwatch is off, and the
    /// screen says how to turn it on instead of offering a form.
    pub roots: Vec<String>,
}

impl axum::response::IntoResponse for DirwatchListing {
    fn into_response(self) -> axum::response::Response {
        axum::Json(self).into_response()
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NewDirwatch {
    pub label: Option<String>,
    pub directory: String,
    pub format: String,
    pub extension: Option<String>,
    pub mask: Option<String>,
    pub system_ref: Option<i64>,
    pub talkgroup_ref: Option<i64>,
    pub frequency: Option<i64>,
    pub delay_ms: Option<i64>,
    #[serde(default)]
    pub delete_after: bool,
    #[serde(default)]
    pub poll: bool,
    #[serde(default)]
    pub disabled: bool,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DirwatchPatch {
    #[serde(default, deserialize_with = "nullable")]
    pub label: Option<Option<String>>,
    pub directory: Option<String>,
    pub format: Option<String>,
    #[serde(default, deserialize_with = "nullable")]
    pub extension: Option<Option<String>>,
    #[serde(default, deserialize_with = "nullable")]
    pub mask: Option<Option<String>>,
    #[serde(default, deserialize_with = "nullable")]
    pub system_ref: Option<Option<i64>>,
    #[serde(default, deserialize_with = "nullable")]
    pub talkgroup_ref: Option<Option<i64>>,
    #[serde(default, deserialize_with = "nullable")]
    pub frequency: Option<Option<i64>>,
    pub delay_ms: Option<i64>,
    pub delete_after: Option<bool>,
    pub poll: Option<bool>,
    pub disabled: Option<bool>,
}

/// A watch as it would be stored — what every write is checked as, whole.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Draft {
    label: Option<String>,
    directory: String,
    format: String,
    extension: Option<String>,
    mask: Option<String>,
    system_ref: Option<i64>,
    talkgroup_ref: Option<i64>,
    frequency: Option<i64>,
    delay_ms: i64,
    delete_after: bool,
    poll: bool,
    disabled: bool,
}

/// `GET /api/admin/dirwatches` — every watch, its health, and the roots.
pub async fn list(State(state): State<AppState>) -> Result<DirwatchListing, Failure> {
    let rows = dirwatch::Entity::find()
        .order_by_asc(dirwatch::Column::Id)
        .all(&state.db)
        .await
        .map_err(Stage::Curate.failed())?;
    Ok(DirwatchListing {
        results: rows.into_iter().map(|row| row_of(&state, row)).collect(),
        roots: state
            .dirwatch
            .config()
            .roots
            .iter()
            .map(|root| root.path().display().to_string())
            .collect(),
    })
}

/// `POST /api/admin/dirwatches` — watch a folder.
pub async fn create(
    State(state): State<AppState>,
    axum::Json(body): axum::Json<NewDirwatch>,
) -> Result<Created<DirwatchRow>, Failure> {
    let draft = Draft {
        label: optional_text(body.label),
        directory: body.directory,
        format: body.format,
        extension: body.extension,
        mask: body.mask,
        system_ref: body.system_ref,
        talkgroup_ref: body.talkgroup_ref,
        frequency: body.frequency,
        delay_ms: body.delay_ms.unwrap_or(DEFAULT_DELAY_MS),
        delete_after: body.delete_after,
        poll: body.poll,
        disabled: body.disabled,
    };
    let others = roster(&state).await?;
    let draft = checked(draft, &state, &others, None)?;

    let now = state.clock.now_ms();
    let row = dirwatch::ActiveModel {
        label: Set(draft.label),
        directory: Set(draft.directory),
        format: Set(draft.format),
        extension: Set(draft.extension),
        mask: Set(draft.mask),
        system_ref: Set(draft.system_ref),
        talkgroup_ref: Set(draft.talkgroup_ref),
        frequency: Set(draft.frequency),
        delay_ms: Set(draft.delay_ms),
        delete_after: Set(draft.delete_after),
        poll: Set(draft.poll),
        disabled: Set(draft.disabled),
        // A new watch owes nothing older than itself: pointing one at a folder
        // of history is not a request to import it.
        seen_through_ms: Set(now),
        created_at_ms: Set(now),
        ..Default::default()
    }
    .insert(&state.db)
    .await
    .map_err(Stage::Curate.failed())?;
    state.dirwatch.rearm();

    Ok(Created(row_of(&state, row)))
}

/// `PATCH /api/admin/dirwatches/{id}` — change anything about a watch.
pub async fn update(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    axum::Json(body): axum::Json<DirwatchPatch>,
) -> Result<DirwatchRow, Failure> {
    let mut others = roster(&state).await?;
    let position = others
        .iter()
        .position(|row| row.id == id)
        .ok_or(Rejected::NotFound(What::Dirwatch))?;
    let existing = others.remove(position);

    let draft = Draft {
        label: match body.label {
            Some(label) => optional_text(label),
            None => existing.label.clone(),
        },
        directory: body.directory.unwrap_or_else(|| existing.directory.clone()),
        format: body.format.unwrap_or_else(|| existing.format.clone()),
        extension: body.extension.unwrap_or_else(|| existing.extension.clone()),
        mask: body.mask.unwrap_or_else(|| existing.mask.clone()),
        system_ref: body.system_ref.unwrap_or(existing.system_ref),
        talkgroup_ref: body.talkgroup_ref.unwrap_or(existing.talkgroup_ref),
        frequency: body.frequency.unwrap_or(existing.frequency),
        delay_ms: body.delay_ms.unwrap_or(existing.delay_ms),
        delete_after: body.delete_after.unwrap_or(existing.delete_after),
        poll: body.poll.unwrap_or(existing.poll),
        disabled: body.disabled.unwrap_or(existing.disabled),
    };
    let draft = checked(draft, &state, &others, Some(&existing))?;

    let moved = draft.directory != existing.directory || draft.format != existing.format;
    let mut row = existing.into_active_model();
    if moved {
        row.seen_through_ms = Set(state.clock.now_ms());
    }
    row.label = Set(draft.label);
    row.directory = Set(draft.directory);
    row.format = Set(draft.format);
    row.extension = Set(draft.extension);
    row.mask = Set(draft.mask);
    row.system_ref = Set(draft.system_ref);
    row.talkgroup_ref = Set(draft.talkgroup_ref);
    row.frequency = Set(draft.frequency);
    row.delay_ms = Set(draft.delay_ms);
    row.delete_after = Set(draft.delete_after);
    row.poll = Set(draft.poll);
    row.disabled = Set(draft.disabled);
    let row = row
        .update(&state.db)
        .await
        .map_err(Stage::Curate.failed())?;
    state.dirwatch.rearm();

    Ok(row_of(&state, row))
}

/// `DELETE /api/admin/dirwatches/{id}` — stop watching. The folder and every
/// Call already ingested from it are untouched.
pub async fn remove(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Removed, Failure> {
    let deleted = dirwatch::Entity::delete_by_id(id)
        .exec(&state.db)
        .await
        .map_err(Stage::Curate.failed())?;
    if deleted.rows_affected == 0 {
        return Err(Rejected::NotFound(What::Dirwatch).into());
    }
    state.dirwatch.rearm();
    Ok(Removed)
}

/// `POST /api/admin/dirwatches/{id}/scan` — look in the folder now, including
/// at files refused this run: what an Operator does after fixing whatever got
/// them refused.
pub async fn scan(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<DirwatchRow, Failure> {
    let row = dirwatch::Entity::find_by_id(id)
        .one(&state.db)
        .await
        .map_err(Stage::Curate.failed())?
        .ok_or(Rejected::NotFound(What::Dirwatch))?;
    state.dirwatch.scan(id);
    Ok(row_of(&state, row))
}

async fn roster(state: &AppState) -> Result<Vec<dirwatch::Model>, Failure> {
    dirwatch::Entity::find()
        .all(&state.db)
        .await
        .map_err(Stage::Curate.failed())
}

/// `draft`, normalized, if it is a watch that could run — or the one refusal
/// that says what to fix.
fn checked(
    draft: Draft,
    state: &AppState,
    others: &[dirwatch::Model],
    existing: Option<&dirwatch::Model>,
) -> Result<Draft, Rejected> {
    let roots = &state.dirwatch.config().roots;
    if roots.is_empty() {
        return Err(Rejected::DirwatchUnavailable);
    }
    let typed = required_name(&draft.directory, "directory")?;
    let directory = match within_roots(std::path::Path::new(&typed), roots) {
        Ok(real) => real,
        Err(Bound::NoSuchDirectory) => return Err(Rejected::NoSuchDirectory { directory: typed }),
        Err(Bound::OutsideRoots) => return Err(Rejected::OutsideRoots { directory: typed }),
    };
    if others.iter().any(|other| {
        let theirs = std::path::Path::new(&other.directory);
        directory.starts_with(theirs) || theirs.starts_with(&directory)
    }) {
        return Err(Rejected::DirwatchOverlaps { directory: typed });
    }

    let format =
        Format::from_slug(draft.format.trim()).ok_or_else(|| Rejected::UnknownDirwatchFormat {
            format: draft.format.clone(),
        })?;
    let extension = optional_text(draft.extension)
        .map(|extension| extension.trim_start_matches('.').to_ascii_lowercase())
        .filter(|extension| !extension.is_empty());
    if let Some(extension) = &extension
        && audio_mime(extension).is_none()
    {
        return Err(Rejected::NotAudio {
            extension: extension.clone(),
        });
    }
    let mask = match format {
        Format::Mask => {
            let text = required_name(draft.mask.as_deref().unwrap_or_default(), "mask")?;
            let compiled = Mask::compile(&text).map_err(|error| Rejected::UnusableMask {
                detail: error.to_string(),
            })?;
            routable(&compiled, draft.talkgroup_ref, draft.system_ref)?;
            Some(text)
        }
        // A mask means nothing to any other format, so it is not kept to
        // mislead whoever reads the row next.
        _ => None,
    };

    for (field, value) in [
        ("systemRef", draft.system_ref),
        ("talkgroupRef", draft.talkgroup_ref),
        ("frequency", draft.frequency),
    ] {
        if value.is_some_and(|value| value < 1) {
            return Err(Rejected::OutOfRange {
                field,
                least: 1,
                most: i64::MAX,
            });
        }
    }
    if !(0..=MAX_DELAY_MS).contains(&draft.delay_ms) {
        return Err(Rejected::OutOfRange {
            field: "delayMs",
            least: 0,
            most: MAX_DELAY_MS,
        });
    }

    // A folder is stored as where it really is. An unchanged one is stored as
    // it already was, so an edit to anything else is never a move.
    let directory = directory.display().to_string();
    let directory = match existing {
        Some(existing)
            if std::path::Path::new(&existing.directory) == std::path::Path::new(&directory) =>
        {
            existing.directory.clone()
        }
        _ => directory,
    };
    Ok(Draft {
        directory,
        format: format.slug().to_owned(),
        extension,
        mask,
        label: draft.label,
        ..draft
    })
}

/// Whether a mask watch's files will all say which Talkgroup and which System
/// they are — from the watch, or from a token the mask reads.
fn routable(mask: &Mask, talkgroup: Option<i64>, system: Option<i64>) -> Result<(), Rejected> {
    let names_a_talkgroup = [
        Token::Talkgroup,
        Token::TalkgroupAfs,
        Token::TalkgroupHz,
        Token::TalkgroupKhz,
        Token::TalkgroupMhz,
    ]
    .into_iter()
    .any(|token| mask.reads(token));
    if talkgroup.is_none() && !names_a_talkgroup {
        return Err(Rejected::Unroutable { field: "talkgroup" });
    }
    let names_a_system = mask.reads(Token::System) || mask.reads(Token::SystemLabel);
    if system.is_none() && !names_a_system {
        return Err(Rejected::Unroutable { field: "system" });
    }
    Ok(())
}

fn row_of(state: &AppState, row: dirwatch::Model) -> DirwatchRow {
    DirwatchRow {
        health: state.dirwatch.health(row.id).unwrap_or_default(),
        id: row.id,
        label: row.label,
        directory: row.directory,
        format: row.format,
        extension: row.extension,
        mask: row.mask,
        system_ref: row.system_ref,
        talkgroup_ref: row.talkgroup_ref,
        frequency: row.frequency,
        delay_ms: row.delay_ms,
        delete_after: row.delete_after,
        poll: row.poll,
        disabled: row.disabled,
        created_at_ms: row.created_at_ms,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    fn mask(text: &str) -> Mask {
        Mask::compile(text).expect("a mask")
    }

    #[rstest]
    #[case::both_from_the_mask("#SYS_#TG", None, None, Ok(()))]
    #[case::both_from_the_watch("#UNIT", Some(5), Some(11), Ok(()))]
    #[case::label_names_a_system("#SYSLBL_#TGKHZ", None, None, Ok(()))]
    #[case::afs_names_a_talkgroup("#SYS_#TGAFS", None, None, Ok(()))]
    #[case::no_talkgroup("#SYS_#UNIT", None, None, Err("talkgroup"))]
    #[case::no_system("#TG", None, None, Err("system"))]
    #[case::talkgroup_label_is_not_a_talkgroup("#SYS_#TGLBL", None, None, Err("talkgroup"))]
    fn a_mask_watch_must_say_where_its_files_go(
        #[case] text: &str,
        #[case] talkgroup: Option<i64>,
        #[case] system: Option<i64>,
        #[case] expected: Result<(), &'static str>,
    ) {
        assert_eq!(
            routable(&mask(text), talkgroup, system),
            expected.map_err(|field| Rejected::Unroutable { field })
        );
    }
}
