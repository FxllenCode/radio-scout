//! **Starring a Call** (#66, spec US 37) — the cheapest form of memory the
//! Archive gets.
//!
//! # Whose mark it is
//!
//! A **Star** is the Instance's, not a browser's. That is the whole design, and
//! it is two refusals rather than a preference:
//!
//! - **No starrer is recorded, because there is nowhere for one to go.** A
//!   `call_stars(call_id, starrer)` table would give each browser its own
//!   shortlist and would cost the two things this project has already refused
//!   twice: a per-browser record of what somebody kept — which is the listening
//!   history ADR-0011 rule 5 exists to stop this process accumulating, and why
//!   #62 gave `listener_samples` three columns — and a table whose rows are
//!   Calls × browsers rather than bounded by the Calls table, which is #64's
//!   abuse bound inverted. Minting takes no credential here for the same reason
//!   it takes none there: a **Listener** holds none.
//! - **So the shortlist is shared**, and "starred" means *somebody* on this
//!   Instance thought this Call mattered. On the county instance this feature
//!   was designed against that is a handful of people, and what it costs is the
//!   thing that made it affordable at all.
//!
//! # What it is worth
//!
//! Nothing, until an Operator says otherwise: `[retention] starred_days` is
//! absent by default, so a Star is a bookmark. An unauthenticated POST that
//! committed a Pi's SD card would be a feature nobody could safely leave on,
//! which is why the exemption is a **window** rather than the boolean the
//! ticket's wording suggests — see [`crate::retention::RetentionConfig`], and
//! [`crate::retention::sweep`] for why the size cap outranks it anyway.
//!
//! What the Instance will do is on the wire ([`crate::catalog::StarOffer`]),
//! `sharing`'s precedent one step on: a control that quietly means less than a
//! Listener thinks it does is a control that lies.

use axum::extract::{Path, State};
use serde::Serialize;

use crate::AppState;
use crate::call::CallId;
use crate::failure::{Failure, Reason, Stage};

/// What a **Star** is worth on this Instance, as [`crate::AppState`] holds it.
///
/// One field read off `[retention] starred_days`, rather than the whole
/// [`crate::retention::RetentionConfig`] in every handler's hands — the
/// [`crate::share::Shares`] shape and for its reason. The policy itself belongs
/// to Retention, which is the only thing that *acts* on it; this is the surface
/// that has to be able to say what it is.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Stars {
    kept_days: Option<u32>,
}

impl Stars {
    /// The read-only half of this Instance's retention policy.
    pub fn of(config: &crate::retention::RetentionConfig) -> Self {
        Stars {
            kept_days: config.starred_days,
        }
    }

    /// How many days past the transmission a Star keeps a Call — `Some(0)` for
    /// good, `None` when a Star keeps nothing, which is what ships.
    pub fn kept_days(&self) -> Option<u32> {
        self.kept_days
    }
}

/// What a Call's Star is now — the answer to both verbs, so a client that
/// missed a response does not have to guess which way the toggle went.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Starred {
    pub starred: bool,
}

crate::answers_json!(Starred);

/// `POST /api/call/{id}/star` — mark this Call.
///
/// Idempotent: the mark is the Instance's, so a second Listener starring what a
/// first already starred has nothing to add and nothing to be refused over.
pub async fn star(
    State(state): State<AppState>,
    Path(id): Path<CallId>,
    viewer: crate::access::Viewer,
) -> Result<Starred, Failure> {
    set(&state, &viewer, id, Some(state.clock.now_ms())).await
}

/// `DELETE /api/call/{id}/star` — take the mark off again.
pub async fn unstar(
    State(state): State<AppState>,
    Path(id): Path<CallId>,
    viewer: crate::access::Viewer,
) -> Result<Starred, Failure> {
    set(&state, &viewer, id, None).await
}

/// Both verbs, which differ only in the instant they write.
///
/// One statement: the update's own row count is what says whether the Call was
/// there, so a typo'd id costs a read that a `find_call` first would have made
/// anyway — and there is no window in which a Call is pruned between the two.
async fn set(
    state: &AppState,
    viewer: &crate::access::Viewer,
    id: CallId,
    at_ms: Option<i64>,
) -> Result<Starred, Failure> {
    // A **Star** is the Instance's mark and anybody may leave one — but only on
    // a Call they could have heard (#68). Without this, a gated Call is
    // unreachable and still starrable, and "Starred" would be a list holding
    // rows that answer nothing when opened.
    if !crate::access::reaches_call(&state.db, &viewer.scope, id)
        .await
        .map_err(Stage::SetStar.failed())?
    {
        return Err(Reason::CallNotFound.into());
    }
    let found = crate::db::repo::set_star(&state.db, id, at_ms)
        .await
        .map_err(Stage::SetStar.failed())?;
    if !found {
        return Err(Reason::CallNotFound.into());
    }

    // **DEBUG, not INFO** — [`crate::share::create`]'s reasoning exactly. The
    // durable record that a Call is starred is the *column*; a line beside it
    // tells an Operator nothing they act on, and this is an unauthenticated
    // write, so at INFO somebody POSTing in a loop would also write a row per
    // request into the operator log (ADR-0011 rule 8).
    tracing::debug!(call_id = id, starred = at_ms.is_some(), "star set");
    Ok(Starred {
        starred: at_ms.is_some(),
    })
}
