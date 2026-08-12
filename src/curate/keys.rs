//! **API keys** — the recorder-facing secrets that authorize ingest (#49,
//! ADR-0008).
//!
//! The browser is where these live now. `RADIO_SCOUT_API_KEY` stays as the
//! **bootstrap**: a zero-configuration first run still generates a key into
//! `.env` so a recorder can be pointed at a fresh Instance without opening a
//! screen, and a wiped database re-seeds from the same place. What it stopped
//! being is an authority — see [`crate::startup::provision_ingest_key`], where a
//! configured key is registered only while the roster is *empty*, so a key an
//! Operator revoked here does not quietly return on the next boot.
//!
//! # What a key is, and is not
//!
//! Stored **hashed** (SHA-256), never in plaintext — so the surface can list
//! keys, label them and revoke them, and can never show one again. A create is
//! the only moment the secret exists in a response, which is why it is the only
//! moment it can be copied.
//!
//! Two ways off:
//!
//! - **Disable** keeps the row and refuses it (`repo::authorizes`). The durable
//!   off — nothing revives a disabled key, which is the property ADR-0008 asks
//!   for and the one `ensure_api_key` has always honoured.
//! - **Revoke** deletes the row. Tidier, and now equally durable.
//!
//! rdio-scanner's equivalent lives inside its whole-config `PUT` and stores the
//! key **in plaintext** (`apikey.go`), so anybody who reaches its admin page —
//! or its database, or a backup of it — reads every recorder's secret.

use axum::extract::{Path, State};
use sea_orm::{ActiveModelTrait, EntityTrait, IntoActiveModel, QueryOrder, Set};
use serde::{Deserialize, Serialize};

use super::{Created, Listing, Rejected, Removed, What, cleared, nullable, optional_text};
use crate::AppState;
use crate::db::entities::api_key;
use crate::db::repo;
use crate::failure::{Failure, Stage};

/// One API key, as the curation screen lists it. **Never the secret** — there is
/// no field for it, so no future edit to this type can accidentally add one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KeyRow {
    pub id: i64,
    pub label: Option<String>,
    /// The System **Ref** this key may ingest into; `null` grants every System.
    /// A Ref rather than an Id on purpose: an Operator can cut a recorder its
    /// key before the System exists, and auto-populate will create it on the
    /// first Call.
    pub system_ref: Option<i64>,
    pub disabled: bool,
    pub created_at_ms: i64,
}

crate::answers_json!(KeyRow);

/// A key that has just been created — the row, plus **the one and only sight of
/// the secret**.
///
/// Flattened rather than nested so the client renders the same row component for
/// a create and for a listing, with one extra field to copy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IssuedKey {
    /// The plaintext, shown once. Nothing stores it and nothing can show it
    /// again.
    pub key: String,
    #[serde(flatten)]
    pub row: KeyRow,
}

crate::answers_json!(IssuedKey);

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NewKey {
    pub label: Option<String>,
    pub system_ref: Option<i64>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct KeyPatch {
    #[serde(default, deserialize_with = "nullable")]
    pub label: Option<Option<String>>,
    #[serde(default, deserialize_with = "nullable")]
    pub system_ref: Option<Option<i64>>,
    pub disabled: Option<bool>,
}

/// `GET /api/admin/api-keys` — every key, newest first, secrets excluded.
pub async fn list(State(state): State<AppState>) -> Result<Listing<KeyRow>, Failure> {
    Ok(Listing::new(
        api_key::Entity::find()
            .order_by_desc(api_key::Column::CreatedAtMs)
            .order_by_desc(api_key::Column::Id)
            .all(&state.db)
            .await
            .map_err(Stage::Curate.failed())?
            .into_iter()
            .map(row_of)
            .collect(),
    ))
}

/// `POST /api/admin/api-keys` — issue one, and show it exactly once.
pub async fn create(
    State(state): State<AppState>,
    axum::Json(body): axum::Json<NewKey>,
) -> Result<Created<IssuedKey>, Failure> {
    // The same generator first run uses, so there is one idea of what a key
    // looks like and a recorder cannot tell where its key came from.
    let key = uuid::Uuid::new_v4().simple().to_string();

    let row = repo::create_api_key(
        &state.db,
        &key,
        body.system_ref,
        optional_text(body.label),
        state.clock.now_ms(),
    )
    .await
    .map_err(Stage::Curate.failed())?;

    Ok(Created(IssuedKey {
        key,
        row: row_of(row),
    }))
}

/// `PATCH /api/admin/api-keys/{id}` — relabel, rescope, disable or re-enable.
pub async fn update(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    axum::Json(body): axum::Json<KeyPatch>,
) -> Result<KeyRow, Failure> {
    let existing = api_key::Entity::find_by_id(id)
        .one(&state.db)
        .await
        .map_err(Stage::Curate.failed())?
        .ok_or(Rejected::NotFound(What::ApiKey))?;

    let mut row = existing.into_active_model();
    if let Some(label) = body.label {
        row.label = Set(cleared(label));
    }
    if let Some(system_ref) = body.system_ref {
        row.system_ref = Set(system_ref);
    }
    if let Some(disabled) = body.disabled {
        row.disabled = Set(disabled);
    }

    Ok(row_of(
        row.update(&state.db)
            .await
            .map_err(Stage::Curate.failed())?,
    ))
}

/// `DELETE /api/admin/api-keys/{id}` — revoke.
///
/// The recorder holding it starts being refused on its next upload, with an
/// `invalid-api-key` line naming the System it tried (ADR-0011 rule 2 keeps the
/// key itself out of that line — and an unknown key has no row to name it by
/// anyway).
pub async fn remove(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Removed, Failure> {
    api_key::Entity::find_by_id(id)
        .one(&state.db)
        .await
        .map_err(Stage::Curate.failed())?
        .ok_or(Rejected::NotFound(What::ApiKey))?;

    api_key::Entity::delete_by_id(id)
        .exec(&state.db)
        .await
        .map_err(Stage::Curate.failed())?;

    Ok(Removed)
}

/// The stored row as the screen reads it — **the hash is dropped here**, which
/// is the one line in this module that matters most.
fn row_of(row: api_key::Model) -> KeyRow {
    KeyRow {
        id: row.id,
        label: row.label,
        system_ref: row.system_ref,
        disabled: row.disabled,
        created_at_ms: row.created_at_ms,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A key row's JSON carries no secret, under any name. Asserted over the
    /// serialized document rather than by reading the struct, because what
    /// leaks a secret is a *field that got added*, and this fails when one is.
    #[test]
    fn a_listed_key_carries_no_secret() {
        let row = row_of(api_key::Model {
            id: 1,
            key_hash: String::from("f00dcafe"),
            label: Some(String::from("the pi")),
            system_ref: Some(11),
            disabled: false,
            created_at_ms: 1_700_000_000_000,
        });

        let json = serde_json::to_string(&row).expect("json");

        assert!(!json.contains("f00dcafe"), "{json}");
        assert!(!json.contains("hash"), "{json}");
        assert!(!json.contains("key"), "{json}");
        assert!(json.contains("the pi"), "{json}");
    }

    /// ...and the one document that *does* carry a secret carries it under
    /// `key`, flattened beside the row so the client renders one shape.
    #[test]
    fn an_issued_key_shows_the_secret_once_beside_its_row() {
        let issued = IssuedKey {
            key: String::from("s3cret"),
            row: row_of(api_key::Model {
                id: 2,
                key_hash: String::from("f00dcafe"),
                label: None,
                system_ref: None,
                disabled: false,
                created_at_ms: 0,
            }),
        };

        let json: serde_json::Value = serde_json::to_value(&issued).expect("json");

        assert_eq!(json["key"], "s3cret");
        assert_eq!(json["id"], 2);
        assert!(json.get("row").is_none(), "flattened, not nested: {json}");
        assert!(!json.to_string().contains("f00dcafe"), "{json}");
    }
}
