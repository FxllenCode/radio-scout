//! **Downstream** peers, curated from the browser (#52, spec US 45).
//!
//! One row per peer: where it is, the key it issued us, what to send it, and
//! whether to send at all. The listing carries its **health** beside it — queue
//! depth, last success, consecutive failures, last reason — because #70's status
//! page does not exist yet and an Operator whose peer has stopped taking Calls
//! has to be able to find that out somewhere.
//!
//! # The key
//!
//! Unlike our own [`super::keys`] roster, a Downstream's key is the *peer's*
//! secret and is stored recoverably: a hash cannot be POSTed. Two rules follow,
//! and both are structural rather than remembered:
//!
//! - [`DownstreamRow`] **has no field for it**, so no future edit to the listing
//!   can start returning one. rdio's admin payload includes every downstream's
//!   `apikey` in plaintext (`downstream.go:129`), which means anyone who reaches
//!   its admin page reads every peer credential it holds.
//! - It is never logged, at any level, in any form (ADR-0011 rule 2). A peer is
//!   named in a log line by its **Id**.
//!
//! A `PATCH` that does not mention `apiKey` leaves the stored one alone, which
//! is what makes editing a peer's scope possible without re-typing its
//! credential — the same reason the field is write-only rather than absent.

use axum::extract::{Path, State};
use sea_orm::{ActiveModelTrait, EntityTrait, IntoActiveModel, QueryOrder, Set};
use serde::{Deserialize, Serialize};

use super::{Created, Listing, Rejected, Removed, What, nullable, optional_text, required_name};
use crate::AppState;
use crate::db::entities::downstream;
use crate::db::repo;
use crate::failure::{Failure, Stage};
use crate::selection::Selection;

/// One Downstream, as the curation screen lists it — **never the key**.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DownstreamRow {
    pub id: i64,
    pub label: Option<String>,
    pub url: String,
    /// Which Calls reach this peer, as the live feed's own **Selection** JSON.
    pub scope: Selection,
    pub disabled: bool,
    /// Whether a key has been set at all. The *fact* is not a secret and is the
    /// whole diagnostic: "this peer is refusing everything" and "this peer has
    /// no credential" look identical without it.
    pub has_key: bool,
    /// **The durable queue depth** — Calls written down and not yet taken. This
    /// is the number that survives a restart, as distinct from the sender
    /// Worker's own depth, which counts attempts in flight.
    pub queued: i64,
    pub last_success_ms: Option<i64>,
    pub last_failure_ms: Option<i64>,
    /// The last failure as a slug plus its status — `peer-refused (401)`.
    pub last_failure: Option<String>,
    pub consecutive_failures: i32,
    pub created_at_ms: i64,
}

crate::answers_json!(DownstreamRow);

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NewDownstream {
    pub label: Option<String>,
    pub url: String,
    pub api_key: String,
    #[serde(default)]
    pub scope: Selection,
    #[serde(default)]
    pub disabled: bool,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DownstreamPatch {
    #[serde(default, deserialize_with = "nullable")]
    pub label: Option<Option<String>>,
    pub url: Option<String>,
    /// Absent leaves the stored key alone — which is what lets an Operator edit
    /// a peer's scope without re-typing a credential the screen can never show
    /// them again.
    pub api_key: Option<String>,
    pub scope: Option<Selection>,
    pub disabled: Option<bool>,
}

/// `GET /api/admin/downstreams` — every peer, with its health.
pub async fn list(State(state): State<AppState>) -> Result<Listing<DownstreamRow>, Failure> {
    let rows = downstream::Entity::find()
        .order_by_asc(downstream::Column::Id)
        .all(&state.db)
        .await
        .map_err(Stage::Curate.failed())?;
    // One `GROUP BY` for the whole roster rather than a `COUNT` per row: the
    // screen renders every peer at once, and #86's N+1 argument applies to a
    // listing whatever its length.
    let depths = repo::delivery_depths(&state.db)
        .await
        .map_err(Stage::Curate.failed())?;

    Ok(Listing::new(
        rows.into_iter()
            .map(|row| {
                let queued = depths.get(&row.id).copied().unwrap_or_default();
                row_of(row, queued)
            })
            .collect(),
    ))
}

/// `POST /api/admin/downstreams` — add a peer.
pub async fn create(
    State(state): State<AppState>,
    axum::Json(body): axum::Json<NewDownstream>,
) -> Result<Created<DownstreamRow>, Failure> {
    let url = required_name(&body.url, "url")?;
    let api_key = required_name(&body.api_key, "apiKey")?;

    let row = downstream::ActiveModel {
        label: Set(optional_text(body.label)),
        url: Set(url),
        api_key: Set(api_key),
        scope: Set(scope_json(&body.scope)),
        disabled: Set(body.disabled),
        last_success_ms: Set(None),
        last_failure_ms: Set(None),
        last_failure: Set(None),
        consecutive_failures: Set(0),
        created_at_ms: Set(state.clock.now_ms()),
        ..Default::default()
    }
    .insert(&state.db)
    .await
    .map_err(Stage::Curate.failed())?;

    Ok(Created(row_of(row, 0)))
}

/// `PATCH /api/admin/downstreams/{id}` — relabel, re-point, rescope, re-key, or
/// switch off.
///
/// **Switching a peer off empties its queue.** A peer disabled for a week and
/// switched back on would otherwise replay the week — which is not what
/// "disabled" reads as, and would deliver a flood of Calls the peer's own
/// **Retention** has probably already aged past.
pub async fn update(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    axum::Json(body): axum::Json<DownstreamPatch>,
) -> Result<DownstreamRow, Failure> {
    let existing = downstream::Entity::find_by_id(id)
        .one(&state.db)
        .await
        .map_err(Stage::Curate.failed())?
        .ok_or(Rejected::NotFound(What::Downstream))?;

    let mut row = existing.into_active_model();
    if let Some(label) = body.label {
        row.label = Set(super::cleared(label));
    }
    if let Some(url) = &body.url {
        row.url = Set(required_name(url, "url")?);
    }
    if let Some(api_key) = &body.api_key {
        row.api_key = Set(required_name(api_key, "apiKey")?);
    }
    if let Some(scope) = &body.scope {
        row.scope = Set(scope_json(scope));
    }
    let switching_off = body.disabled.unwrap_or(false);
    if let Some(disabled) = body.disabled {
        row.disabled = Set(disabled);
    }

    let row = row
        .update(&state.db)
        .await
        .map_err(Stage::Curate.failed())?;
    if switching_off {
        repo::clear_deliveries(&state.db, id)
            .await
            .map_err(Stage::Curate.failed())?;
    }

    let queued = match switching_off {
        true => 0,
        false => repo::delivery_depths(&state.db)
            .await
            .map_err(Stage::Curate.failed())?
            .get(&id)
            .copied()
            .unwrap_or_default(),
    };
    Ok(row_of(row, queued))
}

/// `DELETE /api/admin/downstreams/{id}` — forget a peer, and everything owed it.
///
/// No [`Rejected::HasCalls`] guard, unlike a System or a Talkgroup: what goes
/// here is a queue of *pending forwards*, not Calls. The **Archive** is
/// untouched, and every Call in that queue is still in it.
pub async fn remove(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Removed, Failure> {
    downstream::Entity::find_by_id(id)
        .one(&state.db)
        .await
        .map_err(Stage::Curate.failed())?
        .ok_or(Rejected::NotFound(What::Downstream))?;

    // The queue first: a delivery row outliving its peer would be read on every
    // wake-up by a `next_delivery` nobody asks for, and would hold a Call's
    // audio against orphan-GC for nothing.
    repo::clear_deliveries(&state.db, id)
        .await
        .map_err(Stage::Curate.failed())?;
    downstream::Entity::delete_by_id(id)
        .exec(&state.db)
        .await
        .map_err(Stage::Curate.failed())?;

    Ok(Removed)
}

/// The stored row as the screen reads it — **the key is dropped here**, which is
/// the one line in this module that matters most.
fn row_of(row: downstream::Model, queued: i64) -> DownstreamRow {
    DownstreamRow {
        id: row.id,
        label: row.label,
        url: row.url,
        scope: serde_json::from_str(&row.scope).unwrap_or_default(),
        disabled: row.disabled,
        has_key: !row.api_key.is_empty(),
        queued,
        last_success_ms: row.last_success_ms,
        last_failure_ms: row.last_failure_ms,
        last_failure: row.last_failure,
        consecutive_failures: row.consecutive_failures,
        created_at_ms: row.created_at_ms,
    }
}

/// A Selection as the column stores it.
///
/// A Selection is a map of numbers and booleans and cannot fail to serialize;
/// the fallback is an empty document rather than an `unwrap`, and an empty
/// document forwards **nothing** — the safe direction, since the other one sends
/// a County to a peer scoped to a channel.
pub(crate) fn scope_json(scope: &Selection) -> String {
    serde_json::to_string(scope).unwrap_or_else(|_| String::from("{}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model() -> downstream::Model {
        downstream::Model {
            id: 7,
            label: Some(String::from("county mirror")),
            url: String::from("https://peer.example"),
            api_key: String::from("s3cret-peer-key"),
            scope: serde_json::json!({ "sel": { "11": { "*": true } } }).to_string(),
            disabled: false,
            last_success_ms: Some(1_700_000_000_000),
            last_failure_ms: None,
            last_failure: None,
            consecutive_failures: 0,
            created_at_ms: 1_600_000_000_000,
        }
    }

    /// The peer's credential never leaves, under any name. Asserted over the
    /// serialized document rather than by reading the struct, because what leaks
    /// a secret is a *field somebody adds* — and this fails when one is.
    #[test]
    fn a_listed_downstream_carries_no_key() {
        let json = serde_json::to_string(&row_of(model(), 3)).expect("json");

        assert!(!json.contains("s3cret-peer-key"), "{json}");
        assert!(!json.contains("apiKey"), "{json}");
        assert!(!json.contains("api_key"), "{json}");
        // ...and whether there *is* one still comes through, because "refusing
        // everything" and "never given a credential" are different problems.
        assert!(json.contains(r#""hasKey":true"#), "{json}");
        assert!(json.contains(r#""queued":3"#), "{json}");
    }

    /// A peer nobody has given a key to says so rather than looking configured.
    #[test]
    fn a_peer_with_no_key_says_so() {
        let row = row_of(
            downstream::Model {
                api_key: String::new(),
                ..model()
            },
            0,
        );

        assert!(!row.has_key);
    }

    /// The scope round-trips through the column as the same **Selection** the
    /// live feed speaks, so what an Operator ticked is what routing asks.
    #[test]
    fn the_scope_is_the_selection_the_live_feed_speaks() {
        let row = row_of(model(), 0);

        assert!(row.scope.selects(11, 54241), "the System's wildcard");
        assert!(!row.scope.selects(22, 54241), "another System");
        assert_eq!(
            serde_json::from_str::<Selection>(&scope_json(&row.scope)).expect("round trip"),
            row.scope
        );
    }

    /// A stored scope that will not parse reads as **nothing selected**, in the
    /// listing as well as in the sender — so the screen shows an Operator the
    /// same empty scope that routing is actually applying, rather than a shape
    /// it invented.
    #[test]
    fn an_unreadable_stored_scope_reads_as_empty_on_the_screen_too() {
        let row = row_of(
            downstream::Model {
                scope: String::from("<not json>"),
                ..model()
            },
            0,
        );

        assert!(row.scope.is_all_off());
    }
}
