//! **Events** — incidents that outlive **Retention** (#67, spec US 38).
//!
//! A named collection of **Calls**, assembled by hand, whose audio is **copied**
//! when it is curated. The one thing in the **Archive** that is meant to outlive
//! it.
//!
//! rdio-scanner has nothing like this at all: its archive is a list you re-find
//! things in, and when the retention window passes over an incident the incident
//! is gone.
//!
//! # Seven things worth knowing
//!
//! **A member is a snapshot, not a pointer, and that is the whole feature.**
//! `event_calls.call_id` carries no foreign key, because the row has to still be
//! there after the sweep has taken the Call it was made from. The audio is
//! copied under a key of its own and the Call's own document is frozen beside
//! it, so an Event is readable and playable when nothing it was made from
//! survives. It is [`crate::db::entities::share_link`]'s rule exactly inverted:
//! a share link is a child that goes **with** its Call, which is why
//! [`crate::db::repo::delete_calls`] must name it; a frozen member must survive
//! that same delete, so it must not appear there and must carry nothing that
//! would make it fail.
//!
//! **Copying is what buys a sweep that stays one pass over one archive.** The
//! obvious alternative — leave the bytes where they are and teach **Retention**
//! to spare a Call some Event holds — costs a join on the hot path of every
//! prune, an exemption the size cap would then have to reason about, and a
//! second meaning for `calls.object_key` (sometimes one row's, sometimes two).
//! The spec settled this before the ticket was written, and #66 recorded it
//! while deciding what a **Star** is worth: *Events need no exemption, having
//! copied their audio*. The price is paid once, in bytes, at the moment an
//! Operator says this one matters.
//!
//! **Curating one takes the admin session, and that is a departure from the
//! ticket's own story.** Spec US 38 says "as a listener", and a **Listener**
//! holds no credential — which is exactly why a **Star** is a window rather than
//! a switch and a **Share link** is bounded by the Calls table. Freezing is
//! bounded by *nothing*: it spends disk permanently, and no policy here can
//! reclaim it. So it lives under [`crate::curate`], where the gate is a property
//! of the prefix. A Listener still receives, plays and exports a shared Event;
//! only assembling one is the Operator's.
//!
//! **Frozen bytes are counted by the size cap and never pruned by it.** They are
//! stored audio on the same disk, so a cap that could not see them would stop
//! being true the moment an incident was curated — and `[retention] max_size_gb`
//! exists to stop an SD card filling. See [`crate::retention::sweep`] for what
//! happens when Events alone exceed it, which is the honest and visible thing
//! rather than the quiet one.
//!
//! **An unreadable object is not frozen at all.** A member with an empty key is
//! how an **Encrypted Call** is held — a row, no object, the activity being the
//! fact — so writing one for a Call whose audio merely *would not read* would
//! store a lie in the one table built to be trusted years later. It is counted
//! instead, reported once per request ([`crate::export`]'s rule), and the same
//! Call can be added again when the store is back.
//!
//! **The share link does not expire**, alone among this project's public links.
//! A Call's does because a stranger minted it and a URL in a group chat should
//! not be a permanent endpoint; neither half is true here — the Operator is the
//! one publishing, and an Event is the durable thing by definition, so a link to
//! last winter's tornado that died on Friday would be the wrong promise about
//! the one collection made to last. What replaces the expiry is a **toggle**,
//! and turning it off is a *revoke*: the token is cleared, so that URL is dead
//! for good and sharing again mints a different one (#64's rule, which deletes
//! its row for the same reason). `[share] enabled` still closes every door at
//! once.
//!
//! **Nothing about an Event is a new setting.** It borrows `[share] enabled` for
//! its page and `[export]` for its downloads, because those answer the questions
//! an Operator has already been asked — is this Instance's audio reachable from
//! outside, and how much of it may leave at once.

pub mod page;

use std::collections::HashSet;

use serde::Serialize;

use axum::extract::{Query, State};

use crate::AppState;
use crate::archive::{Exported, Extent};
use crate::blob::StoredAudio;
use crate::call::{CallId, StoredCall};
use crate::db::entities::event_call;
use crate::db::repo;
use crate::failure::{Failure, Stage};
use crate::share::Gone;

/// Where an Event's share page lives, as a path.
///
/// One constant, because the toggle's answer, the page's own `<audio>` sources
/// and the canonical URL in its card must all spell it the same way — and it is
/// deliberately a **sibling of `/s`** rather than under it: both are pages a
/// stranger opens, and a short URL is the thing being pasted into a message.
pub const EVENT_PATH: &str = "/e";

/// ...the bytes of one member...
pub const EVENT_AUDIO_PATH: &str = "/e/audio";

/// ...and the whole incident as a file.
pub const EVENT_EXPORT_PATH: &str = "/e/export";

/// Which member of the Event the audio route is about.
///
/// Its own parameter rather than a path segment for the token's reason: the
/// whole URL is the credential, [`crate::http_log`] logs a path and never a
/// query, and a share surface whose parts were split across the two would make
/// "an Event token is never logged" a thing to keep true rather than a thing
/// that is true.
pub const MEMBER_PARAM: &str = "i";

/// The link to an Event, as a URL.
///
/// `pub` rather than private because the integration harness opens these too,
/// and a test that spelled `/e?t=…` for itself would be a second implementation
/// of this — right up until one of them moved.
pub fn link_to(token: &str) -> String {
    format!("{EVENT_PATH}?{}={token}", crate::share::TOKEN_PARAM)
}

/// ...one member's audio...
pub fn audio_link_to(token: &str, member_id: i64) -> String {
    format!(
        "{EVENT_AUDIO_PATH}?{}={token}&{MEMBER_PARAM}={member_id}",
        crate::share::TOKEN_PARAM
    )
}

/// ...and the download.
pub fn export_link_to(token: &str, format: &str) -> String {
    format!(
        "{EVENT_EXPORT_PATH}?{}={token}&format={format}",
        crate::share::TOKEN_PARAM
    )
}

// ---------------------------------------------------------------------------
// The snapshot
// ---------------------------------------------------------------------------

/// The document a member freezes: the Call as a **Listener** saw it.
///
/// **`audio_url` is cleared first**, deliberately. The Call's own URL points at
/// a row this member is built to outlive, so freezing it would store a link that
/// becomes a lie on the day the feature pays off. Every surface derives its own
/// instead — through the share token, or through the admin route — which is
/// [`crate::share::page::Preview::of`]'s rule one table along.
pub fn snapshot_of(call: &StoredCall) -> String {
    let frozen = StoredCall {
        audio_url: None,
        ..call.clone()
    };
    // A `StoredCall` is plain data with no map keys of its own, so this cannot
    // fail; an empty document still reads back as a Call carrying the columns
    // beside it, which is the fallback below.
    serde_json::to_string(&frozen).unwrap_or_default()
}

/// ...and read back, wearing the frozen copy's key.
///
/// The two halves come from two places on purpose: the snapshot is *the Call as
/// it was*, and `object_key` is *where our copy of it is*. Keeping the second
/// out of the document means there is nothing for them to disagree about.
///
/// **A document that will not parse still answers with a Call.** The columns
/// beside it — which Call, when, how long — are facts SQL kept, so one
/// unreadable row costs its labels rather than the whole Event's page. That is
/// the same direction [`crate::share::page`] fails in: a Call whose stamp no
/// calendar can hold is still playable.
pub fn call_of(member: &event_call::Model) -> StoredCall {
    let mut call: StoredCall = serde_json::from_str(&member.snapshot).unwrap_or_default();
    call.object_key = member.object_key.clone();
    call.audio_url = None;
    if call.id == 0 {
        call.id = member.call_id;
    }
    if call.timestamp.is_none() {
        call.timestamp = Some(member.call_at_ms);
    }
    if call.duration_ms.is_none() {
        call.duration_ms = member.duration_ms;
    }
    call
}

// ---------------------------------------------------------------------------
// Freezing
// ---------------------------------------------------------------------------

/// What one Call's freeze came to.
///
/// A value rather than four branches inside a loop, so every rule about it is
/// something a test constructs — and so the *report* below is a fold rather than
/// four counters somebody has to remember to increment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Froze {
    /// A new member, audio and all.
    Frozen,
    /// This Event already holds that Call. Not a refusal: two Operators
    /// multi-selecting overlapping pages both meant it.
    AlreadyHeld,
    /// No such Call. **Counted rather than fatal**, because an Operator
    /// multi-selecting a page of results may be holding one **Retention** took
    /// while they were reading it, and refusing the batch over it would lose the
    /// nine they meant.
    Missing,
    /// The Call is there and its audio would not read. Counted apart from
    /// `Missing` because they send an Operator to different places — one is a
    /// Call that has gone, the other is a store that has — and **no row is
    /// written**, so adding it again once the store is back really does freeze
    /// it.
    Unreadable,
}

/// What a whole request came to.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Added {
    pub frozen: u64,
    pub already_held: u64,
    pub missing: u64,
    pub unreadable: u64,
}

impl Added {
    /// Fold one Call's ending in.
    pub fn and(mut self, froze: Froze) -> Self {
        match froze {
            Froze::Frozen => self.frozen += 1,
            Froze::AlreadyHeld => self.already_held += 1,
            Froze::Missing => self.missing += 1,
            Froze::Unreadable => self.unreadable += 1,
        }
        self
    }

    /// Whether anything was written — what decides whether the Event's
    /// `updated_at_ms` moves.
    pub fn changed(&self) -> bool {
        self.frozen > 0
    }
}

/// Copy these Calls into an Event.
///
/// Order is the caller's, and duplicates inside one request are answered the
/// same way duplicates across two are: the held set grows as the loop runs, so
/// `[7, 7]` is one member and one `AlreadyHeld` rather than a unique-index
/// violation halfway through.
pub async fn freeze(
    state: &AppState,
    event_id: i64,
    call_ids: &[CallId],
) -> Result<Added, Failure> {
    let mut held: HashSet<CallId> = repo::event_member_call_ids(&state.db, event_id)
        .await
        .map_err(Stage::CurateEvent.failed())?;

    // One read and one denormalizing pass for the whole request rather than one
    // per Call (#86) — an Operator multi-selecting a page is handing us fifty.
    let wanted: Vec<CallId> = call_ids
        .iter()
        .copied()
        .filter(|id| !held.contains(id))
        .collect();
    let rows = repo::find_calls(&state.db, &wanted)
        .await
        .map_err(Stage::CurateEvent.failed())?;
    let views: std::collections::HashMap<CallId, StoredCall> =
        crate::archive::stored_calls(&state.db, &rows)
            .await
            .map_err(Stage::CurateEvent.failed())?
            .into_iter()
            .map(|view| (view.id, view))
            .collect();

    let now_ms = state.clock.now_ms();
    let mut added = Added::default();
    let mut unreadable = Unreadable::default();
    for id in call_ids.iter().copied() {
        if held.contains(&id) {
            added = added.and(Froze::AlreadyHeld);
            continue;
        }
        let Some(call) = views.get(&id) else {
            added = added.and(Froze::Missing);
            continue;
        };
        let froze = freeze_one(state, event_id, call, now_ms, &mut unreadable).await?;
        if froze == Froze::Frozen {
            held.insert(id);
        }
        added = added.and(froze);
    }
    unreadable.report(event_id);

    if added.changed() {
        repo::touch_event(&state.db, event_id, now_ms)
            .await
            .map_err(Stage::CurateEvent.failed())?;
    }
    Ok(added)
}

/// One Call: copy the object if there is one, then write the row that points at
/// it.
///
/// **Object before row**, ADR-0001's order, which a [`StoredAudio`] makes a type
/// rather than a comment — a member row cannot be built without one, and only a
/// completed write produces one. A crash between them leaves an **Orphan** the
/// GC reclaims, where the other order would leave a member whose audio 404s.
async fn freeze_one(
    state: &AppState,
    event_id: i64,
    call: &StoredCall,
    now_ms: i64,
    unreadable: &mut Unreadable,
) -> Result<Froze, Failure> {
    let audio = match copy_audio(state, call, unreadable).await {
        Some(audio) => audio,
        None => return Ok(Froze::Unreadable),
    };
    repo::insert_event_member(
        &state.db,
        event_id,
        call.id,
        call.timestamp.unwrap_or_default(),
        call.duration_ms,
        &audio,
        &snapshot_of(call),
        now_ms,
    )
    .await
    .map_err(Stage::CurateEvent.failed())?;
    Ok(Froze::Frozen)
}

/// The frozen copy of this Call's audio, or nothing at all.
///
/// An **Encrypted Call** has no object and freezes as an empty one — a row, the
/// activity being the fact. Anything else that will not read is `None`, and no
/// member is written for it.
async fn copy_audio(
    state: &AppState,
    call: &StoredCall,
    unreadable: &mut Unreadable,
) -> Option<StoredAudio> {
    if call.object_key.is_empty() {
        return Some(StoredAudio::written(String::new(), 0));
    }
    let bytes = match state.audio.get(&call.object_key).await {
        Ok(Some(bytes)) => bytes,
        // An object the row still points at and the store no longer has is the
        // same answer as a store that refused: nothing to freeze.
        Ok(None) => {
            unreadable.note(call.id, "no such object");
            return None;
        }
        Err(error) => {
            unreadable.note(call.id, error);
            return None;
        }
    };
    let key = crate::blob::new_object_key(&extension_of(call));
    let len = bytes.len();
    match state.audio.put(&key, bytes).await {
        Ok(()) => Some(StoredAudio::written(key, len)),
        Err(error) => {
            unreadable.note(call.id, error);
            None
        }
    }
}

/// What to call the frozen object.
///
/// The key is opaque — nothing reads it back as a filename, and an export names
/// its files from the Call itself ([`crate::archive::export_filename`]) — but
/// the extension is what an Operator sees when they go looking in a bucket with
/// `aws s3 ls`, and a directory of `.bin` would tell them nothing.
///
/// **[`crate::archive::download_extension`]'s answer, not a second one.** It had
/// been a second one, and the second one had already drifted: `audio/x-flac` was
/// in the original and not in the copy. The recorder's own `audio_name` is not
/// part of a snapshot, so `None` is passed and the MIME type decides — which is
/// what that function falls back to anyway.
fn extension_of(call: &StoredCall) -> String {
    crate::archive::download_extension(None, call.audio_mime.as_deref())
}

/// How many of this request's objects would not read, reported **once when it
/// finishes** rather than once per Call.
///
/// [`crate::export::Unreadable`]'s rule, which is the **Mining** sweep's one
/// surface along: a store that has gone away fails every read, and an Operator
/// multi-selecting five hundred Calls must not be answered with five hundred
/// lines (ADR-0011 rule 8).
#[derive(Default)]
struct Unreadable {
    calls: u32,
    /// The first Call it happened to and the first cause, which is the pair
    /// worth saying out loud: five hundred copies of "connection refused" tell
    /// an Operator nothing the first did not.
    first: Option<(CallId, String)>,
}

impl Unreadable {
    fn note(&mut self, call_id: CallId, cause: impl std::fmt::Display) {
        self.calls += 1;
        self.first
            .get_or_insert_with(|| (call_id, cause.to_string()));
    }

    /// WARN, because an Operator must act: they asked for an incident to be kept
    /// and part of it was not. The request answered `200` and its count says how
    /// many — this is where the *reason* is.
    fn report(self, event_id: i64) {
        if let Some((call_id, cause)) = self.first {
            tracing::warn!(
                event_id,
                calls = self.calls,
                first_call_id = call_id,
                %cause,
                "some calls could not be frozen into the event"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Releasing
// ---------------------------------------------------------------------------

/// What one [`release`] reclaimed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Released {
    pub members: u64,
    pub bytes_freed: u64,
    /// Objects whose delete failed. The row is already gone, so the copy is now
    /// an **Orphan** and a later sweep retries it — counted rather than fatal,
    /// so one unhappy object cannot wedge a delete forever.
    pub object_errors: u64,
}

/// Let these members go: **rows first, then objects**.
///
/// [`crate::retention::prune`]'s order and for its reason (ADR-0002, inverted
/// from the write): an object with no row is an **Orphan** the GC reclaims,
/// where a row with no object is a member that plays as a 404 for as long as the
/// Event survives — which is meant to be a very long time.
pub async fn release(state: &AppState, members: &[event_call::Model]) -> Result<Released, Failure> {
    let mut released = Released::default();
    if members.is_empty() {
        return Ok(released);
    }
    let ids: Vec<i64> = members.iter().map(|member| member.id).collect();
    released.members = repo::delete_event_members(&state.db, &ids)
        .await
        .map_err(Stage::CurateEvent.failed())?;

    for member in members {
        // An **Encrypted Call** froze as a row with no object behind it. Asking
        // the store to delete the empty key would fail and be reported as a
        // broken store — once per member, on a System where they may be most of
        // the traffic (`retention::prune`'s own note).
        if member.object_key.is_empty() {
            continue;
        }
        match state.audio.delete(&member.object_key).await {
            Ok(()) => released.bytes_freed += member.audio_size.max(0) as u64,
            Err(error) => {
                // Say *why*, or an Operator gets a count and no lead. The row is
                // already gone, so the object is an orphan the GC pass retries.
                tracing::warn!(
                    object_key = %member.object_key,
                    %error,
                    "could not delete a released event copy"
                );
                released.object_errors += 1;
            }
        }
    }
    Ok(released)
}

// ---------------------------------------------------------------------------
// The public surface: one Event, through its token
// ---------------------------------------------------------------------------

/// A live share link and the Event behind it, or the reason there is neither.
enum Resolved {
    Live(Box<crate::db::entities::event::Model>),
    Gone(Gone),
}

/// Turn a token into the Event it opens.
///
/// **One function for all three routes**, so the page, a member's audio and the
/// download cannot disagree about whether a link is live — which would show a
/// recipient an incident they then cannot play, and read as a broken Instance
/// rather than as a link somebody turned off ([`crate::share::resolve`]'s rule).
///
/// There is no [`Gone::Expired`] here and that is deliberate: an Event's link is
/// a **toggle**, so it is either on or it is not, and the two states a recipient
/// can reach are "no such link" and "this Instance has closed sharing".
async fn resolve(state: &AppState, token: Option<&str>) -> Result<Resolved, Failure> {
    if !state.shares.enabled() {
        return Ok(Resolved::Gone(Gone::Disabled));
    }
    let Some(token) = token.filter(|token| !token.is_empty()) else {
        return Ok(Resolved::Gone(Gone::Unknown));
    };
    match repo::event_by_token(&state.db, token)
        .await
        .map_err(Stage::OpenEvent.failed())?
    {
        Some(row) => Ok(Resolved::Live(Box::new(row))),
        None => Ok(Resolved::Gone(Gone::Unknown)),
    }
}

/// `GET /e?t=…` — the page a recipient opens.
pub async fn open(
    State(state): State<AppState>,
    Query(query): Query<crate::share::TokenQuery>,
) -> Result<crate::share::page::Rendered, Failure> {
    let row = match resolve(&state, query.token()).await? {
        Resolved::Live(row) => row,
        Resolved::Gone(gone) => return Ok(crate::share::page::Rendered::gone(gone)),
    };
    let token = row.share_token.clone().unwrap_or_default();

    // The whole Event's shape in one statement, then only the members the page
    // will draw — so an incident of four hundred Calls costs a page of rows
    // rather than four hundred.
    let whole = extent(&state.db, row.id, false)
        .await
        .map_err(Stage::OpenEvent.failed())?;
    let shown = repo::event_members(&state.db, row.id, page::MEMBERS_SHOWN as u64, 0)
        .await
        .map_err(Stage::OpenEvent.failed())?;

    let members = shown
        .iter()
        .map(|member| {
            let call = call_of(member);
            let audio = (!member.object_key.is_empty()).then(|| audio_link_to(&token, member.id));
            page::Member::of(&call, audio)
        })
        .collect();

    let public_url = state.shares.public_url();
    let offered = |format: &str| {
        state
            .exports
            .enabled()
            .then(|| export_link_to(&token, format))
    };
    Ok(crate::share::page::Rendered::ok(page::render(
        &page::Preview {
            name: row.name.clone(),
            notes: row.notes.clone(),
            members,
            total: whole.calls as usize,
            duration_ms: whole.duration_ms,
            zip_url: offered("zip"),
            stitched_url: offered("wav"),
            canonical: crate::config::absolute_url(&link_to(&token), public_url),
            icon: crate::config::absolute_url(crate::share::page::ICON_PATH, public_url),
        },
    )))
}

/// `GET /e/audio?t=…&i=…` — one member's bytes, through the token.
pub async fn audio(
    State(state): State<AppState>,
    Query(query): Query<MemberQuery>,
    headers: axum::http::HeaderMap,
) -> Result<crate::serve::Audio, Failure> {
    let row = match resolve(&state, query.token.token()).await? {
        Resolved::Live(row) => row,
        Resolved::Gone(gone) => return Err(gone.reason().into()),
    };
    // **Scoped to the Event the token names**, so a member id from another one
    // reaches nothing: a share link opens one incident and nothing else, which
    // is the promise [`crate::share`] makes about one Call.
    let member = repo::find_event_member(&state.db, row.id, query.member.unwrap_or_default())
        .await
        .map_err(Stage::OpenEvent.failed())?
        .ok_or_else(|| Gone::Unknown.reason())?;

    let call = call_of(&member);
    crate::serve::serve_frozen(
        &state,
        &member.object_key,
        call.audio_mime.as_deref(),
        &headers,
    )
    .await
}

/// `GET /e/export?t=…&format=…` — the whole incident as a file.
///
/// The Listener-facing half of the ticket's export criterion: somebody sent an
/// incident, and keeping it should not mean an account on the Instance it came
/// from. It is [`crate::export`]'s own pipeline through a different door — the
/// same refusals, the same one-at-a-time slot and the same streamed body — so
/// `[export] enabled` and `[export] max_calls` govern it exactly as they govern
/// a range.
pub async fn export(
    State(state): State<AppState>,
    Query(query): Query<std::collections::HashMap<String, String>>,
) -> Result<axum::response::Response, Failure> {
    let token = query.get(crate::share::TOKEN_PARAM).map(String::as_str);
    let row = match resolve(&state, token).await? {
        Resolved::Live(row) => row,
        Resolved::Gone(gone) => return Err(gone.reason().into()),
    };
    crate::export::of_event(state, &query, row.id, row.name).await
}

/// The token and which member, together.
#[derive(Debug, serde::Deserialize)]
pub struct MemberQuery {
    #[serde(flatten)]
    token: crate::share::TokenQuery,
    /// Optional so a link with no member names none, which is an ordinary "no
    /// such thing" rather than axum's bare 400 — the [`crate::share::TokenQuery`]
    /// argument, one parameter along.
    #[serde(default, rename = "i")]
    member: Option<i64>,
}

// ---------------------------------------------------------------------------
// What an Event's export is about to be (#67, spec US 38)
// ---------------------------------------------------------------------------

/// **How big this Event's export would be**, in one statement over the same
/// members the export will walk.
///
/// [`crate::archive::extent`]'s counterpart, and deliberately the same
/// [`Extent`] — so [`crate::export`]'s refusals, its headers and its declared
/// WAV timeline are one implementation asked about two subjects rather than two
/// that agree today.
///
/// `with_audio` is the stitched export's filter, spelled here as well as in the
/// page read below because a header that promised a length measured over one set
/// of members and then wrote another would produce a file whose every later Call
/// is the wrong one.
pub async fn extent<C: sea_orm::ConnectionTrait>(
    db: &C,
    event_id: i64,
    with_audio: bool,
) -> Result<Extent, sea_orm::DbErr> {
    use sea_orm::{ColumnTrait, EntityTrait, QuerySelect};

    #[derive(Debug, sea_orm::FromQueryResult)]
    struct Row {
        calls: i64,
        bytes: Option<i64>,
        duration_ms: Option<i64>,
        first_ms: Option<i64>,
    }

    // The two `SUM`s are cast to `BIGINT` for `repo::total_audio_bytes`'s
    // reason: Postgres widens `SUM(bigint)` to `numeric` where SQLite keeps it
    // an integer, and the cast is what makes one query decode on both dialects.
    let row = placeable(event_call::Entity::find(), event_id, with_audio)
        .select_only()
        .column_as(event_call::Column::Id.count(), "calls")
        .column_as(
            event_call::Column::AudioSize
                .sum()
                .cast_as(sea_orm::sea_query::Alias::new("BIGINT")),
            "bytes",
        )
        .column_as(
            event_call::Column::DurationMs
                .sum()
                .cast_as(sea_orm::sea_query::Alias::new("BIGINT")),
            "duration_ms",
        )
        .column_as(event_call::Column::CallAtMs.min(), "first_ms")
        .into_model::<Row>()
        .one(db)
        .await?;

    Ok(row
        .map(|row| Extent {
            calls: row.calls.max(0) as u64,
            bytes: row.bytes.unwrap_or(0).max(0) as u64,
            duration_ms: row.duration_ms.unwrap_or(0).max(0),
            first_ms: row.first_ms,
        })
        .unwrap_or_default())
}

/// One page of an Event's members, as an **export** writes them.
///
/// [`crate::archive::exportable`]'s counterpart, answering the same [`Exported`]
/// — which is what lets [`crate::export`]'s two writers take an Event without
/// knowing what one is.
pub async fn exportable<C: sea_orm::ConnectionTrait>(
    db: &C,
    event_id: i64,
    with_audio: bool,
    limit: u64,
    offset: u64,
) -> Result<Vec<Exported>, sea_orm::DbErr> {
    use sea_orm::{EntityTrait, QueryOrder, QuerySelect};

    let members = placeable(event_call::Entity::find(), event_id, with_audio)
        .order_by_asc(event_call::Column::CallAtMs)
        .order_by_asc(event_call::Column::Id)
        .offset(offset)
        .limit(limit)
        .all(db)
        .await?;

    Ok(members
        .iter()
        .map(|member| {
            let call = call_of(member);
            Exported {
                // **The recorder's own `audio_name` is not frozen**, so the
                // extension comes from the MIME type — which is what
                // `download_extension` falls back to anyway, and the only field
                // of the three that a snapshot could have carried.
                filename: match member.object_key.is_empty() {
                    true => String::new(),
                    false => crate::archive::export_filename(&call, None),
                },
                object_key: member.object_key.clone(),
                call,
            }
        })
        .collect())
}

/// The members a given export can place: all of them for a zip, and only the
/// ones with audio *and* a measured length for a stitch.
///
/// **Written once**, because [`extent`] declares the timeline and [`exportable`]
/// fills it: a filter that moved in one and not the other is a WAV whose header
/// reserved room for Calls the body never writes, which plays as every later
/// Call being the wrong one (#65's own hazard, one subject along).
fn placeable(
    query: sea_orm::Select<event_call::Entity>,
    event_id: i64,
    with_audio: bool,
) -> sea_orm::Select<event_call::Entity> {
    use sea_orm::{ColumnTrait, QueryFilter};

    let query = query.filter(event_call::Column::EventId.eq(event_id));
    match with_audio {
        false => query,
        true => query
            .filter(event_call::Column::ObjectKey.ne(""))
            .filter(event_call::Column::DurationMs.is_not_null()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    fn a_call() -> StoredCall {
        StoredCall {
            id: 42,
            system_ref: 11,
            system_label: Some(String::from("Fulton County")),
            talkgroup_ref: 54241,
            talkgroup_label: Some(String::from("Fire Dispatch")),
            timestamp: Some(1_700_000_000_000),
            duration_ms: Some(7_400),
            audio_mime: Some(String::from("audio/wav")),
            object_key: String::from("ab/original.wav"),
            audio_url: Some(String::from("/api/call/42/audio")),
            ..StoredCall::default()
        }
    }

    fn a_member(snapshot: String) -> event_call::Model {
        event_call::Model {
            id: 7,
            event_id: 1,
            call_id: 42,
            call_at_ms: 1_700_000_000_000,
            duration_ms: Some(7_400),
            object_key: String::from("cd/frozen.wav"),
            audio_size: 1_024,
            snapshot,
            added_at_ms: 1_700_000_500_000,
        }
    }

    /// **The round trip is the feature.** What a Listener saw when the incident
    /// was curated is what they see years later, and the copy's key replaces the
    /// original's — because the original is the row this is built to outlive.
    #[test]
    fn a_frozen_call_reads_back_as_the_call_it_was() {
        let call = a_call();

        let read = call_of(&a_member(snapshot_of(&call)));

        assert_eq!(read.id, call.id);
        assert_eq!(read.system_label, call.system_label);
        assert_eq!(read.talkgroup_label, call.talkgroup_label);
        assert_eq!(read.timestamp, call.timestamp);
        assert_eq!(read.duration_ms, call.duration_ms);
        assert_eq!(read.audio_mime, call.audio_mime);
        assert_eq!(
            read.object_key, "cd/frozen.wav",
            "the frozen copy's key, not the Call's"
        );
    }

    /// **The Call's own audio URL is never frozen.** It points at a row the
    /// member outlives, so keeping it would store a link that becomes a lie on
    /// the day this feature pays off; every surface derives its own instead.
    #[test]
    fn a_snapshot_carries_no_audio_url() {
        let frozen = snapshot_of(&a_call());

        assert!(!frozen.contains("audioUrl"), "{frozen}");
        assert_eq!(call_of(&a_member(frozen)).audio_url, None);
    }

    /// A document this release wrote and a later one reads gets the new field's
    /// default rather than a parse error — which is what makes a snapshot safe
    /// to keep for as long as an Event is meant to last.
    #[test]
    fn a_snapshot_missing_a_field_reads_as_that_fields_default() {
        let read = call_of(&a_member(String::from(r#"{"id":42,"talkgroupRef":9}"#)));

        assert_eq!(read.talkgroup_ref, 9);
        assert!(!read.emergency);
        assert!(read.patches.is_empty());
        assert_eq!(read.system_label, None);
    }

    /// ...and a document that will not parse at all still answers with the Call
    /// the columns beside it describe, so one bad row costs its labels rather
    /// than the whole Event's page.
    #[test]
    fn an_unreadable_snapshot_falls_back_to_the_columns() {
        let read = call_of(&a_member(String::from("{not json")));

        assert_eq!(read.id, 42, "which Call");
        assert_eq!(read.timestamp, Some(1_700_000_000_000), "when");
        assert_eq!(read.duration_ms, Some(7_400), "how long");
        assert_eq!(read.object_key, "cd/frozen.wav", "and where the audio is");
    }

    /// Each ending is counted apart, because an Operator does different things
    /// about them — narrow the selection, wait for the store to come back, or
    /// nothing at all.
    #[test]
    fn a_report_counts_every_ending_apart() {
        let added = [
            Froze::Frozen,
            Froze::Frozen,
            Froze::AlreadyHeld,
            Froze::Missing,
            Froze::Unreadable,
        ]
        .into_iter()
        .fold(Added::default(), Added::and);

        assert_eq!(
            added,
            Added {
                frozen: 2,
                already_held: 1,
                missing: 1,
                unreadable: 1,
            }
        );
        assert!(added.changed());
    }

    /// **Only a freeze moves the Event's clock.** A request that found nothing
    /// to do did not change what the Event holds, and an `updated_at_ms` that
    /// ticked for it would make "last changed" mean "last looked at".
    #[rstest]
    #[case::nothing_at_all(Added::default(), false)]
    #[case::all_already_held(Added { already_held: 9, ..Added::default() }, false)]
    #[case::all_gone(Added { missing: 3, ..Added::default() }, false)]
    #[case::the_store_was_down(Added { unreadable: 3, ..Added::default() }, false)]
    #[case::one_frozen(Added { frozen: 1, already_held: 9, ..Added::default() }, true)]
    fn only_a_freeze_changes_the_event(#[case] added: Added, #[case] changed: bool) {
        assert_eq!(added.changed(), changed);
    }

    /// The frozen object is named after what is in it, so an Operator listing a
    /// bucket by hand sees audio rather than a directory of `.bin`.
    #[rstest]
    #[case::wav(Some("audio/wav"), "wav")]
    #[case::the_other_wav(Some("audio/x-wav"), "wav")]
    #[case::with_a_codec_parameter(Some("audio/mp4; codecs=\"mp4a.40.2\""), "m4a")]
    #[case::shouted(Some("AUDIO/MPEG"), "mp3")]
    #[case::something_else(Some("application/octet-stream"), "bin")]
    #[case::unsaid(None, "bin")]
    fn a_frozen_object_is_named_after_what_is_in_it(
        #[case] mime: Option<&str>,
        #[case] expected: &str,
    ) {
        let call = StoredCall {
            audio_mime: mime.map(str::to_owned),
            ..a_call()
        };

        assert_eq!(extension_of(&call), expected);
    }

    /// The three links are spelled once, and this is what the page, the client
    /// and the harness all read.
    #[test]
    fn every_link_carries_the_token_in_a_query_parameter() {
        assert_eq!(link_to("abc"), "/e?t=abc");
        assert_eq!(audio_link_to("abc", 7), "/e/audio?t=abc&i=7");
        assert_eq!(export_link_to("abc", "wav"), "/e/export?t=abc&format=wav");
    }
}
