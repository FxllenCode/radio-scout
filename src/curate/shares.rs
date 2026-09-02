//! **Share links**, as an Operator sees them (#64, spec US 32).
//!
//! The one thing an Operator does about a link a **Listener** minted is *revoke*
//! it, so this is a listing and a `DELETE` and nothing else — there is no
//! create, because minting is the Listener's, and no edit, because the only
//! field worth changing is the expiry and the answer to a link that has run out
//! is to share the Call again.
//!
//! It lives here rather than in [`crate::share`] for the reason
//! [`super::tones`] lives here rather than in [`crate::tone`]: everything under
//! `/api/admin/` belongs to one router so the session guard is a property of
//! the *prefix* rather than something each handler remembers.
//!
//! # The token is not on this screen
//!
//! [`ShareRow`] has no field for it — the [`super::webhooks`] rule, and for its
//! reason: it is the credential, and a screen that showed it would be a place it
//! could leak from that has nothing to do with sharing a Call. What the listing
//! shows instead is **the Call the link opens**, which is the thing an Operator
//! is actually asking about ("what is being shared from my instance?"), and
//! which they can already open in the app.
//!
//! Revoking **deletes the row**, so that URL is dead for good: a token is 128
//! random bits and is never reissued to the same value. A Listener may share the
//! Call again afterwards, and what they get is a *new* link — which is exactly
//! what revoking a leaked URL should mean.

use std::collections::HashMap;

use axum::extract::{Path, Query, State};
use serde::Serialize;

use super::{Rejected, Removed, What};
use crate::AppState;
use crate::call::StoredCall;
use crate::db::repo;
use crate::failure::{Failure, Stage};
use crate::query::{Filtered, Page, Params};

/// How many links a page carries by default, and the ceiling a request may ask
/// for.
///
/// **Paged, not capped.** Minting takes no credential, so the number of rows is
/// the number of **Calls** rather than the number of times somebody pressed a
/// button — and a listing that showed the newest 200 and stopped would leave
/// every older link invisible *and* unrevocable, which is the one thing this
/// screen exists for. `curate::talkgroups`' numbers, since this is the same
/// question asked of a table that can grow the same way.
const DEFAULT_LIMIT: u64 = 100;
const MAX_LIMIT: u64 = 500;

/// One share link, as the screen lists it — **never the token**.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ShareRow {
    pub id: i64,
    /// When it stops working.
    pub expires_at_ms: i64,
    /// When this Call first started being shared — untouched by a re-mint, so
    /// the column answers "since when" rather than "who clicked most recently".
    pub created_at_ms: i64,
    /// Whether it has already run out. Computed against the Instance's own
    /// clock rather than left to the browser's, so a screen open on a phone
    /// whose clock is wrong still agrees with the link.
    pub expired: bool,
    /// What the link opens — the Archive's own view of the Call, so this screen
    /// invents no second shape for one (#98's denormalizer, unchanged).
    pub call: StoredCall,
}

/// `GET /api/admin/shares` — one page of links, newest first.
pub async fn list(
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Page<ShareRow>, Failure> {
    let (limit, offset) = window(&params)?;
    let (links, count) = repo::shares(&state.db, limit, offset)
        .await
        .map_err(Stage::Curate.failed())?;
    let ids: Vec<i64> = links.iter().map(|link| link.call_id).collect();
    let calls = repo::find_calls(&state.db, &ids)
        .await
        .map_err(Stage::Curate.failed())?;
    // One denormalizing pass for the whole page rather than one per row (#86).
    let mut views: std::collections::HashMap<i64, StoredCall> =
        crate::archive::stored_calls(&state.db, &calls)
            .await
            .map_err(Stage::Curate.failed())?
            .into_iter()
            .map(|view| (view.id, view))
            .collect();

    let now_ms = state.clock.now_ms();
    let rows = links
        .into_iter()
        .filter_map(|link| {
            let call = views.remove(&link.call_id)?;
            Some(ShareRow {
                id: link.id,
                expires_at_ms: link.expires_at_ms,
                created_at_ms: link.created_at_ms,
                expired: !crate::share::live_at(link.expires_at_ms, now_ms),
                call,
            })
        })
        .collect();
    Ok(Page::new(rows, count, limit, offset))
}

/// Which page was asked for — the read surfaces' own parameters, so this screen
/// pages the way every other paged listing here does.
fn window(params: &HashMap<String, String>) -> Filtered<(u64, u64)> {
    let params = Params::new(params);
    Ok((params.limit(DEFAULT_LIMIT, MAX_LIMIT)?, params.offset()?))
}

/// `DELETE /api/admin/shares/{id}` — revoke one.
pub async fn remove(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Removed, Failure> {
    match repo::delete_share(&state.db, id)
        .await
        .map_err(Stage::Curate.failed())?
    {
        0 => Err(Rejected::NotFound(What::ShareLink).into()),
        _ => Ok(Removed),
    }
}
