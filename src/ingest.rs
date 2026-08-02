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
//! The serialized pipeline (ADR-0001): authorize -> auto-populate/blacklist
//! policy (#8) -> dedup -> write audio object -> insert DB row (in a transaction)
//! -> emit to the live feed. A Call dropped by the policy (blacklisted, or an
//! unknown System/Talkgroup with auto-populate off) still returns HTTP 200
//! `Call imported successfully.` so the recorder never retries — matching rdio,
//! which likewise 200s and drops the call asynchronously.
//!
//! Which is exactly why every outcome is written down (ADR-0011 rule 3, #29):
//! two of the five rejections tell the recorder "imported successfully", so the
//! server's own log is the only place the truth exists. Every path that declines
//! to store a Call goes through [`rejected`] and leaves a WARN line carrying a
//! machine-readable `reason`, inside a span naming the System and Talkgroup it
//! was about — and the Call id too, once there is one.

use std::sync::Arc;

use axum::extract::{Multipart, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use tracing::{Instrument, Level, Span, field, info, span, warn};

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
) -> Result<Imported, Failure> {
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
        system_ref,
        system_label: upload.system_label,
        talkgroup_ref,
        // rdio drops empty and the "-" placeholder (parsers.go); recorders send
        // these parts even when the talkgroup is unknown, so clean them to NULL.
        talkgroup_label: clean(upload.talkgroup_label),
        talkgroup_name: clean(upload.talkgroup_name),
        talkgroup_tag: clean(upload.talkgroup_tag),
        talkgroup_groups: parse_groups(upload.talkgroup_group, upload.talkgroup_groups),
        call_at_ms,
        frequency: upload.frequency.as_deref().and_then(parse_i64),
        source_ref: upload.source.as_deref().and_then(parse_i64),
        object_key: String::new(),
        audio_mime: upload.audio_mime,
        audio_name: upload.audio_name,
        // Both filled in by `ingest_call` once the object is written.
        audio_size: None,
        // Both filled in by `ingest_call` once the audio has been read.
        duration_ms: None,
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
        ..Default::default()
    };

    ingest_call(&state, &key, new_call, audio).await
}

/// The shared ingest pipeline used by both upload endpoints (ADR-0001):
/// authorize -> dedup -> write audio object -> insert row (+children) in a
/// transaction -> emit to the live feed. `new_call.object_key` is filled here.
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
async fn ingest_call(
    state: &AppState,
    key: &str,
    new_call: NewCall,
    audio: Vec<u8>,
) -> Result<Imported, Failure> {
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
    mut new_call: NewCall,
    audio: Vec<u8>,
) -> Result<Imported, Failure> {
    // Auth (ADR-0008): recorders always require a valid, in-scope API key.
    //
    // The key itself is never logged, at any level, in any form (rule 2) — and
    // an unknown key has no row to name it by anyway.
    if !repo::authorize_ingest(&state.db, key, new_call.system_ref)
        .await
        .map_err(Stage::Auth.failed())?
    {
        return Err(Reason::InvalidApiKey {
            system_ref: new_call.system_ref,
            talkgroup_ref: new_call.talkgroup_ref,
        }
        .into());
    }

    // Auto-populate + blacklist policy (#8): decide before any audio is written.
    // A dropped Call still returns success so the recorder doesn't retry — which
    // makes the WARN line the only record that it was dropped at all.
    let (auto_populate, talkgroup_id) = match repo::ingest_disposition(
        &state.db,
        new_call.system_ref,
        new_call.talkgroup_ref,
        state.ingest.auto_populate,
    )
    .await
    .map_err(Stage::AutoPopulatePolicy.failed())?
    {
        repo::Disposition::Store {
            auto_populate,
            talkgroup_id,
        } => (auto_populate, talkgroup_id),
        repo::Disposition::Drop(reason) => return Err(dropped(reason).into()),
    };

    // Dedup (ADR-0001): the same *channel* within the window — the Talkgroup the
    // Ref resolved to a moment ago, not the Ref itself (#45), so a transmission
    // uploaded once per member Ref is stored once.
    if repo::is_duplicate_call(
        &state.db,
        talkgroup_id,
        new_call.call_at_ms,
        state.ingest.dedup_window_ms,
    )
    .await
    .map_err(Stage::Dedup.failed())?
    {
        return Err(Reason::Duplicate.into());
    }

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
    if !new_call.encrypted {
        new_call.object_key = crate::blob::new_object_key(&audio_extension(&new_call.audio_name));
        // The byte length rides along on the row so retention's size cap is a
        // `SUM()` rather than a stat per object (#10).
        new_call.audio_size = Some(audio.len() as i64);

        // Write the audio object first (ADR-0001); a failed DB insert afterward
        // leaves an orphan the GC sweep reclaims (#10).
        state
            .audio
            .put(&new_call.object_key, bytes::Bytes::from(audio))
            .await
            .map_err(Stage::StoreAudio.failed())?;
    }
    let audio_bytes = new_call.audio_size.unwrap_or_default();

    // Insert the row (+ children) atomically.
    let call = insert_in_txn(&state.db, &new_call, auto_populate, state.clock.now_ms())
        .await
        .map_err(Stage::StoreCall.failed())?;
    // Everything this upload says from here on names the row it became.
    Span::current().record("call_id", call.id);

    // Emit to the live feed, denormalizing the row already in hand rather than
    // re-fetching it by id (#86). Iterated rather than unwrapped: one row in
    // gives one view out, so an `if let Some` here would be a branch whose empty
    // arm no test can reach — the same case `repo::call_detail` resolves with a
    // `.map` for the same reason.
    for view in repo::stored_calls(&state.db, std::slice::from_ref(&call))
        .await
        .map_err(Stage::BuildCallView.failed())?
    {
        state.publish(Arc::new(view));
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
    if call.has_audio() {
        queue_for_enhancement(state, call.id).await;
    }

    // The other half of rule 3: an ingest that *did* become a row is a notable
    // normal event, so "nothing is arriving" is answerable without waiting for
    // something to go wrong. Per-Call, never per-anything-smaller (rule 8).
    info!(audio_bytes, "call stored");
    Ok(Imported)
}

/// The Call became a row — the one answer this endpoint gives that isn't a
/// [`Failure`].
///
/// Its string is a wire contract (ADR-0001): SDRTrunk reads the body on a 200
/// and branches on it. Two *rejections* answer with the same string so that a
/// recorder never retries a Call we deliberately dropped — which is why they
/// carry a WARN line and this carries an INFO one.
pub struct Imported;

impl IntoResponse for Imported {
    fn into_response(self) -> Response {
        (StatusCode::OK, crate::failure::CALL_IMPORTED).into_response()
    }
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
async fn queue_for_enhancement(state: &AppState, call_id: crate::call::CallId) {
    if !state.enhancer.is_enabled() {
        return;
    }
    let scope = match repo::enhancement_scope(&state.db, call_id).await {
        Ok(Some(scope)) => scope,
        // Pruned between the insert and here, which retention is entitled to do.
        Ok(None) => return,
        Err(error) => {
            return warn!(
                reason = %"scope-unreadable",
                %error,
                "could not decide whether to enhance this Call"
            );
        }
    };
    if !state.enhancer.applies_to(scope) {
        return;
    }
    // Marked before it is offered, so a process that dies between the two finds
    // it again at the next boot rather than losing it. The reverse order would
    // leave a Call queued in memory and `none` on disk.
    if let Err(error) =
        repo::mark_enhancement(&state.db, call_id, call::EnhancementState::PENDING).await
    {
        return warn!(
            reason = %"mark-pending-failed",
            %error,
            "could not mark a Call for enhancement"
        );
    }
    state.enhancer.offer(state, call_id).await;
}

async fn insert_in_txn(
    db: &crate::db::Db,
    new_call: &NewCall,
    auto_populate: bool,
    now_ms: i64,
) -> Result<crate::db::entities::call::Model, sea_orm::DbErr> {
    let txn = db.begin().await?;
    let call = repo::insert_call(&txn, new_call, auto_populate, now_ms).await?;
    txn.commit().await?;
    Ok(call)
}

/// `POST /api/trunk-recorder-call-upload` — Trunk Recorder's native
/// `.wav`+`.json` upload: the metadata rides as a single JSON `meta` part rather
/// than individual form fields (rdio `parsers.go` mapping).
pub async fn trunk_recorder_call_upload(
    State(state): State<AppState>,
    mut multipart: Multipart,
) -> Result<Imported, Failure> {
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
        object_key: String::new(),
        audio_mime,
        audio_name,
        audio_size: None,
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

/// Why the auto-populate/blacklist policy dropped a Call (#8), as the
/// [`Reason`] that carries its wire form and its log line.
///
/// Both answer HTTP 200 `Call imported successfully.` so the recorder never
/// retries — which is exactly why the WARN line beside them is the only record
/// they happened at all.
fn dropped(reason: repo::DropReason) -> Reason {
    match reason {
        repo::DropReason::Blacklisted => Reason::Blacklisted,
        repo::DropReason::NotPopulated => Reason::NotPopulated,
    }
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
    use rstest::rstest;
    use sea_orm::ConnectionTrait;

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

    /// A Call, its System and its Talkgroup, in a database of this test's own.
    async fn one_stored_call() -> (AppState, crate::call::CallId, tempfile::TempDir) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let db = crate::db::connect(&crate::testing::sqlite_url(&tmp))
            .await
            .expect("db");
        let store = Arc::new(crate::BlobStore::filesystem(tmp.path().join("audio")).expect("blob"));
        let call = repo::insert_call(
            &db,
            &NewCall {
                system_ref: 11,
                talkgroup_ref: 54241,
                call_at_ms: 1_000,
                object_key: "aa/1.wav".into(),
                ..Default::default()
            },
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

    /// The scope read fails — a half-applied migration, a table taken out from
    /// under a running process.
    ///
    /// It must cost the recorder nothing. By the time this runs the Call is
    /// stored, answered and already on the live feed, so the only thing at stake
    /// is the levelling — and ingest that started failing over an optional
    /// convenience would be a far worse bug than the one it reported.
    #[tokio::test]
    async fn a_scope_that_cannot_be_read_leaves_the_call_alone_and_says_why() {
        let capture = crate::testing::LogCapture::start();
        let (state, call_id, _tmp) = one_stored_call().await;
        // A column rather than the table: a Call row references its System, so
        // dropping the table is refused outright — and a half-applied migration
        // is what this actually looks like in the field anyway.
        state
            .db
            .execute_unprepared("ALTER TABLE systems DROP COLUMN enhancement")
            .await
            .expect("take the column away");

        queue_for_enhancement(&state, call_id).await;

        let logged = capture.text();
        assert!(
            logged.contains("reason=scope-unreadable"),
            "the operator must be told the decision could not be made: {logged}"
        );
        assert!(
            logged.contains("no such column: systems.enhancement"),
            "...and why: {logged}"
        );
    }

    /// The Call was pruned between its insert and this decision, which retention
    /// is entitled to do. There is nothing to enhance and nothing to say — the
    /// row that would have recorded a complaint is itself gone.
    #[tokio::test]
    async fn a_call_pruned_before_the_decision_is_passed_over_in_silence() {
        let capture = crate::testing::LogCapture::start();
        let (state, _, _tmp) = one_stored_call().await;

        queue_for_enhancement(&state, 999_999).await;

        capture.assert_never_logged("reason=");
    }
}
