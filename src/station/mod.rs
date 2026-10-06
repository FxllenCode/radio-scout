//! The **Station stream** (#74, spec US 60): the scanner as a radio station.
//!
//! One URL, `GET /api/station.mp3?sel=…`, that plays a **Selection** forever —
//! Calls in the order they went out, silence between them, new Calls joining
//! live — to whatever cannot run the app: a smart speaker, a Sonos, VLC, a car's
//! head unit, a browser tab.
//!
//! # Improving on rdio-scanner
//!
//! There is nothing to improve on: rdio-scanner has no stream at all. A
//! Listener who wants the scanner on a speaker keeps a phone awake with the
//! app open, which is the exact thing iOS is least willing to allow. What the
//! community reaches for instead is Icecast fed by a second program on the
//! recorder — a whole server to install, and one that knows nothing about
//! Selections, access codes or a **Delay**.
//!
//! # The shape of it
//!
//! - **It follows the live feed, so it is the live feed's Calls.** A stream is
//!   one more subscriber to the fanout [`crate::AppState::publish`] emits on,
//!   so a **Delay**ed Call is heard when it is released and not before, a
//!   **Patch** reaches it exactly as it reaches a socket, and the question of
//!   whether a Listener hears a Call is [`crate::access::AccessScope::delivers`]
//!   — the socket's own question, lifted out of `live.rs` so there is one.
//! - **MP3, because it is the one format every radio player plays**, at 16 kHz
//!   mono and 32 kbps: 4 KB a second, a quarter of what an app listener
//!   downloading 8 kHz WAVs pulls. Why that encoder and that rate is
//!   [`encode`]'s story; the property everything here stands on is that every
//!   frame is self-contained, so a Call, the silence after it and the next Call
//!   are three encodes simply concatenated.
//! - **Decide purely, then perform.** [`programme::Programme`] is the whole of
//!   what plays — which Calls, in what order, what is skipped to stay near
//!   live, what a player is told is on the air, when a code has run out — as a
//!   function from events to actions. [`pace::Pace`] is how many frames are owed
//!   when. What is left here is the adapter: a fanout to listen to, a clock to
//!   tick, an object to read, bytes to send.
//! - **Silence costs nothing.** A quiet scanner is a stream sending one
//!   pre-encoded frame over and over ([`encode::silence`]) — a reference count,
//!   not an encode — and a Call is decoded and encoded once, on a blocking
//!   thread, a Call ahead of the air.
//! - **Bounded, every way it can grow.** Memory is one Call on the air, one
//!   read ahead and a queue of references; the queue is held to two minutes
//!   behind live by passing over the oldest; a player that stops reading is
//!   given a lead's worth when it comes back, never the backlog; and the number
//!   of streams is `[station] max_streams`.
//! - **No statement per Call.** The Call a stream hears already carries its
//!   object key, and objects are immutable once written (ADR-0002) — an
//!   **Enhancement** or a **Replacement** writes a new key and leaves the old one
//!   to orphan-GC — so the key a Call went out with is readable for as long as
//!   a stream could still want it.
//!
//! # Access
//!
//! A stream is gated exactly as a live socket is (#68), and for the reason that
//! matters most here: a URL pasted into a speaker is the listener nobody closes
//! a tab on. So it carries the **grant** in its query string like every other
//! gated read; it takes one of its **Access code**'s connection slots, so a code
//! limited to one listener cannot be stretched across a house; and it ends when
//! the code runs out, on its own clock. A grant gone stale degrades to open
//! listening, as every read here does.
//!
//! **And it is re-scoped the moment its scope could have changed** — a code
//! edited, disabled, rotated or revoked, or a channel restricted
//! ([`crate::access::Access::watch`]) — not when it opened and never again. A
//! live socket can afford to learn on its next reconnect, because phones
//! reconnect all day; a speaker never does, so a scope read once would be a
//! code an Operator could never take back from somebody's kitchen. What it had
//! already queued under the old scope is asked again too
//! ([`programme::Programme`]); only the Call on the air finishes.
//!
//! Every stream is a **Listener**, so the count an Operator watches (#62)
//! includes the speaker in somebody's kitchen.

pub mod encode;
mod pace;
mod programme;

use std::collections::{HashMap, VecDeque};
use std::convert::Infallible;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, HeaderValue, header};
use axum::response::Response;
use bytes::Bytes;
use serde::{Deserialize, Serialize};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, broadcast};
use tokio::time::{Instant, Interval, MissedTickBehavior};
use tracing::{Instrument, Span, debug, info, warn};

use crate::AppState;
use crate::call::CallId;
use crate::failure::{Failure, Reason};
use crate::live::Emitted;
use crate::selection::Selection;
use programme::{Action, Event, Programme};

/// Where a Station stream is played from.
pub const STATION_PATH: &str = "/api/station.mp3";

/// How many streams one Instance plays at once unless told otherwise.
///
/// Eight is two hundred and fifty kilobits of upload at worst — a speaker in
/// every room of a house and a car — and a fraction of one core of a Pi 5 while
/// every one of them is decoding a Call at the same moment. Above it a player
/// is refused, and told to come back.
const DEFAULT_MAX_STREAMS: u32 = 8;

/// How often a stream looks at the clock: sends what has fallen due, and checks
/// whether its code has run out. A quarter of a second is seven frames — well
/// inside the lead a player is holding — and four wake-ups a second per stream
/// is nothing a Pi notices.
const TICK: Duration = Duration::from_millis(250);

/// `[station]` — the Station stream (#74, spec US 60).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct StationConfig {
    /// How many streams may play at once. **`0` is off**: the number is the
    /// switch, the way `[metrics] token` is, because a station that may play to
    /// nobody is a station that is not there.
    pub max_streams: u32,
}

impl Default for StationConfig {
    fn default() -> Self {
        StationConfig {
            max_streams: DEFAULT_MAX_STREAMS,
        }
    }
}

/// The Station stream as an [`AppState`] holds it: the policy, and the slots.
///
/// A `Semaphore` rather than a counter for [`crate::export::Exports`]' reason:
/// a slot is a thing that is *returned*, and an `OwnedSemaphorePermit` is
/// returned when the stream holding it is dropped, however it ends.
#[derive(Clone)]
pub struct Stations {
    config: StationConfig,
    slots: Arc<Semaphore>,
    /// Whether this run is stopping. A stream is a response that never ends,
    /// and a graceful stop waits for every response to — so the Instance
    /// closes its stations first ([`Stations::close`]) and every stream ends.
    closing: Arc<tokio::sync::watch::Sender<bool>>,
}

impl Default for Stations {
    fn default() -> Self {
        Stations::new(StationConfig::default())
    }
}

impl Stations {
    pub fn new(config: StationConfig) -> Self {
        Stations {
            slots: Arc::new(Semaphore::new(config.max_streams as usize)),
            config,
            closing: Arc::new(tokio::sync::watch::Sender::new(false)),
        }
    }

    /// End every stream, for good — what an Instance does first when it stops.
    pub fn close(&self) {
        self.closing.send_replace(true);
    }

    /// Whether this Instance plays streams at all — read by `GET /api/catalog`,
    /// so a client never offers a URL this would refuse (#64's rule).
    pub fn enabled(&self) -> bool {
        self.config.max_streams > 0
    }

    /// A slot, if one is free.
    fn tune_in(&self) -> Option<OwnedSemaphorePermit> {
        self.slots.clone().try_acquire_owned().ok()
    }
}

/// `GET /api/station.mp3` — the scanner as a radio station.
///
/// `sel` is read the way every surface that takes a Selection reads it
/// ([`crate::query::Params::selection`]); absent — or blank — is everything this
/// viewer may hear, which is what an Operator typing the bare URL into VLC
/// means by it.
pub async fn stream(
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
    headers: HeaderMap,
    viewer: crate::access::Viewer,
) -> Result<Response, Failure> {
    if !state.stations.enabled() {
        return Err(Reason::StationDisabled.into());
    }
    let selection = crate::query::Params::new(&params)
        .selection()?
        .unwrap_or(Selection {
            all: true,
            ..Selection::default()
        });

    // The code's limit first, then the Instance's — so a code over its limit is
    // told about its code, which is the thing its holder can do something about.
    let held = viewer
        .code()
        .map(|code| state.access.hold(code))
        .transpose()?;
    let slot = state.stations.tune_in().ok_or(Reason::StationFull {
        max: state.stations.config.max_streams,
    })?;

    // Subscribed before the response goes out, so a Call published the moment
    // a player has its headers is a Call that player hears — and the same for
    // a change to what this grant reaches.
    let fanout = state.live.subscribe();
    let rescoped = state.access.watch();
    let closing = state.stations.closing.subscribe();
    // Kept to resolve again when it may have changed (`Access::watch`). Held in
    // memory for the life of the stream, as the request that presented it was,
    // and never written anywhere.
    let grant = crate::query::Params::new(&params)
        .raw(crate::access::GRANT_PARAM)
        .map(str::to_owned);
    let announcing = headers
        .get("icy-metadata")
        .is_some_and(|value| value.as_bytes() == b"1");
    let programme = Programme::new(selection, viewer.scope.clone()).holding(viewer.code().cloned());
    let programme = match announcing {
        true => programme.announcing(),
        false => programme,
    };

    // One line when a player arrives and one when it leaves — the live feed's
    // pair, and for its reason never one per Call (ADR-0011 rule 8). No
    // address: a Listener's never appears above DEBUG (rule 5).
    info!(announcing, "station stream started");
    let span = Span::current();
    let on_air = OnAir {
        _present: state.listeners.arrive(),
        programme,
        fanout,
        rescoped,
        closing,
        grant,
        preparing: None,
        ticker: ticker(),
        pace: pace::Pace::default(),
        started: Instant::now(),
        outbox: VecDeque::new(),
        ended: false,
        span: span.clone(),
        state,
        _slot: slot,
        held,
    };
    let body = futures_util::stream::unfold(on_air, |mut on_air| async move {
        let span = on_air.span.clone();
        let chunk = on_air.next_chunk().instrument(span).await?;
        Some((Ok::<_, Infallible>(chunk), on_air))
    });

    let mut response = Response::new(Body::from_stream(body));
    let set = response.headers_mut();
    set.insert(header::CONTENT_TYPE, HeaderValue::from_static("audio/mpeg"));
    // A stream is now; nothing between here and the player may keep a copy of
    // it and hand that to somebody later.
    set.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    // Nothing to seek to: a player that probes with a `Range` request — Safari
    // asks for `bytes=0-1` before it commits to anything — is told plainly,
    // and is answered with the stream from now, never a 206.
    set.insert(header::ACCEPT_RANGES, HeaderValue::from_static("none"));
    // What a radio player calls it, and what it says the stream is.
    set.insert("icy-name", HeaderValue::from_static("Radio-Scout"));
    set.insert("icy-br", HeaderValue::from(encode::BITRATE_KBPS));
    set.insert("icy-sr", HeaderValue::from(encode::RATE));
    if announcing {
        set.insert("icy-metaint", HeaderValue::from(programme::METAINT));
    }
    Ok(response)
}

/// The clock a stream sends by: the first tick at once, so a player is handed
/// its lead the moment it connects.
fn ticker() -> Interval {
    let mut ticker = tokio::time::interval(TICK);
    // A tick missed while the player was not reading is not owed twice —
    // [`pace::Pace`] already knows what is owed, from the time that passed.
    ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
    ticker
}

/// The Call being read, and what reading it will come to.
type Reading = (
    CallId,
    Pin<Box<dyn Future<Output = Option<Vec<Bytes>>> + Send>>,
);

/// One stream, on the air: the programme, and everything it is the adapter
/// for. Dropped when the player goes, which gives its slot, its code's slot and
/// its place in the Listener count back — however it went.
struct OnAir {
    programme: Programme,
    fanout: broadcast::Receiver<Emitted>,
    /// Told when what a grant reaches may have changed ([`crate::access::Access::watch`]).
    rescoped: tokio::sync::watch::Receiver<u64>,
    /// Told when the Instance stops ([`Stations::close`]).
    closing: tokio::sync::watch::Receiver<bool>,
    /// The grant this stream was opened with, if it was opened with one.
    grant: Option<String>,
    /// The one Call being read, if one is — held here rather than spawned, so a
    /// stream that ends takes its read with it.
    preparing: Option<Reading>,
    ticker: Interval,
    pace: pace::Pace,
    started: Instant,
    /// Bytes decided and not yet handed to the player.
    outbox: VecDeque<Bytes>,
    ended: bool,
    span: Span,
    state: AppState,
    /// This stream's place in the Listener count (#62).
    _present: crate::listeners::Present,
    _slot: OwnedSemaphorePermit,
    /// The code's connection this stream holds — let go of the moment its
    /// grant stops resolving to a live code.
    held: Option<crate::access::Holding>,
}

impl OnAir {
    /// The next bytes for the player, or `None` when the stream is over.
    async fn next_chunk(&mut self) -> Option<Bytes> {
        loop {
            if let Some(chunk) = self.outbox.pop_front() {
                return Some(chunk);
            }
            if self.ended {
                return None;
            }
            let actions = self.wake().await;
            self.perform(actions);
        }
    }

    /// Wait for something to happen, and ask the programme what it means.
    async fn wake(&mut self) -> Vec<Action> {
        let OnAir {
            programme,
            fanout,
            rescoped,
            closing,
            grant,
            preparing,
            ticker,
            pace,
            started,
            state,
            held,
            ..
        } = self;
        tokio::select! {
            // In this order when more than one is ready. A change of scope
            // first, because it happened before whatever Call is waiting
            // behind it on the fanout, and must decide that Call; the clock
            // next, so a burst of Calls cannot hold up what is owed. (`changed`
            // never fails while this stream holds the `AppState` the sender
            // lives in.)
            biased;
            // An Instance stopping ends the stream, whatever else is going on.
            // The guard `wait_for` answers with is let go inside, so nothing
            // borrowed from the channel is held across the awaits below.
            () = async {
                let _ = closing.wait_for(|closed| *closed).await;
            } => vec![Action::End(None)],
            _ = rescoped.changed() => {
                match state.access.viewer(&state.db, grant.as_deref(), state.clock.now_ms()).await {
                    Ok(viewer) => {
                        if viewer.code().is_none() {
                            *held = None;
                        }
                        programme.on(Event::Rescoped {
                            scope: viewer.scope.clone(),
                            held: viewer.code().cloned(),
                        })
                    }
                    // Keep the last answer — `Access::arm`'s rule, for its
                    // reason: an error is not evidence that anything changed.
                    Err(failure) => {
                        state.metrics.fail(&failure);
                        Vec::new()
                    }
                }
            }
            _ = ticker.tick() => {
                let ended = programme.on(Event::Tick(state.clock.now_ms()));
                match ended.is_empty() {
                    true => programme.on(Event::Due(pace.owed(started.elapsed()))),
                    false => ended,
                }
            }
            heard = fanout.recv() => programme.on(Event::from_fanout(&heard)),
            frames = async { preparing.as_mut().expect("guarded").1.as_mut().await },
                if preparing.is_some() =>
            {
                let (id, _) = preparing.take().expect("guarded");
                programme.on(Event::Prepared(id, frames))
            }
        }
    }

    /// Do what the programme decided.
    fn perform(&mut self, actions: Vec<Action>) {
        for action in actions {
            match action {
                Action::Send(bytes) => {
                    if !bytes.is_empty() {
                        self.outbox.push_back(bytes);
                    }
                }
                Action::Prepare { id, object_key } => {
                    self.preparing = Some((id, Box::pin(read(self.state.clone(), id, object_key))));
                }
                Action::End(refused) => {
                    // Through the one vocabulary, as a live socket's expiry is:
                    // there is no response left to carry it, the stream already
                    // has one.
                    if let Some(reason) = &refused {
                        self.state.metrics.refuse(reason);
                    }
                    self.ended = true;
                }
            }
        }
    }
}

impl Drop for OnAir {
    fn drop(&mut self) {
        // `connected_ms`, the live feed's word for the same thing: a Call
        // already has a `duration_ms`, and one grep should mean one thing.
        let connected_ms = self.started.elapsed().as_millis() as u64;
        self.span
            .in_scope(|| info!(connected_ms, "station stream ended"));
    }
}

/// One Call's object, read and prepared for the air — or `None`, which the
/// programme passes over.
async fn read(state: AppState, id: CallId, object_key: String) -> Option<Vec<Bytes>> {
    let audio = match state.audio.get(&object_key).await {
        Ok(Some(audio)) => audio,
        // Pruned between going out and being read: a stream two minutes behind
        // on a short retention window. Nothing to play, and nothing wrong.
        Ok(None) => {
            debug!(
                call_id = id,
                "station stream found nothing at a Call's object"
            );
            return None;
        }
        Err(error) => {
            warn!(call_id = id, %error, "station stream could not read a Call's audio");
            return None;
        }
    };
    let frames = tokio::task::spawn_blocking(move || encode::prepare(&audio))
        .await
        .ok()
        .flatten();
    if frames.is_none() {
        debug!(call_id = id, "station stream found no audio in a Call");
    }
    frames
}
