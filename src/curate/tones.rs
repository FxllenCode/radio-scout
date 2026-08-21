//! **Tone profiles**, curated from the browser (#55, spec US 20 and US 45).
//!
//! One row per station: the tones it is paged with, in order, and how much slack
//! to allow. They hang off a **Talkgroup**, because a page-out happens on a
//! channel and a profile that named no channel would be looked for in every
//! Call this Instance takes.
//!
//! # Two things this module owes that the others do not
//!
//! - **The roster gate.** [`crate::tone::Tones`] keeps one bit saying whether
//!   any enabled profile exists at all, so an Instance with none spends nothing
//!   per upload. Every write here re-reads it *on the same request*, which is
//!   why each handler ends the same way: a profile written now must be looked
//!   for in the very next Call, and a bit refreshed only at boot would mean
//!   "restart before your pager works".
//! - **Refusing a profile that could never fire.** A step outside the band
//!   [`crate::tone::detect`] listens to is not a strict configuration, it is a
//!   silent one — so [`crate::tone::unusable`] is asked here, and asked by the
//!   configuration importer and the browser too, because three surfaces
//!   disagreeing about it is invisible until a station stops being paged.
//!
//! Deleting one is deliberately **not** guarded by [`Rejected::HasCalls`]: the
//! pages it already caught are rows on those Calls carrying their own snapshot
//! of the label ([`crate::db::entities::call_tone`]), so the Archive keeps
//! saying what happened. What stops is future detection.

use axum::extract::{Path, State};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, EntityTrait, IntoActiveModel, QueryFilter, QueryOrder, Set,
};
use serde::{Deserialize, Serialize};

use super::{Created, Listing, Rejected, Removed, What, required_name};
use crate::AppState;
use crate::db::entities::{talkgroup, tone_profile};
use crate::failure::{Failure, Stage};
use crate::tone::{DEFAULT_GAP_MAX_MS, DEFAULT_TOLERANCE_PCT, Step};

/// One Tone profile, as the curation screen lists it.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToneProfileRow {
    pub id: i64,
    pub talkgroup_id: i64,
    pub label: String,
    pub tolerance_pct: f64,
    pub gap_max_ms: i64,
    /// The tones, in order — **parsed**, so the screen shows the sequence the
    /// detector is really matching rather than the text of a column.
    pub steps: Vec<Step>,
    pub disabled: bool,
    pub created_at_ms: i64,
}

crate::answers_json!(ToneProfileRow);

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NewToneProfile {
    pub label: String,
    #[serde(default)]
    pub steps: Vec<Step>,
    /// Absent means [`DEFAULT_TOLERANCE_PCT`] — twice the ±1% a Quick Call set
    /// is specified to, which is what every field tool defaults to.
    pub tolerance_pct: Option<f64>,
    /// Absent means [`DEFAULT_GAP_MAX_MS`].
    pub gap_max_ms: Option<i64>,
    #[serde(default)]
    pub disabled: bool,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ToneProfilePatch {
    pub label: Option<String>,
    /// The **whole** sequence, not a delta. A tone list is short, ordered and
    /// meaningless in pieces — where a Talkgroup's members are a set that two
    /// tabs can edit at once (#50), a station's tones are one value.
    pub steps: Option<Vec<Step>>,
    pub tolerance_pct: Option<f64>,
    pub gap_max_ms: Option<i64>,
    pub disabled: Option<bool>,
}

/// `GET /api/admin/talkgroups/{id}/tones` — the profiles paged on one channel.
pub async fn list(
    State(state): State<AppState>,
    Path(talkgroup_id): Path<i64>,
) -> Result<Listing<ToneProfileRow>, Failure> {
    talkgroup_or_404(&state, talkgroup_id).await?;

    Ok(Listing::new(
        rows_of(&state, talkgroup_id).await?.into_iter().collect(),
    ))
}

/// `POST /api/admin/talkgroups/{id}/tones` — add one.
pub async fn create(
    State(state): State<AppState>,
    Path(talkgroup_id): Path<i64>,
    axum::Json(body): axum::Json<NewToneProfile>,
) -> Result<Created<ToneProfileRow>, Failure> {
    talkgroup_or_404(&state, talkgroup_id).await?;
    let label = required_name(&body.label, "label")?;
    let tolerance_pct = body.tolerance_pct.unwrap_or(DEFAULT_TOLERANCE_PCT);
    let gap_max_ms = body.gap_max_ms.unwrap_or(DEFAULT_GAP_MAX_MS);
    checked(&body.steps, tolerance_pct, gap_max_ms)?;

    let row = tone_profile::ActiveModel {
        talkgroup_id: Set(talkgroup_id),
        label: Set(label),
        tolerance_pct: Set(tolerance_pct),
        gap_max_ms: Set(gap_max_ms),
        steps: Set(crate::tone::steps_json(&body.steps)),
        disabled: Set(body.disabled),
        created_at_ms: Set(state.clock.now_ms()),
        ..Default::default()
    }
    .insert(&state.db)
    .await
    .map_err(Stage::Curate.failed())?;

    state.tones.rearm(&state.db).await;
    Ok(Created(row_of(row)))
}

/// `PATCH /api/admin/tones/{id}` — rename, re-tone, re-slack, or switch off.
pub async fn update(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    axum::Json(body): axum::Json<ToneProfilePatch>,
) -> Result<ToneProfileRow, Failure> {
    let existing = tone_profile::Entity::find_by_id(id)
        .one(&state.db)
        .await
        .map_err(Stage::Curate.failed())?
        .ok_or(Rejected::NotFound(What::ToneProfile))?;

    // Checked against what the row will *become*, not against what arrived: a
    // `PATCH` that widens the tolerance without resending the tones has to be
    // held to the same rule as one that sends both, or the two orders of the
    // same two edits would end in different places.
    let steps = match &body.steps {
        Some(steps) => steps.clone(),
        None => crate::tone::steps_of(&existing.steps),
    };
    let tolerance_pct = body.tolerance_pct.unwrap_or(existing.tolerance_pct);
    let gap_max_ms = body.gap_max_ms.unwrap_or(existing.gap_max_ms);
    checked(&steps, tolerance_pct, gap_max_ms)?;

    let mut row = existing.into_active_model();
    if let Some(label) = &body.label {
        row.label = Set(required_name(label, "label")?);
    }
    row.steps = Set(crate::tone::steps_json(&steps));
    row.tolerance_pct = Set(tolerance_pct);
    row.gap_max_ms = Set(gap_max_ms);
    if let Some(disabled) = body.disabled {
        row.disabled = Set(disabled);
    }

    let row = row
        .update(&state.db)
        .await
        .map_err(Stage::Curate.failed())?;

    state.tones.rearm(&state.db).await;
    Ok(row_of(row))
}

/// `DELETE /api/admin/tones/{id}` — stop watching for this station.
///
/// The pages it already caught stay on their Calls: each one snapshotted this
/// profile's label when it fired, so the Archive keeps saying who was paged
/// after the profile that noticed is gone.
pub async fn remove(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Removed, Failure> {
    tone_profile::Entity::find_by_id(id)
        .one(&state.db)
        .await
        .map_err(Stage::Curate.failed())?
        .ok_or(Rejected::NotFound(What::ToneProfile))?;

    tone_profile::Entity::delete_by_id(id)
        .exec(&state.db)
        .await
        .map_err(Stage::Curate.failed())?;

    state.tones.rearm(&state.db).await;
    Ok(Removed)
}

/// A profile that could actually fire, or the refusal saying why not.
fn checked(steps: &[Step], tolerance_pct: f64, gap_max_ms: i64) -> Result<(), Rejected> {
    match crate::tone::unusable(steps, tolerance_pct, gap_max_ms) {
        None => Ok(()),
        Some(detail) => Err(Rejected::UnusableToneProfile { detail }),
    }
}

/// The channel exists — asked before anything else, so a profile can never be
/// written against a Talkgroup that is not there.
async fn talkgroup_or_404(state: &AppState, talkgroup_id: i64) -> Result<(), Failure> {
    talkgroup::Entity::find_by_id(talkgroup_id)
        .one(&state.db)
        .await
        .map_err(Stage::Curate.failed())?
        .ok_or(Rejected::NotFound(What::Talkgroup))?;
    Ok(())
}

async fn rows_of(state: &AppState, talkgroup_id: i64) -> Result<Vec<ToneProfileRow>, Failure> {
    Ok(tone_profile::Entity::find()
        .filter(tone_profile::Column::TalkgroupId.eq(talkgroup_id))
        .order_by_asc(tone_profile::Column::Id)
        .all(&state.db)
        .await
        .map_err(Stage::Curate.failed())?
        .into_iter()
        .map(row_of)
        .collect())
}

fn row_of(row: tone_profile::Model) -> ToneProfileRow {
    ToneProfileRow {
        id: row.id,
        talkgroup_id: row.talkgroup_id,
        label: row.label,
        tolerance_pct: row.tolerance_pct,
        gap_max_ms: row.gap_max_ms,
        steps: crate::tone::steps_of(&row.steps),
        disabled: row.disabled,
        created_at_ms: row.created_at_ms,
    }
}
