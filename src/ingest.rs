//! Ingest: the rdio-scanner-compatible `POST /api/call-upload` endpoint.
//!
//! Byte-compatibility is load-bearing (ADR-0001): recorders branch on the exact
//! response strings and status codes, verified against the rdio-scanner server
//! source (`api.go`, `parsers.go`) and the SDRTrunk client:
//! - success: HTTP 200 `Call imported successfully.\n`
//! - duplicate: HTTP 200 `duplicate call rejected\n` (SDRTrunk reads the body only on 200, then drops without retry)
//! - no talkgroup: HTTP 417 `Incomplete call data: no talkgroup\n`
//! - bad key: HTTP 401 `Invalid API key for system <s> talkgroup <t>.\n`
//!
//! **Resolve, then decide purely, then perform** (#96), answering with an
//! [`Admission`] rather than an HTTP response:
//!
//! - [`resolve`] reads what the database knows about this Call, **once**: the
//!   API key, the System, the Talkgroup its Ref resolves to (#45), and the
//!   Calls already inside the dedup window. Before this, the System and the
//!   Talkgroup were each read twice — the policy resolved them, and the insert
//!   resolved them again from scratch.
//! - [`admit`] turns those facts and the `[ingest]` configuration into an
//!   Admission. No database, no store, no clock — so the dedup window's edges
//!   are property-tested as values, where a `COUNT` over a window could only be
//!   approached through a socket and two uploads.
//! - [`perform`] writes the audio object, inserts the row (ADR-0001's order,
//!   now expressed in the types: the insert takes a [`crate::blob::StoredAudio`]
//!   that only a completed write produces), publishes to the live feed, and
//!   offers the Call for enhancement.
//!
//! A Call dropped by the auto-populate/blacklist policy (#8) still answers HTTP
//! 200 `Call imported successfully.` so the recorder never retries — matching
//! rdio, which likewise 200s and drops the call asynchronously.
//!
//! Which is exactly why every Admission is written down (ADR-0011 rule 3, #29):
//! two of the four tell the recorder "imported successfully", so the server's
//! own log is the only place the truth exists. [`Admission::record`] leaves that
//! line **where the Admission is decided** rather than where it is rendered —
//! inside a span naming the System and Talkgroup it was about, and the Call id
//! too once there is one — because **Dirwatch** (#72) will decide one with no
//! request in reach to answer, and rule 3 is about what the server wrote down.

use std::sync::Arc;

use axum::extract::{Multipart, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use tracing::{Instrument, Level, Span, field, info, span, warn};

use crate::archive;
use crate::call::{CallId, Candidate};
use crate::db::entities::call;
use crate::db::repo::{self, NewCall, NewCallFrequency, NewCallUnit};
use crate::failure::{Failure, Incomplete, Reason, Stage};
use crate::{AppState, now_ms};

/// Ingest tuning — and the `[ingest]` section of `radio-scout.toml` itself
/// (#17, #87). One type, so the shipped defaults below and the ones
/// `--write-config` documents are the same values rather than two copies a
/// translation function keeps in step.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct IngestConfig {
    /// Duplicate-detection window in milliseconds (rdio's default is ~500ms).
    pub dedup_window_ms: i64,
    /// Global auto-populate toggle (#8). On by default, matching rdio-scanner.
    /// When off, unknown Systems are dropped and only Systems whose own
    /// per-system flag is set still auto-create Talkgroups/Units.
    pub auto_populate: bool,
}

impl Default for IngestConfig {
    fn default() -> Self {
        IngestConfig {
            dedup_window_ms: 500,
            auto_populate: true,
        }
    }
}

/// The span of time a Call at `call_at_ms` is deduplicated against — inclusive
/// on both sides, and **the only place that arithmetic is written**.
///
/// The candidate query bounds on this range and the decision below re-applies
/// it, so the two cannot disagree about an edge: a mutation of either bound
/// moves both, and the property test sees it. Saturating rather than wrapping,
/// because a recorder that sends `i64::MAX` as its timestamp must be answered,
/// never panicked at, in the middle of an upload.
pub(crate) fn dedup_window(call_at_ms: i64, window_ms: i64) -> std::ops::RangeInclusive<i64> {
    call_at_ms.saturating_sub(window_ms)..=call_at_ms.saturating_add(window_ms)
}

/// The stored Call this one is a duplicate of, if any — the **nearest** of the
/// candidates inside the window (ADR-0001's duplicate detection).
///
/// Nearest rather than first: a recorder uploading one transmission once per
/// member Ref can put two candidates in the window, and "duplicate of call 41"
/// is only useful to an operator if 41 is the Call a listener would call the
/// same transmission. It is also the copy #46 will compare against.
///
/// Pure, which is the point: every near miss is a value a test can construct,
/// where a `COUNT` over a window could only be approached through a socket, a
/// multipart body and two uploads.
fn duplicate_of(candidates: &[Candidate], call_at_ms: i64, window_ms: i64) -> Option<CallId> {
    let window = dedup_window(call_at_ms, window_ms);
    candidates
        .iter()
        .filter(|candidate| window.contains(&candidate.call_at_ms))
        // `abs_diff` rather than a subtraction: two Calls at opposite ends of
        // the epoch are a difference no `i64` holds, and this decision must not
        // be the place that discovers it.
        .min_by_key(|candidate| (candidate.call_at_ms.abs_diff(call_at_ms), candidate.id))
        .map(|candidate| candidate.id)
}

/// Raw multipart fields, collected before validation. Arrays stay as raw JSON
/// text until we build the call.
#[derive(Default)]
struct RawUpload {
    key: Option<String>,
    system: Option<String>,
    system_label: Option<String>,
    talkgroup: Option<String>,
    talkgroup_label: Option<String>,
    talkgroup_name: Option<String>,
    talkgroup_group: Option<String>,
    talkgroup_groups: Option<String>,
    talkgroup_tag: Option<String>,
    frequency: Option<String>,
    frequencies: Option<String>,
    source: Option<String>,
    sources: Option<String>,
    /// SDRTrunk's `talkerAlias` — the name the source radio put over the air
    /// (`FormField.java`, sent on every upload). rdio-scanner reads the field
    /// and discards it, which is why its units stay bare numbers (#42, US 12).
    talker_alias: Option<String>,
    unit: Option<String>,
    units: Option<String>,
    /// rdio's documented `site` (`docs/api.md`, `parsers.go:344`) — the tower a
    /// multi-site System heard this on (spec US 11).
    site: Option<String>,
    patches: Option<String>,
    date_time: Option<String>,
    timestamp: Option<String>,
    audio: Option<Vec<u8>>,
    audio_name: Option<String>,
    audio_mime: Option<String>,
}

/// `POST /api/call-upload` — accept a call from a recorder.
pub async fn call_upload(
    State(state): State<AppState>,
    mut multipart: Multipart,
) -> Result<Recorded, Failure> {
    let mut upload = RawUpload::default();

    loop {
        let part = match multipart.next_field().await {
            Ok(Some(part)) => part,
            Ok(None) => break,
            Err(_) => return Err(Incomplete::MalformedMultipartBody.into()),
        };

        // Borrow-then-consume: capture metadata off the field before its body is
        // read (which consumes it).
        let name = part.name().unwrap_or("").to_string();
        if name == "audio" {
            upload.audio_name = part.file_name().map(str::to_string);
            upload.audio_mime = part.content_type().map(str::to_string);
            match part.bytes().await {
                Ok(bytes) => upload.audio = Some(bytes.to_vec()),
                Err(_) => return Err(Incomplete::CouldNotReadAudio.into()),
            }
            continue;
        }

        let value = match part.text().await {
            Ok(value) => value,
            Err(_) => return Err(Incomplete::CouldNotReadField.into()),
        };
        match name.as_str() {
            "key" => upload.key = Some(value),
            "system" => upload.system = Some(value),
            "systemLabel" => upload.system_label = Some(value),
            "talkgroup" => upload.talkgroup = Some(value),
            "talkgroupLabel" => upload.talkgroup_label = Some(value),
            "talkgroupName" => upload.talkgroup_name = Some(value),
            "talkgroupGroup" => upload.talkgroup_group = Some(value),
            "talkgroupGroups" => upload.talkgroup_groups = Some(value),
            "talkgroupTag" => upload.talkgroup_tag = Some(value),
            "frequency" => upload.frequency = Some(value),
            "frequencies" => upload.frequencies = Some(value),
            "source" => upload.source = Some(value),
            "sources" => upload.sources = Some(value),
            "talkerAlias" => upload.talker_alias = Some(value),
            "unit" => upload.unit = Some(value),
            "units" => upload.units = Some(value),
            "site" => upload.site = Some(value),
            "patches" | "patched_talkgroups" => upload.patches = Some(value),
            "dateTime" => upload.date_time = Some(value),
            "timestamp" => upload.timestamp = Some(value),
            // audioName/audioFilename and audioMime/audioType if sent as fields.
            "audioName" | "audioFilename" => upload.audio_name = Some(value),
            "audioMime" | "audioType" => upload.audio_mime = Some(value),
            _ => {} // a field we don't model: ignored, never an error
        }
    }

    // A talkgroup is mandatory (the load-bearing health-check string).
    let Some(talkgroup_ref) = upload.talkgroup.as_deref().and_then(parse_i64) else {
        return Err(Incomplete::NoTalkgroup.into());
    };
    let audio = match upload.audio.take() {
        Some(audio) if !audio.is_empty() => audio,
        _ => return Err(Incomplete::NoAudio.into()),
    };
    // A recorder normally sends a numeric `system`; if it doesn't (or sends a
    // non-positive value), give the new System the lowest-free Ref (#8).
    let system_ref = match upload.system.as_deref().and_then(parse_i64) {
        Some(system_ref) if system_ref > 0 => system_ref,
        _ => repo::lowest_free_system_ref(&state.db)
            .await
            .map_err(Stage::AssignSystemRef.failed())?,
    };
    let key = upload.key.take().unwrap_or_default();
    let call_at_ms = parse_call_time(upload.timestamp.as_deref(), upload.date_time.as_deref())
        .unwrap_or_else(now_ms);

    let new_call = NewCall {
        system_label: upload.system_label,
        // rdio drops empty and the "-" placeholder (parsers.go); recorders send
        // these parts even when the talkgroup is unknown, so clean them to NULL.
        talkgroup_label: clean(upload.talkgroup_label),
        talkgroup_name: clean(upload.talkgroup_name),
        talkgroup_tag: clean(upload.talkgroup_tag),
        talkgroup_groups: parse_groups(upload.talkgroup_group, upload.talkgroup_groups),
        frequency: upload.frequency.as_deref().and_then(parse_i64),
        source_ref: upload.source.as_deref().and_then(parse_i64),
        audio_mime: upload.audio_mime,
        audio_name: upload.audio_name,
        patches: parse_patches(upload.patches.as_deref()),
        // A Site Ref is only ever positive; rdio's own parser takes any
        // unsigned value, and a `0` from a recorder that fills every field
        // means "none" rather than "tower zero".
        site_ref: upload
            .site
            .as_deref()
            .and_then(parse_i64)
            .filter(|s| *s > 0),
        units: parse_units(
            upload.units.as_deref(),
            upload.sources.as_deref(),
            // SDRTrunk sends the singular radio as `source` and everyone else
            // as `unit`; both name one radio, and `talkerAlias` is its name.
            upload.unit.as_deref().or(upload.source.as_deref()),
            clean(upload.talker_alias),
        ),
        frequencies: parse_frequencies(upload.frequencies.as_deref()),
        // Everything left: `duration_ms`, which the perform phase reads out of
        // the audio's own header, and the transmission flags the rdio dialect
        // has no field for at all — only Trunk Recorder's own does
        // (`build_tr_call`).
        ..NewCall::new(system_ref, talkgroup_ref, call_at_ms)
    };

    ingest_call(&state, &key, new_call, audio).await
}

/// The shared ingest pipeline used by both upload endpoints — resolve, decide,
/// perform (#96), answering with the [`Admission`] it decided.
///
/// Everything one upload says shares a span naming the System and Talkgroup it
/// was about, so no line has to repeat them and every line can be read together
/// (#29). The span exists at ERROR level — the level at which it *exists*, not a
/// level it is reported at — because the lines that need it most are the WARN
/// rejections, which an operator may well be watching with everything else
/// turned down (the same reasoning as the request span in [`crate::http_log`]).
///
/// The span starts *here* rather than at the handler because this is where an
/// upload first has an identity: a body too malformed to yield a System and a
/// Talkgroup is rejected before it, and those lines carry the request id alone —
/// which is the whole of what is known about them.
///
/// **This is the entry point a non-HTTP caller uses** (#96): it takes a Call as
/// a Recorder described it plus its audio, and answers with the [`Admission`] it
/// decided — already written down. #72's Dirwatch has a `.wav` and a filename
/// and no request to answer, and needs exactly this and nothing above it.
pub async fn ingest_call(
    state: &AppState,
    key: &str,
    new_call: NewCall,
    audio: Vec<u8>,
) -> Result<Recorded, Failure> {
    let span = span!(
        Level::ERROR,
        "ingest",
        system_ref = new_call.system_ref,
        talkgroup_ref = new_call.talkgroup_ref,
        // Recorded once the row exists; absent, not `None`, until then.
        call_id = field::Empty,
    );
    run_pipeline(state, key, new_call, audio)
        .instrument(span)
        .await
}

async fn run_pipeline(
    state: &AppState,
    key: &str,
    new_call: NewCall,
    audio: Vec<u8>,
) -> Result<Recorded, Failure> {
    let facts = resolve(state, key, &new_call).await?;

    let admission = match admit(&facts, &new_call, &state.ingest) {
        Decision::Admit { auto_populate } => {
            perform(state, new_call, audio, &facts.resolved, auto_populate).await?
        }
        // Nothing is performed for a Call that is not stored, so a refusal is
        // already the whole Admission.
        Decision::Refused(admission) => admission,
    };

    // **The one line**, and the one place it is written (#96). Here rather than
    // where the Admission is rendered, because Dirwatch (#72) decides one with
    // no request to answer — and ADR-0011 rule 3 is about what the *server*
    // wrote down, not about what a recorder was told. What comes back is the
    // receipt, which is the only thing that can be rendered.
    Ok(admission.record())
}

/// Everything the database knows about an arriving Call, read **once** (#96).
///
/// Before this the System and the Talkgroup were each looked up twice — once to
/// decide the auto-populate/blacklist policy and again from scratch inside the
/// insert — and the dedup question was a `COUNT` the decision could not see.
///
/// A struct rather than an authorized/unauthorized pair, because an
/// unauthorized upload has simply had nothing read for it: nothing resolved and
/// no candidates, which is what "nothing is known" looks like. Written as two
/// shapes, every consumer would carry an arm that the decision above it has
/// already ruled out and no test could ever reach.
#[derive(Debug, Default)]
struct Facts {
    /// Whether the key authorizes ingesting into the System named (ADR-0008).
    /// When it does not, nothing below was read at all — refusing costs one
    /// statement, not four.
    authorized: bool,
    resolved: repo::Resolved,
    /// The Calls already stored on that Talkgroup inside the dedup window.
    candidates: Vec<Candidate>,
}

/// Read what the decision needs (ADR-0008's authorization first, so a recorder
/// with a bad key never reaches the rest).
///
/// The key itself is never logged, at any level, in any form (rule 2) — and an
/// unknown key has no row to name it by anyway.
async fn resolve(state: &AppState, key: &str, new_call: &NewCall) -> Result<Facts, Failure> {
    if !repo::authorize_ingest(&state.db, key, new_call.system_ref)
        .await
        .map_err(Stage::Auth.failed())?
    {
        return Ok(Facts::default());
    }

    let resolved = repo::resolve_refs(&state.db, new_call.system_ref, new_call.talkgroup_ref)
        .await
        .map_err(Stage::ResolveRefs.failed())?;

    // Dedup (ADR-0001) looks on the same *channel* the Ref resolved to, not the
    // Ref itself (#45), so a transmission uploaded once per member Ref is
    // stored once.
    let candidates = repo::calls_within(
        &state.db,
        resolved.talkgroup_id(),
        dedup_window(new_call.call_at_ms, state.ingest.dedup_window_ms),
    )
    .await
    .map_err(Stage::Dedup.failed())?;

    Ok(Facts {
        authorized: true,
        resolved,
        candidates,
    })
}

/// Whether this Call is admitted — **the decision, and nothing else** (#96).
///
/// Pure: no database, no store, no clock. Every arm is therefore a value a test
/// can construct, which is what lets the dedup window be property-tested at its
/// edges and what #46's near-miss cases need. It was previously spread across
/// three `await`s that each read something and then decided about it, so the
/// only way to reach an arm was to arrange the whole world first.
fn admit(facts: &Facts, call: &NewCall, config: &IngestConfig) -> Decision {
    if !facts.authorized {
        return Decision::Refused(Admission::Unauthorized {
            system_ref: call.system_ref,
            talkgroup_ref: call.talkgroup_ref,
        });
    }

    // Auto-populate + blacklist policy (#8): decided before any audio is
    // written. A dropped Call still answers success so the recorder doesn't
    // retry — which makes its line the only record that it was dropped at all.
    let auto_populate =
        match repo::disposition(&facts.resolved, call.talkgroup_ref, config.auto_populate) {
            repo::Disposition::Store { auto_populate } => auto_populate,
            repo::Disposition::Drop(reason) => {
                return Decision::Refused(Admission::Dropped(reason));
            }
        };

    match duplicate_of(&facts.candidates, call.call_at_ms, config.dedup_window_ms) {
        Some(of) => Decision::Refused(Admission::Duplicate { of }),
        None => Decision::Admit { auto_populate },
    }
}

/// What [`admit`] decided: either something to perform, or an [`Admission`]
/// that is already complete.
#[derive(Debug, PartialEq, Eq)]
enum Decision {
    /// Admitted. `auto_populate` is the effective flag the insert needs, and
    /// this becomes [`Admission::Stored`] once there is a row.
    Admit { auto_populate: bool },
    /// Not admitted — and there is nothing to perform, so this *is* the
    /// Admission ingest answers with.
    Refused(Admission),
}

/// Write the audio object, insert the row, publish it, offer it for
/// enhancement — everything an admitted Call costs, and nothing that decides
/// anything.
async fn perform(
    state: &AppState,
    mut new_call: NewCall,
    audio: Vec<u8>,
    resolved: &repo::Resolved,
    auto_populate: bool,
) -> Result<Admission, Failure> {
    // The *playing* length, so a one-second kerchunk and a forty-second
    // dispatch are distinguishable everywhere (#42, spec US 8). The recorder's
    // own figure wins when it sent one — only Trunk Recorder's native meta
    // does, and it knows the call it recorded better than its own encoder's
    // header does. Everything else is read here, from the container header
    // alone: no decode, no sample touched, microseconds on a Pi. Audio whose
    // header says nothing leaves the column `NULL`, which is the honest answer
    // and never a failed ingest.
    //
    // Read *before* the encrypted check, because the bytes are in hand either
    // way and an encrypted Call that never stores them still gets a length.
    if new_call.duration_ms.is_none() {
        new_call.duration_ms = crate::audio_meta::duration_ms(&audio);
    }

    // An encrypted Call is a row and nothing else (spec US 9): the activity is
    // worth seeing, the audio is worth nothing — it is the vocoder's noise, not
    // speech, and storing it would spend a Pi's disk on something no listener
    // can use. `object_key` stays empty, which is what the serve path and the
    // wire both read as "there is nothing here", and `audio_size` stays `NULL`,
    // so retention's cap counts what actually exists.
    //
    // The bytes are still *required* on the wire: this endpoint's dialect asks
    // for an audio part and refusing one for some Calls and not others would
    // make a recorder's success depend on a flag it also sent.
    let stored = match new_call.encrypted {
        true => None,
        false => {
            let key = crate::blob::new_object_key(&audio_extension(&new_call.audio_name));
            let bytes = audio.len();
            // Write the audio object first (ADR-0001); a failed DB insert
            // afterward leaves an orphan the GC sweep reclaims (#10). The row
            // below cannot be inserted without the value this produces, which
            // is that ordering expressed rather than commented (#96).
            state
                .audio
                .put(&key, bytes::Bytes::from(audio))
                .await
                .map_err(Stage::StoreAudio.failed())?;
            Some(crate::blob::StoredAudio::written(key, bytes))
        }
    };
    let audio_bytes = stored.as_ref().map(|a| a.bytes()).unwrap_or_default();

    // Insert the row (+ children) atomically, into the channel already resolved
    // for this Call rather than one looked up a second time (#96).
    let call = insert_in_txn(
        &state.db,
        &new_call,
        stored,
        resolved,
        auto_populate,
        state.clock.now_ms(),
    )
    .await
    .map_err(Stage::StoreCall.failed())?;
    // Everything this upload says from here on names the row it became.
    Span::current().record("call_id", call.id);

    // Emit to the live feed, denormalizing the row already in hand rather than
    // re-fetching it by id (#86). Iterated rather than unwrapped: one row in
    // gives one view out, so an `if let Some` here would be a branch whose empty
    // arm no test can reach — the same case `archive::detail` resolves with a
    // `.map` for the same reason.
    for view in archive::stored_calls(&state.db, std::slice::from_ref(&call))
        .await
        .map_err(Stage::BuildCallView.failed())?
    {
        state.publish(Arc::new(view)).await;
    }

    // Enhancement (#20) starts *here* — after the recorder has its answer and
    // after the live feed already has the Call. Scope is resolved now rather
    // than before the insert because auto-populate may have created the System
    // or the Talkgroup a moment ago, and a row that has just been created says
    // `NULL`, which is the value that inherits.
    //
    // An encrypted Call has no object to enhance (#42), and offering one is not
    // harmless: the worker would mark it `pending`, ask the store for the
    // object named by the empty string, fail, and settle it `skipped` with a
    // WARN — once per Call, forever, on a System whose traffic is mostly
    // encrypted. Asked here rather than inside the worker so the three queries
    // the scope lookup costs are never spent either.
    // Deliberately not part of the Admission, and not on its line: the Call is
    // stored, answered and already on the live feed, so what the queue makes of
    // it costs a listener nothing either way — and an `enhancement=not-enhancing`
    // field on every Call would be a per-Call field about a feature nobody
    // turned on. The outcome exists so the arms are assertable (#96) and for
    // #70's status surface to read.
    if call.has_audio() {
        let _queued = queue_for_enhancement(state, call.id).await;
    }

    Ok(Admission::Stored {
        call_id: call.id,
        audio_bytes,
    })
}

/// **What Ingest decided about one Call** (CONTEXT.md's *Admission*).
///
/// A *value*, not a response. The two HTTP endpoints render one into the rdio
/// wire strings; Dirwatch (#72) will decide one with no request in reach, and
/// #46 will decide one over candidate copies rather than a count. Everything
/// that says what happened to a Call says it here, once.
///
/// Its reason is one closed vocabulary — [`Reason`], which owns the wire
/// strings, the status codes and the levels (#92) — so the slug an operator
/// greps and the bytes a recorder branches on derive from the same value, and a
/// new rejection path cannot skip the funnel because there is nowhere else to
/// obtain one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Admission {
    /// The Call became a row.
    Stored {
        call_id: CallId,
        /// How many bytes of audio were stored — `0` for an **Encrypted Call**,
        /// which is a row and nothing else (#42).
        audio_bytes: i64,
    },
    /// The same transmission, already stored inside the dedup window (ADR-0001).
    Duplicate { of: CallId },
    /// The auto-populate/blacklist policy said no (#8).
    Dropped(repo::DropReason),
    /// The key was not valid for the System the upload named (ADR-0008).
    Unauthorized { system_ref: i64, talkgroup_ref: i64 },
}

impl Admission {
    /// What the caller is told, as the one closed vocabulary — `None` for a
    /// Call that was stored, which is the only arm that is not a refusal.
    fn reason(&self) -> Option<Reason> {
        match self {
            Admission::Stored { .. } => None,
            Admission::Duplicate { of } => Some(Reason::Duplicate { of: *of }),
            Admission::Dropped(repo::DropReason::Blacklisted) => Some(Reason::Blacklisted),
            Admission::Dropped(repo::DropReason::NotPopulated) => Some(Reason::NotPopulated),
            Admission::Unauthorized {
                system_ref,
                talkgroup_ref,
            } => Some(Reason::InvalidApiKey {
                system_ref: *system_ref,
                talkgroup_ref: *talkgroup_ref,
            }),
        }
    }

    /// **Write it down**, and hand back the receipt — ADR-0011 rule 3, made
    /// structural for a caller that has no response to render (#96).
    ///
    /// Every ingest leaves exactly one line here, whether or not anybody is
    /// waiting for an answer: three of the four arms below tell the recorder
    /// `200`, two of them with the *success* string, so the server's own log is
    /// the only place the truth exists.
    pub fn record(self) -> Recorded {
        match &self {
            // An ingest that became a row is a notable normal event, so
            // "nothing is arriving" is answerable without waiting for something
            // to go wrong. Per-Call, never per-anything-smaller (rule 8).
            Admission::Stored { audio_bytes, .. } => info!(audio_bytes, "call stored"),
            // Iterated rather than unwrapped: the arm above is the only one
            // with no reason, so an `if let Some` here would be a branch whose
            // empty half no test can reach.
            refused => refused.reason().iter().for_each(Reason::record),
        }
        Recorded(self)
    }
}

/// An [`Admission`] that **has been written down** — and the only thing that
/// can become a response.
///
/// [`Reason::into_response`] records and renders in one step, which is what
/// makes ADR-0011 rule 3 structural rather than a convention (#92). Ingest has
/// to do the two at different moments, because the line belongs where the
/// Admission is *decided* — so that #72's Dirwatch, which has no request to
/// answer, still leaves one. This type is what keeps that from being a
/// weakening: [`Admission::record`] is the only way to make one, and this is
/// the only thing that renders. An unrecorded Admission cannot reach a caller,
/// exactly as before.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recorded(Admission);

impl Recorded {
    /// What was decided — for a caller that wants the value rather than a
    /// response.
    pub fn admission(&self) -> &Admission {
        &self.0
    }
}

impl IntoResponse for Recorded {
    /// The rdio wire contract (ADR-0001): SDRTrunk reads the body on a 200 and
    /// branches on it. Two *rejections* answer with the success string so that
    /// a recorder never retries a Call we deliberately dropped — which is
    /// exactly why the line was written when the Admission was decided and not
    /// here.
    fn into_response(self) -> Response {
        match self.0.reason() {
            Some(reason) => reason.respond(),
            None => (StatusCode::OK, crate::failure::CALL_IMPORTED).into_response(),
        }
    }
}

/// What became of the offer to enhance a stored Call (#96).
///
/// Returned rather than only logged, because every arm below is a decision an
/// Operator can be shown and a test can assert on — where before, four
/// different endings were all `()` and three of them were silent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Queued {
    /// Marked `pending` and handed to the enhancement worker.
    Offered,
    /// This Instance does not enhance at all — the shipped default.
    NotEnhancing,
    /// It does, but not for this Call's System or Talkgroup (#20's scope).
    OutOfScope,
    /// The Call was pruned between its insert and this decision, which
    /// retention is entitled to do.
    Vanished,
    /// Something went wrong, named by the `reason` it logged. The Call keeps
    /// the audio the recorder sent, which is the whole cost.
    Failed(&'static str),
}

/// Offer a stored Call to the enhancement queue, if this instance enhances it.
///
/// Everything here is best-effort by design: the Call is already stored, already
/// answered and already on the live feed, so nothing that goes wrong from this
/// point costs a listener anything — it costs only the levelling.
///
/// **The disabled check comes first, and buys no I/O.** On the shipped default
/// there is nothing to enhance, and an ingest path that spent three SELECTs
/// rediscovering that would make every operator pay for a feature none of them
/// turned on — on the hardware least able to afford it.
async fn queue_for_enhancement(state: &AppState, call_id: CallId) -> Queued {
    if !state.enhancer.is_enabled() {
        return Queued::NotEnhancing;
    }
    let scope = match repo::enhancement_scope(&state.db, call_id).await {
        Ok(Some(scope)) => scope,
        Ok(None) => return Queued::Vanished,
        Err(error) => {
            // One slug, bound once: the field an operator greps and the value
            // this returns cannot drift into two spellings.
            let reason = "scope-unreadable";
            warn!(%reason, %error, "could not decide whether to enhance this Call");
            return Queued::Failed(reason);
        }
    };
    if !state.enhancer.applies_to(scope) {
        return Queued::OutOfScope;
    }
    // Marked before it is offered, so a process that dies between the two finds
    // it again at the next boot rather than losing it. The reverse order would
    // leave a Call queued in memory and `none` on disk.
    if let Err(error) =
        repo::mark_enhancement(&state.db, call_id, call::EnhancementState::PENDING).await
    {
        let reason = "mark-pending-failed";
        warn!(%reason, %error, "could not mark a Call for enhancement");
        return Queued::Failed(reason);
    }
    state.enhancer.offer(state, call_id).await;
    Queued::Offered
}

async fn insert_in_txn(
    db: &crate::db::Db,
    new_call: &NewCall,
    audio: Option<crate::blob::StoredAudio>,
    resolved: &repo::Resolved,
    auto_populate: bool,
    now_ms: i64,
) -> Result<crate::db::entities::call::Model, sea_orm::DbErr> {
    let txn = db.begin().await?;
    let call = repo::insert_call(&txn, new_call, audio, resolved, auto_populate, now_ms).await?;
    txn.commit().await?;
    Ok(call)
}

/// `POST /api/trunk-recorder-call-upload` — Trunk Recorder's native
/// `.wav`+`.json` upload: the metadata rides as a single JSON `meta` part rather
/// than individual form fields (rdio `parsers.go` mapping).
pub async fn trunk_recorder_call_upload(
    State(state): State<AppState>,
    mut multipart: Multipart,
) -> Result<Recorded, Failure> {
    let mut key = String::new();
    let mut meta_json: Option<String> = None;
    let mut audio: Option<Vec<u8>> = None;
    let mut audio_name = None;
    let mut audio_mime = None;

    loop {
        let part = match multipart.next_field().await {
            Ok(Some(part)) => part,
            Ok(None) => break,
            Err(_) => return Err(Incomplete::MalformedMultipartBody.into()),
        };
        let name = part.name().unwrap_or("").to_string();
        match name.as_str() {
            "audio" => {
                audio_name = part.file_name().map(str::to_string);
                audio_mime = part.content_type().map(str::to_string);
                match part.bytes().await {
                    Ok(bytes) => audio = Some(bytes.to_vec()),
                    Err(_) => return Err(Incomplete::CouldNotReadAudio.into()),
                }
            }
            "key" => key = part.text().await.unwrap_or_default(),
            "meta" => meta_json = part.text().await.ok(),
            _ => {}
        }
    }

    let meta_json = meta_json.ok_or(Incomplete::NoMeta)?;
    // TR's own dialect has its own wire string; unlike the `Incomplete` family
    // it is not "Incomplete call data: …", so it is a `Reason` of its own.
    let meta: TrMeta = serde_json::from_str(&meta_json).map_err(|_| Reason::InvalidMeta)?;

    let talkgroup_ref = meta
        .talkgroup
        .filter(|tg| *tg > 0)
        .ok_or(Incomplete::NoTalkgroup)?;
    let audio = match audio {
        Some(audio) if !audio.is_empty() => audio,
        _ => return Err(Incomplete::NoAudio.into()),
    };

    // TR has no numeric system ref — resolve one from `short_name`.
    let short_name = clean(meta.short_name.clone());
    let system_ref = match &short_name {
        Some(name) => repo::system_ref_for_short_name(&state.db, name)
            .await
            .map_err(Stage::ResolveSystem.failed())?,
        None => 0,
    };

    let new_call = build_tr_call(
        meta,
        system_ref,
        short_name,
        talkgroup_ref,
        audio_name,
        audio_mime,
    );
    ingest_call(&state, &key, new_call, audio).await
}

/// Trunk Recorder's call `.json` metadata.
///
/// The field set is `create_call_json` in
/// `trunk-recorder/call_concluder/call_concluder.cc` — the only definition of
/// this shape there is. rdio-scanner's parser (`parsers.go:477`) reads six of
/// these keys and walks past the rest; everything the recorder knows about the
/// *transmission* — that the emergency button was pressed, that the talkgroup
/// was encrypted, how long the call ran, what the radios called themselves over
/// the air — is in the half it discards (#42, spec US 5).
///
/// Every field is `#[serde(default)]` because a recorder that adds or drops a
/// key must not fail an upload: TR's JSON has grown over versions and will
/// again, and a Call refused for an unrecognised shape is a Call lost.
#[derive(Deserialize, Default)]
struct TrMeta {
    #[serde(default)]
    short_name: Option<String>,
    #[serde(default)]
    talkgroup: Option<i64>,
    #[serde(default)]
    talkgroup_tag: Option<String>, // -> talkgroup label
    #[serde(default)]
    talkgroup_description: Option<String>, // -> talkgroup name
    #[serde(default)]
    talkgroup_group: Option<String>, // -> group
    #[serde(default)]
    talkgroup_group_tag: Option<String>, // -> tag
    #[serde(default)]
    start_time: Option<f64>, // unix seconds
    #[serde(default)]
    stop_time: Option<f64>, // unix seconds
    #[serde(default)]
    timestamp: Option<f64>, // unix milliseconds (overrides start_time)
    /// The call's length in milliseconds — the recorder's own figure, and the
    /// best one there is: it counted the samples it wrote.
    #[serde(default)]
    call_length_ms: Option<f64>,
    /// The same length rounded to whole seconds. Older recorders write only
    /// this one, and one second of resolution still separates a kerchunk from a
    /// dispatch.
    #[serde(default)]
    call_length: Option<f64>,
    #[serde(default)]
    emergency: Option<i64>,
    #[serde(default)]
    encrypted: Option<i64>,
    #[serde(default)]
    priority: Option<i64>,
    #[serde(default)]
    audio_type: Option<String>,
    /// Not a field Trunk Recorder writes today — `create_call_json` has no site
    /// at all, and neither does its rdio uploader plugin. Read anyway so the
    /// shipped `uploadScript` (#43) and the first-party plugin (#44) can add one
    /// without a parser change, and so a fork that already does is not silently
    /// ignored.
    #[serde(default)]
    site: Option<i64>,
    #[serde(default)]
    freq: Option<f64>,
    #[serde(default)]
    patched_talkgroups: Vec<f64>,
    #[serde(default, rename = "freqList")]
    freq_list: Vec<TrFreq>,
    #[serde(default, rename = "srcList")]
    src_list: Vec<TrSrc>,
}

#[derive(Deserialize, Default)]
struct TrFreq {
    #[serde(default)]
    freq: f64,
    #[serde(default)]
    pos: f64,
    #[serde(default)]
    len: f64,
    /// Wall-clock start of this segment, unix **seconds**.
    #[serde(default)]
    time: Option<f64>,
    #[serde(default)]
    error_count: Option<i64>,
    #[serde(default)]
    spike_count: Option<i64>,
}

#[derive(Deserialize, Default)]
struct TrSrc {
    #[serde(default)]
    src: i64,
    #[serde(default)]
    pos: f64,
    /// Wall-clock start of this source's transmission, unix **seconds**.
    #[serde(default)]
    time: Option<f64>,
    #[serde(default)]
    emergency: Option<i64>,
    #[serde(default)]
    signal_system: Option<String>,
    #[serde(default)]
    tag: Option<String>,
    /// The alias the radio itself put over the air, which nothing configured.
    #[serde(default)]
    tag_ota: Option<String>,
}

/// The TR call time: `timestamp` (unix ms) if present, else `start_time`
/// (unix s). Deliberately does NOT clobber it with `now()` — rdio's parser has a
/// leftover-debug bug that does, which this ticket exists to avoid.
fn tr_call_time(meta: &TrMeta) -> Option<i64> {
    if let Some(ms) = meta.timestamp {
        return Some(ms as i64);
    }
    meta.start_time.map(|seconds| (seconds * 1000.0) as i64)
}

/// rdio's placeholder test: an empty string or the literal `"-"` means "no
/// value" — rdio's `parsers.go` guards talkgroup label/name/tag/group with
/// `len > 0 && != "-"`. The single source of truth for that rule.
fn is_placeholder(value: &str) -> bool {
    value.is_empty() || value == "-"
}

/// Drop empty and rdio's `"-"` placeholder strings.
fn clean(value: Option<String>) -> Option<String> {
    value.filter(|v| !is_placeholder(v))
}

fn build_tr_call(
    meta: TrMeta,
    system_ref: i64,
    short_name: Option<String>,
    talkgroup_ref: i64,
    audio_name: Option<String>,
    audio_mime: Option<String>,
) -> NewCall {
    let call_at_ms = tr_call_time(&meta).unwrap_or_else(now_ms);
    let frequencies = meta
        .freq_list
        .into_iter()
        .map(|f| NewCallFrequency {
            freq: f.freq as i64,
            pos_ms: Some((f.pos * 1000.0) as i64),
            len_ms: Some((f.len * 1000.0) as i64),
            dbm: None,
            error_count: f.error_count.map(|n| n as i32),
            spike_count: f.spike_count.map(|n| n as i32),
            at_ms: f.time.map(seconds_to_ms),
        })
        .collect();
    let units = meta
        .src_list
        .into_iter()
        .filter(|s| s.src > 0)
        .map(|s| NewCallUnit {
            unit_ref: s.src,
            label: clean(s.tag),
            offset_ms: Some((s.pos * 1000.0) as i64),
            tag_ota: clean(s.tag_ota),
            emergency: is_set(s.emergency),
            signal_system: clean(s.signal_system),
            at_ms: s.time.map(seconds_to_ms),
        })
        .collect();
    let patches = meta
        .patched_talkgroups
        .into_iter()
        .filter(|p| *p > 0.0)
        .map(|p| p as i64)
        .collect();

    NewCall {
        system_ref,
        system_label: short_name,
        talkgroup_ref,
        talkgroup_label: clean(meta.talkgroup_tag), // TR talkgroup_tag -> label
        talkgroup_name: clean(meta.talkgroup_description), // TR description -> name
        talkgroup_tag: clean(meta.talkgroup_group_tag), // TR group_tag -> tag
        talkgroup_groups: clean(meta.talkgroup_group).into_iter().collect(),
        call_at_ms,
        frequency: meta.freq.map(|f| f as i64),
        source_ref: None,
        audio_mime,
        audio_name,
        // The recorder counted the samples it wrote, so its figure beats
        // anything its encoder's header would say — and it is the only figure
        // an *encrypted* Call has, since no audio is kept to measure (#42).
        // Milliseconds when TR gives them; older recorders write whole seconds.
        duration_ms: meta
            .call_length_ms
            .map(|ms| ms as i64)
            .or_else(|| meta.call_length.map(seconds_to_ms))
            .filter(|ms| *ms > 0),
        stop_at_ms: meta.stop_time.map(seconds_to_ms),
        emergency: is_set(meta.emergency),
        encrypted: is_set(meta.encrypted),
        priority: meta.priority.map(|p| p as i32),
        audio_type: clean(meta.audio_type),
        site_ref: meta.site.filter(|s| *s > 0),
        patches,
        units,
        frequencies,
    }
}

/// A unix-**seconds** value from a recorder, in the milliseconds every column
/// here stores. Trunk Recorder writes seconds for `stop_time`, `call_length`,
/// and the `time` on every `freqList`/`srcList` entry; every one of those is a
/// place a factor of a thousand could hide.
fn seconds_to_ms(seconds: f64) -> i64 {
    (seconds * 1000.0) as i64
}

/// Trunk Recorder writes its booleans as `int(bool)` (`call_concluder.cc`), so
/// they arrive as `0`/`1`. Anything non-zero is true and an absent field is
/// false — a recorder that never mentions emergencies is one where none was
/// pressed, which is the reading #53 alerts on.
fn is_set(flag: Option<i64>) -> bool {
    flag.is_some_and(|value| value != 0)
}

/// Parse a decimal integer field, tolerating surrounding whitespace.
fn parse_i64(value: &str) -> Option<i64> {
    value.trim().parse().ok()
}

/// The audio object-key extension, from the uploaded filename (default `wav`).
fn audio_extension(name: &Option<String>) -> String {
    name.as_deref()
        .and_then(|name| name.rsplit_once('.'))
        .map(|(_, ext)| ext.to_ascii_lowercase())
        .filter(|ext| !ext.is_empty() && ext.chars().all(|c| c.is_ascii_alphanumeric()))
        .unwrap_or_else(|| "wav".to_string())
}

/// Resolve the call time: `timestamp` is unix **milliseconds**; `dateTime` is
/// RFC3339 or unix **seconds** (per rdio's `api.md`).
fn parse_call_time(timestamp: Option<&str>, date_time: Option<&str>) -> Option<i64> {
    if let Some(ms) = timestamp.and_then(parse_i64) {
        return Some(ms);
    }
    let date_time = date_time?.trim();
    if let Some(seconds) = parse_i64(date_time) {
        return Some(seconds * 1000);
    }
    use time::OffsetDateTime;
    use time::format_description::well_known::Rfc3339;
    OffsetDateTime::parse(date_time, &Rfc3339)
        .ok()
        .map(|dt| (dt.unix_timestamp_nanos() / 1_000_000) as i64)
}

/// Combine the single `talkgroupGroup` with the comma-separated `talkgroupGroups`
/// list. Empty and rdio's `"-"` placeholder are dropped — mirroring rdio's
/// per-field guard (parsers.go) for the singular field; for the multi-value list
/// we additionally trim and de-duplicate as a data-quality step (rdio splits the
/// list raw). Real recorders send only the singular field, so the two agree in
/// practice.
fn parse_groups(single: Option<String>, multiple: Option<String>) -> Vec<String> {
    let mut groups = Vec::new();
    let push = |g: &str, groups: &mut Vec<String>| {
        let g = g.trim();
        if !is_placeholder(g) && !groups.iter().any(|existing| existing == g) {
            groups.push(g.to_string());
        }
    };
    if let Some(g) = single {
        push(&g, &mut groups);
    }
    if let Some(list) = multiple {
        for g in list.split(',') {
            push(g, &mut groups);
        }
    }
    groups
}

/// Parse the `patches` / `patched_talkgroups` array (numbers or numeric strings).
///
/// Every entry, verbatim — which of them are Talkgroup Refs is not decidable
/// here. SDRTrunk appends a patch group's radio IDs behind its talkgroups in the
/// same array with nothing marking the boundary, so membership is settled
/// against the System's Talkgroups in [`repo::insert_call`] (#81).
fn parse_patches(raw: Option<&str>) -> Vec<i64> {
    let Some(raw) = raw else { return Vec::new() };
    let Ok(values) = serde_json::from_str::<Vec<serde_json::Value>>(raw) else {
        return Vec::new();
    };
    values
        .into_iter()
        .filter_map(|v| v.as_i64().or_else(|| v.as_str().and_then(parse_i64)))
        .collect()
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct FreqJson {
    #[serde(default)]
    freq: f64,
    #[serde(default)]
    pos: f64,
    #[serde(default)]
    len: f64,
    #[serde(default)]
    dbm: Option<f64>,
    #[serde(default)]
    error_count: Option<i64>,
    #[serde(default)]
    spike_count: Option<i64>,
}

fn parse_frequencies(raw: Option<&str>) -> Vec<NewCallFrequency> {
    let Some(raw) = raw else { return Vec::new() };
    serde_json::from_str::<Vec<FreqJson>>(raw)
        .unwrap_or_default()
        .into_iter()
        .map(|f| NewCallFrequency {
            freq: f.freq as i64,
            pos_ms: Some((f.pos * 1000.0) as i64),
            len_ms: Some((f.len * 1000.0) as i64),
            dbm: f.dbm,
            error_count: f.error_count.map(|n| n as i32),
            spike_count: f.spike_count.map(|n| n as i32),
            // rdio's generic `frequencies[]` has no wall-clock time in it; TR's
            // native `freqList` does.
            ..Default::default()
        })
        .collect()
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct UnitJson {
    #[serde(default)]
    id: i64,
    #[serde(default)]
    label: Option<String>,
    #[serde(default)]
    offset: f64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SourceJson {
    #[serde(default)]
    src: i64,
    #[serde(default)]
    pos: f64,
    #[serde(default)]
    tag: Option<String>,
}

/// Units heard, from `units[]` (rdio-native), `sources[]` (Trunk Recorder), or a
/// singular radio — `unit`, or SDRTrunk's `source` — named by `talker_alias`.
///
/// The arrays win over the singular pair, and deliberately: they describe every
/// radio that keyed across the call, with offsets, where the alias describes one
/// of them. A recorder that sent both took the trouble to send the better thing.
fn parse_units(
    units: Option<&str>,
    sources: Option<&str>,
    unit: Option<&str>,
    talker_alias: Option<String>,
) -> Vec<NewCallUnit> {
    if let Some(raw) = units
        && let Ok(list) = serde_json::from_str::<Vec<UnitJson>>(raw)
    {
        return list
            .into_iter()
            .map(|u| NewCallUnit {
                unit_ref: u.id,
                label: u.label,
                offset_ms: Some((u.offset * 1000.0) as i64),
                ..Default::default()
            })
            .collect();
    }
    if let Some(raw) = sources
        && let Ok(list) = serde_json::from_str::<Vec<SourceJson>>(raw)
    {
        return list
            .into_iter()
            .map(|s| NewCallUnit {
                unit_ref: s.src,
                label: s.tag,
                offset_ms: Some((s.pos * 1000.0) as i64),
                ..Default::default()
            })
            .collect();
    }
    if let Some(unit_ref) = unit.and_then(parse_i64) {
        return vec![NewCallUnit {
            unit_ref,
            label: talker_alias,
            ..Default::default()
        }];
    }
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use rstest::rstest;
    use sea_orm::ConnectionTrait;

    // -- The dedup window, decided over candidate Calls (#96) ---------------

    /// One stored Call, `offset` milliseconds from the arriving one.
    fn stored_at(offset: i64) -> Vec<Candidate> {
        vec![Candidate {
            id: 7,
            call_at_ms: 1_000_000 + offset,
        }]
    }

    /// **Adjacency is the whole of this decision**, so both edges are named and
    /// both are named twice — on the edge, and one millisecond past it.
    ///
    /// The window is symmetric because a recorder catching up after a network
    /// blip uploads older Calls after newer ones: the Call already stored is
    /// routinely *later* than the one arriving. A mutation of the forward bound
    /// survived the entire integration suite for exactly that reason (#83), and
    /// it is what a table of near misses closes.
    #[rstest]
    #[case::the_same_instant(0, true)]
    #[case::inside_the_window_behind(-499, true)]
    #[case::on_the_trailing_edge(-500, true)]
    #[case::one_past_the_trailing_edge(-501, false)]
    #[case::inside_the_window_ahead(499, true)]
    #[case::on_the_leading_edge(500, true)]
    #[case::one_past_the_leading_edge(501, false)]
    #[case::an_out_of_order_backfill(100_000, false)]
    fn a_stored_call_within_the_window_is_the_same_transmission(
        #[case] offset: i64,
        #[case] duplicate: bool,
    ) {
        assert_eq!(
            duplicate_of(&stored_at(offset), 1_000_000, 500).is_some(),
            duplicate,
            "a Call stored {offset}ms away, window 500ms"
        );
    }

    /// A window of zero is the operator saying "only the very same instant",
    /// and it must not collapse into "everything" or "nothing".
    #[rstest]
    #[case::exactly_now(0, true)]
    #[case::one_millisecond_later(1, false)]
    #[case::one_millisecond_earlier(-1, false)]
    fn a_zero_window_admits_only_the_same_instant(#[case] offset: i64, #[case] duplicate: bool) {
        assert_eq!(
            duplicate_of(&stored_at(offset), 1_000_000, 0).is_some(),
            duplicate
        );
    }

    /// A clock at the end of time is still a clock. The bounds saturate rather
    /// than wrap, so a recorder that sends `i64::MAX` as its timestamp gets a
    /// decision — never a panic in the middle of an upload.
    ///
    /// The answers are what a saturated window really says: at `i64::MAX` with
    /// the widest window there is, the range is `0..=MAX` and an ordinary
    /// stored Call is inside it; at `i64::MIN` the range ends at `-1` and it is
    /// not. Both are absurd inputs, and both are answered.
    #[rstest]
    #[case::the_end_of_time(i64::MAX, true)]
    #[case::before_the_beginning(i64::MIN, false)]
    fn an_absurd_call_time_decides_rather_than_overflows(#[case] at: i64, #[case] duplicate: bool) {
        assert_eq!(
            duplicate_of(&stored_at(0), at, i64::MAX).is_some(),
            duplicate
        );
        // ...and a Call at that same absurd instant is a duplicate of itself
        // however far apart the two ends of the epoch are.
        assert_eq!(
            duplicate_of(
                &[Candidate {
                    id: 7,
                    call_at_ms: at
                }],
                at,
                i64::MAX
            ),
            Some(7)
        );
    }

    /// The Call it names is the *nearest* stored one, which is what an operator
    /// reading "duplicate of call 41" wants and what #46's keep-best will
    /// compare against — never merely the first row the query happened to hand
    /// back.
    #[test]
    fn the_duplicate_it_names_is_the_nearest_stored_call() {
        let candidates = vec![
            Candidate {
                id: 41,
                call_at_ms: 1_000_400,
            },
            Candidate {
                id: 42,
                call_at_ms: 1_000_100,
            },
        ];

        assert_eq!(duplicate_of(&candidates, 1_000_000, 500), Some(42));
    }

    proptest! {
        /// Whatever the candidates, the decision *is* the window: a Call is a
        /// duplicate exactly when some stored Call lies within `window`
        /// milliseconds of it, on either side.
        ///
        /// Written as `|offset| <= window` where the code is a range that
        /// contains — two independent spellings of the same rule, so the
        /// assertion cannot agree with the code by construction.
        #[test]
        fn a_call_is_a_duplicate_exactly_when_something_stored_is_within_the_window(
            at in -1_000_000_000_000i64..1_000_000_000_000,
            window in 0i64..86_400_000,
            offsets in proptest::collection::vec(-200_000i64..200_000, 0..8),
        ) {
            let candidates: Vec<Candidate> = offsets
                .iter()
                .enumerate()
                .map(|(i, offset)| Candidate { id: i as i64 + 1, call_at_ms: at + offset })
                .collect();

            let expected = offsets.iter().any(|offset| offset.abs() <= window);

            prop_assert_eq!(duplicate_of(&candidates, at, window).is_some(), expected);
        }

        /// ...and the one it names is inside the window and no further away
        /// than any other candidate.
        #[test]
        fn the_named_duplicate_is_inside_the_window_and_nearest(
            at in -1_000_000_000_000i64..1_000_000_000_000,
            window in 0i64..86_400_000,
            offsets in proptest::collection::vec(-200_000i64..200_000, 1..8),
        ) {
            let candidates: Vec<Candidate> = offsets
                .iter()
                .enumerate()
                .map(|(i, offset)| Candidate { id: i as i64 + 1, call_at_ms: at + offset })
                .collect();

            if let Some(named) = duplicate_of(&candidates, at, window) {
                let named = candidates.iter().find(|c| c.id == named).expect("a candidate");
                let distance = (named.call_at_ms - at).abs();
                prop_assert!(distance <= window);
                for other in &candidates {
                    prop_assert!(distance <= (other.call_at_ms - at).abs());
                }
            }
        }
    }

    #[test]
    fn call_time_prefers_timestamp_millis() {
        assert_eq!(
            parse_call_time(Some("1669740338000"), Some("2022-11-29T18:05:38Z")),
            Some(1669740338000)
        );
    }

    #[test]
    fn call_time_parses_unix_seconds_datetime() {
        assert_eq!(
            parse_call_time(None, Some("1669740338")),
            Some(1669740338000)
        );
    }

    #[test]
    fn call_time_parses_rfc3339_with_millis() {
        let a = parse_call_time(None, Some("2022-11-29T18:05:38.000Z")).unwrap();
        let b = parse_call_time(None, Some("2022-11-29T18:05:38.500Z")).unwrap();
        assert_eq!(b - a, 500, "millisecond precision preserved");
        assert!(a > 1_600_000_000_000, "plausible 2022 timestamp");
    }

    #[test]
    fn frequencies_parse_from_json() {
        // `pos` deliberately non-zero: at 0.0 every arithmetic mutation of the
        // seconds -> ms conversion still lands on 0, so the assertion below
        // would hold whatever the code did (#83).
        let f = parse_frequencies(Some(
            r#"[{"freq":774031250,"pos":0.25,"len":1.5,"dbm":-50,"errorCount":2,"spikeCount":1}]"#,
        ));
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].freq, 774031250);
        assert_eq!(f[0].pos_ms, Some(250), "seconds -> ms");
        assert_eq!(f[0].len_ms, Some(1500));
        assert_eq!(f[0].error_count, Some(2));
        assert_eq!(f[0].dbm, Some(-50.0));
    }

    #[test]
    fn units_from_units_sources_or_singular() {
        let from_units = parse_units(
            Some(r#"[{"id":4424000,"label":"Engine 1","offset":0.5}]"#),
            None,
            None,
            None,
        );
        assert_eq!(from_units[0].unit_ref, 4424000);
        assert_eq!(from_units[0].label.as_deref(), Some("Engine 1"));
        assert_eq!(from_units[0].offset_ms, Some(500));

        // `pos` is seconds and the column is milliseconds. Asserted on a value
        // where the conversion is the only arithmetic that produces it: 1.75 s
        // is 1750 ms, which neither `+ 1000` nor `/ 1000` can reach (#83).
        let from_sources = parse_units(
            None,
            Some(r#"[{"src":123,"pos":1.75,"tag":"Medic"}]"#),
            None,
            None,
        );
        assert_eq!(from_sources[0].unit_ref, 123);
        assert_eq!(from_sources[0].label.as_deref(), Some("Medic"));
        assert_eq!(from_sources[0].offset_ms, Some(1750), "seconds -> ms");

        let from_singular = parse_units(None, None, Some("999"), None);
        assert_eq!(from_singular.len(), 1);
        assert_eq!(from_singular[0].unit_ref, 999);
    }

    #[test]
    fn patches_parse_numbers_and_strings() {
        assert_eq!(parse_patches(Some("[100, 200]")), vec![100, 200]);
        assert_eq!(parse_patches(Some(r#"["300","400"]"#)), vec![300, 400]);
        assert_eq!(parse_patches(None), Vec::<i64>::new());
    }

    #[test]
    fn groups_combine_single_and_comma_list_without_dupes() {
        assert_eq!(
            parse_groups(Some("Fire".into()), Some("Fire, Law".into())),
            vec!["Fire".to_string(), "Law".to_string()]
        );
    }

    #[test]
    fn groups_drop_empty_and_dash_placeholder() {
        // Singular field: empty / "-" dropped, mirroring rdio (parsers.go).
        assert!(parse_groups(Some("-".into()), None).is_empty());
        assert!(parse_groups(Some(String::new()), None).is_empty());
        // Multi-value list: placeholders dropped + trimmed + de-duped (our
        // normalization; rdio splits the list raw).
        assert_eq!(
            parse_groups(Some("-".into()), Some("Fire,-, ,Law".into())),
            vec!["Fire".to_string(), "Law".to_string()]
        );
    }

    #[test]
    fn audio_extension_derives_or_defaults() {
        assert_eq!(audio_extension(&Some("call.m4a".into())), "m4a");
        assert_eq!(audio_extension(&Some("weird".into())), "wav");
        assert_eq!(audio_extension(&None), "wav");
    }

    /// Both halves of the extension guard, because the extension becomes an
    /// object key: a trailing dot yields an *empty* extension (which `all()`
    /// answers vacuously true for), and anything non-alphanumeric is a filename
    /// we will not echo into the store (#83).
    #[rstest]
    #[case::trailing_dot("call.", "wav")]
    #[case::path_separator_in_extension("call.wav/../../etc", "wav")]
    #[case::space("call.w v", "wav")]
    #[case::uppercased("CALL.WAV", "wav")]
    #[case::alphanumeric_is_kept("call.mp3", "mp3")]
    fn audio_extension_refuses_anything_but_an_alphanumeric_suffix(
        #[case] name: &str,
        #[case] expected: &str,
    ) {
        assert_eq!(audio_extension(&Some(name.into())), expected, "for {name}");
    }

    #[test]
    fn tr_time_uses_start_time_not_now() {
        let meta = TrMeta {
            start_time: Some(1669740338.0),
            ..Default::default()
        };
        // start_time (seconds) -> milliseconds, NOT clobbered with now().
        assert_eq!(tr_call_time(&meta), Some(1669740338000));
    }

    #[test]
    fn tr_time_prefers_timestamp_millis() {
        let meta = TrMeta {
            start_time: Some(1669740338.0),
            timestamp: Some(1669740999000.0),
            ..Default::default()
        };
        assert_eq!(tr_call_time(&meta), Some(1669740999000));
    }

    // -- `queue_for_enhancement`'s own failure arms (#37) --------------------
    //
    // Tested here rather than over ingest's HTTP boundary because they cannot be
    // reached from there: every table the scope read touches is one the insert
    // *before* it has already used, so taking any of them away fails the upload
    // long before the decision is reached. There is no seam in between — the
    // window holds no I/O to park in and no statement a trigger can fire on.
    // What is asserted is the same as everywhere else: the operator's log line,
    // and that nothing panicked.

    // -- The Admission itself (#96) -----------------------------------------

    fn a_system(
        auto_populate: bool,
        blacklist: Option<&str>,
    ) -> crate::db::entities::system::Model {
        crate::db::entities::system::Model {
            id: 1,
            r#ref: 11,
            label: None,
            auto_populate,
            blacklist: blacklist.map(str::to_string),
            enhancement: None,
            created_at_ms: 0,
        }
    }

    fn a_talkgroup() -> crate::db::entities::talkgroup::Model {
        crate::db::entities::talkgroup::Model {
            id: 7,
            system_id: 1,
            r#ref: 54241,
            label: None,
            name: None,
            tag_id: None,
            led: None,
            enhancement: None,
            created_at_ms: 0,
        }
    }

    /// What the database knew, as a value — the whole input to the decision.
    fn facts(
        authorized: bool,
        system: Option<crate::db::entities::system::Model>,
        talkgroup: Option<crate::db::entities::talkgroup::Model>,
        candidates: Vec<Candidate>,
    ) -> Facts {
        Facts {
            authorized,
            resolved: repo::Resolved { system, talkgroup },
            candidates,
        }
    }

    fn a_stored_call_at(call_at_ms: i64) -> Vec<Candidate> {
        vec![Candidate { id: 41, call_at_ms }]
    }

    /// The configuration an operator who changed nothing is running.
    fn shipped() -> IngestConfig {
        IngestConfig::default()
    }

    /// **The decision, as a table** — every way an upload can end, over facts a
    /// test constructs rather than a world it has to arrange.
    ///
    /// The order of the arms is itself behaviour, and the last two cases pin
    /// it: authorization is answered before anything is read, and the
    /// auto-populate/blacklist policy is answered before the dedup window — so
    /// a Call an Operator blacklisted is reported as blacklisted rather than as
    /// a duplicate of the copy that arrived a moment earlier.
    #[rstest]
    #[case::a_bad_key_is_refused_before_anything_is_read(
        facts(false, None, None, vec![]),
        shipped(),
        Decision::Refused(Admission::Unauthorized { system_ref: 11, talkgroup_ref: 54241 })
    )]
    #[case::an_ordinary_first_call_is_admitted(
        facts(true, Some(a_system(false, None)), Some(a_talkgroup()), vec![]),
        shipped(),
        Decision::Admit { auto_populate: true }
    )]
    #[case::an_unknown_system_with_auto_populate_off_is_dropped(
        facts(true, None, None, vec![]),
        IngestConfig { auto_populate: false, ..shipped() },
        Decision::Refused(Admission::Dropped(repo::DropReason::NotPopulated))
    )]
    #[case::a_blacklisted_talkgroup_is_dropped(
        facts(true, Some(a_system(false, Some("54241"))), Some(a_talkgroup()), vec![]),
        shipped(),
        Decision::Refused(Admission::Dropped(repo::DropReason::Blacklisted))
    )]
    #[case::a_second_copy_inside_the_window_is_a_duplicate(
        facts(true, Some(a_system(false, None)), Some(a_talkgroup()), a_stored_call_at(1_000_200)),
        shipped(),
        Decision::Refused(Admission::Duplicate { of: 41 })
    )]
    #[case::a_copy_outside_the_window_is_not(
        facts(true, Some(a_system(false, None)), Some(a_talkgroup()), a_stored_call_at(1_000_501)),
        shipped(),
        Decision::Admit { auto_populate: true }
    )]
    // ...and the window it is outside of is the operator's, not a constant.
    #[case::the_window_is_the_configured_one(
        facts(true, Some(a_system(false, None)), Some(a_talkgroup()), a_stored_call_at(1_000_501)),
        IngestConfig { dedup_window_ms: 2_000, ..shipped() },
        Decision::Refused(Admission::Duplicate { of: 41 })
    )]
    #[case::the_policy_is_answered_before_the_window(
        facts(
            true,
            Some(a_system(false, Some("54241"))),
            Some(a_talkgroup()),
            a_stored_call_at(1_000_000)
        ),
        shipped(),
        Decision::Refused(Admission::Dropped(repo::DropReason::Blacklisted))
    )]
    fn every_way_an_upload_can_end(
        #[case] facts: Facts,
        #[case] config: IngestConfig,
        #[case] expected: Decision,
    ) {
        let call = NewCall::new(11, 54241, 1_000_000);

        assert_eq!(admit(&facts, &call, &config), expected);
    }

    /// **An Admission is written down where it is decided, and rendering it
    /// does not write it down again** (#96).
    ///
    /// That is what makes ADR-0011 rule 3 hold for a caller with no request to
    /// answer: Dirwatch (#72) will decide one of these with nothing to render,
    /// and the line has to exist anyway. Two of the arms below answer the
    /// recorder `200 Call imported successfully.`, so this line is the only
    /// record that the Call was refused at all.
    ///
    /// The rendering below goes through the [`Recorded`] this returns, which is
    /// the only thing that renders — so "recorded, then answered" is the shape
    /// of the types rather than the order of two statements.
    #[rstest]
    #[case::stored(Admission::Stored { call_id: 1, audio_bytes: 44 }, "call stored", " INFO ")]
    #[case::duplicate(Admission::Duplicate { of: 41 }, "reason=duplicate", " WARN ")]
    #[case::blacklisted(
        Admission::Dropped(repo::DropReason::Blacklisted),
        "reason=blacklisted",
        " WARN "
    )]
    #[case::not_populated(
        Admission::Dropped(repo::DropReason::NotPopulated),
        "reason=not-populated",
        " WARN "
    )]
    #[case::unauthorized(
        Admission::Unauthorized { system_ref: 11, talkgroup_ref: 54241 },
        "reason=invalid-api-key",
        " WARN "
    )]
    #[tokio::test]
    async fn an_admission_leaves_exactly_one_line_and_rendering_adds_none(
        #[case] admission: Admission,
        #[case] expected: &str,
        #[case] level: &str,
    ) {
        let capture = crate::testing::LogCapture::start();

        let _rendered = admission.record().into_response();

        let logged = capture.text();
        assert_eq!(
            logged.matches(expected).count(),
            1,
            "recorded once, at the decision: {logged}"
        );
        assert!(logged.contains(level), "{logged}");
    }

    /// **The recorder-facing wire contract, in one artifact** (ADR-0001).
    ///
    /// Every way an ingest can end, as the bytes and the status a recorder
    /// actually branches on: SDRTrunk reads the body on a `200`, health-checks
    /// on the `417`, and drops without retry on `duplicate call rejected`.
    /// Two *rejections* answer `Call imported successfully.` on purpose — a
    /// recorder must never retry a Call we deliberately dropped.
    ///
    /// A snapshot rather than five assertions because what matters is the
    /// contract *as a whole*: a diff here is a diff every Trunk Recorder and
    /// SDRTrunk in the field would see.
    #[tokio::test]
    async fn the_wire_contract_every_admission_answers_with() {
        let mut rendered = String::new();
        for admission in [
            Admission::Stored {
                call_id: 1,
                audio_bytes: 44,
            },
            Admission::Duplicate { of: 41 },
            Admission::Dropped(repo::DropReason::Blacklisted),
            Admission::Dropped(repo::DropReason::NotPopulated),
            Admission::Unauthorized {
                system_ref: 11,
                talkgroup_ref: 54241,
            },
        ] {
            let name = format!("{admission:?}");
            let response = admission.record().into_response();
            let status = response.status().as_u16();
            let body = axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .expect("body");
            rendered.push_str(&format!(
                "{name}\n  {status} {:?}\n",
                String::from_utf8_lossy(&body)
            ));
        }

        insta::assert_snapshot!(rendered);
    }

    // -- `queue_for_enhancement`'s own arms (#37, reshaped by #96) ----------
    //
    // The decision now returns what it decided, so each arm is a value rather
    // than only a log line — and the one arm that needs a failing read gets it
    // by **closing the pool**, not by taking a column away from the schema.
    // (`mark-pending-failed` needs the read to succeed and only the update to
    // fail, which is `TestApp::refuse_updates_to` in `tests/enhance.rs`.)

    /// A Call, its System and its Talkgroup, in a database of this test's own.
    async fn one_stored_call() -> (AppState, CallId, tempfile::TempDir) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let db = crate::db::connect(&crate::testing::sqlite_url(&tmp))
            .await
            .expect("db");
        let store = Arc::new(crate::BlobStore::filesystem(tmp.path().join("audio")).expect("blob"));
        let call = repo::insert_call(
            &db,
            &NewCall::new(11, 54241, 1_000),
            Some(crate::blob::StoredAudio::written("aa/1.wav".into(), 3)),
            &repo::Resolved::unresolved(),
            true,
            0,
        )
        .await
        .expect("store a call");

        let mut state = AppState::new(store, db, IngestConfig::default());
        state.enhancer = crate::enhance::Enhancer::from_config(crate::enhance::EnhancementConfig {
            mode: crate::enhance::Mode::Normalize,
            ..Default::default()
        });
        (state, call.id, tmp)
    }

    /// This instance does not enhance at all — the shipped default — and finding
    /// that out must cost no query, on the hardware least able to afford one.
    #[tokio::test]
    async fn an_instance_that_does_not_enhance_asks_the_database_nothing() {
        let (mut state, call_id, _tmp) = one_stored_call().await;
        state.enhancer = crate::enhance::Enhancer::disabled();

        assert_eq!(
            queue_for_enhancement(&state, call_id).await,
            Queued::NotEnhancing
        );
    }

    /// Enhancement is on, but not for this Call's Talkgroup (#20's nullable
    /// scope). The Call keeps the audio the recorder sent.
    #[tokio::test]
    async fn a_call_outside_the_enhancement_scope_is_left_alone() {
        let (state, call_id, _tmp) = one_stored_call().await;
        state
            .db
            .execute_unprepared(r#"UPDATE "systems" SET "enhancement" = false"#)
            .await
            .expect("opt this System out");

        assert_eq!(
            queue_for_enhancement(&state, call_id).await,
            Queued::OutOfScope
        );
    }

    /// A Call inside the scope is marked `pending` before it is offered, so a
    /// process that dies between the two finds it again at the next boot.
    #[tokio::test]
    async fn a_call_in_scope_is_marked_pending_and_offered() {
        let (state, call_id, _tmp) = one_stored_call().await;

        assert_eq!(
            queue_for_enhancement(&state, call_id).await,
            Queued::Offered
        );

        assert_eq!(
            repo::find_call(&state.db, call_id)
                .await
                .expect("read it back")
                .expect("the Call")
                .enhancement,
            call::EnhancementState::PENDING,
            "marked on disk before it was offered in memory"
        );
    }

    /// The scope read fails — a database that has gone away under a running
    /// process.
    ///
    /// It must cost the recorder nothing. By the time this runs the Call is
    /// stored, answered and already on the live feed, so the only thing at stake
    /// is the levelling — and ingest that started failing over an optional
    /// convenience would be a far worse bug than the one it reported.
    #[tokio::test]
    async fn a_scope_that_cannot_be_read_leaves_the_call_alone_and_says_why() {
        let capture = crate::testing::LogCapture::start();
        let (state, call_id, _tmp) = one_stored_call().await;
        // Closed rather than damaged: the arm is "the read failed", and a
        // schema this test edited would be proving something about our
        // migrations instead (#96).
        state.db.close().await.expect("close the pool");

        let queued = queue_for_enhancement(&state, call_id).await;

        assert_eq!(queued, Queued::Failed("scope-unreadable"));
        assert!(
            capture.text().contains("reason=scope-unreadable"),
            "the operator must be told the decision could not be made: {}",
            capture.text()
        );
    }

    /// The Call was pruned between its insert and this decision, which retention
    /// is entitled to do. There is nothing to enhance and nothing to say — the
    /// row that would have recorded a complaint is itself gone.
    #[tokio::test]
    async fn a_call_pruned_before_the_decision_is_passed_over_in_silence() {
        let capture = crate::testing::LogCapture::start();
        let (state, _, _tmp) = one_stored_call().await;

        assert_eq!(
            queue_for_enhancement(&state, 999_999).await,
            Queued::Vanished
        );

        capture.assert_never_logged("reason=");
    }
}
