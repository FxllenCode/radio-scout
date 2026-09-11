//! **Events**, curated from the browser (#67, spec US 38).
//!
//! Assembling an incident: a row with a name, a set of frozen members, and a
//! share link that is on or off. It lives here rather than in [`crate::event`]
//! for the reason [`super::tones`] lives here rather than in [`crate::tone`] —
//! everything under `/api/admin/` belongs to one router, so the session guard is
//! a property of the **prefix** rather than something each handler remembers.
//!
//! # Why this is behind the gate at all
//!
//! Spec US 38 is a **Listener**'s story, and every other Listener-facing mark
//! this project has shipped is unauthenticated: a **Star** takes no credential,
//! a **Share link** takes none. Both are bounded — a Star by a window an
//! Operator sets and by the size cap that outranks it, a link by the Calls table
//! itself. Freezing is bounded by *nothing at all*: it copies audio, and
//! **Retention** is then structurally unable to reclaim it, which is the entire
//! point of the feature. An unauthenticated POST that permanently commits a Pi's
//! SD card is the thing #66 refused to build, so what a Listener gets here is
//! the *reading* half — the share page, its audio, and both downloads.
//!
//! # Two verbs, and only one of them is a write to the store
//!
//! Creating, renaming and sharing are rows. **Adding and removing members moves
//! bytes**, which is why those are their own routes with their own stage
//! ([`crate::failure::Stage::CurateEvent`]) and their own report: an Operator who
//! multi-selected fifty Calls is owed an answer about all fifty, and "the store
//! was down for nine of them" is a different sentence from "nine of them had
//! already aged out".
//!
//! # Deleting is the one thing here that destroys audio
//!
//! ...and it is deliberately **not** guarded by [`super::Force`] the way deleting
//! a System is. That guard exists because deleting a Talkgroup would take Calls
//! an Operator did not know were behind it; an Event *is* its members, the screen
//! shows how many, and a confirmation the client already asks for is the right
//! place for "are you sure" rather than a second round trip.

use std::collections::HashMap;

use axum::extract::{Path, Query, State};
use serde::{Deserialize, Serialize};

use super::{Created, Rejected, Removed, What, cleared, nullable, optional_text, required_name};
use crate::AppState;
use crate::call::{CallId, StoredCall};
use crate::db::entities::event;
use crate::db::repo;
use crate::event::{Added, call_of};
use crate::failure::{Failure, Stage};
use crate::query::{Filtered, Page, Params};

/// How many Calls one request may freeze.
///
/// A **constant and not a setting**, unlike `[export] max_calls`: an export is a
/// stranger's request and this one takes the admin session, so what is bounded
/// here is not abuse but a Pi. Each Call is an object read and an object
/// written, sequentially, inside one HTTP request — so a select-all over a
/// county would be a request that outlives its own timeout while the work went
/// on behind it. Five hundred is ten screens of results; above it the refusal
/// names the count and an Operator adds in batches.
const MAX_PER_REQUEST: usize = 500;

/// How many Events a page carries by default, and the ceiling a request may ask
/// for.
///
/// Paged rather than listed whole, unlike the Groups and Tags next door: those
/// are counted in tens on the largest instance anybody runs, and an Event is a
/// thing an Operator makes every time something happens.
const DEFAULT_LIMIT: u64 = 50;
const MAX_LIMIT: u64 = 200;

/// One Event, as a listing shows it — **never the share token**.
///
/// [`super::webhooks::WebhookRow`]'s rule and for its reason: the whole link is
/// the credential, so no future edit to this shape can start returning one. What
/// a screen needs instead is whether it is shared *at all*, which is the
/// question an Operator is asking, plus the link itself — which the client is
/// given only at the moment it turns sharing on, by [`Shared`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EventRow {
    pub id: i64,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    /// How many Calls are frozen into it, and how many bytes they come to — one
    /// `GROUP BY` for the whole page rather than two statements per row (#86).
    pub calls: u64,
    pub bytes: u64,
    /// Whether a link to it is live. The link itself is not here.
    pub shared: bool,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

crate::answers_json!(EventRow);

/// One Event with its members — what opening it shows.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EventDetail {
    #[serde(flatten)]
    pub event: EventRow,
    /// The frozen Calls, oldest first. Deliberately the wire shape a search page
    /// answers with, so a screen that can draw a search row can draw one of
    /// these and the client invents no second Call type (#65's manifest rule).
    ///
    /// **`members`, not `calls`**, because [`EventRow`] already spends that key
    /// on the *count* — and a flattened row beside an array of the same name is
    /// one key in the document rather than two, with whichever serde wrote last
    /// silently winning. That is not a naming preference: it shipped that way
    /// for the length of one test run, and what it cost was the count.
    pub members: Vec<EventMember>,
    /// What the request that produced this document did to the Calls it named —
    /// present on a **create** and absent on a read.
    ///
    /// It is here rather than in a wrapper because the two things a client wants
    /// after assembling an incident are *the Event* (to open it) and *what
    /// happened to the fifty Calls I selected* (to say so), and a nested shape
    /// would make the common read carry an envelope it has no use for.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub added: Option<Added>,
}

crate::answers_json!(EventDetail);

/// One frozen member.
///
/// **`memberId`, not `id`** — and that is [`EventDetail`]'s own `members`-not-
/// `calls` note paid a second time, one struct below where it was written. A
/// flattened [`StoredCall`] carries an `id` of its own (the Call's, and never
/// `skip_serializing_if`), so a field called `id` here serialized to the same
/// key and the *last* one written won. The document said `id` and meant the
/// Call, while the client read it as the member and used it to address a
/// delete. `tests/events.rs::a_member_says_which_call_it_was_and_which_member_it_is`
/// is what would notice now.
///
/// So the flattened document **is** a Call — `id` is the Call's, exactly as it
/// is on a search row, a live frame and an export manifest — and what this adds
/// is the member's own id beside it. There is no separate `callId`: it would be
/// a third spelling of `call.id`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EventMember {
    /// What a remove, and the audio route, address — not the Call's id, which
    /// may name a row that no longer exists.
    pub member_id: i64,
    pub added_at_ms: i64,
    #[serde(flatten)]
    pub call: StoredCall,
}

/// The link, handed over exactly once — when an Operator turns sharing on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Shared {
    /// `/e?t=…`, as a **path**: the browser turning sharing on is the side that
    /// knows which origin it reached this Instance on, and an Instance may not
    /// know its own address at all ([`crate::share::Minted`]'s rule).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    pub shared: bool,
}

crate::answers_json!(Shared);

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NewEvent {
    pub name: String,
    pub notes: Option<String>,
    /// The Calls to freeze straight away — which is how the screen is really
    /// used, because an Event is made *from* a multi-selection rather than made
    /// and then filled.
    #[serde(default)]
    pub call_ids: Vec<CallId>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EventPatch {
    pub name: Option<String>,
    #[serde(default, deserialize_with = "nullable")]
    pub notes: Option<Option<String>>,
    /// Turn the share link on or off.
    ///
    /// **Off is a revoke**: the token is cleared, so that URL is dead for good
    /// and sharing again mints a different one. It has to be — this toggle is
    /// the *only* revoke an Operator has here, and one that merely closed the
    /// door would leave a leaked link with no way to kill it at all. #64's rule,
    /// which deletes the row for the same reason.
    ///
    /// Turning it on when it is already on changes nothing, which is what keeps
    /// a screen that re-submits a form from quietly breaking a live link.
    pub shared: Option<bool>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AddCalls {
    pub call_ids: Vec<CallId>,
}

/// `GET /api/admin/events` — one page of Events, newest first.
pub async fn list(
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Page<EventRow>, Failure> {
    let (limit, offset) = window(&params)?;
    let (rows, count) = repo::events(&state.db, limit, offset)
        .await
        .map_err(Stage::Curate.failed())?;
    Ok(Page::new(
        rows_of(&state, rows).await?,
        count,
        limit,
        offset,
    ))
}

/// Which page was asked for — the read surfaces' own parameters.
fn window(params: &HashMap<String, String>) -> Filtered<(u64, u64)> {
    let params = Params::new(params);
    Ok((params.limit(DEFAULT_LIMIT, MAX_LIMIT)?, params.offset()?))
}

/// Put the two totals beside a page of Events — one statement for all of them.
async fn rows_of(state: &AppState, rows: Vec<event::Model>) -> Result<Vec<EventRow>, Failure> {
    let ids: Vec<i64> = rows.iter().map(|row| row.id).collect();
    let totals = repo::event_totals(&state.db, &ids)
        .await
        .map_err(Stage::Curate.failed())?;
    Ok(rows
        .into_iter()
        .map(|row| {
            let totals = totals.get(&row.id).copied().unwrap_or_default();
            row_of(row, totals)
        })
        .collect())
}

fn row_of(row: event::Model, totals: repo::EventTotals) -> EventRow {
    EventRow {
        id: row.id,
        name: row.name,
        notes: row.notes,
        calls: totals.calls,
        bytes: totals.bytes,
        shared: row.share_token.is_some(),
        created_at_ms: row.created_at_ms,
        updated_at_ms: row.updated_at_ms,
    }
}

/// `GET /api/admin/events/{id}` — one Event and every Call frozen into it.
///
/// Unwindowed on purpose. An Event is curated by hand, the client needs the
/// whole of it to play through, and paging a list somebody assembled themselves
/// would be a control that gets in the way of the one screen where the size is
/// already known — and it is bounded anyway, because [`MAX_PER_REQUEST`] bounds
/// what one request can put in.
///
/// The **share page** does not page either: it renders the first
/// [`crate::event::page::MEMBERS_SHOWN`] and **says so**, because that one is
/// handed to strangers and a stranger who was sent an incident has to be able to
/// tell "that is all of it" from "that is the start of it". What the rest is
/// reachable through is the download the same page offers.
pub async fn detail(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<EventDetail, Failure> {
    detail_of(&state, id).await
}

/// The same document, for a caller that already holds the state.
///
/// `create` and `add` both answer with it, and both used to reach it by calling
/// the handler through hand-built extractors — which re-issued the lookup and
/// read as a route calling a route.
async fn detail_of(state: &AppState, id: i64) -> Result<EventDetail, Failure> {
    let row = found(state, id).await?;
    let frozen = repo::event_members(&state.db, id, 0, 0)
        .await
        .map_err(Stage::Curate.failed())?;
    let bytes = frozen
        .iter()
        .map(|member| member.audio_size.max(0) as u64)
        .sum();

    let members = frozen
        .iter()
        .map(|member| EventMember {
            member_id: member.id,
            added_at_ms: member.added_at_ms,
            call: StoredCall {
                // Through the **member**, never `/api/call/{id}/audio`: the Call
                // may be long gone, and the bytes an Event plays are its own
                // copy of them.
                audio_url: (!member.object_key.is_empty()).then(|| member_audio_url(id, member.id)),
                ..call_of(member)
            },
        })
        .collect::<Vec<_>>();

    Ok(EventDetail {
        // **Summed from the rows already in hand**, rather than through
        // [`repo::event_totals`]: they are the same column over the same rows,
        // so the two cannot disagree, and asking SQL for what is already in
        // memory would be a statement bought for nothing.
        event: row_of(
            row,
            repo::EventTotals {
                calls: members.len() as u64,
                bytes,
            },
        ),
        members,
        added: None,
    })
}

/// Where one member's audio is, for an Operator who is signed in.
///
/// Its own route rather than the share link's, because an Event that is **not
/// shared** still has to be playable on the screen that curates it — and because
/// a page that reached for the token would be a screen holding a credential it
/// has no use for.
pub fn member_audio_url(event_id: i64, member_id: i64) -> String {
    format!("/api/admin/events/{event_id}/calls/{member_id}/audio")
}

/// `GET /api/admin/events/{id}/calls/{member}/audio` — one frozen copy.
///
/// [`crate::serve`]'s decision through a third door (the Archive's is the first,
/// a **Share link**'s the second), so ranges, presigning and the cache policy are
/// that module's rather than rebuilt here — which is what makes an Event play on
/// both storage backends for free.
pub async fn member_audio(
    State(state): State<AppState>,
    Path((id, member_id)): Path<(i64, i64)>,
    headers: axum::http::HeaderMap,
) -> Result<crate::serve::Audio, Failure> {
    let member = repo::find_event_member(&state.db, id, member_id)
        .await
        .map_err(Stage::CurateEvent.failed())?
        .ok_or(Rejected::NotFound(What::EventCall))?;
    let call = call_of(&member);

    crate::serve::serve_frozen(
        &state,
        &member.object_key,
        call.audio_mime.as_deref(),
        &headers,
    )
    .await
}

/// `POST /api/admin/events` — a named incident, optionally filled in one go.
pub async fn create(
    State(state): State<AppState>,
    axum::Json(body): axum::Json<NewEvent>,
) -> Result<Created<EventDetail>, Failure> {
    let name = required_name(&body.name, "name")?;
    within_reach(&body.call_ids)?;
    let row = repo::insert_event(
        &state.db,
        &name,
        optional_text(body.notes).as_deref(),
        state.clock.now_ms(),
    )
    .await
    .map_err(Stage::CurateEvent.failed())?;

    // **A create that could not be filled is undone.** The row and the members
    // are two writes and nothing can make them one — freezing copies *objects*,
    // which no database transaction reaches. So the compensation is explicit: an
    // Event named and left empty by a 500 is one an Operator retries, and a
    // retry that accumulates duplicates is worse than a failure leaving nothing
    // behind. If the compensation fails too — the database is refusing, which is
    // the only thing `freeze` returns an `Err` for — what survives is an empty
    // Event on the listing, which is visible and deletable.
    let added = match crate::event::freeze(&state, row.id, &body.call_ids).await {
        Ok(added) => added,
        Err(failure) => {
            // **`?cause`, and this line is the only record there is.** A
            // `Failure` writes its ERROR line when it is rendered into a
            // response (ADR-0011 rule 4), and this one is dropped — the caller
            // is told about the *freeze* that failed, which is the useful
            // answer. So what the undo hit has to be said here or nowhere.
            if let Err(cause) = discard(&state, row.id).await {
                tracing::warn!(
                    event_id = row.id,
                    ?cause,
                    "could not undo an event that failed to fill; it is empty and can be deleted"
                );
            }
            return Err(failure);
        }
    };
    // INFO, not DEBUG: this is an Operator deliberately committing storage that
    // **Retention** will never reclaim, and it is the one line that says an
    // incident was kept. One per curated Event is not a hot loop (rule 8).
    tracing::info!(
        event_id = row.id,
        frozen = added.frozen,
        missing = added.missing,
        unreadable = added.unreadable,
        "event created"
    );
    Ok(Created(EventDetail {
        added: Some(added),
        ..detail_of(&state, row.id).await?
    }))
}

/// `PATCH /api/admin/events/{id}` — rename it, annotate it, share it or stop.
pub async fn update(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    axum::Json(body): axum::Json<EventPatch>,
) -> Result<EventRow, Failure> {
    let row = found(&state, id).await?;
    let name = body
        .name
        .map(|name| required_name(&name, "name"))
        .transpose()?;

    let edit = repo::EventEdit {
        name,
        notes: body.notes.map(cleared),
        share_token: token_change(body.shared, &row.share_token),
    };
    let updated = repo::update_event(&state.db, id, edit, state.clock.now_ms())
        .await
        .map_err(Stage::CurateEvent.failed())?
        .ok_or(Rejected::NotFound(What::Event))?;

    let totals = repo::event_totals(&state.db, &[id])
        .await
        .map_err(Stage::Curate.failed())?
        .get(&id)
        .copied()
        .unwrap_or_default();
    Ok(row_of(updated, totals))
}

/// `GET /api/admin/events/{id}/share` — the link, for the screen that offers it.
///
/// A read rather than a field on [`EventRow`], so the token reaches exactly one
/// response: the one an Operator asked for by pressing *copy link*. A listing
/// that carried it would put a credential into every page of every screen that
/// renders Events.
pub async fn share(State(state): State<AppState>, Path(id): Path<i64>) -> Result<Shared, Failure> {
    let row = found(&state, id).await?;
    Ok(Shared {
        url: row.share_token.as_deref().map(crate::event::link_to),
        shared: row.share_token.is_some(),
    })
}

/// `POST /api/admin/events/{id}/calls` — freeze more Calls into it.
///
/// Answers with the **whole Event again**, report and all — the same document
/// [`create`] answers with, so "Calls were added to an Event" has one shape on
/// the wire rather than two that a client has to tell apart. It also happens to
/// be what the screen needs next: the member list it is about to redraw.
pub async fn add(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    axum::Json(body): axum::Json<AddCalls>,
) -> Result<EventDetail, Failure> {
    found(&state, id).await?;
    within_reach(&body.call_ids)?;
    let added = crate::event::freeze(&state, id, &body.call_ids).await?;
    Ok(EventDetail {
        added: Some(added),
        ..detail_of(&state, id).await?
    })
}

/// Refuse a batch this Instance should not try to copy in one request.
fn within_reach(call_ids: &[CallId]) -> Result<(), Rejected> {
    match call_ids.len() > MAX_PER_REQUEST {
        true => Err(Rejected::TooManyCalls {
            calls: call_ids.len() as u64,
            max: MAX_PER_REQUEST as u64,
        }),
        false => Ok(()),
    }
}

/// `DELETE /api/admin/events/{id}/calls/{member}` — let one member go.
pub async fn remove_call(
    State(state): State<AppState>,
    Path((id, member_id)): Path<(i64, i64)>,
) -> Result<Removed, Failure> {
    found(&state, id).await?;
    let member = repo::find_event_member(&state.db, id, member_id)
        .await
        .map_err(Stage::CurateEvent.failed())?
        .ok_or(Rejected::NotFound(What::EventCall))?;

    crate::event::release(&state, std::slice::from_ref(&member)).await?;
    repo::touch_event(&state.db, id, state.clock.now_ms())
        .await
        .map_err(Stage::CurateEvent.failed())?;
    Ok(Removed)
}

/// `DELETE /api/admin/events/{id}` — the Event, and the copies it was holding.
pub async fn remove(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Removed, Failure> {
    found(&state, id).await?;
    let released = discard(&state, id).await?;

    // INFO for [`create`]'s reason, and it carries the bytes because this is the
    // only place they come back.
    tracing::info!(
        event_id = id,
        members = released.members,
        bytes_freed = released.bytes_freed,
        object_errors = released.object_errors,
        "event deleted"
    );
    Ok(Removed)
}

/// What the `shared` flag does to the stored token.
///
/// **Off clears it, and that is a revoke.** This toggle is the only one an
/// Operator has, so a link they turn off has to be dead for good — otherwise a
/// URL that leaked could never be killed, only hidden for a while. #64 deletes
/// its row for exactly that reason, and a token is 128 random bits, so the one
/// that comes back next time is a different link.
///
/// `None` means "leave the stored value alone", which is both *unmentioned* and
/// *already the way you asked* — the two cases a handler must not tell apart,
/// because writing a token back over an identical request is an update that
/// changes nothing and a chance to hand out a URL nobody asked for.
fn token_change(shared: Option<bool>, held: &Option<String>) -> Option<Option<String>> {
    shared.and_then(|shared| match (shared, held) {
        (true, Some(_)) | (false, None) => None,
        (true, None) => Some(Some(crate::share::new_token())),
        (false, Some(_)) => Some(None),
    })
}

/// Let an Event and every copy it is holding go.
///
/// **Members first, then the row**, which is the order that makes a crash
/// survivable: a half-deleted Event still names its remaining copies, where an
/// Event row deleted first would leave members no query can reach and no sweep
/// can reclaim — the one way this feature could leak storage for good.
///
/// Two callers, one pass, for [`crate::retention::prune`]'s reason: a delete
/// written twice is how audio gets stranded in a bucket.
async fn discard(state: &AppState, id: i64) -> Result<crate::event::Released, Failure> {
    let members = repo::event_members(&state.db, id, 0, 0)
        .await
        .map_err(Stage::CurateEvent.failed())?;
    let released = crate::event::release(state, &members).await?;
    repo::delete_event(&state.db, id)
        .await
        .map_err(Stage::CurateEvent.failed())?;
    Ok(released)
}

/// The Event this path names, or the refusal for one that is not there.
async fn found(state: &AppState, id: i64) -> Result<event::Model, Failure> {
    repo::find_event(&state.db, id)
        .await
        .map_err(Stage::CurateEvent.failed())?
        .ok_or_else(|| Rejected::NotFound(What::Event).into())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **Sharing an Event that is already shared changes nothing**, so a screen
    /// that re-submits its form cannot quietly break a live link — and turning
    /// it *off* clears the token, because this toggle is the only revoke an
    /// Operator has and a link they revoked must be dead for good (#64's rule).
    #[test]
    fn turning_sharing_on_twice_keeps_the_token() {
        let held = Some(String::from("already-out-there"));

        assert_eq!(token_change(Some(true), &held), None, "already on");
        assert_eq!(token_change(Some(false), &None), None, "already off");
        assert_eq!(
            token_change(Some(false), &held),
            Some(None),
            "off clears it"
        );
        assert_eq!(
            token_change(None, &held),
            None,
            "unmentioned changes nothing"
        );
    }

    /// ...and turning it on for the first time mints one.
    #[test]
    fn sharing_an_unshared_event_mints_a_token() {
        let minted = token_change(Some(true), &None).expect("a change");

        let token = minted.expect("a token");
        assert_eq!(token.len(), 32, "{token}");
        assert!(token.chars().all(|c| c.is_ascii_hexdigit()), "{token}");
    }
}
