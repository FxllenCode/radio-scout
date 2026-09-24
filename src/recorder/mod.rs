//! **The recorder dashboard** (#71, spec US 50) — what the SDRs are doing right
//! now, fed by the status WebSocket Trunk Recorder dials out to.
//!
//! A **Recorder** here is CONTEXT.md's Recorder: the *process* receiving radio
//! and uploading Calls. It connects to `/api/recorder-status`, presents an
//! **API key**, and then talks; this module folds what it says into one value an
//! Operator reads, and [`crate::recorder::ws`] is the adapter that pumps frames
//! into it.
//!
//! # Four nouns, and only one of them is TR's own word
//!
//! Trunk Recorder calls the demodulator slots inside one process "recorders",
//! which would make a *recorder* contain *recorders*. So:
//!
//! - a **Recorder** is the process (CONTEXT.md's noun, unchanged);
//! - an **SDR** is one radio device it has open (TR's `source`, which this
//!   project cannot spell that way — CONTEXT.md's Recorder entry lists *source*
//!   among the words it exists to replace);
//! - a **demodulator** is one of the several channels a single SDR carries
//!   (TR's `recorders` array) — what an Operator counts when they ask whether
//!   they have enough to cover a busy afternoon;
//! - a **System** is ours already.
//!
//! # Nothing here is persisted, and that is the design
//!
//! This is a *right now* view: a recorder's own truth, which it re-sends in full
//! the moment it reconnects (`Stat_Socket::on_open` pushes config, systems and
//! demodulators before anything else). Writing it down would buy a history of
//! something that is only ever interesting live, and would put a row per
//! three-second `rates` frame on a Pi's SD card. The **history** worth keeping
//! is the receive conditions behind each Call, and that is [`crate::rf`]'s
//! rollup — measured from the audio rather than from this socket, which carries
//! no error, spike, signal or drift figure at all.
//!
//! What *is* kept is the fact that a Recorder **left**: a row that silently
//! vanishes is indistinguishable from one that never dialled in, which is the
//! one thing spec US 50 asks this screen not to do. So a disconnected Recorder
//! stays on the list, marked, until [`REMEMBERED`] newer ones have come and
//! gone.
//!
//! # There is no `[recorder]` section
//!
//! A Recorder is a **connection**, so an Instance nobody dials into has an empty
//! roster rather than a feature switched off — the **Webhook**'s shape, not
//! enhancement's. The gate is the API key, which every Recorder already holds,
//! and the cost of this feature on an Instance with none is one route that is
//! never hit.

pub mod dialect;
pub mod ws;

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};

use tokio::sync::watch;

use serde::Serialize;

use crate::recorder::dialect::{CallStats, Message, RecorderStats, SystemStats};

/// How many departed Recorders the list still shows.
///
/// Small on purpose: this exists so an Operator can tell "my recorder stopped
/// talking to me" from "I never set it up", and eight is more than any Instance
/// has receivers. Newest first; a Recorder that dials back in under the same
/// name takes its own row back.
const REMEMBERED: usize = 8;

/// How many refused transmissions are kept, per Recorder, as examples.
///
/// The **tally** is what answers "is this happening a lot"; this answers "to
/// what". Bounded because a county with one blacklisted channel would otherwise
/// grow this list all afternoon.
const RECENT_REASONS: usize = 20;

// ---------------------------------------------------------------------------
// The vocabularies
// ---------------------------------------------------------------------------

/// What a call or a demodulator is doing, as a slug.
///
/// The recorder's `State` enum (`trunk-recorder/state.h`), spelled out here
/// because a number on a wire is a number an Operator has to look up. A closed
/// vocabulary, #92's rule and #70's: nothing a recorder sends can put an
/// unbounded value behind this.
pub fn state_name(state: i64) -> &'static str {
    match state {
        0 => "monitoring",
        1 => "recording",
        2 => "inactive",
        3 => "active",
        4 => "idle",
        6 => "stopped",
        7 => "available",
        8 => "ignore",
        _ => "unknown",
    }
}

/// **Why a transmission was not recorded**, as a slug — the recorder's
/// `MonitoringState` enum, and the whole of spec US 50's third clause.
///
/// `None` for `UNSPECIFIED`, which is what every call that *was* recorded
/// carries, and for anything this release has not heard of: an unexplained
/// silence is better than a wrong explanation.
///
/// The ticket names three of these; the recorder has seven, and the other four
/// are the ones an Operator can actually act on — a talkgroup they forgot to
/// add, one they told it to ignore, or a receiver that has run out of
/// demodulators on a busy afternoon.
pub fn not_recorded_reason(mon_state: i64) -> Option<&'static str> {
    match mon_state {
        1 => Some("unknown-talkgroup"),
        2 => Some("ignored-talkgroup"),
        3 => Some("no-source"),
        4 => Some("no-recorder"),
        5 => Some("encrypted"),
        6 => Some("duplicate"),
        7 => Some("superseded"),
        _ => None,
    }
}

/// Every reason [`not_recorded_reason`] can give, for a screen that wants to
/// show a zero rather than "no data" (#70's `Admission::OUTCOMES` rule).
pub const REASONS: &[&str] = &[
    "unknown-talkgroup",
    "ignored-talkgroup",
    "no-source",
    "no-recorder",
    "encrypted",
    "duplicate",
    "superseded",
];

// ---------------------------------------------------------------------------
// The view
// ---------------------------------------------------------------------------

/// One dialed-in **Recorder**, folded — the document an Operator reads.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecorderView {
    /// This Instance's own number for the connection. Not the recorder's
    /// `instanceId`, which is empty on nearly every install and is carried as
    /// [`RecorderView::name`] when it is not.
    pub id: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub connected: bool,
    pub connected_at_ms: i64,
    /// When this Recorder last said anything. The whole of "is it still there":
    /// the socket being open proves only that nothing has closed it, and TR
    /// sends a `rates` frame every three seconds whether or not anything is
    /// happening.
    pub last_message_ms: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disconnected_at_ms: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capture_dir: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub call_timeout_secs: Option<i64>,
    pub sdrs: Vec<SdrView>,
    pub systems: Vec<SystemView>,
    pub demodulators: Vec<DemodulatorView>,
    /// What it has in hand right now — recording, and monitoring-but-not.
    pub calls: Vec<CallView>,
    /// How often each reason has come up since this Recorder dialled in,
    /// worst-first, with the last channel it happened to.
    pub not_recorded: Vec<NotRecordedView>,
    /// The last few transmissions it turned down, newest first, each as it was
    /// when it was refused — the tally says *how often*, this says *what*.
    pub refused: Vec<CallView>,
}

/// One SDR — the device, which is the thing an Operator unplugs.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SdrView {
    pub source_num: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub driver: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub antenna: Option<String>,
    pub center_hz: f64,
    pub rate_hz: f64,
    pub gain: f64,
    /// The **configured** correction, in Hz. Deliberately not called drift: what
    /// the health charts call drift is measured per Call (#71, [`crate::rf`]),
    /// and confusing a setting with a measurement is how a chart comes to say
    /// something false.
    pub error_hz: f64,
    pub min_hz: f64,
    pub max_hz: f64,
    /// How many demodulators this device carries — the number that runs out on
    /// a busy afternoon and produces `no-recorder`.
    pub demodulators: i64,
}

/// One System this Recorder is watching.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SystemView {
    pub sys_num: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub short_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sysid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nac: Option<String>,
    /// Control-channel messages decoded per second. **Zero is the alarm** on a
    /// trunked System and is perfectly normal on a conventional one, which has
    /// no control channel — which is why the type rides beside it.
    pub decode_rate: f64,
    pub control_channels: Vec<f64>,
}

/// One demodulator.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DemodulatorView {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    pub source_num: i64,
    pub rec_num: i64,
    pub state: &'static str,
    /// Calls taken since the Recorder started. A demodulator that has never
    /// taken one reports uninitialised memory as its duration, so the count is
    /// the number worth reading and the duration is only shown beside it.
    pub calls: i64,
    pub recorded_seconds: f64,
}

/// One call the Recorder has in hand.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CallView {
    pub id: String,
    pub talkgroup: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub talkgroup_label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub short_name: Option<String>,
    pub freq: f64,
    pub state: &'static str,
    /// Why this one is **not** being recorded, or nothing at all — which is what
    /// every call that is being recorded carries.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub not_recorded: Option<&'static str>,
    pub encrypted: bool,
    pub emergency: bool,
    pub source_num: i64,
    pub started_at_ms: i64,
    pub elapsed_seconds: f64,
}

/// How often one reason has come up.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NotRecordedView {
    pub reason: &'static str,
    pub count: i64,
    pub last_at_ms: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_talkgroup: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_talkgroup_label: Option<String>,
}

/// Everything this Instance knows about the Recorders that have dialled in.
///
/// [`Dashboard::at_ms`] rides on the wire because every age on this screen is a
/// subtraction, and a browser subtracting its **own** clock from a server's
/// would report a recorder as minutes stale on any machine whose time is a
/// little off. `lib/panel.ts`'s no-clock rule, one screen along: the pure half
/// is handed the moment rather than reading one.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Dashboard {
    pub at_ms: i64,
    pub recorders: Vec<RecorderView>,
}

crate::answers_json!(Dashboard);

// ---------------------------------------------------------------------------
// The roster
// ---------------------------------------------------------------------------

/// The Recorders dialled in right now, and the last few that were.
///
/// Held in [`crate::AppState`] like [`crate::listeners::Listeners`], and for its
/// reason: a handler has to be able to read it and can never see the connections
/// themselves.
#[derive(Clone)]
pub struct Recorders(Arc<Shared>);

struct Shared {
    roster: Mutex<Roster>,
    /// How many times the dashboard has changed — a Recorder arriving, a frame
    /// folded, a Recorder leaving. A `watch` rather than a counter, so a reader
    /// can wait for the *next* change instead of asking again on a timer.
    changes: watch::Sender<u64>,
}

impl Default for Recorders {
    fn default() -> Self {
        Recorders(Arc::new(Shared {
            roster: Mutex::default(),
            changes: watch::Sender::new(0),
        }))
    }
}

#[derive(Default)]
struct Roster {
    next_id: i64,
    live: BTreeMap<i64, Folded>,
    /// Departed, newest first, capped at [`REMEMBERED`].
    gone: VecDeque<RecorderView>,
}

impl Recorders {
    /// A Recorder has dialled in. The guard takes it off the live list however
    /// the connection ends — a close frame, a socket error, a task cancelled out
    /// from under it — which is [`crate::listeners::Listeners::arrive`]'s shape
    /// and its reason.
    pub fn arrive(&self, now_ms: i64) -> Dialed {
        let mut roster = self.lock();
        roster.next_id += 1;
        let id = roster.next_id;
        roster.live.insert(id, Folded::new(id, now_ms));
        drop(roster);
        self.changed();
        Dialed {
            recorders: self.clone(),
            id,
        }
    }

    /// Fold one frame into what we know about `id`.
    ///
    /// Silently does nothing for a connection that has already gone, which is a
    /// race the adapter cannot close: a frame can be in flight while the guard
    /// drops.
    pub fn apply(&self, id: i64, message: Message, now_ms: i64) {
        let mut roster = self.lock();
        let Some(folded) = roster.live.get_mut(&id) else {
            return;
        };
        folded.apply(message, now_ms);
        // **A returning Recorder takes its own row back.** Its name arrives in
        // the `config` frame rather than with the connection, so the ghost can
        // only be cleared once it has said who it is — otherwise an Operator
        // whose recorder restarted would see it twice, once live and once as
        // itself a minute ago.
        let name = folded.name.clone();
        if let Some(name) = name {
            roster
                .gone
                .retain(|remembered| remembered.name.as_deref() != Some(name.as_str()));
        }
        drop(roster);
        self.changed();
    }

    /// The whole dashboard: connected Recorders first, then the departed.
    pub fn dashboard(&self, now_ms: i64) -> Dashboard {
        let roster = self.lock();
        let mut recorders: Vec<RecorderView> =
            roster.live.values().map(Folded::view).rev().collect();
        recorders.extend(roster.gone.iter().cloned());
        Dashboard {
            at_ms: now_ms,
            recorders,
        }
    }

    /// How many are connected right now — what a status page (#70) would show
    /// beside its Listener count.
    pub fn connected(&self) -> usize {
        self.lock().live.len()
    }

    /// A poisoned lock is not worth taking a scanner down over: every field
    /// behind it is a display value, and the worst a clobbered one can do is
    /// draw a stale row. [`crate::listeners`] takes the same view.
    fn lock(&self) -> std::sync::MutexGuard<'_, Roster> {
        self.0
            .roster
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Every change to the dashboard, as it happens — so a reader waits for the
    /// next one rather than asking again on a timer (the test harness, #93's
    /// rule that nothing waits on a sleep).
    pub fn changes(&self) -> watch::Receiver<u64> {
        self.0.changes.subscribe()
    }

    fn changed(&self) {
        self.0.changes.send_modify(|count| *count += 1);
    }
}

impl Roster {
    /// Move a departed Recorder's last view onto the departed list.
    fn remember(&mut self, mut view: RecorderView, now_ms: i64) {
        view.connected = false;
        view.disconnected_at_ms = Some(now_ms);
        // A Recorder that left twice leaves one ghost, not two: this is the
        // other half of the rule [`Recorders::apply`] keeps when one returns.
        if let Some(name) = view.name.as_deref() {
            self.gone
                .retain(|remembered| remembered.name.as_deref() != Some(name));
        }
        self.gone.push_front(view);
        self.gone.truncate(REMEMBERED);
    }
}

/// A connected Recorder's place on the roster, given up on drop.
pub struct Dialed {
    recorders: Recorders,
    id: i64,
}

impl Dialed {
    /// Which connection this is — what the adapter hands back to
    /// [`Recorders::apply`].
    pub fn id(&self) -> i64 {
        self.id
    }
}

impl Drop for Dialed {
    fn drop(&mut self) {
        let now_ms = crate::now_ms();
        let mut roster = self.recorders.lock();
        // Always there: only `arrive` inserts, and only this drop removes.
        let departed = roster.live.remove(&self.id).map(|folded| folded.view());
        departed
            .into_iter()
            .for_each(|view| roster.remember(view, now_ms));
        drop(roster);
        self.recorders.changed();
    }
}

// ---------------------------------------------------------------------------
// The fold
// ---------------------------------------------------------------------------

/// One Recorder's accumulated truth.
///
/// Pure: [`Folded::apply`] takes a frame and a moment and returns nothing,
/// awaits nothing and reads no clock of its own — so every rule about what a
/// recorder's messages *mean* is a value a test constructs, which is
/// [`crate::live::Connection`]'s shape applied to a socket that only ever
/// receives.
#[derive(Debug, Clone)]
struct Folded {
    id: i64,
    name: Option<String>,
    connected_at_ms: i64,
    last_message_ms: i64,
    capture_dir: Option<String>,
    call_timeout_secs: Option<i64>,
    sdrs: BTreeMap<i64, SdrView>,
    systems: BTreeMap<i64, SystemView>,
    demodulators: BTreeMap<String, DemodulatorView>,
    calls: BTreeMap<String, CallView>,
    not_recorded: BTreeMap<&'static str, NotRecordedView>,
    /// The transmissions it turned down, newest first — each one once.
    refused: VecDeque<CallView>,
}

impl Folded {
    fn new(id: i64, now_ms: i64) -> Self {
        Folded {
            id,
            name: None,
            connected_at_ms: now_ms,
            last_message_ms: now_ms,
            capture_dir: None,
            call_timeout_secs: None,
            sdrs: BTreeMap::new(),
            systems: BTreeMap::new(),
            demodulators: BTreeMap::new(),
            calls: BTreeMap::new(),
            not_recorded: BTreeMap::new(),
            refused: VecDeque::new(),
        }
    }

    fn apply(&mut self, message: Message, now_ms: i64) {
        self.last_message_ms = now_ms;
        match message {
            Message::Config(config) => {
                self.name = config.name().or(self.name.take());
                self.capture_dir = config.capture_dir().or(self.capture_dir.take());
                self.call_timeout_secs = Some(config.call_timeout.0);
                for source in &config.sources {
                    self.sdrs.insert(
                        source.source_num.0,
                        SdrView {
                            source_num: source.source_num.0,
                            driver: text(&source.driver),
                            device: text(&source.device),
                            antenna: text(&source.antenna),
                            center_hz: source.center.0,
                            rate_hz: source.rate.0,
                            gain: source.gain.0,
                            error_hz: source.error.0,
                            min_hz: source.min_hz.0,
                            max_hz: source.max_hz.0,
                            demodulators: source.analog_recorders.0 + source.digital_recorders.0,
                        },
                    );
                }
                for system in &config.systems {
                    let entry = self.system(system.sys_num.0);
                    entry.short_name = text(&system.short_name).or(entry.short_name.take());
                    entry.system_type = text(&system.system_type).or(entry.system_type.take());
                    entry.control_channels = system.channels.iter().map(|hz| hz.0).collect();
                }
            }
            Message::Rates { rates } => {
                for rate in rates {
                    self.system(rate.id.0).decode_rate = rate.decoderate.0;
                }
            }
            Message::Systems { systems } => {
                for system in systems {
                    self.merge_system(&system);
                }
            }
            Message::System { system } => self.merge_system(&system),
            // **Authoritative**: the recorder sends the whole list, so a call
            // missing from it is over. Anything else and a call the recorder
            // finished while nobody was reading would sit on the screen forever
            // — and `call_end` is documented but never actually sent
            // (`stat_socket.cc` returns before building one).
            Message::CallsActive { calls } => {
                let mut fresh = BTreeMap::new();
                for call in &calls {
                    let view = self.take_call(call, now_ms);
                    fresh.insert(view.id.clone(), view);
                }
                self.calls = fresh;
            }
            Message::CallStart { call } => {
                let view = self.take_call(&call, now_ms);
                self.calls.insert(view.id.clone(), view);
            }
            Message::CallEnd { call } => {
                self.calls.remove(&call.id);
            }
            Message::Recorders { recorders } => {
                for recorder in recorders {
                    self.merge_demodulator(&recorder);
                }
            }
            Message::Recorder { recorder } => self.merge_demodulator(&recorder),
            Message::Other => {}
        }
    }

    /// One System's row, created empty the first time anything mentions it — a
    /// `rates` frame can name a System whose identity has not arrived yet, and
    /// a decode rate with nowhere to go is the one number this screen exists to
    /// show.
    fn system(&mut self, sys_num: i64) -> &mut SystemView {
        self.systems.entry(sys_num).or_insert_with(|| SystemView {
            sys_num,
            short_name: None,
            system_type: None,
            sysid: None,
            nac: None,
            decode_rate: 0.0,
            control_channels: Vec::new(),
        })
    }

    /// Merge a System's identity, **never clearing what is already known**: the
    /// `system` frame is sent the moment a sysid becomes known and carries no
    /// control channels, where the `config` frame carries the channels and no
    /// sysid. Either one overwriting the other wholesale would make the row
    /// flicker between two halves of the truth.
    fn merge_system(&mut self, stats: &SystemStats) {
        let entry = self.system(stats.id.0);
        entry.short_name = text(&stats.name).or(entry.short_name.take());
        entry.system_type = text(&stats.system_type).or(entry.system_type.take());
        entry.sysid = identifier(&stats.sysid).or(entry.sysid.take());
        entry.nac = identifier(&stats.nac).or(entry.nac.take());
    }

    fn merge_demodulator(&mut self, stats: &RecorderStats) {
        self.demodulators.insert(
            stats.id.clone(),
            DemodulatorView {
                id: stats.id.clone(),
                kind: text(&stats.kind),
                source_num: stats.src_num.0,
                rec_num: stats.rec_num.0,
                state: state_name(stats.state.0),
                calls: stats.count.0,
                recorded_seconds: stats.duration.0,
            },
        );
    }

    /// Turn one call's stats into a row, **tallying a not-recorded reason the
    /// first time this call carries it**.
    ///
    /// The first time is the whole of it: `calls_active` is re-sent on every
    /// call start and every call end, so a monitored-and-refused call sits in it
    /// for as long as the transmission lasts. Counting each frame would turn one
    /// encrypted call on a busy channel into hundreds, and the tally would
    /// measure how chatty the *socket* is rather than how often the recorder
    /// turned something down.
    fn take_call(&mut self, stats: &CallStats, now_ms: i64) -> CallView {
        let reason = not_recorded_reason(stats.mon_state.0);
        let already = self
            .calls
            .get(&stats.id)
            .map(|known| known.not_recorded)
            .unwrap_or(None);
        let view = CallView {
            id: stats.id.clone(),
            talkgroup: stats.talkgroup.0,
            talkgroup_label: stats.talkgroup_label(),
            short_name: text(&stats.short_name),
            freq: stats.freq.0,
            state: state_name(stats.state.0),
            not_recorded: reason,
            encrypted: stats.encrypted.0,
            emergency: stats.emergency.0,
            source_num: stats.src_num.0,
            started_at_ms: stats.started_at_ms(),
            elapsed_seconds: stats.elapsed.0,
        };
        if let Some(reason) = reason
            && already != Some(reason)
        {
            self.tally(reason, &view, now_ms);
        }
        view
    }

    fn tally(&mut self, reason: &'static str, call: &CallView, now_ms: i64) {
        let entry = self
            .not_recorded
            .entry(reason)
            .or_insert_with(|| NotRecordedView {
                reason,
                count: 0,
                last_at_ms: now_ms,
                last_talkgroup: None,
                last_talkgroup_label: None,
            });
        entry.count += 1;
        entry.last_at_ms = now_ms;
        entry.last_talkgroup = Some(call.talkgroup);
        entry.last_talkgroup_label = call.talkgroup_label.clone();
        // A call whose reason *changed* is refused again, and listed again —
        // the same rule the count keeps.
        self.refused.retain(|earlier| earlier.id != call.id);
        self.refused.push_front(call.clone());
        self.refused.truncate(RECENT_REASONS);
    }

    fn view(&self) -> RecorderView {
        let mut not_recorded: Vec<NotRecordedView> = self.not_recorded.values().cloned().collect();
        // Worst first, then most recent — one glance is one answer (#70's rule
        // for its own concern list).
        not_recorded.sort_by(|a, b| b.count.cmp(&a.count).then(b.last_at_ms.cmp(&a.last_at_ms)));
        let mut calls: Vec<CallView> = self.calls.values().cloned().collect();
        // Newest first: a dashboard is read from the top, and what just keyed up
        // is what an Operator is looking for.
        calls.sort_by_key(|call| std::cmp::Reverse(call.started_at_ms));
        RecorderView {
            id: self.id,
            name: self.name.clone(),
            connected: true,
            connected_at_ms: self.connected_at_ms,
            last_message_ms: self.last_message_ms,
            disconnected_at_ms: None,
            capture_dir: self.capture_dir.clone(),
            call_timeout_secs: self.call_timeout_secs,
            sdrs: self.sdrs.values().cloned().collect(),
            systems: self.systems.values().cloned().collect(),
            demodulators: self.demodulators.values().cloned().collect(),
            calls,
            not_recorded,
            refused: self.refused.iter().cloned().collect(),
        }
    }
}

/// A recorder-supplied string, or nothing where it sent an empty one.
fn text(value: &str) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

/// The same, for the identifiers a recorder writes as `"0"` until it has
/// actually decoded one — a conventional System has no sysid, and showing
/// `sysid 0` beside it says something false.
fn identifier(value: &str) -> Option<String> {
    text(value).filter(|id| id != "0")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folded() -> Folded {
        Folded::new(1, 1_000)
    }

    fn frame(json: &str) -> Message {
        Message::read(json).expect("a readable frame")
    }

    /// The opening exchange: config, then systems, then demodulators — which is
    /// exactly the order `Stat_Socket::on_open` sends them in.
    #[test]
    fn the_opening_exchange_describes_the_whole_recorder() {
        let mut recorder = folded();

        recorder.apply(
            frame(
                r#"{"type":"config","instanceId":"butco-pi","captureDir":"/captures",
                "callTimeout":"3","sources":[{"source_num":"0","driver":"osmosdr",
                "device":"rtl=0","center":"770000000","rate":"2400000","gain":"40",
                "error":"0","min_hz":"769000000","max_hz":"771000000",
                "analog_recorders":"0","digital_recorders":"4"}],
                "systems":[{"sysNum":"0","shortName":"butco","systemType":"p25",
                "channels":["770031250"]}]}"#,
            ),
            2_000,
        );
        recorder.apply(
            frame(
                r#"{"type":"systems","systems":[{"id":"0","name":"butco","type":"p25",
                "sysid":"123","wacn":"456","nac":"789"}]}"#,
            ),
            2_100,
        );
        recorder.apply(
            frame(
                r#"{"type":"recorders","recorders":[
                {"id":"0_0","type":"P25","srcNum":"0","recNum":"0","count":"6",
                 "duration":"76.86","state":"1"},
                {"id":"0_1","type":"P25","srcNum":"0","recNum":"1","count":"0",
                 "duration":"0","state":"7"}]}"#,
            ),
            2_200,
        );

        let view = recorder.view();
        assert_eq!(view.name.as_deref(), Some("butco-pi"));
        assert_eq!(view.capture_dir.as_deref(), Some("/captures"));
        assert_eq!(view.call_timeout_secs, Some(3));
        assert_eq!(view.last_message_ms, 2_200);
        assert_eq!(view.sdrs.len(), 1);
        assert_eq!(view.sdrs[0].device.as_deref(), Some("rtl=0"));
        assert_eq!(
            view.sdrs[0].demodulators, 4,
            "analog plus digital: what runs out on a busy afternoon"
        );
        assert_eq!(view.demodulators.len(), 2);
        assert_eq!(view.demodulators[0].state, "recording");
        assert_eq!(view.demodulators[1].state, "available");
    }

    /// A single `recorder` frame is one demodulator changing state, and it
    /// replaces that one's row without disturbing its neighbour's.
    #[test]
    fn one_demodulator_changing_state_moves_only_its_own_row() {
        let mut recorder = folded();
        recorder.apply(
            frame(
                r#"{"type":"recorders","recorders":[
                {"id":"0_0","srcNum":"0","recNum":"0","state":"7"},
                {"id":"0_1","srcNum":"0","recNum":"1","state":"7"}]}"#,
            ),
            2_000,
        );

        recorder.apply(
            frame(
                r#"{"type":"recorder","recorder":{"id":"0_1","srcNum":"0",
                "recNum":"1","count":"1","state":"1"}}"#,
            ),
            2_100,
        );

        let view = recorder.view();
        assert_eq!(view.demodulators.len(), 2);
        assert_eq!(view.demodulators[0].state, "available");
        assert_eq!(view.demodulators[1].state, "recording");
        assert_eq!(view.demodulators[1].calls, 1);
    }

    /// A frame of a kind this does not know is still proof the recorder is
    /// talking, and changes nothing else.
    #[test]
    fn an_unknown_frame_is_a_sign_of_life_and_nothing_more() {
        let mut recorder = folded();
        let before = recorder.view();

        recorder.apply(frame(r#"{"type":"something_new","x":"1"}"#), 5_000);

        let after = recorder.view();
        assert_eq!(after.last_message_ms, 5_000);
        assert_eq!(after.demodulators.len(), before.demodulators.len());
        assert_eq!(after.name, before.name);
    }

    /// The `config` frame carries the control channels and no sysid; the
    /// `system` frame carries the sysid and no channels. Either overwriting the
    /// other wholesale makes the row flicker between two halves of the truth.
    #[test]
    fn a_systems_two_halves_are_merged_rather_than_replaced() {
        let mut recorder = folded();

        recorder.apply(
            frame(
                r#"{"type":"config","sources":[],"systems":[{"sysNum":"0",
                "shortName":"butco","systemType":"p25","channels":["770031250"]}]}"#,
            ),
            2_000,
        );
        recorder.apply(
            frame(r#"{"type":"system","system":{"id":"0","sysid":"123","nac":"789"}}"#),
            2_100,
        );

        let view = recorder.view();
        assert_eq!(view.systems[0].short_name.as_deref(), Some("butco"));
        assert_eq!(view.systems[0].control_channels, vec![770031250.0]);
        assert_eq!(view.systems[0].sysid.as_deref(), Some("123"));
    }

    /// A `rates` frame can name a System whose identity has not arrived yet, and
    /// a decode rate with nowhere to go is the number this screen exists for:
    /// zero on a trunked System is a receiver that has stopped hearing.
    #[test]
    fn a_decode_rate_arriving_first_still_lands() {
        let mut recorder = folded();

        recorder.apply(
            frame(r#"{"type":"rates","rates":[{"id":"0","decoderate":"39.3"}]}"#),
            2_000,
        );

        let view = recorder.view();
        assert_eq!(view.systems.len(), 1);
        assert!((view.systems[0].decode_rate - 39.3).abs() < 1e-9);
    }

    /// A conventional System has no sysid, and the recorder writes `"0"` rather
    /// than leaving it out — which shown as-is says something false.
    #[test]
    fn an_undecoded_identifier_is_not_shown() {
        let mut recorder = folded();

        recorder.apply(
            frame(
                r#"{"type":"system","system":{"id":"1","name":"SYS 2",
                "type":"conventionalP25","sysid":"0","wacn":"0","nac":"0"}}"#,
            ),
            2_000,
        );

        let view = recorder.view();
        assert_eq!(view.systems[0].sysid, None);
        assert_eq!(view.systems[0].nac, None);
    }

    /// The why-not-recorded reason, which is the ticket's own headline.
    #[test]
    fn a_monitored_call_says_why_it_is_not_being_recorded() {
        let mut recorder = folded();

        recorder.apply(
            frame(
                r#"{"type":"call_start","call":{"id":"0_1001_1515574637","state":"0",
                "monState":"4","talkgroup":"1001","talkgrouptag":"FIRE DISPATCH",
                "shortName":"butco","freq":"419000000","startTime":"1515574637"}}"#,
            ),
            2_000,
        );

        let view = recorder.view();
        assert_eq!(view.calls[0].state, "monitoring");
        assert_eq!(view.calls[0].not_recorded, Some("no-recorder"));
        assert_eq!(view.not_recorded[0].reason, "no-recorder");
        assert_eq!(view.not_recorded[0].count, 1);
        assert_eq!(view.not_recorded[0].last_talkgroup, Some(1001));
        assert_eq!(
            view.not_recorded[0].last_talkgroup_label.as_deref(),
            Some("FIRE DISPATCH")
        );
    }

    /// A call that **is** being recorded explains nothing, because there is
    /// nothing to explain.
    #[test]
    fn a_recorded_call_carries_no_reason() {
        let mut recorder = folded();

        recorder.apply(
            frame(r#"{"type":"call_start","call":{"id":"a","state":"1","monState":"0"}}"#),
            2_000,
        );

        assert_eq!(recorder.view().calls[0].not_recorded, None);
        assert!(recorder.view().not_recorded.is_empty());
    }

    /// **The tally counts transmissions, not frames.** `calls_active` is re-sent
    /// on every call start and every call end, so one encrypted call on a busy
    /// channel would otherwise be counted dozens of times and the number would
    /// measure how chatty the socket is.
    /// The tally says *how often*; the refused list says *what* — each
    /// transmission once, newest first, however long it stayed in the active
    /// list, and bounded so a busy blacklisted channel cannot grow it all day.
    #[test]
    fn the_refused_list_names_each_transmission_once_newest_first() {
        let mut recorder = folded();
        for (index, id) in ["a", "b"].iter().enumerate() {
            let active = format!(
                r#"{{"type":"calls_active","calls":[{{"id":"{id}","state":"0",
                "monState":"5","talkgroup":"5{index}","freq":"774031250"}}]}}"#
            );
            // Resent while it lasts, the way `calls_active` really is.
            recorder.apply(frame(&active), 2_000 + index as i64 * 10);
            recorder.apply(frame(&active), 2_001 + index as i64 * 10);
        }

        let refused = recorder.view().refused;
        assert_eq!(refused.len(), 2);
        assert_eq!(refused[0].id, "b", "newest first");
        assert_eq!(refused[0].talkgroup, 51);
        assert_eq!(refused[0].not_recorded, Some("encrypted"));
        assert_eq!(refused[1].id, "a");

        for n in 0..RECENT_REASONS + 5 {
            recorder.apply(
                frame(&format!(
                    r#"{{"type":"calls_active","calls":[{{"id":"x{n}","state":"0",
                    "monState":"5","talkgroup":"9"}}]}}"#
                )),
                3_000 + n as i64,
            );
        }
        assert_eq!(recorder.view().refused.len(), RECENT_REASONS);
    }

    #[test]
    fn one_refused_transmission_is_counted_once_however_often_it_is_resent() {
        let mut recorder = folded();
        let active = r#"{"type":"calls_active","calls":[{"id":"a","state":"0",
            "monState":"5","talkgroup":"55"}]}"#;

        for tick in 0..5 {
            recorder.apply(frame(active), 2_000 + tick);
        }

        assert_eq!(recorder.view().not_recorded[0].count, 1);
    }

    /// And a *second* transmission on the same channel is a second count — the
    /// distinction the test above is guarding is "the same call re-sent", not
    /// "the same channel again".
    #[test]
    fn a_second_refused_transmission_is_counted_again() {
        let mut recorder = folded();

        recorder.apply(
            frame(
                r#"{"type":"calls_active","calls":[{"id":"a","state":"0",
                "monState":"5","talkgroup":"55"}]}"#,
            ),
            2_000,
        );
        recorder.apply(frame(r#"{"type":"calls_active","calls":[]}"#), 2_100);
        recorder.apply(
            frame(
                r#"{"type":"calls_active","calls":[{"id":"b","state":"0",
                "monState":"5","talkgroup":"55"}]}"#,
            ),
            2_200,
        );

        assert_eq!(recorder.view().not_recorded[0].count, 2);
    }

    /// A call that starts as monitored-and-refused and then *is* recorded — a
    /// demodulator freed up — counts once for the refusal it really had.
    #[test]
    fn a_call_that_changes_its_reason_counts_the_new_one() {
        let mut recorder = folded();

        recorder.apply(
            frame(r#"{"type":"call_start","call":{"id":"a","state":"0","monState":"4"}}"#),
            2_000,
        );
        recorder.apply(
            frame(r#"{"type":"call_start","call":{"id":"a","state":"0","monState":"6"}}"#),
            2_100,
        );

        let view = recorder.view();
        assert_eq!(view.not_recorded.len(), 2);
        assert_eq!(view.calls[0].not_recorded, Some("duplicate"));
    }

    /// `calls_active` is the whole truth, so a call that has left it is over —
    /// which matters because `call_end` is documented and never actually sent.
    #[test]
    fn a_call_missing_from_the_active_list_is_over() {
        let mut recorder = folded();

        recorder.apply(
            frame(r#"{"type":"calls_active","calls":[{"id":"a","state":"1"}]}"#),
            2_000,
        );
        assert_eq!(recorder.view().calls.len(), 1);

        recorder.apply(frame(r#"{"type":"calls_active","calls":[]}"#), 2_100);
        assert!(recorder.view().calls.is_empty());
    }

    /// Read anyway, so a fork or a later release that starts sending them does
    /// not leave a finished call on the screen forever.
    #[test]
    fn a_call_end_frame_takes_the_call_off() {
        let mut recorder = folded();

        recorder.apply(
            frame(r#"{"type":"call_start","call":{"id":"a","state":"1"}}"#),
            2_000,
        );
        recorder.apply(
            frame(r#"{"type":"call_end","call":{"id":"a","state":"2"}}"#),
            2_100,
        );

        assert!(recorder.view().calls.is_empty());
    }

    /// Newest first: a dashboard is read from the top, and what just keyed up is
    /// what an Operator is looking for.
    #[test]
    fn active_calls_are_newest_first() {
        let mut recorder = folded();

        recorder.apply(
            frame(
                r#"{"type":"calls_active","calls":[
                {"id":"old","state":"1","startTime":"100"},
                {"id":"new","state":"1","startTime":"200"}]}"#,
            ),
            2_000,
        );

        let view = recorder.view();
        let ids: Vec<&str> = view.calls.iter().map(|call| call.id.as_str()).collect();
        assert_eq!(ids, vec!["new", "old"]);
    }

    /// Worst first, so one glance is one answer.
    #[test]
    fn the_tally_is_worst_first() {
        let mut recorder = folded();

        for (n, id) in ["a", "b", "c"].into_iter().enumerate() {
            recorder.apply(
                frame(&format!(
                    r#"{{"type":"call_start","call":{{"id":"{id}","state":"0","monState":"2"}}}}"#
                )),
                2_000 + n as i64,
            );
        }
        recorder.apply(
            frame(r#"{"type":"call_start","call":{"id":"d","state":"0","monState":"5"}}"#),
            2_100,
        );

        let view = recorder.view();
        assert_eq!(view.not_recorded[0].reason, "ignored-talkgroup");
        assert_eq!(view.not_recorded[0].count, 3);
        assert_eq!(view.not_recorded[1].reason, "encrypted");
    }

    // -- The roster ---------------------------------------------------------

    /// An Instance nobody has dialled into says so, rather than failing: "nothing
    /// breaks when no recorder dials in" is one of the ticket's four criteria.
    #[test]
    fn an_instance_with_no_recorder_has_an_empty_fleet() {
        let recorders = Recorders::default();

        let dashboard = recorders.dashboard(5_000);

        assert_eq!(dashboard.at_ms, 5_000);
        assert!(dashboard.recorders.is_empty());
        assert_eq!(recorders.connected(), 0);
    }

    /// **A row that vanishes is indistinguishable from one that never
    /// connected**, which is the one thing this screen must not do — so a
    /// departed Recorder stays, marked, with the moment it went.
    #[test]
    fn a_recorder_that_hangs_up_is_still_on_the_list() {
        let recorders = Recorders::default();

        {
            let dialed = recorders.arrive(1_000);
            recorders.apply(
                dialed.id(),
                frame(r#"{"type":"config","instanceId":"butco-pi","sources":[],"systems":[]}"#),
                1_100,
            );
            assert_eq!(recorders.connected(), 1);
        }

        assert_eq!(recorders.connected(), 0);
        let dashboard = recorders.dashboard(9_000);
        assert_eq!(dashboard.recorders.len(), 1);
        assert!(!dashboard.recorders[0].connected);
        assert!(dashboard.recorders[0].disconnected_at_ms.is_some());
        assert_eq!(dashboard.recorders[0].name.as_deref(), Some("butco-pi"));
    }

    /// And one that dials back in takes its own row back, rather than appearing
    /// twice — once as itself and once as its own ghost.
    #[test]
    fn a_recorder_that_returns_replaces_its_own_ghost() {
        let recorders = Recorders::default();
        let named = r#"{"type":"config","instanceId":"butco-pi","sources":[],"systems":[]}"#;

        drop({
            let dialed = recorders.arrive(1_000);
            recorders.apply(dialed.id(), frame(named), 1_100);
            dialed
        });
        let back = recorders.arrive(2_000);
        recorders.apply(back.id(), frame(named), 2_100);

        let dashboard = recorders.dashboard(3_000);
        assert_eq!(dashboard.recorders.len(), 1, "one recorder, one row");
        assert!(dashboard.recorders[0].connected);
    }

    /// A Recorder that has come and gone twice leaves **one** ghost, not two —
    /// the other half of the rule the test above keeps when one returns.
    #[test]
    fn a_recorder_that_leaves_twice_leaves_one_ghost() {
        let recorders = Recorders::default();
        let named = r#"{"type":"config","instanceId":"butco-pi","sources":[],"systems":[]}"#;

        for at in [1_000, 2_000] {
            let dialed = recorders.arrive(at);
            recorders.apply(dialed.id(), frame(named), at + 100);
        }

        let dashboard = recorders.dashboard(3_000);
        assert_eq!(dashboard.recorders.len(), 1);
        assert!(!dashboard.recorders[0].connected);
    }

    /// The departed list is bounded — an Instance whose recorder is flapping
    /// must not accumulate a row per attempt.
    #[test]
    fn the_departed_list_is_bounded() {
        let recorders = Recorders::default();

        for _ in 0..(REMEMBERED + 4) {
            drop(recorders.arrive(1_000));
        }

        assert_eq!(recorders.dashboard(2_000).recorders.len(), REMEMBERED);
    }

    /// Connected Recorders come first, whatever order they arrived in: the
    /// departed are context, not the answer.
    #[test]
    fn the_connected_are_listed_before_the_departed() {
        let recorders = Recorders::default();
        drop(recorders.arrive(1_000));
        let _live = recorders.arrive(2_000);

        let dashboard = recorders.dashboard(3_000);

        assert_eq!(dashboard.recorders.len(), 2);
        assert!(dashboard.recorders[0].connected);
        assert!(!dashboard.recorders[1].connected);
    }

    /// A frame for a connection that has already gone is dropped rather than
    /// resurrecting it — the adapter cannot close that race, because a frame can
    /// be in flight while the guard drops.
    #[test]
    fn a_frame_after_the_hang_up_changes_nothing() {
        let recorders = Recorders::default();
        let id = {
            let dialed = recorders.arrive(1_000);
            dialed.id()
        };

        recorders.apply(id, frame(r#"{"type":"rates","rates":[]}"#), 2_000);

        assert_eq!(recorders.connected(), 0);
    }

    /// Every reason the recorder can give is in the list a screen seeds its
    /// zeroes from, and nothing else is — #70's `Admission::OUTCOMES` rule, held
    /// in both directions.
    #[test]
    fn the_reason_vocabulary_is_exactly_what_can_be_produced() {
        let produced: Vec<&'static str> = (0..16).filter_map(not_recorded_reason).collect();

        assert_eq!(produced, REASONS);
    }

    /// Every state the recorder can be in has a word, and anything else says so
    /// rather than showing a number an Operator has to look up.
    #[test]
    fn every_recorder_state_has_a_name() {
        let named: Vec<&'static str> = (0..9).map(state_name).collect();

        assert_eq!(
            named,
            vec![
                "monitoring",
                "recording",
                "inactive",
                "active",
                "idle",
                "unknown",
                "stopped",
                "available",
                "ignore",
            ],
            "5 is not a state trunk-recorder has"
        );
        assert_eq!(state_name(99), "unknown");
    }
}
