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
use tracing::{Instrument, Level, Span, debug, field, info, span, warn};

use crate::archive;
use crate::call::{CallId, Candidate, Quality};
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
    /// What counts as the same transmission arriving twice (#46).
    pub dedup_scope: Scope,
    /// Which copy of it is kept (#46).
    pub dedup_keep: Keep,
    /// How long a stored Call stays open to a better copy of its transmission
    /// (#46) — **a different quantity from [`Self::dedup_window_ms`]**, and
    /// that is why it is its own setting.
    ///
    /// The window above measures how far apart two *transmissions* may be to be
    /// one, over `call_at_ms`, which every copy of a transmission reports
    /// identically. This one measures how far apart their *uploads* may be, over
    /// the clock — and Trunk Recorder posting one file per patched member,
    /// back to back, routinely takes longer than half a second. Sharing the one
    /// number would mean either keep-best almost never firing or the matching
    /// window widening until genuinely distinct back-to-back Calls collapse.
    ///
    /// It is also exactly how long [`crate::serve`] withholds `immutable` from
    /// a Call's audio, because for that long the bytes really can change.
    #[serde(with = "crate::config::secs", rename = "dedup_replace_secs")]
    pub dedup_replace: std::time::Duration,
    /// Global auto-populate toggle (#8). On by default, matching rdio-scanner.
    /// When off, unknown Systems are dropped and only Systems whose own
    /// per-system flag is set still auto-create Talkgroups/Units.
    pub auto_populate: bool,
}

impl Default for IngestConfig {
    fn default() -> Self {
        IngestConfig {
            dedup_window_ms: 500,
            dedup_scope: Scope::Patched,
            dedup_keep: Keep::Best,
            // Generous enough that a Recorder posting one file per patched
            // member lands them all inside it, short enough that the caching it
            // costs is only ever on a Call somebody is playing live.
            dedup_replace: std::time::Duration::from_secs(30),
            auto_populate: true,
        }
    }
}

/// How wide the duplicate test reaches (#46, spec US 10).
///
/// The default is the improvement; the other value is the way back to rdio's
/// behaviour for an Operator whose System mints patches that lie — because a
/// patch array is the radio network's claim, and a claim can be wrong in a way
/// that silently costs a Listener real traffic.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Scope {
    /// The same canonical Talkgroup only — one channel, one window. What rdio
    /// does, and what Radio-Scout did until #46.
    Talkgroup,
    /// ...or an overlapping **patch** membership, so one transmission uploaded
    /// once per member of a console patch is one Call.
    #[default]
    Patched,
}

/// Which copy of one transmission is kept when it arrives more than once (#46).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Keep {
    /// The one already stored — rdio's first-wins, where the copy that happened
    /// to be uploaded first is the copy a Listener is stuck with.
    First,
    /// The better one: fewer decode errors, then longer duration
    /// ([`Quality::better_than`]). A later-arriving winner replaces the stored
    /// copy under the same Call id.
    #[default]
    Best,
}

impl Scope {
    /// The spelling used in the file and the environment.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Scope::Talkgroup => "talkgroup",
            Scope::Patched => "patched",
        }
    }

    /// Every spelling, for an error message that lists what was expected.
    pub(crate) const ALL: [Scope; 2] = [Scope::Talkgroup, Scope::Patched];
}

/// Deliberately **no `Display`**: nothing logs a dedup policy, and an impl
/// whose only caller is the test that checks it is an impl no test can really
/// check — a no-op body passed, because `"…".contains("")` is true of every
/// string. [`Scope::as_str`] is the one spelling, and the test below asserts
/// the *quoted* form the error message actually carries.
impl std::str::FromStr for Scope {
    type Err = ();

    /// One spelling per value, shared by the file and the environment, so
    /// `RADIO_SCOUT_INGEST_DEDUP_SCOPE=patched` and `dedup_scope = "patched"`
    /// cannot drift apart.
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        Scope::ALL
            .into_iter()
            .find(|scope| scope.as_str() == text)
            .ok_or(())
    }
}

impl Keep {
    /// The spelling used in the file and the environment.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Keep::First => "first",
            Keep::Best => "best",
        }
    }

    pub(crate) const ALL: [Keep; 2] = [Keep::First, Keep::Best];
}

impl std::str::FromStr for Keep {
    type Err = ();

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        Keep::ALL
            .into_iter()
            .find(|keep| keep.as_str() == text)
            .ok_or(())
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

/// The Call now arriving, as the duplicate decision sees it (#46) — which
/// Talkgroups it reaches, when it happened, and how good a copy it is.
///
/// Its own type rather than three arguments because the channel half of the
/// test is symmetric: the arriving copy and a stored [`Candidate`] are compared
/// as two sets of Talkgroups, and a pair of loose `&[i64]`s in that position is
/// exactly the shape that gets passed the wrong way round.
struct Arriving<'a> {
    /// The **canonical** Talkgroup Ref this Call's own Ref resolved to, or
    /// `None` when no Talkgroup owns it yet.
    ///
    /// `None` is not the dead end it looks like. A console minting a fresh TGID
    /// per patch event (rdio's issue #466) sends exactly this: a Ref nothing
    /// has ever heard of, patched to channels the System knows perfectly well —
    /// so the match is carried by `patches` alone, and refusing to look would
    /// store the churn as a Call per patch event.
    talkgroup: Option<i64>,
    /// The canonical Talkgroup Refs it is patched to, already resolved to their
    /// owning channels the same way [`Candidate::patches`] was.
    patches: &'a [i64],
    call_at_ms: i64,
    quality: Quality,
    /// Whether this copy will store audio at all. An **Encrypted Call** stores
    /// none (spec US 9), and that outranks every quality figure in [`replaces`]
    /// — in both directions.
    has_audio: bool,
}

impl Arriving<'_> {
    /// Every Talkgroup this copy reaches — its own, then the ones it is patched
    /// to. The same reading [`Candidate::talkgroups`] gives a stored copy.
    fn talkgroups(&self) -> impl Iterator<Item = i64> + '_ {
        self.talkgroup
            .into_iter()
            .chain(self.patches.iter().copied())
    }

    /// Whether a stored Call is **the same transmission** as this one, so far
    /// as channel goes — the time half is the window in [`duplicate_of`], and
    /// the System half is the read (a candidate is on this System already).
    fn reaches(&self, candidate: &Candidate, scope: Scope) -> bool {
        match scope {
            // rdio's test, and ours until #46: one channel, the canonical one.
            Scope::Talkgroup => self.talkgroup == Some(candidate.talkgroup),
            // Widened to every channel either copy reaches, so a transmission a
            // console patched and a recorder uploaded once per member collapses
            // into the one Call a Listener heard (spec US 10).
            //
            // Scanned rather than collected into a set: both sides are a
            // Talkgroup plus a patch array that is empty on almost every Call
            // and a handful long on the rest, so allocating to compare them
            // would cost a Pi more than the comparison does.
            Scope::Patched => self
                .talkgroups()
                .any(|mine| candidate.talkgroups().any(|theirs| theirs == mine)),
        }
    }
}

/// The stored Call this one is a copy of, if any — the **nearest** of the
/// candidates that are inside the window *and* reach a channel in common
/// (ADR-0001's duplicate detection, widened by #46).
///
/// Nearest rather than first: since the read widened to the whole System, the
/// window routinely holds Calls this one has nothing to do with, and among the
/// ones it does match, "duplicate of call 41" is only useful to an operator if
/// 41 is the Call a Listener would call the same transmission. It is also the
/// copy keep-best compares against.
///
/// **Matched before nearest**, and the order is load-bearing: choosing the
/// nearest Call first and testing its channel afterwards would refuse an upload
/// as a duplicate of an unrelated Call that merely happened to be closer.
///
/// Pure, which is the point: every near miss is a value a test can construct,
/// where a `COUNT` over a window could only be approached through a socket, a
/// multipart body and two uploads.
fn duplicate_of<'a>(
    candidates: &'a [Candidate],
    arriving: &Arriving,
    config: &IngestConfig,
) -> Option<&'a Candidate> {
    let window = dedup_window(arriving.call_at_ms, config.dedup_window_ms);
    candidates
        .iter()
        .filter(|candidate| window.contains(&candidate.call_at_ms))
        .filter(|candidate| arriving.reaches(candidate, config.dedup_scope))
        // `abs_diff` rather than a subtraction: two Calls at opposite ends of
        // the epoch are a difference no `i64` holds, and this decision must not
        // be the place that discovers it.
        .min_by_key(|candidate| {
            (
                candidate.call_at_ms.abs_diff(arriving.call_at_ms),
                candidate.id,
            )
        })
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
    mut new_call: NewCall,
    audio: Vec<u8>,
) -> Result<Recorded, Failure> {
    let facts = resolve(state, key, &new_call).await?;

    // The *playing* length, so a one-second kerchunk and a forty-second
    // dispatch are distinguishable everywhere (#42, spec US 8). The recorder's
    // own figure wins when it sent one — only Trunk Recorder's native meta
    // does, and it knows the call it recorded better than its own encoder's
    // header does. Everything else is read here, from the container header
    // alone: no decode, no sample touched, microseconds on a Pi. Audio whose
    // header says nothing leaves the column `NULL`, which is the honest answer
    // and never a failed ingest.
    //
    // Read **before the decision** since #46, where it used to be read inside
    // `perform`: keep-best compares durations, so how long this copy runs is one
    // of the facts the Admission depends on rather than something discovered
    // while storing it.
    //
    // ...but **after authorization** (ADR-0008), which is the one thing that
    // must not move: parsing a container header is small, and it is still
    // arbitrary bytes from an unauthenticated caller, on the hardware least able
    // to afford spending anything on them. An unauthorized upload is refused
    // having had nothing read for it and nothing parsed of it. The encrypted
    // check is deliberately still below this — the bytes are in hand either way,
    // and an **Encrypted Call** that stores none of them still gets a length.
    //
    // **The same pass is where a Call is mined** (#48). SDRTrunk writes an
    // ID3 tag ahead of the MPEG frames it uploads, carrying a configured radio
    // alias and a tower's name that no wire field of its carries at all — and
    // this probe was already walking those bytes for the duration. Mining runs
    // *here*, on the ingest path, rather than in the off-path worker its ticket
    // asked for, for three reasons: the live-feed frame is published at ingest
    // and nothing republishes one (#46), so a name arriving later never reaches
    // the Listener who heard the Call; Enhancement rewrites the audio object
    // and destroys the tag, which a worker would race and lose; and what it
    // costs is a header read, where "never on the ingest path" was written
    // about enhancement's decode-and-encode.
    if facts.authorized {
        enrich(&mut new_call, &audio, state.clock.now_ms());
    }

    let admission = match admit(&facts, &new_call, &state.ingest, state.clock.now_ms()) {
        Decision::Admit { auto_populate } => {
            perform(state, new_call, audio, &facts.resolved, auto_populate).await?
        }
        // The same transmission, arrived better (#46). The stored Call keeps
        // its id and every routing fact a Listener may already be holding; what
        // it gains is this copy's audio and what the recorder said about it.
        Decision::Replace { of, auto_populate } => {
            replace(state, of, new_call, audio, &facts.resolved, auto_populate).await?
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

/// What this Call's own audio can add to what its Recorder said — read in one
/// pass, and only where there is something to read (#42, #48).
///
/// Two gates, and between them a Trunk Recorder Call pays exactly what it paid
/// before this existed:
///
/// - **A duration is read only where the recorder gave none.** TR's native meta
///   counts the samples it wrote, which beats any header.
/// - **A container is probed for metadata only where it opens with an ID3 tag.**
///   Three bytes decide it. SDRTrunk writes its tag *before* the first MPEG
///   frame (`AudioSegmentRecorder.recordMP3`), so this is exact for the one
///   recorder that embeds anything — and it keeps a full isomp4 probe off every
///   TR upload, which is the Pi's whole reason for a gate here.
fn enrich(new_call: &mut NewCall, audio: &[u8], now_ms: i64) {
    // Stamped whatever is found, including nothing at all: the column records
    // that this Call's audio has been *read*, so the sweep never comes back
    // to a Call ingest already looked inside.
    new_call.mined_at_ms = Some(now_ms);
    let wants_duration = new_call.duration_ms.is_none();
    let wants_mining = audio.starts_with(crate::audio_meta::ID3);
    if !wants_duration && !wants_mining {
        return;
    }
    let facts = crate::audio_meta::read(audio);
    if wants_duration {
        new_call.duration_ms = facts.duration_ms;
    }
    let Some(mined) = wants_mining.then(|| crate::mining::mine(&facts)).flatten() else {
        return;
    };
    if crate::mining::apply(new_call, &mined) {
        // DEBUG: routine, once per Call, and an Operator does not act on it —
        // but "why is my SDRTrunk alias not showing?" has no other answer
        // (ADR-0011 rule 7).
        debug!(?mined, "mined the Call's own audio");
    }
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
    /// The System, the Talkgroup its Ref resolved to, and the channels its
    /// `patches` array named — the last of which is new to [`repo::Resolved`]
    /// with #46, because the widened duplicate test is asked over them and the
    /// insert would otherwise resolve the same array a second time.
    resolved: repo::Resolved,
    /// The Calls already stored on this **System** inside the dedup window.
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

    // The System, the Talkgroup, and — since #46 — the channels this Call's
    // `patches` array names, because the widened duplicate test is asked over
    // them. Free on almost every Call: an empty array costs no statement.
    let resolved = repo::resolve_refs(
        &state.db,
        new_call.system_ref,
        new_call.talkgroup_ref,
        &new_call.patches,
    )
    .await
    .map_err(Stage::ResolveRefs.failed())?;

    // Dedup (ADR-0001) looks across the whole **System** inside the window, not
    // one channel: since #46 a copy of the same transmission may arrive on a
    // patched Talkgroup, or on a patch-minted Ref no Talkgroup owns yet, and
    // neither is reachable from a query keyed on the channel this one resolved
    // to. Which of them is the same transmission is [`admit`]'s decision.
    let candidates = repo::calls_within(
        &state.db,
        resolved.system_id(),
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
fn admit(facts: &Facts, call: &NewCall, config: &IngestConfig, now_ms: i64) -> Decision {
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

    let arriving = Arriving {
        talkgroup: facts.resolved.talkgroup_ref(),
        // Empty until `resolve` has run, which is exactly the unauthorized case
        // the arm above already returned for.
        patches: facts.resolved.patches.as_deref().unwrap_or_default(),
        call_at_ms: call.call_at_ms,
        quality: call.quality(),
        has_audio: !call.encrypted,
    };

    match duplicate_of(&facts.candidates, &arriving, config) {
        // Keep-best (#46): the same transmission, arrived twice. Whichever copy
        // is better is the one a Listener ends up with — and when it is the one
        // arriving, it takes the stored Call's place rather than its own row,
        // so the Call a Listener already has in hand quietly improves.
        Some(stored) => match replaces(&arriving, stored, config, now_ms) {
            true => Decision::Replace {
                of: stored.id,
                auto_populate,
            },
            false => Decision::Refused(Admission::Duplicate { of: stored.id }),
        },
        None => Decision::Admit { auto_populate },
    }
}

/// Whether an arriving copy **takes the stored Call's place**, or is merely a
/// duplicate of it (#46).
///
/// Being better is not enough, and the two extra conditions are both easy to
/// miss.
///
/// **The stored Call must still be young.** Replaceability is measured from when
/// it was *stored*, not from when its transmission happened: two copies of one
/// transmission share a `call_at_ms` however far apart they arrive, so without
/// that bound a Call would be replaceable forever and [`crate::serve`] could
/// never tell a Listener that its audio is `immutable`, because it would not be.
/// So the dedup window means two things, deliberately and in the same units —
/// how far apart two transmissions may be to be one, and how long a stored copy
/// stays open to a better one — and an Operator whose recorders upload minutes
/// apart raises it and gets both. What that gives up is small and degrades in
/// the right direction: a better copy arriving after the window is still
/// recognised as a duplicate, so nothing plays twice; only the upgrade is
/// missed.
///
/// **And the arriving copy must have audio to give.** An **Encrypted Call**
/// stores none at all (spec US 9), yet an encrypted copy can genuinely *win* the
/// comparison: its Duration is Trunk Recorder's own figure and its error count
/// its `freqList`'s, so it can be both longer and cleaner than a copy somebody
/// actually recorded. Letting it win would take playable audio away from a
/// Listener and leave a metadata-only row, which is not "the better copy" by any
/// reading a Listener would recognise. It stays a duplicate — the activity is
/// recorded once, as it should be — it just cannot displace what plays.
fn replaces(arriving: &Arriving, stored: &Candidate, config: &IngestConfig, now_ms: i64) -> bool {
    if config.dedup_keep != Keep::Best
        || !still_replaceable(stored.stored_ms, now_ms, config.dedup_replace)
    {
        return false;
    }
    match (arriving.has_audio, stored.has_audio) {
        // **Audio beats no audio, in both directions and before anything
        // else.** An Encrypted Call is a row with nothing to hear (spec US 9),
        // and a copy of the same transmission that somebody did decode is worth
        // more than any quality figure — so it replaces one, and one never
        // replaces it. The comparison is not even asked: an encrypted copy can
        // *win* it on Trunk Recorder's own duration while carrying no audio at
        // all, and either arm of that would leave a Listener hearing a call
        // they could have heard.
        (true, false) => true,
        (false, true) => false,
        // Both have audio, or neither does. Now quality decides.
        _ => arriving.quality.better_than(&stored.quality),
    }
}

/// How long a stored Call stays open to a better copy of its transmission —
/// **the only place that arithmetic is written** (#46).
///
/// [`crate::serve`] asks the same question to decide whether it may promise a
/// Listener that a Call's audio is `immutable`, and asks it *here* rather than
/// spelling it again: the header is a promise this rule keeps, so a mutation of
/// either bound has to move both or a test sees it. The same discipline
/// [`dedup_window`] applies to the window's other meaning.
///
/// Saturating for the same reason as [`dedup_window`]: a Recorder is entitled to
/// send an absurd timestamp, and no arithmetic on the ingest path may be the
/// thing that panics because of it.
pub(crate) fn still_replaceable(
    stored_ms: i64,
    now_ms: i64,
    replace_window: std::time::Duration,
) -> bool {
    now_ms.saturating_sub(stored_ms) <= replace_window.as_millis() as i64
}

/// What [`admit`] decided: something to perform, or an [`Admission`] that is
/// already complete.
#[derive(Debug, PartialEq, Eq)]
enum Decision {
    /// Admitted. `auto_populate` is the effective flag the insert needs, and
    /// this becomes [`Admission::Stored`] once there is a row.
    Admit { auto_populate: bool },
    /// The same transmission as Call `of`, and a better copy of it (#46). The
    /// stored Call keeps its id and gains this copy's audio and metadata; this
    /// becomes [`Admission::Replaced`].
    Replace { of: CallId, auto_populate: bool },
    /// Not admitted — and there is nothing to perform, so this *is* the
    /// Admission ingest answers with.
    Refused(Admission),
}

/// Write a Call's audio object, or decide there is none to write.
///
/// An encrypted Call is a row and nothing else (spec US 9): the activity is
/// worth seeing, the audio is worth nothing — it is the vocoder's noise, not
/// speech, and storing it would spend a Pi's disk on something no listener can
/// use. `object_key` stays empty, which is what the serve path and the wire both
/// read as "there is nothing here", and `audio_size` stays `NULL`, so
/// retention's cap counts what actually exists.
///
/// The bytes are still *required* on the wire: this endpoint's dialect asks for
/// an audio part and refusing one for some Calls and not others would make a
/// recorder's success depend on a flag it also sent.
///
/// Written before any row (ADR-0001); a failed write afterward leaves an orphan
/// the GC sweep reclaims (#10). Neither the insert nor the replacement below can
/// name an object without the value this produces, which is that ordering
/// expressed rather than commented (#96).
async fn store_audio(
    state: &AppState,
    new_call: &NewCall,
    audio: Vec<u8>,
) -> Result<Option<crate::blob::StoredAudio>, Failure> {
    if new_call.encrypted {
        return Ok(None);
    }
    let key = crate::blob::new_object_key(&audio_extension(&new_call.audio_name));
    let bytes = audio.len();
    state
        .audio
        .put(&key, bytes::Bytes::from(audio))
        .await
        .map_err(Stage::StoreAudio.failed())?;
    Ok(Some(crate::blob::StoredAudio::written(key, bytes)))
}

/// Point an already-stored Call at this better copy of its transmission (#46,
/// spec US 10) — everything a replacement costs, and nothing that decides
/// anything.
///
/// **The live feed is deliberately not told.** A Listener already has this Call:
/// it is in their queue, or played, or on an Archive page in front of them. A
/// second frame carrying the same id is exactly the double-play keep-best exists
/// to prevent, and the audio URL is stable, so a Listener who has not fetched
/// yet simply gets the better copy. That is the whole of "listeners notice
/// nothing".
///
/// The object the Call used to point at is left where it is rather than deleted.
/// ADR-0002's ordering rests on objects being immutable once written, so a
/// Listener mid-download keeps reading a file that still exists until #10's
/// orphan-GC reclaims it — the same reasoning as [`crate::enhance::step`], which
/// swaps an object under a Call for the same reason and never deletes one
/// either.
async fn replace(
    state: &AppState,
    of: CallId,
    new_call: NewCall,
    audio: Vec<u8>,
    resolved: &repo::Resolved,
    auto_populate: bool,
) -> Result<Admission, Failure> {
    // There is always an object here in practice — [`replaces`] refuses to let a
    // copy with no audio displace one that plays — but the shape is shared with
    // [`perform`], where an **Encrypted Call** legitimately writes none.
    let stored = store_audio(state, &new_call, audio).await?;
    let audio_bytes = stored.as_ref().map(|a| a.bytes()).unwrap_or_default();

    let txn = state
        .db
        .begin()
        .await
        .map_err(Stage::ReplaceCall.failed())?;
    let replacement = repo::store_replacement(
        &txn,
        of,
        &new_call,
        stored,
        resolved,
        auto_populate,
        state.clock.now_ms(),
    )
    .await
    .map_err(Stage::ReplaceCall.failed())?;
    // **A replacement is forwarded again** (#52). The peer is holding the copy
    // this Instance has just decided was the worse one — which for an
    // encrypted-versus-decoded pair means it is holding no audio where we have
    // some. A peer running Radio-Scout applies its own keep-best and upgrades;
    // an rdio peer answers `duplicate call rejected` and keeps what it had, so
    // the cost of asking is one refused upload on a path that only fires inside
    // the replace window.
    let forwarding = enqueue_forwarding(
        &txn,
        &new_call,
        resolved,
        replacement.call().id,
        state.clock.now_ms(),
    )
    .await
    .map_err(Stage::ReplaceCall.failed())?;
    // **And posted again** (#54), for a different reason than the forward: a
    // **Replacement** can turn an encrypted Call into a decoded one, or a
    // truncated copy into a whole one, and a webhook that fired on the worse
    // copy would otherwise link an Operator to audio this Instance has since
    // improved on. The unique index means the *usual* case — a replacement
    // arriving before the first copy was sent — is a no-op rather than a second
    // message.
    let posting = enqueue_webhooks(
        &txn,
        &new_call,
        resolved,
        replacement.call().id,
        state.clock.now_ms(),
    )
    .await
    .map_err(Stage::ReplaceCall.failed())?;
    txn.commit().await.map_err(Stage::ReplaceCall.failed())?;
    state.downstreams.owes(forwarding);
    state.webhooks.owes(posting);

    let call = match replacement {
        repo::Replacement::Replaced(call) => call,
        // The Call this was a better copy of is gone — retention is entitled to
        // prune one between the decision and this write. So the copy became a
        // Call of its own, and unlike a replacement it *has* to be published:
        // nobody has heard this transmission at all.
        repo::Replacement::Stored(call) => {
            Span::current().record("call_id", call.id);
            publish(state, &call).await?;
            offer_for_enhancement(state, &call).await;
            return Ok(Admission::Stored {
                call_id: call.id,
                audio_bytes,
            });
        }
    };
    Span::current().record("call_id", call.id);

    // The levelled audio a previous pass produced describes a copy nobody holds
    // any more, so the row went back to `none` and the Call is offered again.
    offer_for_enhancement(state, &call).await;

    Ok(Admission::Replaced {
        call_id: call.id,
        audio_bytes,
    })
}

/// Write the audio object, insert the row, publish it, offer it for
/// enhancement — everything an admitted Call costs, and nothing that decides
/// anything.
async fn perform(
    state: &AppState,
    new_call: NewCall,
    audio: Vec<u8>,
    resolved: &repo::Resolved,
    auto_populate: bool,
) -> Result<Admission, Failure> {
    let stored = store_audio(state, &new_call, audio).await?;
    let audio_bytes = stored.as_ref().map(|a| a.bytes()).unwrap_or_default();

    // Insert the row (+ children) atomically, into the channel already resolved
    // for this Call rather than one looked up a second time (#96) — and, in the
    // same transaction, queue it for every **Downstream** it reaches (#52).
    let Stored {
        call,
        forwarding,
        posting,
    } = insert_in_txn(
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
    // After the commit, never inside it: an attempt admitted for a delivery
    // that then rolled back would be a debt nothing could settle.
    state.downstreams.owes(forwarding);
    state.webhooks.owes(posting);

    publish(state, &call).await?;
    offer_for_enhancement(state, &call).await;

    Ok(Admission::Stored {
        call_id: call.id,
        audio_bytes,
    })
}

/// Emit a newly stored Call to the live feed, denormalizing the row already in
/// hand rather than re-fetching it by id (#86).
///
/// Iterated rather than unwrapped: one row in gives one view out, so an `if let
/// Some` here would be a branch whose empty arm no test can reach — the same
/// case `archive::detail` resolves with a `.map` for the same reason.
async fn publish(state: &AppState, call: &call::Model) -> Result<(), Failure> {
    for view in archive::stored_calls(&state.db, std::slice::from_ref(call))
        .await
        .map_err(Stage::BuildCallView.failed())?
    {
        state.publish(Arc::new(view)).await;
    }
    Ok(())
}

/// Offer a stored Call to the enhancement queue (#20).
///
/// Starts *here* — after the recorder has its answer and after the live feed
/// already has the Call. Scope is resolved now rather than before the insert
/// because auto-populate may have created the System or the Talkgroup a moment
/// ago, and a row that has just been created says `NULL`, which is the value
/// that inherits.
///
/// An encrypted Call has no object to enhance (#42), and offering one is not
/// harmless: the worker would mark it `pending`, ask the store for the object
/// named by the empty string, fail, and settle it `skipped` with a WARN — once
/// per Call, forever, on a System whose traffic is mostly encrypted. Asked here
/// so the three queries the scope lookup costs are never spent either.
///
/// Deliberately not part of the Admission, and not on its line: the Call is
/// stored, answered and already on the live feed, so what the queue makes of it
/// costs a listener nothing either way — and an `enhancement=not-enhancing`
/// field on every Call would be a per-Call field about a feature nobody turned
/// on. The outcome exists so the arms are assertable (#96) and for #70's status
/// surface to read.
async fn offer_for_enhancement(state: &AppState, call: &call::Model) {
    if call.has_audio() {
        let _queued = queue_for_enhancement(state, call.id).await;
    }
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
    /// The same transmission as a Call already stored, and a **better copy** of
    /// it (#46): that Call now carries this audio, under its own id.
    ///
    /// Not a refusal — the copy was taken, so the recorder is told it was
    /// imported, which is true. It is its own arm rather than a `Stored` because
    /// no new Call exists and the live feed was deliberately not told.
    Replaced { call_id: CallId, audio_bytes: i64 },
    /// The same transmission, already stored inside the dedup window (ADR-0001),
    /// and this copy was no better than the one there.
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
            // The two arms that took the copy. A replacement answers the
            // recorder exactly as a store does: its audio *was* imported, and
            // where it landed is Radio-Scout's business rather than something a
            // recorder has a branch for (ADR-0001 pins the strings SDRTrunk
            // reads, and "imported successfully" is the true one here).
            Admission::Stored { .. } | Admission::Replaced { .. } => None,
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
            // Its own line, and INFO for the same reason: a Call quietly
            // improving under a Listener is a notable normal event, and it is
            // the *only* record that this upload happened at all — the recorder
            // was told "imported successfully" and no new row exists to find.
            Admission::Replaced { audio_bytes, .. } => {
                info!(audio_bytes, "call replaced with a better copy")
            }
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
) -> Result<Stored, sea_orm::DbErr> {
    let txn = db.begin().await?;
    let call = repo::insert_call(&txn, new_call, audio, resolved, auto_populate, now_ms).await?;
    let forwarding = enqueue_forwarding(&txn, new_call, resolved, call.id, now_ms).await?;
    let posting = enqueue_webhooks(&txn, new_call, resolved, call.id, now_ms).await?;
    txn.commit().await?;
    Ok(Stored {
        call,
        forwarding,
        posting,
    })
}

/// A Call that is now a row, and how many outbound sinks are owed it.
///
/// Facts from one transaction, because the counts are only true if the insert
/// committed — and they have to leave the transaction to be admitted to their
/// senders' meters afterwards. See [`crate::delivery::Dispatcher::owes`] for why
/// they cannot be admitted inside.
struct Stored {
    call: crate::db::entities::call::Model,
    forwarding: usize,
    posting: usize,
}

/// Queue this Call for every **Downstream** whose scope it reaches (#52).
///
/// **Inside the storing transaction, on purpose.** "This Call exists" and "this
/// Call is owed to these peers" are one fact; written separately, a crash
/// between them loses the forward silently and forever. It costs one indexed
/// read of a table with single-digit rows — the one statement `tests/ingest.rs`
/// accounts for — and that read is what buys the guarantee.
///
/// **Resolve, decide purely, then perform** (#96's shape, in miniature): the
/// roster is read, [`crate::downstream::routed_to`] decides, and
/// [`repo::queue_deliveries`] writes the ids it is handed. The Selection is
/// never compared inside the write, which is what keeps a domain module out of
/// the data layer.
///
/// An **Encrypted Call** is never queued: it has no audio object at all (spec US
/// 9) and the rdio dialect requires one, so a peer could only ever refuse it.
async fn enqueue_forwarding<C: sea_orm::ConnectionTrait>(
    db: &C,
    new_call: &NewCall,
    resolved: &repo::Resolved,
    call_id: CallId,
    now_ms: i64,
) -> Result<usize, sea_orm::DbErr> {
    if new_call.encrypted {
        return Ok(0);
    }
    let talkgroups = reached_channels(new_call, resolved);
    let roster = repo::forwarding_downstreams(db).await?;
    let owed = crate::downstream::routed_to(&roster, new_call.system_ref, &talkgroups);
    repo::queue_deliveries(db, call_id, &owed, now_ms).await
}

/// Queue this Call for every **Webhook** that asked for one of its marks (#54).
///
/// [`enqueue_forwarding`]'s rules, with two differences that are the whole
/// shape of this feature:
///
/// **It costs nothing at all unless the Call carries a mark.** Any Call may
/// reach a Downstream, so that roster is read on every upload; a Call carrying
/// no mark can reach no Webhook, so the roster read is behind
/// [`crate::webhook::Marks::is_empty`] and an ordinary Call issues **no
/// statement here**. On a Pi taking a Call a second with Emergencies a few times
/// a day, that is the difference between one extra statement per Call and one
/// per Emergency.
///
/// **An Encrypted Call is queued.** Forwarding refuses one because the rdio
/// dialect needs an audio object it does not have; a webhook carries facts, and
/// an encrypted Emergency is exactly the fact an Operator most wants to be told
/// about.
async fn enqueue_webhooks<C: sea_orm::ConnectionTrait>(
    db: &C,
    new_call: &NewCall,
    resolved: &repo::Resolved,
    call_id: CallId,
    now_ms: i64,
) -> Result<usize, sea_orm::DbErr> {
    let marks = crate::webhook::Marks::on_call(new_call.emergency);
    if marks.is_empty() {
        return Ok(0);
    }
    let talkgroups = reached_channels(new_call, resolved);
    let roster = repo::delivering_webhooks(db).await?;
    let owed = crate::webhook::routed_to(&roster, &marks, new_call.system_ref, &talkgroups);
    repo::queue_webhook_deliveries(db, call_id, &owed, now_ms).await
}

/// Every Talkgroup this Call reaches: the channel it resolved to, then
/// everything it is patched to.
///
/// The same set the live feed routes on, which is the half rdio's own forwarder
/// omits — and written once because both outbound sinks scope on it and a set
/// that differed between them would mean a **Patch** reaching a peer and not a
/// webhook, or the reverse.
///
/// It costs no lookup of its own: both halves come from the [`repo::Resolved`]
/// the pipeline already read (#96). The canonical Ref falls back to the one the
/// recorder sent, which is the auto-populate case — `insert_call` has just
/// created that Talkgroup under exactly this Ref, and it was `None` here only
/// because nothing had resolved it beforehand.
fn reached_channels(new_call: &NewCall, resolved: &repo::Resolved) -> Vec<i64> {
    let mut talkgroups = vec![resolved.talkgroup_ref().unwrap_or(new_call.talkgroup_ref)];
    for patched in resolved.patches.as_deref().unwrap_or_default() {
        if !talkgroups.contains(patched) {
            talkgroups.push(*patched);
        }
    }
    talkgroups
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
        // Trunk Recorder names no tower, and **Mining** (#48) fills this on the
        // one dialect that does — inside the audio, after the parse. `enrich`
        // stamps `mined_at_ms` for the same reason, in the same place.
        site_label: None,
        mined_at_ms: None,
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
            // The **OTA alias**, not the configured one (CONTEXT.md, #47).
            // SDRTrunk's `getTalkerAlias` reads a `TalkerAliasIdentifier` — a
            // name the radio put over the air — where `label` means the alias
            // an operator wrote down. Storing it as the latter would make the
            // one column that records *who said this name* say the wrong thing,
            // on every SDRTrunk upload there is.
            tag_ota: talker_alias,
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

    // -- The dedup window, decided over candidate Calls (#96, widened by #46) -

    /// The Talkgroup the arriving Call in these tests is on, and one it is not.
    const OURS: i64 = 54241;
    const ANOTHER: i64 = 54999;

    /// The instant these tests call "now" — and, unless a case says otherwise,
    /// the instant every stored Call was stored, so a Call is always young
    /// enough to be replaced and the decisions below turn on nothing else.
    const NOW: i64 = 5_000_000;

    /// A stored Call on Talkgroup `talkgroup`, `offset` milliseconds from the
    /// arriving one, patched to `patches`, and a perfectly ordinary copy.
    fn stored(offset: i64, talkgroup: i64, patches: &[i64]) -> Candidate {
        Candidate {
            id: 7,
            call_at_ms: 1_000_000 + offset,
            stored_ms: NOW,
            talkgroup,
            patches: patches.to_vec(),
            quality: Quality::default(),
            has_audio: true,
        }
    }

    /// One stored Call on the arriving Call's own Talkgroup, `offset`
    /// milliseconds away — the plain same-channel case the window tests want.
    fn stored_at(offset: i64) -> Vec<Candidate> {
        vec![stored(offset, OURS, &[])]
    }

    /// The arriving Call: on [`OURS`], patched to `patches`, at 1_000_000.
    fn arriving(patches: &[i64]) -> Arriving<'_> {
        Arriving {
            talkgroup: Some(OURS),
            patches,
            call_at_ms: 1_000_000,
            quality: Quality::default(),
            has_audio: true,
        }
    }

    /// The id of the Call an arriving one duplicates, under a given window.
    fn duplicate_id(candidates: &[Candidate], call_at_ms: i64, window_ms: i64) -> Option<CallId> {
        let arriving = Arriving {
            call_at_ms,
            ..arriving(&[])
        };
        duplicate_of(
            candidates,
            &arriving,
            &IngestConfig {
                dedup_window_ms: window_ms,
                ..IngestConfig::default()
            },
        )
        .map(|candidate| candidate.id)
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
            duplicate_id(&stored_at(offset), 1_000_000, 500).is_some(),
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
            duplicate_id(&stored_at(offset), 1_000_000, 0).is_some(),
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
            duplicate_id(&stored_at(0), at, i64::MAX).is_some(),
            duplicate
        );
        // ...and a Call at that same absurd instant is a duplicate of itself
        // however far apart the two ends of the epoch are. Written as an
        // absolute time rather than an offset, because the offset from the
        // helper's own epoch is itself a subtraction that overflows here.
        assert_eq!(
            duplicate_id(
                &[Candidate {
                    call_at_ms: at,
                    ..stored(0, OURS, &[])
                }],
                at,
                i64::MAX
            ),
            Some(7)
        );
    }

    /// **An arriving copy replaces a stored one only if it is better, the
    /// stored one is still young, and keep-best is on** — every condition
    /// named, each one alone, so none of them can be dropped silently.
    ///
    /// The last pair is the one worth reading twice. An **Encrypted Call**
    /// stores no audio at all (spec US 9), and an encrypted copy can genuinely
    /// win the comparison — its Duration comes from Trunk Recorder's own meta
    /// and its error count from its `freqList`, so it can be both longer and
    /// cleaner than a copy somebody actually recorded. Letting it win would
    /// take away audio a Listener could play and leave a metadata-only row
    /// behind, which is not "the better copy" by any reading a Listener would
    /// recognise. It is still recognised as the same transmission and still
    /// refused as a duplicate — it simply cannot displace what plays.
    #[rstest]
    #[case::a_better_copy_replaces(false, true, true, Keep::Best, 0, true)]
    #[case::a_worse_one_does_not(true, true, true, Keep::Best, 0, false)]
    #[case::not_after_the_window_has_closed(false, true, true, Keep::Best, 30_001, false)]
    #[case::not_with_keep_best_off(false, true, true, Keep::First, 0, false)]
    // **Audio outranks quality, in both directions.** The first row is the one
    // that costs a Listener the call entirely if it is missed: an encrypted copy
    // lands first (Trunk Recorder gives it a duration and no `freqList`, so it
    // wins on abstention alone), and unless a decodable copy can displace it,
    // the audio somebody really captured is discarded and the call is heard
    // *zero* times.
    #[case::audio_replaces_a_call_with_none_however_bad(true, true, false, Keep::Best, 0, true)]
    #[case::and_a_copy_with_no_audio_never_replaces_one(false, false, true, Keep::Best, 0, false)]
    #[case::two_encrypted_copies_are_compared_as_usual(false, false, false, Keep::Best, 0, true)]
    fn a_copy_replaces_only_when_every_condition_holds(
        #[case] worse: bool,
        #[case] arriving_has_audio: bool,
        #[case] stored_has_audio: bool,
        #[case] dedup_keep: Keep,
        #[case] aged_by_ms: i64,
        #[case] replaces_it: bool,
    ) {
        let stored = Candidate {
            quality: Quality {
                decode_errors: Some(9),
                duration_ms: Some(4_000),
            },
            stored_ms: NOW - aged_by_ms,
            has_audio: stored_has_audio,
            ..stored(0, OURS, &[])
        };
        let arriving = Arriving {
            quality: Quality {
                // Cleaner than the stored copy, unless this case wants worse.
                decode_errors: Some(if worse { 400 } else { 0 }),
                duration_ms: Some(4_000),
            },
            has_audio: arriving_has_audio,
            ..arriving(&[])
        };
        let config = IngestConfig {
            dedup_keep,
            ..IngestConfig::default()
        };

        assert_eq!(replaces(&arriving, &stored, &config, NOW), replaces_it);
        // ...and whichever way that went, the two are the same transmission, so
        // the copy is never stored a second time.
        assert!(duplicate_of(std::slice::from_ref(&stored), &arriving, &config).is_some());
    }

    /// The Call it names is the *nearest* stored one, which is what an operator
    /// reading "duplicate of call 41" wants and what keep-best compares
    /// against — never merely the first row the query happened to hand back.
    #[test]
    fn the_duplicate_it_names_is_the_nearest_stored_call() {
        let candidates = vec![
            Candidate {
                id: 41,
                ..stored(400, OURS, &[])
            },
            Candidate {
                id: 42,
                ..stored(100, OURS, &[])
            },
        ];

        assert_eq!(duplicate_id(&candidates, 1_000_000, 500), Some(42));
    }

    // -- The channel half of the test: same Talkgroup, or overlapping patch
    //    membership (#46, spec US 10) --------------------------------------

    /// **One transmission, N uploads, one Call.** A console patch makes a
    /// recorder upload the same audio once per member Talkgroup, and rdio's
    /// duplicate key — one channel, one window — cannot see that they are one
    /// transmission, so the Listener hears it three times.
    ///
    /// The widened test is: the two copies **reach a channel in common**. Every
    /// way that can happen is a row here, and so is every way it can fail to,
    /// because a predicate that matches too much silently eats real traffic —
    /// which is the failure a Listener cannot detect and an Operator cannot
    /// debug.
    ///
    /// The last three rows are the near misses the acceptance criteria name.
    /// `adjacent_real_calls` is the one that matters most: two Talkgroups that
    /// happen to be busy at the same instant, patched to nothing and to nobody,
    /// stay two Calls.
    #[rstest]
    // The plain case, and the one that has always worked: the same channel.
    #[case::the_same_talkgroup(Some(OURS), &[], OURS, &[], true)]
    // A patch re-broadcast: the copy arrives on a member, naming ours.
    #[case::arriving_is_patched_to_the_stored_ones_channel(Some(ANOTHER), &[OURS], OURS, &[], true)]
    // ...and the mirror, because a recorder's upload order is not ours to pick.
    #[case::the_stored_one_is_patched_to_ours(Some(OURS), &[], ANOTHER, &[OURS], true)]
    // Both copies name the union, neither is on the other's own channel.
    #[case::both_name_a_common_member(Some(OURS), &[70_000], ANOTHER, &[70_000], true)]
    // **A patch-minted TGID nothing owns yet** (rdio's issue #466): the
    // arriving Ref resolves to no Talkgroup at all, and the match is carried
    // entirely by the patch array. Storing this as a second Call is exactly the
    // duplicate-button flood the spec set out to end.
    #[case::a_brand_new_patch_tgid_still_matches(None, &[OURS], OURS, &[], true)]
    // ...and the near misses.
    #[case::adjacent_real_calls(Some(OURS), &[], ANOTHER, &[], false)]
    #[case::patched_to_different_channels(Some(OURS), &[70_000], ANOTHER, &[70_001], false)]
    #[case::an_unresolved_ref_patched_to_nothing_reaches_nobody(None, &[], OURS, &[], false)]
    fn two_copies_are_one_transmission_when_they_reach_a_channel_in_common(
        #[case] talkgroup: Option<i64>,
        #[case] patches: &[i64],
        #[case] stored_talkgroup: i64,
        #[case] stored_patches: &[i64],
        #[case] duplicate: bool,
    ) {
        let candidates = [stored(0, stored_talkgroup, stored_patches)];
        let arriving = Arriving {
            talkgroup,
            patches,
            ..arriving(&[])
        };

        assert_eq!(
            duplicate_of(&candidates, &arriving, &IngestConfig::default()).is_some(),
            duplicate
        );
    }

    /// **`dedup_scope = "talkgroup"` is the way back to rdio's narrower test**
    /// — the same channel and nothing else — for an operator whose System mints
    /// patches that lie.
    ///
    /// The same two inputs under both scopes, so what the setting *changes* is
    /// what is asserted rather than what one of its values happens to do.
    #[rstest]
    #[case::patched(Scope::Patched, true)]
    #[case::talkgroup_only(Scope::Talkgroup, false)]
    fn the_scope_decides_whether_a_patch_overlap_counts(
        #[case] dedup_scope: Scope,
        #[case] duplicate: bool,
    ) {
        let candidates = [stored(0, ANOTHER, &[OURS])];

        assert_eq!(
            duplicate_of(
                &candidates,
                &arriving(&[]),
                &IngestConfig {
                    dedup_scope,
                    ..IngestConfig::default()
                }
            )
            .is_some(),
            duplicate
        );
    }

    /// The nearest candidate is chosen from the ones that **match**, not from
    /// every Call the window happened to contain.
    ///
    /// The widened read hands back every Call on the System inside the window,
    /// so an unrelated Talkgroup's Call is now routinely nearer than the real
    /// duplicate. Filtering after choosing would name it — or, worse, refuse the
    /// upload as a duplicate of a Call it has nothing to do with.
    #[test]
    fn a_nearer_call_on_another_channel_is_not_the_one_it_names() {
        let candidates = vec![
            Candidate {
                id: 41,
                ..stored(10, ANOTHER, &[])
            },
            Candidate {
                id: 42,
                ..stored(300, OURS, &[])
            },
        ];

        assert_eq!(duplicate_id(&candidates, 1_000_000, 500), Some(42));
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
                .map(|(i, offset)| Candidate { id: i as i64 + 1, ..stored(at + offset - 1_000_000, OURS, &[]) })
                .collect();

            let expected = offsets.iter().any(|offset| offset.abs() <= window);

            prop_assert_eq!(duplicate_id(&candidates, at, window).is_some(), expected);
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
                .map(|(i, offset)| Candidate { id: i as i64 + 1, ..stored(at + offset - 1_000_000, OURS, &[]) })
                .collect();

            if let Some(named) = duplicate_id(&candidates, at, window) {
                let named = candidates.iter().find(|c| c.id == named).expect("a candidate");
                let distance = (named.call_at_ms - at).abs();
                prop_assert!(distance <= window);
                for other in &candidates {
                    prop_assert!(distance <= (other.call_at_ms - at).abs());
                }
            }
        }

        /// **The widened test is exactly "the channel sets intersect *and* the
        /// times are within the window"** — both halves, over the same inputs,
        /// so the near misses of each are generated against every state of the
        /// other (#46).
        ///
        /// Two things make this a real check rather than a re-derivation.
        ///
        /// The **sets are the generated values**, and the inputs are built
        /// *from* them — a Talkgroup plus the rest as patches — rather than the
        /// expectation being rebuilt by chaining `talkgroup` onto `patches` the
        /// way `Arriving::talkgroups` does. Written the other way round the
        /// property re-runs the membership rule it is meant to be checking and
        /// cannot fail if that rule is wrong.
        ///
        /// And Refs come from a deliberately tiny pool, so overlaps happen often
        /// enough to be interesting; over a realistic Ref space almost every
        /// pair would be disjoint and this would only ever prove that unrelated
        /// Calls stay distinct, which is half the rule.
        ///
        /// The System half is *not* here, and deliberately: it is enforced by
        /// the candidate query rather than by this decision, so it is proved
        /// over real rows on both dialects
        /// (`tests/ingest.rs::two_systems_numbering_a_talkgroup_alike_do_not_dedup_against_each_other`).
        #[test]
        fn one_transmission_is_two_copies_that_overlap_in_channel_and_in_time(
            mine in proptest::collection::hash_set(1i64..6, 1..4),
            theirs in proptest::collection::hash_set(1i64..6, 1..4),
            offset in -800i64..800,
            window in 0i64..600,
        ) {
            // The sets are the subject; which member is "its own Talkgroup" and
            // which are patches is an encoding detail the rule must not depend
            // on, so the first of each is taken as the Talkgroup.
            let split = |set: &std::collections::HashSet<i64>| {
                let mut refs: Vec<i64> = set.iter().copied().collect();
                refs.sort();
                (refs[0], refs[1..].to_vec())
            };
            let (my_talkgroup, my_patches) = split(&mine);
            let (their_talkgroup, their_patches) = split(&theirs);

            let candidates = [stored(offset, their_talkgroup, &their_patches)];
            let arriving = Arriving {
                talkgroup: Some(my_talkgroup),
                patches: &my_patches,
                ..arriving(&[])
            };
            let config = IngestConfig { dedup_window_ms: window, ..IngestConfig::default() };

            // Both halves, spelled independently of the code: set intersection
            // over the *generated* sets, and `|offset| <= window` where the code
            // is a range that contains.
            let expected = !mine.is_disjoint(&theirs) && offset.abs() <= window;

            prop_assert_eq!(
                duplicate_of(&candidates, &arriving, &config).is_some(),
                expected
            );
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

    // -- What a Call's own audio adds to it (#42, #48) -----------------------

    /// **`enrich`'s two gates, all four ways.**
    ///
    /// Both exist to keep a Trunk Recorder Call paying what it paid before #48,
    /// and the interesting corner is the one no upload in the suite reaches: a
    /// recorder that sent its own duration *and* embedded a tag. Skipping the
    /// probe there because the duration was already known would throw away
    /// every name in the file — so the gates are an `or`, not an `and`, and
    /// each is asserted on its own.
    #[rstest]
    // A plain WAV: nothing embedded, so the probe runs for the length alone.
    #[case::a_length_to_read_and_nothing_embedded(None, false, Some(1000), None)]
    // SDRTrunk: both, which is the ordinary case.
    #[case::a_length_to_read_and_a_tag(None, true, Some(1044), Some("Engine 1"))]
    // The corner: the recorder counted its own samples, and still wrote a tag.
    #[case::a_length_already_known_and_a_tag(Some(8_250), true, Some(8_250), Some("Engine 1"))]
    // A Trunk Recorder Call: nothing to read, nothing to mine, no probe.
    #[case::nothing_to_do_at_all(Some(8_250), false, Some(8_250), None)]
    fn enrich_probes_exactly_when_there_is_something_to_read(
        #[case] duration_from_recorder: Option<i64>,
        #[case] embedded: bool,
        #[case] expected_duration: Option<i64>,
        #[case] expected_label: Option<&str>,
    ) {
        let audio = match embedded {
            true => crate::testing::id3::tagged_mp3(vec![
                crate::testing::id3::text(b"TCOM", "sdrtrunk v0.6.1"),
                crate::testing::id3::text(b"TPE1", "1234567 Engine 1"),
            ]),
            // A one-second 8 kHz WAV — a real container with no tag in it.
            false => crate::testing::id3::wav(8_000, 8_000),
        };
        let mut call = NewCall {
            duration_ms: duration_from_recorder,
            units: vec![NewCallUnit {
                unit_ref: 1234567,
                ..Default::default()
            }],
            ..NewCall::new(11, 54241, 1_000)
        };

        enrich(&mut call, &audio, 7_000);

        assert_eq!(call.duration_ms, expected_duration);
        assert_eq!(call.units[0].label.as_deref(), expected_label);
        assert_eq!(
            call.mined_at_ms,
            Some(7_000),
            "stamped whatever was found, including nothing — the sweep must \
             never come back to a Call ingest already looked inside"
        );
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
            resolved: repo::Resolved {
                system,
                talkgroup,
                patches: Some(Vec::new()),
            },
            candidates,
        }
    }

    /// A stored Call on the arriving one's own Talkgroup, at `call_at_ms`, of
    /// exactly the quality the arriving copy has — so the decision below turns
    /// on the *window*, and never on which copy is better.
    fn a_stored_call_at(call_at_ms: i64) -> Vec<Candidate> {
        vec![Candidate {
            id: 41,
            call_at_ms,
            stored_ms: NOW,
            talkgroup: 54241,
            patches: Vec::new(),
            quality: Quality::default(),
            has_audio: true,
        }]
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

        assert_eq!(admit(&facts, &call, &config, NOW), expected);
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
    // A replacement is the *only* record that its upload happened: no new row
    // exists to find, and the recorder was told "imported successfully".
    #[case::replaced(
        Admission::Replaced { call_id: 1, audio_bytes: 44 },
        "call replaced with a better copy",
        " INFO "
    )]
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
            Admission::Replaced {
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

    // -- The replacement whose Call is gone (#46) ---------------------------
    //
    // Tested here rather than over ingest's HTTP boundary because the arm lives
    // in the window between the decision reading a candidate and the write
    // pointing it at better audio, and nothing outside can open that window: it
    // holds no I/O to park in, and the row has to *vanish* rather than fail, so
    // the statement seam cannot reach it either. What is asserted is what a
    // caller would see — the Admission, and a Call in the Archive afterwards.

    /// Retention is entitled to prune a Call between the moment keep-best
    /// decided this copy was a better one and the moment it writes.
    ///
    /// Rare but real: the size cap prunes oldest-first regardless of age, and a
    /// Recorder backfilling old Calls puts candidates right at that edge. The
    /// transmission is then not in the Archive at all, so the honest answer is
    /// that this copy becomes it — a **stored** Call, published like any other,
    /// rather than a replacement of something that is gone or a duplicate of it.
    #[tokio::test]
    async fn a_better_copy_whose_call_was_pruned_becomes_a_call_of_its_own() {
        let (state, call_id, _tmp) = one_stored_call().await;
        repo::delete_calls(&state.db, &[call_id])
            .await
            .expect("prune the Call out from under the replacement");

        let admission = replace(
            &state,
            call_id,
            NewCall::new(11, 54241, 1_000),
            b"the-better-copy".to_vec(),
            &repo::Resolved::unresolved(),
            true,
        )
        .await
        .expect("a replacement whose Call is gone still stores the copy");

        assert!(
            matches!(admission, Admission::Stored { .. }),
            "the copy became a Call of its own: {admission:?}"
        );
        use sea_orm::{EntityTrait, PaginatorTrait};
        assert_eq!(
            call::Entity::find()
                .count(&state.db)
                .await
                .expect("count Calls"),
            1,
            "...and it is really in the Archive"
        );
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
