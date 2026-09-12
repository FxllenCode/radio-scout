//! **Access codes**, curated from the browser (#68, spec US 45, US 52).
//!
//! One row per code: what an Operator calls it, which restricted channels it
//! opens, when it runs out, how many people may be connected on it at once, and
//! whether it works at all. **Which channels are sensitive is not here** — that
//! is `restricted` on the System and Talkgroup forms, and the separation is the
//! feature (see [`crate::access`]): a code names who may hear a gated channel,
//! never which channels are gated.
//!
//! # Two secrets, and the listing carries neither
//!
//! The **code** is Argon2id at rest and cannot be read back at all — not by this
//! surface, not by anybody. The **grant** could be, and deliberately is not:
//! [`CodeRow`] has no field for either, the [`crate::curate::webhooks::WebhookRow`]
//! shape, so no later edit to the listing can start returning one. What an
//! Operator gets instead is the label, the scope, and the live connection count
//! — which is what "is this code being used, and by how many people" actually
//! asks.
//!
//! **Changing the code re-mints the grant**, which is the one behaviour here
//! worth stating twice: rotating a secret because it leaked must not leave every
//! browser holding the old one still connected. rdio-scanner stores its
//! equivalent in plaintext, returns it from the admin API and exports it in the
//! configuration document, and editing it invalidates nothing.

use axum::extract::{Path, State};
use sea_orm::{ActiveModelTrait, EntityTrait, IntoActiveModel, QueryOrder, Set};
use serde::{Deserialize, Serialize};

use super::{Created, Listing, Rejected, Removed, What, cleared, nullable, optional_text};
use crate::AppState;
use crate::access;
use crate::db::entities::access_code;
use crate::failure::{Failure, Stage};
use crate::selection::Selection;

/// The shortest code this surface will store.
///
/// A floor rather than a complexity rule, because a complexity rule on a
/// credential people say out loud pushes them towards writing it down. What
/// makes a memorable code defensible is the three things together: this floor,
/// Argon2id at rest, and `[access] lockout_attempts` bounding how fast anyone
/// may guess.
const MIN_CODE: usize = 8;

/// One Access code, as the curation screen lists it — **never the code, and
/// never the grant**.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeRow {
    pub id: i64,
    pub label: Option<String>,
    /// Which restricted channels it opens, as the live feed's own **Selection**
    /// JSON — the shape a **Downstream**'s and a **Webhook**'s scope already
    /// take, so one editor serves all three and a **Patch** onto an opened
    /// channel is reached by the same rule everywhere.
    pub scope: Selection,
    /// When it stops working; `null` never expires.
    pub expires_at_ms: Option<i64>,
    /// How many live-feed connections may hold it at once; `null` is unlimited.
    pub max_connections: Option<i64>,
    /// How many are holding it **right now**. In-memory and therefore honest
    /// about this process only, which is the same thing the limit is about —
    /// there is one process, and a restart is what ends every connection there
    /// is.
    pub connections: u32,
    pub disabled: bool,
    pub created_at_ms: i64,
}

crate::answers_json!(CodeRow);

/// A code that has just been issued or rotated — the row, plus **the one and
/// only sight of the grant**.
///
/// Flattened so the client renders the same row component for a create and for
/// a listing, the [`crate::curate::keys::IssuedKey`] shape. What is shown once
/// is the *grant*, not the code: the Operator already knows the code, because
/// they chose it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IssuedCode {
    /// The credential a browser will carry, shown once. An Operator who wants a
    /// link that unlocks itself pastes this; everyone else just says the code
    /// out loud and lets the unlock form mint it again.
    pub grant: String,
    #[serde(flatten)]
    pub row: CodeRow,
}

crate::answers_json!(IssuedCode);

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NewCode {
    pub label: Option<String>,
    /// What a Listener types. Required — there is no such thing as a code with
    /// no code.
    pub code: String,
    #[serde(default)]
    pub scope: Selection,
    pub expires_at_ms: Option<i64>,
    pub max_connections: Option<i64>,
    #[serde(default)]
    pub disabled: bool,
}

/// What an edit carries. Absent leaves alone; `null` clears.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CodePatch {
    #[serde(default, deserialize_with = "nullable")]
    pub label: Option<Option<String>>,
    /// A new code. Absent leaves the stored one alone — which is what lets an
    /// Operator edit a scope without re-choosing a secret thirty people already
    /// know — and **present re-mints the grant**, because rotating a code that
    /// leaked must not leave the browsers holding the old grant connected.
    pub code: Option<String>,
    pub scope: Option<Selection>,
    #[serde(default, deserialize_with = "nullable")]
    pub expires_at_ms: Option<Option<i64>>,
    #[serde(default, deserialize_with = "nullable")]
    pub max_connections: Option<Option<i64>>,
    pub disabled: Option<bool>,
}

/// `GET /api/admin/codes` — every code, newest first.
pub async fn list(State(state): State<AppState>) -> Result<Listing<CodeRow>, Failure> {
    Ok(Listing::new(
        access_code::Entity::find()
            .order_by_desc(access_code::Column::CreatedAtMs)
            .order_by_desc(access_code::Column::Id)
            .all(&state.db)
            .await
            .map_err(Stage::Curate.failed())?
            .into_iter()
            .map(|row| row_of(row, &state))
            .collect(),
    ))
}

/// `POST /api/admin/codes` — issue one, and show its grant exactly once.
pub async fn create(
    State(state): State<AppState>,
    axum::Json(body): axum::Json<NewCode>,
) -> Result<Created<IssuedCode>, Failure> {
    let code = checked_code(&body.code)?;
    let grant = access::mint_grant();

    let row = access_code::ActiveModel {
        label: Set(optional_text(body.label)),
        code_hash: Set(access::hash_code(&code)),
        grant: Set(grant.clone()),
        scope: Set(Some(super::downstreams::scope_json(&body.scope))),
        expires_at_ms: Set(body.expires_at_ms),
        max_connections: Set(body.max_connections),
        disabled: Set(body.disabled),
        created_at_ms: Set(state.clock.now_ms()),
        ..Default::default()
    }
    .insert(&state.db)
    .await
    .map_err(Stage::Curate.failed())?;

    Ok(Created(IssuedCode {
        grant,
        row: row_of(row, &state),
    }))
}

/// `PATCH /api/admin/codes/{id}` — change what was named, leave the rest.
pub async fn update(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    axum::Json(body): axum::Json<CodePatch>,
) -> Result<CodeRow, Failure> {
    let existing = access_code::Entity::find_by_id(id)
        .one(&state.db)
        .await
        .map_err(Stage::Curate.failed())?
        .ok_or(Rejected::NotFound(What::AccessCode))?;

    let mut row = existing.into_active_model();
    if let Some(label) = body.label {
        row.label = Set(cleared(label));
    }
    if let Some(code) = body.code {
        let code = checked_code(&code)?;
        row.code_hash = Set(access::hash_code(&code));
        // **The grant goes with the code.** A rotation that left the old grant
        // working would be a rotation in name: every browser that ever unlocked
        // keeps its access, which is exactly the case an Operator rotates for.
        row.grant = Set(access::mint_grant());
    }
    if let Some(scope) = body.scope {
        row.scope = Set(Some(super::downstreams::scope_json(&scope)));
    }
    if let Some(expires_at_ms) = body.expires_at_ms {
        row.expires_at_ms = Set(expires_at_ms);
    }
    if let Some(max_connections) = body.max_connections {
        row.max_connections = Set(max_connections);
    }
    if let Some(disabled) = body.disabled {
        row.disabled = Set(disabled);
    }
    let row = row
        .update(&state.db)
        .await
        .map_err(Stage::Curate.failed())?;

    Ok(row_of(row, &state))
}

/// `DELETE /api/admin/codes/{id}` — the code and the grant with it.
///
/// No `?force`, and nothing to refuse: a code owns no Calls and nothing points
/// at it. What it takes with it is the access every browser holding its grant
/// had, which is what a revoke is for — and, unlike disabling it, it cannot be
/// undone by turning the row back on.
pub async fn remove(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Removed, Failure> {
    let deleted = access_code::Entity::delete_by_id(id)
        .exec(&state.db)
        .await
        .map_err(Stage::Curate.failed())?;
    match deleted.rows_affected {
        0 => Err(Rejected::NotFound(What::AccessCode).into()),
        _ => Ok(Removed),
    }
}

/// A code the surface will store, or the reason it will not.
///
/// **The refusal names the rule and never the code**, which is
/// [`Rejected::UnusableWebhookUrl`]'s reason one secret along: what is being
/// refused here is a credential an Operator is about to hand out, and this
/// refusal is rendered into a form *and* answered as a 400.
fn checked_code(raw: &str) -> Result<String, Rejected> {
    // Trimmed, because a code is read out loud and typed back in, and a trailing
    // space is not part of what anybody said. The same reading `optional_text`
    // takes of every other field on this form.
    let code = raw.trim();
    match code.chars().count() >= MIN_CODE {
        true => Ok(code.to_owned()),
        false => Err(Rejected::ShortAccessCode { least: MIN_CODE }),
    }
}

/// A stored row as the screen shows it. Private, which is the shape that keeps
/// `code_hash` and `grant` from ever reaching a wire type by accident.
fn row_of(row: access_code::Model, state: &AppState) -> CodeRow {
    CodeRow {
        connections: state.access.connections_held(row.id),
        scope: crate::selection::stored(row.scope.as_deref().unwrap_or_default()),
        id: row.id,
        label: row.label,
        expires_at_ms: row.expires_at_ms,
        max_connections: row.max_connections,
        disabled: row.disabled,
        created_at_ms: row.created_at_ms,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    /// The floor is a floor and nothing else: eight characters of anything, and
    /// the surrounding whitespace of a code somebody pasted is not part of it.
    #[rstest]
    #[case::plenty("FIRE-2026-OPS", Some("FIRE-2026-OPS"))]
    #[case::exactly_the_floor("fireops1", Some("fireops1"))]
    #[case::padded("  fireops1  ", Some("fireops1"))]
    #[case::one_short("fireops", None)]
    #[case::blank("        ", None)]
    #[case::empty("", None)]
    fn a_code_has_to_be_worth_hashing(#[case] raw: &str, #[case] stored: Option<&str>) {
        assert_eq!(checked_code(raw).ok().as_deref(), stored, "raw={raw:?}");
    }

    /// The refusal says the rule and never quotes what it was given — the one
    /// thing that must not end up in a 400 body or in a form's error text.
    #[test]
    fn a_refused_code_is_never_quoted_back() {
        let told = checked_code("hunter2").expect_err("too short").to_string();

        assert!(!told.contains("hunter2"), "{told}");
        assert!(told.contains(&MIN_CODE.to_string()), "{told}");
    }
}
