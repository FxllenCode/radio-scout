//! Taking a range of the **Archive** with you (#65, spec US 33).
//!
//! A **Talkgroup** (or a whole **Selection**) and a time range, as either a zip
//! of Calls with a manifest or one stitched audio file — streamed, so that a
//! county's night never sits in a Pi's memory on its way to somebody's laptop.
//!
//! # Improving on rdio-scanner
//!
//! There is nothing to improve on: rdio has no export at all. The nearest thing
//! it offers is downloading Calls one at a time from its search screen, which
//! for an incident means a hundred clicks and a folder of files named after
//! whatever the recorder called them — no manifest, no order, no way to hand
//! somebody "the fire, from the tone-out to the all-clear".
//!
//! # The shape of it
//!
//! - **The filters are the search's own.** The query string is the one
//!   `GET /api/calls` takes, read by the same parser, so the export's contents
//!   are the results a Listener was looking at — including `sel=`, which is how
//!   the **DVR**'s scope exports without this module knowing what a DVR is.
//! - **Chronological, always.** The stitch has no other legal order and an
//!   incident has no other useful one, so the sort a search carried is
//!   deliberately overridden rather than honoured.
//! - **Everything is decided before a byte is written.** [`Extent`] — one
//!   statement — answers how many Calls, how many bytes and how long, and every
//!   refusal below is taken from it. What follows is a stream that cannot
//!   change its mind: a zip's index is written last but its entries' offsets are
//!   already committed, and a stitched file states its length in its first 44
//!   bytes.
//! - **Access scope costs this module nothing** (#68). An export builds one
//!   [`crate::archive::CallSearch`] and reads it through [`crate::archive`]'s
//!   own query — the same door search, the DVR and the filter options go
//!   through — so whatever scopes those scopes this, at the moment they gain
//!   it, and there is nothing here to remember to gate.
//! - **One at a time.** This is the only unauthenticated surface on the
//!   Instance that will read a thousand objects and decode an hour of audio, on
//!   a box that is usually also recording. A second export is refused rather
//!   than queued, because a queue would only mean the Pi is late for two people
//!   instead of one.

pub mod stitch;
pub mod zip;

use std::collections::HashMap;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Query, State};
use axum::http::{HeaderValue, header};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use sea_orm::DbErr;
use serde::{Deserialize, Serialize};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc};
use tracing::Instrument;

use crate::AppState;
use crate::archive::{CallSearch, CallSort, Exported, Extent};
use crate::failure::{Failure, Reason, Stage};

/// How many Calls one export may carry, unless an Operator says otherwise.
///
/// A thousand transmissions is a long incident and about a gigabyte of audio —
/// enough that "the whole fire" fits, small enough that a stranger cannot ask a
/// Pi for a year of the county by editing a URL. It is a **cap and not a page
/// size**: an export above it is refused, naming the count, so a Listener
/// narrows the range rather than silently receiving part of it.
const DEFAULT_MAX_CALLS: u64 = 1_000;

/// `[export]` — range export (#65, spec US 33).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ExportConfig {
    /// Export at all. **On**, for `[share] enabled`'s reason: an Instance as it
    /// ships already serves its whole Archive to anyone who asks (ADR-0008), so
    /// an export exposes nothing a patient stranger could not already have —
    /// and a Listener should not have to find a setting before they can keep
    /// what they heard.
    ///
    /// Off is for the Instance whose listening is *closed*, or one whose upload
    /// bandwidth is the scarce thing.
    pub enabled: bool,
    /// The most Calls one export may carry. See [`DEFAULT_MAX_CALLS`].
    pub max_calls: u64,
}

impl Default for ExportConfig {
    fn default() -> Self {
        ExportConfig {
            enabled: true,
            max_calls: DEFAULT_MAX_CALLS,
        }
    }
}

/// The export surface as an [`crate::AppState`] holds it: the policy, and the
/// right to be the export that is running.
///
/// The permit is the shape [`crate::worker::Handoff`] uses one layer along and
/// for the same reason — a thing only one holder can have at a time — but it is
/// a `Semaphore` rather than a `Handoff` because this one is *returned*: an
/// export ends and the next may start, where a Worker's right to drain is taken
/// once for the life of the process.
#[derive(Clone)]
pub struct Exports {
    config: ExportConfig,
    running: Arc<Semaphore>,
}

impl Default for Exports {
    fn default() -> Self {
        Exports::new(ExportConfig::default())
    }
}

impl Exports {
    pub fn new(config: ExportConfig) -> Self {
        Exports {
            config,
            running: Arc::new(Semaphore::new(1)),
        }
    }

    /// Whether exporting is offered at all — read by `GET /api/catalog`, so a
    /// client never draws a control this would refuse (#64's rule).
    pub fn enabled(&self) -> bool {
        self.config.enabled
    }

    /// The most Calls one export may carry, which the client shows *before*
    /// offering the control rather than after refusing it.
    pub fn max_calls(&self) -> u64 {
        self.config.max_calls
    }

    /// Take the right to be the export that is running, or `None` if one
    /// already is. The permit lives as long as the response body, so it is
    /// returned when the last byte is sent — or when the Listener closes the
    /// tab, which is the case a queue would have got wrong.
    fn begin(&self) -> Option<OwnedSemaphorePermit> {
        Arc::clone(&self.running).try_acquire_owned().ok()
    }
}

// ---------------------------------------------------------------------------
// What an export is
// ---------------------------------------------------------------------------

/// How many Calls one page of the export reads at a time.
///
/// The export's memory is one page of *metadata* plus one Call's *audio*, and
/// this is the first of those. Fifty rows is a few tens of kilobytes and one
/// round trip per fifty Calls, which on a Pi is the right side of both trades.
const PAGE: u64 = 50;

/// The manifest's name inside the archive.
const MANIFEST: &str = "manifest.json";

/// Which of the two things an export is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// Every Call as its own file, plus a manifest describing them.
    Zip,
    /// One stitched file, oldest first.
    Stitched,
}

impl Format {
    /// The `format` parameter, or the zip — which is the one that keeps
    /// everything, and so the safe thing for a URL that did not say.
    fn parse(raw: Option<&str>) -> Result<Self, Reason> {
        match raw.unwrap_or("zip") {
            "zip" => Ok(Format::Zip),
            "wav" => Ok(Format::Stitched),
            _ => Err(crate::query::bad("format must be zip or wav")),
        }
    }

    fn mime(self) -> &'static str {
        match self {
            Format::Zip => "application/zip",
            Format::Stitched => "audio/wav",
        }
    }

    fn extension(self) -> &'static str {
        match self {
            Format::Zip => "zip",
            Format::Stitched => "wav",
        }
    }
}

/// **What an export is of.**
///
/// Two subjects, one pipeline. Everything downstream of this — the refusals, the
/// headers, the streaming body, both writers and the manifest — is written once
/// and asked about whichever this is, because a zip of a range and a zip of an
/// **Event** differ in *which Calls* and in nothing else (#67).
#[derive(Debug)]
enum Source {
    /// A range of the Archive, as a search (#65, spec US 33).
    ///
    /// **Boxed**, because a `CallSearch` is a dozen filters and an Event is two
    /// fields — so the unboxed enum would make every `Asked` in the process as
    /// big as the larger arm, for no reason.
    Range {
        /// The search the export walks — the Listener's own, with the parts an
        /// export is not free to honour already overridden.
        search: Box<CallSearch>,
        /// The filters, as the manifest records them.
        filters: String,
    },
    /// One **Event**, whole (#67, spec US 38). No filters: an Event *is* its
    /// members, and there is nothing about it a URL could narrow.
    Event { id: i64, name: String },
}

impl Source {
    /// How big this is going to be, asked **before a byte is written** — the one
    /// statement every refusal below is taken from.
    async fn extent(&self, state: &AppState, format: Format) -> Result<Extent, Failure> {
        match self {
            Source::Range { search, .. } => crate::archive::extent(&state.db, search)
                .await
                .map_err(Stage::MeasureExport.failed()),
            Source::Event { id, .. } => {
                crate::event::extent(&state.db, *id, format == Format::Stitched)
                    .await
                    .map_err(Stage::MeasureExport.failed())
            }
        }
    }

    /// What the manifest says this export was *of* — the one field of it that
    /// differs between the two, as a key and a value.
    fn described(&self) -> (&'static str, serde_json::Value) {
        match self {
            Source::Range { filters, .. } => ("search", serde_json::Value::String(filters.clone())),
            Source::Event { name, .. } => ("event", serde_json::Value::String(name.clone())),
        }
    }

    /// What the browser saves it as — a stamp for a range, the incident's own
    /// name for an Event.
    ///
    /// An Event is remembered by what it was called; a range is remembered by
    /// the night it happened on, which is why [`filename`] leads with the stamp
    /// and this does not.
    fn download_name(&self, first_ms: Option<i64>, format: Format) -> String {
        match self {
            Source::Range { .. } => filename(first_ms, format),
            Source::Event { id, name } => format!(
                "radio-scout-{}.{}",
                // An Event's **Id** where the name survives no part of the
                // slug, because `event-7` is a file somebody can find again and
                // `call` — this slug's own fallback — is the wrong noun for an
                // incident of four hundred of them.
                crate::archive::slug_named(name, &format!("event-{id}")),
                format.extension()
            ),
        }
    }
}

/// A request, read — everything that comes from the query string and nothing
/// that comes from the database.
///
/// It is a value because the source is needed **twice**: once to measure the
/// export and again to walk it, and parsing it twice would be two readings of
/// one URL that could come to disagree.
#[derive(Debug)]
struct Asked {
    format: Format,
    source: Source,
}

impl Asked {
    /// A range of the Archive, from the query string a search page built.
    fn read(
        params: &HashMap<String, String>,
        viewer: &crate::access::Viewer,
    ) -> Result<Asked, Reason> {
        let format = Format::parse(crate::query::Params::new(params).raw("format"))?;
        Ok(Asked {
            format,
            source: Source::Range {
                search: Box::new(exported_search(params, viewer, format)?),
                filters: describe(params),
            },
        })
    }

    /// ...and one Event, whole.
    fn for_event(params: &HashMap<String, String>, id: i64, name: String) -> Result<Asked, Reason> {
        Ok(Asked {
            format: Format::parse(crate::query::Params::new(params).raw("format"))?,
            source: Source::Event { id, name },
        })
    }
}

/// Everything decided before a byte is written: what to write, what to write
/// it from, and how big it is going to be.
#[derive(Debug)]
struct Plan {
    asked: Asked,
    extent: Extent,
    /// What the browser saves it as.
    filename: String,
}

impl Plan {
    /// Decide. Every refusal an export has lives here, so nothing downstream
    /// has to be able to change its mind.
    fn read(asked: Asked, exports: &Exports, extent: Extent) -> Result<Plan, Reason> {
        if extent.calls == 0 {
            return Err(Reason::ExportEmpty);
        }
        if extent.calls > exports.max_calls() {
            return Err(Reason::ExportTooManyCalls {
                calls: extent.calls,
                max: exports.max_calls(),
            });
        }
        let bytes = projected(&extent, asked.format);
        if bytes > ADDRESSABLE_BYTES {
            return Err(Reason::ExportTooLarge { bytes });
        }
        Ok(Plan {
            filename: asked.source.download_name(extent.first_ms, asked.format),
            asked,
            extent,
        })
    }

    /// What the response says about itself before its first byte.
    fn headers(&self) -> axum::http::HeaderMap {
        let mut headers = axum::http::HeaderMap::new();
        headers.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static(self.asked.format.mime()),
        );
        headers.insert(
            header::CONTENT_DISPOSITION,
            // A filename this cannot be is not worth failing an export over,
            // and the browser falls back to the URL's last segment.
            HeaderValue::from_str(&format!("attachment; filename=\"{}\"", self.filename))
                .unwrap_or(HeaderValue::from_static("attachment")),
        );
        if let Some(length) = self.content_length() {
            headers.insert(header::CONTENT_LENGTH, HeaderValue::from(length));
        }
        headers
    }

    /// How long the body will be, where that is knowable before it is written —
    /// which is the whole point of the stitched export's declared timeline, and
    /// is exactly what a zip cannot say (its entries carry their own headers,
    /// and its index is written last).
    fn content_length(&self) -> Option<u64> {
        match self.asked.format {
            Format::Zip => None,
            Format::Stitched => Some(projected(&self.extent, self.asked.format)),
        }
    }
}

/// The 44 bytes a stitched export opens with.
const WAV_HEADER: u64 = 44;

/// What a zip spends on one Call besides its audio: two file headers, two
/// copies of the name, a data descriptor, and the Call's row in the manifest.
///
/// **Headroom, not a prediction.** Its only job is that the check below cannot
/// admit an archive whose *total* overflows a 32-bit offset while its audio
/// alone fits — a real archive spends a few hundred bytes here, and being
/// generous costs nothing but a slightly earlier refusal at the four-gigabyte
/// edge.
const ZIP_OVERHEAD_PER_CALL: u64 = 4_096;

/// The most bytes either container can address.
///
/// A ZIP's local-header offsets and a WAV's two lengths are both 32-bit, so one
/// ceiling serves both — a coincidence worth naming rather than two constants
/// that happen to be equal. Checked in the one statement before anything is
/// written, so the failure is a sentence rather than a download that goes wrong
/// four gigabytes in.
const ADDRESSABLE_BYTES: u64 = u32::MAX as u64;

/// How big the file is going to be — **the whole file**, not its audio.
///
/// The distinction is the point. A zip carries per-entry headers, an index and
/// a manifest beside the audio; a stitch carries its 44-byte header. Measuring
/// only the audio would let an archive through whose last entry's offset does
/// not fit, and the same arithmetic that decides the refusal is what
/// [`Plan::content_length`] and [`stitch::header`] then state — so the check and
/// the promise cannot disagree by the length of a header.
fn projected(extent: &Extent, format: Format) -> u64 {
    match format {
        Format::Zip => extent
            .bytes
            .saturating_add(extent.calls.saturating_mul(ZIP_OVERHEAD_PER_CALL)),
        Format::Stitched => WAV_HEADER + stitch::samples_for(extent.duration_ms) * 2,
    }
}

/// The search an export walks: the Listener's own, with three things taken out
/// of their hands.
///
/// - **Oldest first.** An incident has one useful order and a stitched file has
///   one legal one.
/// - **No window.** `limit`/`offset` describe where a *screen* is; an export is
///   the whole of what matched, and this module does its own paging.
/// - **For a stitched export, only what can be placed on a timeline** — audio
///   that exists, and a length that was measured. The second is spelled as the
///   kerchunk filter at zero, because "a Call whose length was never measured
///   never matches" is already that filter's own documented rule, and a second
///   way of saying it would be a second thing to keep true.
fn exported_search(
    params: &HashMap<String, String>,
    viewer: &crate::access::Viewer,
    format: Format,
) -> Result<CallSearch, Reason> {
    let mut search = crate::archive::parse_search(params, viewer)?;
    search.sort = CallSort::Oldest;
    search.limit = 0;
    search.offset = 0;
    if format == Format::Stitched {
        search.with_audio = true;
        search.min_duration_ms = search.min_duration_ms.max(Some(0));
    }
    Ok(search)
}

/// What the browser saves the export as: `radio-scout-export-2026-09-03-0214`.
///
/// Named after **when the range starts** rather than when it was downloaded,
/// because that is what makes it findable a year later: an incident is
/// remembered by the night it happened on. An instant that will not format
/// leaves the stamp off rather than failing an export
/// ([`crate::curate::document`]'s rule).
fn filename(first_ms: Option<i64>, format: Format) -> String {
    let stamp = first_ms
        .and_then(|at_ms| time::OffsetDateTime::from_unix_timestamp(at_ms.div_euclid(1_000)).ok())
        .and_then(|at| {
            at.format(&time::macros::format_description!(
                "-[year]-[month]-[day]-[hour][minute]"
            ))
            .ok()
        })
        .unwrap_or_default();

    format!("radio-scout-export{stamp}.{}", format.extension())
}

/// The filter parameters a manifest may record — every dimension
/// [`crate::archive::parse_search`] reads, and nothing else.
///
/// **An allow-list, deliberately, and not "everything but `format`".** A
/// manifest is a file handed to strangers, and #68's **Access code** will ride
/// in the query string (ADR-0008) — which is exactly why [`crate::http_log`]
/// logs a path and never a query. Denying one key by name would put the next
/// credential somebody adds straight into everybody's export; allowing by name
/// fails the other way, so a filter added later is *missing* from the manifest
/// until it is listed here rather than a secret being published.
const MANIFEST_FILTERS: &[&str] = &[
    "after",
    "before",
    "system",
    "talkgroup",
    "group",
    "tag",
    "minDuration",
    "unit",
    "mark",
    "sel",
    "starred",
    "sort",
];

/// The filters this export was asked for, as the manifest records them.
///
/// Sorted, so two exports of one range describe themselves identically whatever
/// order a client built the URL in — and `sort` is recorded though the export
/// overrides it, because what belongs here is *what was asked*.
fn describe(params: &HashMap<String, String>) -> String {
    let mut named: Vec<(&String, &String)> = params
        .iter()
        .filter(|(key, value)| MANIFEST_FILTERS.contains(&key.as_str()) && !value.trim().is_empty())
        .collect();
    named.sort();
    named
        .iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect::<Vec<_>>()
        .join("&")
}

// ---------------------------------------------------------------------------
// The handler
// ---------------------------------------------------------------------------

/// `GET /api/calls/export` — a range of the Archive as a file (#65, spec
/// US 33).
///
/// Every filter `GET /api/calls` takes, read by the same parser, plus `format`.
/// The body is **streamed**: what this returns is a plan and a task, not an
/// archive, so the Instance's memory during a county's night is one page of
/// metadata and one Call's audio.
pub async fn export(
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
    viewer: crate::access::Viewer,
) -> Result<Response, Failure> {
    stream_export(state, Asked::read(&params, &viewer)?).await
}

/// `GET /api/admin/events/{id}/export` — one **Event** as a file (#67, spec
/// US 38).
///
/// The same two formats, the same refusals, the same streamed body — this is one
/// [`Source`] rather than the other, which is the whole of what an Event costs
/// this module.
pub async fn event(
    State(state): State<AppState>,
    axum::extract::Path(id): axum::extract::Path<i64>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Response, Failure> {
    let row = crate::db::repo::find_event(&state.db, id)
        .await
        .map_err(Stage::MeasureExport.failed())?
        .ok_or(crate::curate::Rejected::NotFound(
            crate::curate::What::Event,
        ))?;

    of_event(state, &params, row.id, row.name).await
}

/// One **Event** as a file, for a caller that arrived some other way (#67).
///
/// The share page's download comes through here: its token has already named the
/// Event, so what is left is the format and the same pipeline every other export
/// uses. A parameter rather than a copy, for the reason [`crate::serve`] took
/// one — a second implementation would be right on one door and wrong on the
/// other the first time either moved.
pub async fn of_event(
    state: AppState,
    params: &HashMap<String, String>,
    id: i64,
    name: String,
) -> Result<Response, Failure> {
    stream_export(state, Asked::for_event(params, id, name)?).await
}

/// Decide it, then stream it.
///
/// **Every refusal happens before the permit is taken**, so a hand-edited URL
/// that was never going to produce a file cannot make somebody with a real one
/// wait — and every refusal happens before the first byte, so nothing downstream
/// can change its mind about an export that has already begun.
async fn stream_export(state: AppState, asked: Asked) -> Result<Response, Failure> {
    if !state.exports.enabled() {
        return Err(Reason::ExportDisabled.into());
    }
    let extent = asked.source.extent(&state, asked.format).await?;
    let plan = Plan::read(asked, &state.exports, extent)?;

    let permit = state.exports.begin().ok_or(Reason::ExportBusy)?;

    let headers = plan.headers();
    let mut response = Body::from_stream(streamed(state, plan, permit)).into_response();
    response.headers_mut().extend(headers);
    Ok(response)
}

/// Why an export stopped early.
enum Stop {
    /// The Listener closed the connection, or navigated away. Not a failure:
    /// there is nobody left to tell.
    Gone,
    /// The Archive could not be read part-way through. The body is already
    /// committed — a 200 with headers has been sent — so all this can do is
    /// stop, which reaches the client as a truncated download.
    Failed(DbErr),
}

impl From<DbErr> for Stop {
    fn from(error: DbErr) -> Self {
        Stop::Failed(error)
    }
}

/// The body, as bytes arriving over time.
///
/// A **task writing into a bounded channel**, rather than a hand-rolled
/// `Stream` state machine. Two reasons, and the second is the load-bearing one:
/// the writers below then read as the sequential thing they are — a manifest,
/// then a file per Call, then an index — and the channel's *depth* is the
/// memory bound, stated in one place instead of implied by a state enum. When
/// the Listener closes the tab the receiver drops, the next send fails, and the
/// task ends: the permit goes back with it, which is the case a queue would
/// have got wrong.
fn streamed(
    state: AppState,
    plan: Plan,
    permit: OwnedSemaphorePermit,
) -> impl futures_util::Stream<Item = Result<Bytes, std::io::Error>> {
    let (out, mut received) = mpsc::channel::<Result<Bytes, std::io::Error>>(2);
    let span = tracing::Span::current();
    tokio::spawn(
        async move {
            // Held until the last byte is sent, or until the Listener goes away.
            let _permit = permit;
            let outcome = match plan.asked.format {
                Format::Zip => write_zip(&state, &plan, &out).await,
                Format::Stitched => write_stitched(&state, &plan, &out).await,
            };
            if let Err(Stop::Failed(error)) = outcome {
                // ERROR because an Operator must act: somebody's download broke
                // and the only record of it is here — the request already
                // answered `200` and its log line will say so (ADR-0011 rule 4,
                // in the one place where the status cannot).
                // `stage=` and `cause=` are the middleware's own field names
                // for a server error (ADR-0011 rule 4). This is the one such
                // failure that never becomes a status code — the `200` has
                // gone — so a `cause=` grep must still find it.
                tracing::error!(
                    cause = %error,
                    stage = %Stage::ReadExport.slug(),
                    "export failed part-way"
                );
                let _ = out.send(Err(std::io::Error::other("export failed"))).await;
            }
        }
        .instrument(span),
    );

    futures_util::stream::poll_fn(move |cx| received.poll_recv(cx))
}

/// Hand the next piece of the body over, or stop because nobody is listening.
///
/// **An empty piece is not sent at all.** In chunked transfer encoding a
/// zero-length chunk is not a piece of a body — it is the *end* of one — so a
/// writer that had nothing to add for this Call would otherwise be able to
/// finish the response early. Both writers can legitimately produce nothing (a
/// zip entry for an object that has gone, a stitched tail with nothing left to
/// pad), so the rule lives here rather than at each of them.
async fn send(out: &mpsc::Sender<Result<Bytes, std::io::Error>>, bytes: Bytes) -> Result<(), Stop> {
    if bytes.is_empty() {
        return Ok(());
    }
    out.send(Ok(bytes)).await.map_err(|_| Stop::Gone)
}

/// The export's Calls, a page at a time, in the order they will be written.
///
/// A **cursor** rather than a callback, so that both writers read as the
/// sequential loops they are; the zip runs one of these twice — once to write
/// the manifest and once to write the audio — which is what lets the manifest
/// come **first** while the export's memory stays one page. It costs a second
/// pass over *metadata* and no second read of a single object.
struct Pages<'a> {
    state: &'a AppState,
    plan: &'a Plan,
    offset: u64,
}

impl<'a> Pages<'a> {
    fn over(state: &'a AppState, plan: &'a Plan) -> Self {
        Pages {
            state,
            plan,
            offset: 0,
        }
    }

    /// The next page, or an empty one when there is no more — which is also
    /// what a range retention pruned underneath the export answers with.
    async fn next(&mut self) -> Result<Vec<Exported>, Stop> {
        if self.offset >= self.plan.extent.calls {
            return Ok(Vec::new());
        }
        let page = match &self.plan.asked.source {
            Source::Range { search, .. } => {
                crate::archive::exportable(
                    &self.state.db,
                    &CallSearch {
                        limit: PAGE,
                        offset: self.offset,
                        ..(**search).clone()
                    },
                )
                .await?
            }
            Source::Event { id, .. } => {
                crate::event::exportable(
                    &self.state.db,
                    *id,
                    self.plan.asked.format == Format::Stitched,
                    PAGE,
                    self.offset,
                )
                .await?
            }
        };
        self.offset += page.len() as u64;
        Ok(page)
    }
}

/// How many of this export's objects would not read, reported **once when it
/// finishes** rather than once per Call.
///
/// A store that has gone away fails every read, and an export may hold a
/// thousand Calls — so the per-Call line an Operator would actually want is the
/// one thing that must not be written here (ADR-0011 rule 8). This is the
/// **Mining** sweep's rule one surface along: "failures are reported once per
/// sweep rather than once per Call".
#[derive(Default)]
struct Unreadable {
    calls: u32,
    /// The first store error, which is the one worth saying out loud: a
    /// thousand copies of "connection refused" tell an Operator nothing the
    /// first did not.
    first: Option<String>,
}

impl Unreadable {
    fn note(&mut self, error: impl std::fmt::Display) {
        self.calls += 1;
        self.first.get_or_insert_with(|| error.to_string());
    }

    /// Say it once, at the end. WARN because a stored object that will not read
    /// is an Operator's problem — a bucket that has gone away, a permission —
    /// and the download it damaged carries no other record of it.
    fn report(self) {
        if let Some(cause) = self.first {
            tracing::warn!(
                calls = self.calls,
                %cause,
                "export could not read some Calls' audio"
            );
        }
    }
}

/// This Call's audio, or nothing at all.
///
/// An object retention took between the pre-pass and the read is `None` here,
/// and both writers have an answer for that — a zip writes the entry empty, a
/// stitch writes silence — because the alternative in either case is a file
/// that is *structurally* wrong rather than one Call short.
async fn audio_of(
    state: &AppState,
    exported: &Exported,
    unreadable: &mut Unreadable,
) -> Option<Bytes> {
    if exported.object_key.is_empty() {
        return None;
    }
    match state.audio.get(&exported.object_key).await {
        Ok(bytes) => bytes,
        Err(error) => {
            unreadable.note(error);
            None
        }
    }
}

// ---------------------------------------------------------------------------
// The two writers
// ---------------------------------------------------------------------------

/// The zip: a manifest, then every Call's audio, then the index.
///
/// **The manifest is first**, which is only possible because a Call's name
/// inside the archive is a function of the Call and its position (see
/// [`crate::archive::exportable`]) — so the pass that writes the manifest can
/// name files the pass behind it has not written yet. A manifest written *last*
/// would be one accumulated in memory, which is the thing this feature is not
/// allowed to do.
async fn write_zip(
    state: &AppState,
    plan: &Plan,
    out: &mpsc::Sender<Result<Bytes, std::io::Error>>,
) -> Result<(), Stop> {
    let stamp = plan.extent.first_ms.unwrap_or_default();
    let mut zip = zip::ZipStream::new();

    let (mut open, header) = zip.begin(MANIFEST, stamp);
    send(out, header).await?;
    let (subject, described) = plan.asked.source.described();
    send(
        out,
        open.chunk(Bytes::from(format!(
            "{{\"exportedAt\":{},\"{subject}\":{described},\"calls\":[",
            state.clock.now_ms(),
        ))),
    )
    .await?;
    let mut written = 0usize;
    let mut pages = Pages::over(state, plan);
    loop {
        let page = pages.next().await?;
        if page.is_empty() {
            break;
        }
        for exported in page {
            let separator = if written == 0 { "" } else { "," };
            written += 1;
            send(
                out,
                open.chunk(Bytes::from(format!(
                    "{separator}{}",
                    manifest_row(&exported)
                ))),
            )
            .await?;
        }
    }
    send(out, open.chunk(Bytes::from_static(b"]}"))).await?;
    send(out, open.end(&mut zip)).await?;

    let mut unreadable = Unreadable::default();
    let mut pages = Pages::over(state, plan);
    loop {
        let page = pages.next().await?;
        if page.is_empty() {
            break;
        }
        for exported in page {
            // An **Encrypted Call** has no file, and the manifest already said
            // so.
            if exported.filename.is_empty() {
                continue;
            }
            let audio = audio_of(state, &exported, &mut unreadable).await;
            let (mut open, header) =
                zip.begin(&exported.filename, exported.call.timestamp.unwrap_or(stamp));
            send(out, header).await?;
            // An object that has gone leaves the entry **empty rather than
            // absent**: the manifest promised this file, and a visibly empty
            // one keeps the archive and its index describing the same Calls.
            send(out, open.chunk(audio.unwrap_or_default())).await?;
            send(out, open.end(&mut zip)).await?;
        }
    }

    unreadable.report();
    send(out, zip.finish()).await
}

/// One Call's line in the manifest: the wire shape a search page answers with,
/// **flattened**, plus which file in the archive it is.
///
/// Deliberately [`crate::call::StoredCall`] and not a shape of its own — a
/// script reading a manifest and a script reading `GET /api/calls` are then
/// reading one thing, and a field added to the Archive's wire reaches the
/// manifest without anybody remembering. `file` is `null` for an **Encrypted
/// Call**, which is in the manifest because the activity is the point and has
/// no audio to be in a file.
#[derive(serde::Serialize)]
struct ManifestRow<'a> {
    #[serde(flatten)]
    call: &'a crate::call::StoredCall,
    file: Option<&'a str>,
}

fn manifest_row(exported: &Exported) -> String {
    serde_json::to_string(&ManifestRow {
        call: &exported.call,
        file: Some(exported.filename.as_str()).filter(|name| !name.is_empty()),
    })
    .unwrap_or_default()
}

/// The stitched file: a header stating the whole length, then each Call fitted
/// into the room that length reserved for it.
/// **The header cannot be taken back, and that is a promise about the whole
/// file rather than about one Call.** The Archive is live: a Call may be
/// pruned, or a delayed upload may land inside the range, between the statement
/// that measured this export and the pages that walk it. So the remaining room
/// is carried, each Call is fitted into *at most* what is left, and whatever is
/// still unfilled at the end is silence — which is what keeps the body exactly
/// the length the `Content-Length` promised, whatever moved underneath it.
async fn write_stitched(
    state: &AppState,
    plan: &Plan,
    out: &mpsc::Sender<Result<Bytes, std::io::Error>>,
) -> Result<(), Stop> {
    let total = stitch::samples_for(plan.extent.duration_ms);
    send(out, stitch::header(total)).await?;

    let mut left = total;
    let mut unreadable = Unreadable::default();
    let mut pages = Pages::over(state, plan);
    loop {
        let page = pages.next().await?;
        if page.is_empty() {
            unreadable.report();
            // Whatever the pages did not fill. It is a *piece at a time* for
            // the same reason a Call's place is: after a prune this is the
            // whole remaining timeline, which is a number nothing here bounded.
            for piece in stitch::segment(None, left) {
                send(out, piece).await?;
            }
            return Ok(());
        }
        for exported in page {
            let want = stitch::samples_for(exported.call.duration_ms.unwrap_or_default()).min(left);
            left -= want;
            let audio = audio_of(state, &exported, &mut unreadable).await;
            // **On `spawn_blocking`**, [`crate::enhance`]'s reason: decoding and
            // resampling a transmission is solid arithmetic, and doing it on a
            // runtime thread would stall every request behind it — an export of
            // an hour, done in a hundred of these, would otherwise be an hour of
            // a Pi's web server being intermittently unavailable.
            let pieces =
                tokio::task::spawn_blocking(move || stitch::segment(audio.as_deref(), want))
                    .await
                    // A decode that took the thread down with it still owes
                    // exactly its declared length: the header has been sent and
                    // cannot be taken back.
                    .unwrap_or_else(|_| stitch::segment(None, want));
            for piece in pieces {
                send(out, piece).await?;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    /// A request as it arrived, for the tests that are about what was decided
    /// rather than about what was typed.
    fn asked(format: Format) -> Asked {
        Asked {
            format,
            source: Source::Range {
                search: Box::new(
                    exported_search(&params(&[]), &crate::access::Viewer::unrestricted(), format)
                        .expect("a search"),
                ),
                filters: String::new(),
            },
        }
    }

    /// ...and one that is about an **Event** instead, for the handful of rules
    /// that differ between the two subjects.
    fn asked_for_an_event(format: Format, name: &str) -> Asked {
        Asked {
            format,
            source: Source::Event {
                id: 1,
                name: String::from(name),
            },
        }
    }

    fn params(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect()
    }

    /// The default is the format that keeps *everything*, because a URL that
    /// did not say is a URL somebody typed.
    #[rstest]
    #[case::unsaid(None, Format::Zip)]
    #[case::a_zip(Some("zip"), Format::Zip)]
    #[case::a_stitch(Some("wav"), Format::Stitched)]
    fn a_format_is_one_of_two(#[case] raw: Option<&str>, #[case] expected: Format) {
        assert_eq!(Format::parse(raw).expect("a format"), expected);
    }

    #[test]
    fn anything_else_is_refused_by_name() {
        let refused = Format::parse(Some("mp3")).expect_err("not a format");

        assert!(format!("{refused:?}").contains("format"));
    }

    /// **Three things are taken out of the Listener's hands**, and each for its
    /// own reason — the ordering because an incident has one useful one, the
    /// window because an export is the whole of what matched, and (for a
    /// stitch) the two filters that decide what can be placed on a declared
    /// timeline at all.
    #[test]
    fn an_export_walks_the_whole_of_what_matched_oldest_first() {
        let asked = params(&[("talkgroup", "7"), ("sort", "newest"), ("offset", "300")]);

        let search = exported_search(&asked, &crate::access::Viewer::unrestricted(), Format::Zip)
            .expect("a search");

        assert_eq!(search.talkgroup_ref, Some(7));
        assert_eq!(search.sort, CallSort::Oldest);
        assert_eq!((search.limit, search.offset), (0, 0));
        assert!(!search.with_audio, "a zip carries an encrypted Call's row");
        assert_eq!(search.min_duration_ms, None);
    }

    #[test]
    fn a_stitch_takes_only_what_it_can_place_on_a_timeline() {
        let search = exported_search(
            &params(&[]),
            &crate::access::Viewer::unrestricted(),
            Format::Stitched,
        )
        .expect("a search");

        assert!(search.with_audio);
        assert_eq!(
            search.min_duration_ms,
            Some(0),
            "a measured length, spelled as the kerchunk filter's own rule"
        );
    }

    /// ...without loosening a threshold the Listener set: the two readings are
    /// the *same* filter, so the stricter one wins rather than the later one.
    #[test]
    fn a_stitch_never_widens_a_duration_filter_the_listener_set() {
        let asked = params(&[("minDuration", "5")]);

        let search = exported_search(
            &asked,
            &crate::access::Viewer::unrestricted(),
            Format::Stitched,
        )
        .expect("a search");

        assert_eq!(search.min_duration_ms, Some(5_000));
    }

    /// Named after **when the range starts**, because that is how an incident is
    /// remembered a year later.
    #[rstest]
    #[case::a_night_in_november(Some(1_700_000_000_000), "radio-scout-export-2023-11-14-2213.zip")]
    #[case::an_instant_that_will_not_format(Some(i64::MIN), "radio-scout-export.zip")]
    #[case::a_range_with_no_start(None, "radio-scout-export.zip")]
    fn a_download_is_named_after_the_incident(#[case] first_ms: Option<i64>, #[case] name: &str) {
        assert_eq!(filename(first_ms, Format::Zip), name);
    }

    #[test]
    fn a_stitch_is_named_the_same_way_with_the_other_extension() {
        assert_eq!(
            filename(Some(1_700_000_000_000), Format::Stitched),
            "radio-scout-export-2023-11-14-2213.wav"
        );
    }

    /// The manifest records what was *asked for*, sorted — so two exports of one
    /// range describe themselves identically whatever order a client built the
    /// URL in — and never the `format`, which says what the file is rather than
    /// what is in it.
    /// **An allow-list, and the reason is what #68 is about to put in a query
    /// string.** A manifest is a file handed to strangers, so a parameter this
    /// does not recognise is left out rather than published — the same
    /// direction [`crate::http_log`] fails in when it logs a path and never a
    /// query.
    #[test]
    fn the_manifest_records_the_filters_and_only_the_filters() {
        let asked = params(&[
            ("talkgroup", "7"),
            ("format", "wav"),
            ("after", "1000"),
            ("tag", ""),
            ("code", "a-listener-s-access-code"),
        ]);

        assert_eq!(describe(&asked), "after=1000&talkgroup=7");
    }

    /// ...and everything the search parser reads *is* recognised, or an export
    /// would quietly stop recording what it was asked for.
    ///
    /// **The `CallSearch` is destructured exhaustively on purpose**, and that
    /// is the half of this test that earns its keep. Walking
    /// [`MANIFEST_FILTERS`] proves only that every *listed* key is a real
    /// filter — the direction that cannot go wrong. The direction that can, and
    /// did (#66 added `starred` and this list was not told), is a dimension the
    /// parser reads and the manifest has never heard of. Nothing in Rust can
    /// enumerate the fields of a struct, but a destructure that names them all
    /// stops compiling the moment one is added, which sends whoever added it
    /// here. `with_audio`, `limit` and `offset` are named and *not* asserted
    /// for: the first is set by the export itself rather than typed, and the
    /// other two are the window rather than the search.
    #[test]
    fn every_filter_a_search_takes_is_one_the_manifest_records() {
        let asked = params(
            &MANIFEST_FILTERS
                .iter()
                .map(|key| match *key {
                    "mark" => (*key, "emergency"),
                    "sel" => (*key, "0_100.1"),
                    "sort" => (*key, "oldest"),
                    "group" | "tag" => (*key, "Fire"),
                    _ => (*key, "1"),
                })
                .collect::<Vec<_>>(),
        );

        assert_eq!(
            describe(&asked).split('&').count(),
            MANIFEST_FILTERS.len(),
            "every allowed filter survives"
        );

        let crate::archive::CallSearch {
            after_ms,
            before_ms,
            system_ref,
            talkgroup_ref,
            group_name,
            tag_name,
            min_duration_ms,
            unit_ref,
            mark,
            selection,
            starred,
            with_audio: _,
            // Never a filter, and deliberately never recorded in a manifest:
            // what a Listener was *allowed* to reach is not a description of
            // the range they asked for (#68).
            scope: _,
            sort: _,
            limit: _,
            offset: _,
        } = crate::archive::parse_search(&asked, &crate::access::Viewer::unrestricted())
            .expect("every listed key is a real filter");

        // Every dimension the manifest can record, really recorded — so a
        // filter the list has never been told about is a field left at its
        // default here and an assertion nobody wrote.
        assert!(after_ms.is_some(), "after");
        assert!(before_ms.is_some(), "before");
        assert!(system_ref.is_some(), "system");
        assert!(talkgroup_ref.is_some(), "talkgroup");
        assert!(group_name.is_some(), "group");
        assert!(tag_name.is_some(), "tag");
        assert!(min_duration_ms.is_some(), "minDuration");
        assert!(unit_ref.is_some(), "unit");
        assert!(mark.is_some(), "mark");
        assert!(selection.is_some(), "sel");
        assert!(starred, "starred");
    }

    /// Only the stitched export can say how long it will be, and that is the
    /// whole difference between the two: a zip's entries carry their own
    /// headers and its index is written last.
    #[test]
    fn only_a_declared_timeline_can_state_its_length() {
        let extent = Extent {
            calls: 2,
            bytes: 0,
            duration_ms: 2_000,
            first_ms: Some(1),
        };
        let exports = Exports::default();

        let zip = Plan::read(asked(Format::Zip), &exports, extent).expect("a plan");
        let stitch = Plan::read(asked(Format::Stitched), &exports, extent).expect("a plan");

        assert_eq!(zip.content_length(), None);
        assert_eq!(stitch.content_length(), Some(44 + 2 * 8_000 * 2));
    }

    /// **Every refusal is decided here**, in the one place, from the one
    /// statement — so nothing downstream of it can change its mind about an
    /// export that has already begun.
    #[rstest]
    #[case::nothing_matched(Extent::default(), "ExportEmpty")]
    #[case::more_than_the_cap(
        Extent { calls: 3, bytes: 10, duration_ms: 10, first_ms: Some(1) },
        "ExportTooManyCalls"
    )]
    #[case::more_audio_than_a_zip_can_address(
        Extent { calls: 1, bytes: 5_000_000_000, duration_ms: 10, first_ms: Some(1) },
        "ExportTooLarge"
    )]
    fn a_range_that_cannot_be_exported_is_refused_before_anything_is_written(
        #[case] extent: Extent,
        #[case] expected: &str,
    ) {
        let exports = Exports::new(ExportConfig {
            enabled: true,
            max_calls: 2,
        });

        let refused = Plan::read(asked(Format::Zip), &exports, extent).expect_err("refused");

        assert!(format!("{refused:?}").starts_with(expected), "{refused:?}");
    }

    /// **The whole file, not its audio.** A zip's per-entry headers, index and
    /// manifest are what the last entry's offset has to reach past, so an
    /// archive whose audio alone fits could still be one no reader can open.
    #[test]
    fn what_is_measured_is_the_file_and_not_the_audio_in_it() {
        let extent = Extent {
            calls: 10,
            bytes: 1_000,
            duration_ms: 1_000,
            first_ms: Some(1),
        };

        assert_eq!(projected(&extent, Format::Zip), 1_000 + 10 * 4_096);
        assert_eq!(projected(&extent, Format::Stitched), 44 + 8_000 * 2);
    }

    /// ...and what it measures is what the header then states, so a stitch this
    /// admitted never reaches [`stitch::header`]'s clamp.
    #[test]
    fn a_stitch_that_is_admitted_is_one_the_header_can_state() {
        let extent = Extent {
            calls: 1,
            bytes: 0,
            duration_ms: (ADDRESSABLE_BYTES / 16) as i64,
            first_ms: Some(1),
        };
        let exports = Exports::new(ExportConfig {
            enabled: true,
            max_calls: 10,
        });

        let refused = Plan::read(asked(Format::Stitched), &exports, extent);

        assert!(refused.is_err(), "one sample past what a WAV can state");
        assert!(
            stitch::samples_for(extent.duration_ms) > stitch::MAX_SAMPLES,
            "and it is past the clamp too, which is the pair being asserted"
        );
    }

    /// **An Event's download is named after the incident**, where a range's is
    /// named after the night it happened on — because that is how each is
    /// remembered a year later. The name is an Operator's free text, so it goes
    /// through the same slug that guards every other `Content-Disposition`.
    #[rstest]
    #[case::an_ordinary_name("Mill Street fire", "radio-scout-Mill-Street-fire.zip")]
    #[case::punctuation_and_spaces(
        "2026/07/25 — 2nd alarm",
        "radio-scout-2026-07-25-2nd-alarm.zip"
    )]
    #[case::nothing_survives_the_slug("///", "radio-scout-event-1.zip")]
    fn an_events_download_is_named_after_the_event(#[case] name: &str, #[case] expected: &str) {
        let extent = Extent {
            calls: 1,
            bytes: 10,
            duration_ms: 10,
            first_ms: Some(1_700_000_000_000),
        };

        let plan = Plan::read(
            asked_for_an_event(Format::Zip, name),
            &Exports::default(),
            extent,
        )
        .expect("a plan");

        assert_eq!(plan.filename, expected);
    }

    /// ...and its manifest says it was an **Event**, not a search — a file
    /// handed to a stranger should say which of the two it describes.
    #[test]
    fn an_events_manifest_names_the_event_rather_than_a_search() {
        let (subject, described) = asked_for_an_event(Format::Zip, "The tornado")
            .source
            .described();

        assert_eq!(subject, "event");
        assert_eq!(described, serde_json::Value::String("The tornado".into()));
    }

    /// The slot is a slot: taken once, and back when the export that had it is
    /// finished with it.
    #[test]
    fn only_one_export_holds_the_slot() {
        let exports = Exports::default();

        let running = exports.begin().expect("the first export");
        assert!(exports.begin().is_none(), "a second while one is running");

        drop(running);
        assert!(exports.begin().is_some(), "and it comes back");
    }
}
