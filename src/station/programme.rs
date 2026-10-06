//! What one Station stream plays, decided purely (#74).
//!
//! The live feed's [`crate::live::Connection`] shape: `on(Event) -> Vec<Action>`
//! with everything that decides anything as its state, and nothing that awaits,
//! reads a store or touches a socket. So every rule below is a value a test
//! constructs and asserts on — and the property everything else stands on,
//! that `Due(n)` puts exactly `n` frames on the air whatever else is happening,
//! is a `proptest` rather than a hope.
//!
//! Frames are opaque here. What makes concatenating them a valid stream is
//! [`super::encode`]'s story; this decides *which* frames, in what order.
//!
//! - **Arrival order, one Call ahead.** A heard Call waits its turn; one is read
//!   at a time, and the next is not read until the one before it goes on the
//!   air — so a burst of traffic costs one Call of memory, not a burst of them.
//! - **Two minutes behind live, at most.** A station twenty minutes behind is
//!   not live; past the bound the oldest waiting Calls are passed over, never
//!   the newest.
//! - **Half a second between transmissions**, so two Calls do not run into one
//!   another, and silence whenever nothing is ready.
//! - **What is on the air, for a player that asked** (ICY metadata,
//!   `Icy-MetaData: 1`). A block can only fall every [`METAINT`] bytes, so the
//!   first one after a Call goes on the air names it — never before its audio,
//!   and at most 288 ms into it. The last title stands through the silence
//!   after it, the way a scanner's display holds the last channel it stopped
//!   on.

use std::collections::VecDeque;
use std::sync::Arc;

use bytes::Bytes;

use super::encode::FRAME_BYTES;
use crate::access::AccessScope;
use crate::call::{CallId, StoredCall};
use crate::selection::Selection;

/// Everything that can reach a stream's programme.
pub(crate) enum Event {
    /// A Call went out on the live feed.
    Heard(Arc<StoredCall>),
    /// The Call this programme asked for, as frames — or `None`, nothing to play.
    Prepared(CallId, Option<Vec<Bytes>>),
    /// This many frames are owed to the listener now.
    Due(u64),
    /// What time it is — for the one decision here that needs a clock.
    Tick(i64),
    /// The stream fell this many Calls behind the fanout, and missed them.
    Lagged(u64),
    /// The fanout has nobody left to send on it.
    Closed,
    /// What this stream may hear, as it stands now — because something that
    /// decides it changed: its code was edited, revoked or rotated, or a
    /// channel was restricted.
    Rescoped {
        scope: AccessScope,
        held: Option<crate::access::CodeHeld>,
    },
}

impl Event {
    /// What a `broadcast` receiver answered, as the programme hears it.
    ///
    /// Here rather than in the adapter so that all three answers are decided —
    /// and tested — in one place, including the two a running Instance makes
    /// rare: a stream a thousand Calls behind, and a fanout with no sender left.
    pub(crate) fn from_fanout(
        answer: &Result<crate::live::Emitted, tokio::sync::broadcast::error::RecvError>,
    ) -> Self {
        use tokio::sync::broadcast::error::RecvError;
        match answer {
            Ok(emitted) => Event::Heard(emitted.call.clone()),
            Err(RecvError::Lagged(skipped)) => Event::Lagged(*skipped),
            Err(RecvError::Closed) => Event::Closed,
        }
    }
}

/// What the adapter is to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Action {
    /// Read and encode this Call, and hand it back as [`Event::Prepared`].
    Prepare { id: CallId, object_key: String },
    /// Send these bytes.
    Send(Bytes),
    /// End the stream — refused, for this reason, or simply over.
    End(Option<crate::failure::Reason>),
}

/// Silent frames between one transmission and the next: 504 ms.
const GAP_FRAMES: usize = 14;

/// How far behind live the Calls waiting to be read may hold a stream.
const MAX_BEHIND_MS: i64 = 2 * 60 * 1_000;

/// The least a waiting Call costs the backlog, whatever it says it lasts — so a
/// flood of Calls claiming no length at all is bounded by count as well.
const LEAST_COST_MS: i64 = 1_000;

/// Audio bytes between two metadata blocks, for a player that asked for them —
/// the `icy-metaint` a response states. Eight frames, 288 ms: short, because a
/// scanner's transmissions are seconds long and a title that arrives after the
/// Call it names is a title for the wrong one. A block that has nothing new to
/// say is one zero byte, so the cost of short is a third of a percent.
pub(crate) const METAINT: usize = 8 * FRAME_BYTES;

/// The longest title a block carries, in characters — comfortably inside the
/// 4080 bytes one length byte can describe, whatever the characters are.
const MAX_TITLE_CHARS: usize = 200;

/// A Call heard and not yet read.
///
/// The Call itself, not only its id, because a scope that narrows while it
/// waits has to be able to ask about it again ([`Event::Rescoped`]) — and the
/// fanout already shares it, so holding it costs a reference count.
struct Waiting {
    call: Arc<StoredCall>,
    /// What a player is told while it is on the air.
    title: String,
    /// What it holds the stream behind live by ([`cost_of`]).
    cost_ms: i64,
}

/// A Call read and encoded.
struct Segment {
    call: Arc<StoredCall>,
    frames: VecDeque<Bytes>,
    title: String,
    /// What it holds the stream behind live by until it goes on the air.
    cost_ms: i64,
}

/// How long a Call will take on the air: its length — or [`LEAST_COST_MS`],
/// when that is less or unknown — and the gap after it.
fn cost_of(call: &StoredCall) -> i64 {
    let gap_ms = (GAP_FRAMES as u128 * super::encode::FRAME.as_millis()) as i64;
    call.duration_ms.unwrap_or(0).max(LEAST_COST_MS) + gap_ms
}

/// Where a player that asked for metadata (`Icy-MetaData: 1`) has got to.
struct Announcer {
    /// Audio bytes before the next block.
    left: usize,
    /// The title the last block carried.
    said: String,
}

impl Announcer {
    /// The block due before a frame, if one is due: the title when it changed,
    /// one zero byte when it did not.
    fn before_frame(&mut self, title: &str, out: &mut Vec<u8>) {
        if self.left > 0 {
            return;
        }
        self.left = METAINT;
        if self.said == title {
            out.push(0);
            return;
        }
        self.said = title.to_string();
        let mut block = format!("StreamTitle='{title}';").into_bytes();
        block.resize(block.len().div_ceil(16) * 16, 0);
        out.push((block.len() / 16) as u8);
        out.extend(block);
    }
}

/// What a player is told is on the air: `System - Talkgroup`, the order the
/// convention reads as *artist - title* — the lock screen's own pairing — and
/// named the way the app names them.
fn title_of(call: &StoredCall) -> String {
    let system = call
        .system_label
        .clone()
        .unwrap_or_else(|| format!("System {}", call.system_ref));
    let talkgroup = call
        .talkgroup_label
        .clone()
        .unwrap_or_else(|| format!("Talkgroup {}", call.talkgroup_ref));
    format!("{system} - {talkgroup}")
        .chars()
        // A player finds the end of a title by looking for `';`, so a quote in
        // an Operator's label would end it early; the typographic one reads the
        // same. Control characters have no business on a display at all.
        .filter(|c| !c.is_control())
        .map(|c| if c == '\'' { '’' } else { c })
        .take(MAX_TITLE_CHARS)
        .collect()
}

/// Whether a stream listening to `selection` with `scope` plays `call`: what
/// the live feed would deliver to the same Selection and scope
/// ([`AccessScope::delivers`]) — and an Encrypted Call never, which the live
/// feed delivers because it is a fact worth seeing and which has nothing in it
/// to hear.
fn plays(selection: &Selection, scope: &AccessScope, call: &StoredCall) -> bool {
    !call.encrypted && scope.delivers(selection, call)
}

/// One stream's programme.
pub(crate) struct Programme {
    selection: Selection,
    scope: AccessScope,
    /// Heard and not yet read, in arrival order.
    waiting: VecDeque<Waiting>,
    /// The Call being read, if one is.
    preparing: Option<Waiting>,
    /// A Call read and waiting its turn behind the one on the air.
    ready: Option<Segment>,
    /// What is left of the Call on the air.
    playing: VecDeque<Bytes>,
    /// Silent frames still owed after the Call that just ended.
    gap: usize,
    /// The last Call to go on the air, as a player is told it — kept through
    /// the silence after it, the way a scanner's display holds the last
    /// channel it stopped on.
    title: String,
    /// Metadata, for a player that asked for it.
    announcer: Option<Announcer>,
    /// The **Access code** this stream opened with, if it opened with one.
    held: Option<crate::access::CodeHeld>,
}

impl Programme {
    pub(crate) fn new(selection: Selection, scope: AccessScope) -> Self {
        Programme {
            selection,
            scope,
            waiting: VecDeque::new(),
            preparing: None,
            ready: None,
            playing: VecDeque::new(),
            gap: 0,
            title: String::new(),
            announcer: None,
            held: None,
        }
    }

    /// The same programme, opened with an **Access code**.
    pub(crate) fn holding(mut self, held: Option<crate::access::CodeHeld>) -> Self {
        self.held = held;
        self
    }

    /// The same programme, telling the player what is on the air every
    /// [`METAINT`] bytes.
    pub(crate) fn announcing(mut self) -> Self {
        self.announcer = Some(Announcer {
            left: METAINT,
            said: String::new(),
        });
        self
    }

    pub(crate) fn on(&mut self, event: Event) -> Vec<Action> {
        match event {
            Event::Heard(call) => {
                if !self.reaches(&call) {
                    return Vec::new();
                }
                self.wait(Waiting {
                    title: title_of(&call),
                    cost_ms: cost_of(&call),
                    call,
                });
                self.read_next().into_iter().collect()
            }
            Event::Prepared(id, frames) => {
                let Some(read) = self.preparing.take_if(|waiting| waiting.call.id == id) else {
                    return Vec::new();
                };
                // Asked again, because the scope may have narrowed while it was
                // being read.
                self.ready = frames
                    .filter(|frames| !frames.is_empty() && self.reaches(&read.call))
                    .map(|frames| Segment {
                        frames: VecDeque::from(frames),
                        title: read.title,
                        cost_ms: read.cost_ms,
                        call: read.call,
                    });
                let mut actions = Vec::new();
                self.air_ready(&mut actions);
                actions.extend(self.read_next());
                actions
            }
            Event::Due(frames) => {
                let mut actions = Vec::new();
                let mut sent = Vec::with_capacity(frames as usize * FRAME_BYTES);
                for _ in 0..frames {
                    let frame = self.next_frame(&mut actions);
                    if let Some(announcer) = &mut self.announcer {
                        announcer.before_frame(&self.title, &mut sent);
                        announcer.left = announcer.left.saturating_sub(frame.len());
                    }
                    sent.extend(frame);
                }
                actions.push(Action::Send(Bytes::from(sent)));
                actions
            }
            Event::Tick(now_ms) => self
                .held
                .as_ref()
                .and_then(|held| held.ran_out(now_ms))
                .map(|reason| Action::End(Some(reason)))
                .into_iter()
                .collect(),
            // A player that stopped reading long enough for a thousand Calls to
            // go out. What it missed is missed — the two-minute bound would have
            // passed most of it over anyway — and the count is the measure of
            // it, as it is on a live socket.
            Event::Lagged(skipped) => {
                tracing::warn!(skipped, "station stream lagged behind the fanout");
                Vec::new()
            }
            Event::Closed => vec![Action::End(None)],
            Event::Rescoped { scope, held } => {
                self.scope = scope;
                self.held = held;
                self.forget_what_is_out_of_reach();
                let mut actions = Vec::new();
                self.air_ready(&mut actions);
                actions.extend(self.read_next());
                actions
            }
        }
    }

    /// Whether this stream plays `call` — [`plays`], asked of where it stands.
    fn reaches(&self, call: &StoredCall) -> bool {
        plays(&self.selection, &self.scope, call)
    }

    /// Let go of every Call queued under a scope that no longer reaches it —
    /// the one held ready and the ones waiting. The Call on the air finishes,
    /// and the one being read is asked again when it comes back.
    fn forget_what_is_out_of_reach(&mut self) {
        self.ready = self.ready.take().filter(|ready| self.reaches(&ready.call));
        let (selection, scope) = (&self.selection, &self.scope);
        self.waiting
            .retain(|waiting| plays(selection, scope, &waiting.call));
    }

    /// How far behind live everything ahead in line holds the stream: what is
    /// waiting, what is being read, and what is held ready — each for as long
    /// as it will take on the air.
    ///
    /// Summed when it is asked rather than kept as a running total, because a
    /// total has to be kept right through every way a Call leaves the line —
    /// read, aired, passed over, let go when a scope narrows — and one of them
    /// would be the one somebody forgot. The line is bounded by this very sum,
    /// so the count it walks is too.
    fn behind_ms(&self) -> i64 {
        let waiting: i64 = self.waiting.iter().map(|waiting| waiting.cost_ms).sum();
        let preparing = self.preparing.as_ref().map_or(0, |read| read.cost_ms);
        let ready = self.ready.as_ref().map_or(0, |ready| ready.cost_ms);
        waiting + preparing + ready
    }

    /// Queue a heard Call, passing over the oldest waiting ones if it would
    /// otherwise hold the stream more than [`MAX_BEHIND_MS`] behind live.
    ///
    /// Never the newest: it is the one nearest live, and a single Call longer
    /// than the whole bound is still a Call somebody wanted to hear.
    fn wait(&mut self, call: Waiting) {
        self.waiting.push_back(call);
        let mut behind_ms = self.behind_ms();
        let mut skipped = 0u32;
        while behind_ms > MAX_BEHIND_MS && self.waiting.len() > 1 {
            if let Some(oldest) = self.waiting.pop_front() {
                behind_ms -= oldest.cost_ms;
                skipped += 1;
            }
        }
        if skipped > 0 {
            // Once per arrival that caused it, never once per Call skipped
            // (ADR-0011 rule 8) — and DEBUG, because a busy evening on a
            // selection this wide is the policy working, not a fault.
            tracing::debug!(
                skipped,
                "station stream passed over Calls to stay near live"
            );
        }
    }

    /// Read the next waiting Call, if nothing is being read and nothing is held
    /// ready — one Call ahead of the air, never more.
    fn read_next(&mut self) -> Option<Action> {
        if self.preparing.is_some() || self.ready.is_some() {
            return None;
        }
        let next = self.waiting.pop_front()?;
        let action = Action::Prepare {
            id: next.call.id,
            object_key: next.call.object_key.clone(),
        };
        self.preparing = Some(next);
        Some(action)
    }

    /// Put the ready Call on the air if the air is free — nothing playing, and
    /// the gap after the last one served — and start reading the one behind it.
    fn air_ready(&mut self, actions: &mut Vec<Action>) {
        if !self.playing.is_empty() || self.gap > 0 {
            return;
        }
        if let Some(call) = self.ready.take() {
            self.playing = call.frames;
            self.title = call.title;
            actions.extend(self.read_next());
        }
    }

    /// The next frame on the air.
    fn next_frame(&mut self, actions: &mut Vec<Action>) -> Bytes {
        self.air_ready(actions);
        if let Some(frame) = self.playing.pop_front() {
            if self.playing.is_empty() {
                self.gap = GAP_FRAMES;
            }
            return frame;
        }
        self.gap = self.gap.saturating_sub(1);
        super::encode::silence()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::station::encode::silence;
    use std::sync::Arc;

    /// A Call on System 1, Talkgroup `talkgroup`, stored at `k<id>`.
    fn call(id: CallId, talkgroup: i64) -> StoredCall {
        StoredCall {
            id,
            system_ref: 1,
            talkgroup_ref: talkgroup,
            duration_ms: Some(1_000),
            object_key: format!("k{id}"),
            audio_url: Some(format!("/api/call/{id}/audio")),
            ..StoredCall::default()
        }
    }

    /// Everything on.
    fn everything() -> Selection {
        Selection {
            all: true,
            ..Selection::default()
        }
    }

    /// A stand-in frame, recognisable on the tape by its tag.
    fn frame(tag: u8) -> Bytes {
        Bytes::from(vec![tag; FRAME_BYTES])
    }

    /// What a round of actions put on the air, one character a frame: the
    /// tag of a stand-in frame, or `.` for silence.
    fn tape(actions: &[Action]) -> String {
        let sent: Vec<u8> = actions
            .iter()
            .filter_map(|action| match action {
                Action::Send(bytes) => Some(bytes.to_vec()),
                _ => None,
            })
            .flatten()
            .collect();
        sent.chunks(FRAME_BYTES)
            .map(|frame| match frame == silence().as_ref() {
                true => '.',
                false => frame[0] as char,
            })
            .collect()
    }

    #[test]
    fn a_call_in_the_selection_is_prepared_as_soon_as_it_is_heard() {
        let mut programme = Programme::new(everything(), AccessScope::All);

        let actions = programme.on(Event::Heard(Arc::new(call(7, 100))));

        assert_eq!(
            actions,
            vec![Action::Prepare {
                id: 7,
                object_key: "k7".into()
            }]
        );
    }

    /// Only Talkgroup 100 on System 1.
    fn just_100() -> Selection {
        Selection {
            sel: [("1".into(), [("100".into(), true)].into())].into(),
            all: false,
        }
    }

    /// **What the live feed would deliver, and nothing else** — the stream is
    /// one more way to hear the same Calls, so it asks the live feed's own
    /// question rather than a copy of it.
    #[rstest::rstest]
    #[case::selected(just_100(), AccessScope::All, call(7, 100), true)]
    #[case::not_selected(just_100(), AccessScope::All, call(7, 200), false)]
    #[case::patched_onto_a_selected_channel(
        just_100(),
        AccessScope::All,
        StoredCall { patches: vec![100], ..call(7, 200) },
        true
    )]
    #[case::gated_and_not_granted(
        everything(),
        AccessScope::open(),
        StoredCall { restricted: true, ..call(7, 100) },
        false
    )]
    #[case::gated_and_granted(
        everything(),
        AccessScope::Granted(just_100()),
        StoredCall { restricted: true, ..call(7, 100) },
        true
    )]
    #[case::open_channel_to_an_open_listener(everything(), AccessScope::open(), call(7, 100), true)]
    #[case::encrypted_so_nothing_to_hear(
        everything(),
        AccessScope::All,
        StoredCall { encrypted: true, audio_url: None, ..call(7, 100) },
        false
    )]
    fn plays_what_the_live_feed_would_deliver(
        #[case] selection: Selection,
        #[case] scope: AccessScope,
        #[case] heard: StoredCall,
        #[case] plays: bool,
    ) {
        let mut programme = Programme::new(selection, scope);

        let actions = programme.on(Event::Heard(Arc::new(heard)));

        assert_eq!(!actions.is_empty(), plays, "{actions:?}");
    }

    fn prepare(id: CallId) -> Action {
        Action::Prepare {
            id,
            object_key: format!("k{id}"),
        }
    }

    fn silent(frames: usize) -> String {
        ".".repeat(frames)
    }

    /// **Arrival order, a breath between transmissions, and never more than one
    /// Call held ahead.** Three arrive at once; the first is read while the
    /// others wait their turn, and the third is not read until the second has
    /// gone on the air — so a burst costs one Call of memory, not a burst of
    /// them.
    #[test]
    fn calls_play_in_arrival_order_with_a_gap_between_them() {
        let mut programme = Programme::new(everything(), AccessScope::All);
        assert_eq!(
            programme.on(Event::Heard(Arc::new(call(1, 100)))),
            vec![prepare(1)]
        );
        assert_eq!(programme.on(Event::Heard(Arc::new(call(2, 100)))), vec![]);
        assert_eq!(programme.on(Event::Heard(Arc::new(call(3, 100)))), vec![]);

        // The first goes on the air, so the second may be read behind it.
        let ready = programme.on(Event::Prepared(1, Some(vec![frame(b'A'); 2])));
        assert_eq!(ready, vec![prepare(2)]);
        assert_eq!(
            programme.on(Event::Prepared(2, Some(vec![frame(b'B'); 2]))),
            vec![]
        );

        let aired = programme.on(Event::Due((2 + GAP_FRAMES + 2) as u64));
        assert_eq!(tape(&aired), format!("AA{}BB", silent(GAP_FRAMES)));
        assert!(
            aired.contains(&prepare(3)),
            "the third is read once the second airs"
        );
    }

    /// Half a second — long enough that two transmissions do not run into one
    /// another, short enough that a busy channel does not drift behind live.
    #[test]
    fn the_gap_is_half_a_second() {
        let gap = super::super::encode::FRAME * GAP_FRAMES as u32;
        assert!(gap.abs_diff(std::time::Duration::from_millis(500)) < super::super::encode::FRAME);
    }

    /// A Call that could not be read is passed over, and the next one is read
    /// in its place — and an answer about a Call nobody is waiting for is
    /// ignored rather than played.
    #[test]
    fn an_unplayable_call_is_passed_over() {
        let mut programme = Programme::new(everything(), AccessScope::All);
        programme.on(Event::Heard(Arc::new(call(1, 100))));
        programme.on(Event::Heard(Arc::new(call(2, 100))));

        assert_eq!(programme.on(Event::Prepared(1, None)), vec![prepare(2)]);
        assert_eq!(
            programme.on(Event::Prepared(9, Some(vec![frame(b'Z')]))),
            vec![]
        );
        programme.on(Event::Prepared(2, Some(vec![frame(b'B')])));

        assert_eq!(tape(&programme.on(Event::Due(2))), "B.");
    }

    fn lasting(duration_ms: Option<i64>, call: StoredCall) -> StoredCall {
        StoredCall {
            duration_ms,
            ..call
        }
    }

    /// Every Call this programme reads from here on, answering each with
    /// nothing so the next is read at once.
    fn drain(programme: &mut Programme, first: Vec<Action>) -> Vec<CallId> {
        let mut read = Vec::new();
        let mut actions = first;
        while let Some(Action::Prepare { id, .. }) = actions.pop() {
            read.push(id);
            actions = programme.on(Event::Prepared(id, None));
        }
        read
    }

    /// **A station that is twenty minutes behind is not live.** When the Calls
    /// queued ahead of the air would hold the stream more than two minutes
    /// behind, the oldest waiting ones are passed over — and the newest is
    /// never one of them, because it is the one nearest live.
    ///
    /// Everything ahead in line counts, and what each one costs is the time it
    /// will take on the air: its length *and* the half-second after it. Here
    /// that is 30.504 s a Call, and the one being read is ahead in line too —
    /// so it and two waiting fit in two minutes, and a third would not.
    #[test]
    fn a_backlog_beyond_two_minutes_skips_the_oldest() {
        let mut programme = Programme::new(everything(), AccessScope::All);
        programme.on(Event::Heard(Arc::new(lasting(Some(30_000), call(1, 100)))));
        for id in 2..=6 {
            programme.on(Event::Heard(Arc::new(lasting(Some(30_000), call(id, 100)))));
        }

        let first = programme.on(Event::Prepared(1, None));

        assert_eq!(drain(&mut programme, first), vec![5, 6]);
    }

    /// The Call held ready behind the one on the air is ahead in line as well.
    #[test]
    fn a_call_held_ready_counts_against_the_backlog() {
        let mut programme = Programme::new(everything(), AccessScope::All);
        programme.on(Event::Heard(Arc::new(call(1, 100))));
        programme.on(Event::Prepared(1, Some(vec![frame(b'A')])));
        programme.on(Event::Heard(Arc::new(lasting(Some(30_000), call(2, 100)))));
        programme.on(Event::Prepared(2, Some(vec![frame(b'B')])));
        for id in 3..=6 {
            programme.on(Event::Heard(Arc::new(lasting(Some(30_000), call(id, 100)))));
        }

        // 1 airs, then the gap, then 2 — which is when 2's successor is read.
        let aired = programme.on(Event::Due(1 + GAP_FRAMES as u64 + 1));
        let read: Vec<Action> = aired
            .into_iter()
            .filter(|action| matches!(action, Action::Prepare { .. }))
            .collect();

        assert_eq!(drain(&mut programme, read), vec![5, 6]);
    }

    /// A Call that says it lasts no time at all — or says nothing — still costs
    /// the backlog a second, so a flood of them is bounded by count too: 1.504
    /// s each with the gap, so the one being read and 78 waiting.
    #[rstest::rstest]
    #[case::zero_length(Some(0))]
    #[case::unmeasured(None)]
    fn a_flood_of_instant_calls_is_bounded(#[case] duration_ms: Option<i64>) {
        let mut programme = Programme::new(everything(), AccessScope::All);
        programme.on(Event::Heard(Arc::new(call(1, 100))));
        for id in 2..=301 {
            programme.on(Event::Heard(Arc::new(lasting(duration_ms, call(id, 100)))));
        }

        let first = programme.on(Event::Prepared(1, None));

        assert_eq!(
            drain(&mut programme, first),
            (224..=301).collect::<Vec<_>>()
        );
    }

    /// One Call longer than the whole bound still plays: it is the newest.
    #[test]
    fn one_call_longer_than_the_bound_still_plays() {
        let mut programme = Programme::new(everything(), AccessScope::All);
        programme.on(Event::Heard(Arc::new(call(1, 100))));
        programme.on(Event::Heard(Arc::new(lasting(Some(600_000), call(2, 100)))));

        let first = programme.on(Event::Prepared(1, None));

        assert_eq!(drain(&mut programme, first), vec![2]);
    }

    /// A stream announcing what is on the air, read back the way a player
    /// reads it: [`METAINT`] bytes of audio, then a length byte, then sixteen
    /// times that many bytes of metadata — over and over.
    ///
    /// Returns the audio alone, and every block's `StreamTitle` in order
    /// (`None` for a block that said nothing new).
    fn announced(actions: &[Action]) -> (Vec<u8>, Vec<Option<String>>) {
        let sent: Vec<u8> = actions
            .iter()
            .filter_map(|action| match action {
                Action::Send(bytes) => Some(bytes.to_vec()),
                _ => None,
            })
            .flatten()
            .collect();
        let (mut audio, mut blocks, mut rest) = (Vec::new(), Vec::new(), &sent[..]);
        while rest.len() > METAINT {
            audio.extend(&rest[..METAINT]);
            let length = rest[METAINT] as usize * 16;
            let block = &rest[METAINT + 1..METAINT + 1 + length];
            blocks.push((length > 0).then(|| {
                let text = String::from_utf8(block.to_vec()).expect("UTF-8");
                let text = text.trim_end_matches('\0');
                text.strip_prefix("StreamTitle='")
                    .and_then(|title| title.strip_suffix("';"))
                    .expect("one StreamTitle")
                    .to_string()
            }));
            rest = &rest[METAINT + 1 + length..];
        }
        audio.extend(rest);
        (audio, blocks)
    }

    fn named(call: StoredCall) -> StoredCall {
        StoredCall {
            system_label: Some("Fulton County".into()),
            talkgroup_label: Some("Fire Dispatch".into()),
            ..call
        }
    }

    /// **What is playing, on the player's own screen** — VLC, a Sonos app, a
    /// car's head unit. Said once when a Call goes on the air and then left
    /// alone, so the screen keeps showing the last transmission through the
    /// silence after it, the way a scanner's display does.
    #[test]
    fn a_player_that_asks_is_told_what_is_on_the_air() {
        let mut programme = Programme::new(everything(), AccessScope::All).announcing();
        let interval = METAINT / FRAME_BYTES;
        programme.on(Event::Heard(Arc::new(named(call(1, 100)))));

        let quiet = programme.on(Event::Due(interval as u64 * 2));
        programme.on(Event::Prepared(1, Some(vec![frame(b'A'); interval])));
        let on_air = programme.on(Event::Due(interval as u64 * 3));

        let (audio, blocks) = announced(&[quiet, on_air].concat());
        assert_eq!(
            audio.len(),
            interval * 5 * FRAME_BYTES,
            "every frame is still there"
        );
        assert_eq!(
            blocks,
            vec![
                None,
                Some("Fulton County - Fire Dispatch".into()),
                None,
                None
            ]
        );
    }

    /// A Call nobody named is called what the app calls it.
    #[test]
    fn an_unnamed_channel_is_announced_by_its_numbers() {
        let mut programme = Programme::new(everything(), AccessScope::All).announcing();
        programme.on(Event::Heard(Arc::new(call(1, 100))));
        programme.on(Event::Prepared(1, Some(vec![frame(b'A')])));

        let (_, blocks) = announced(&programme.on(Event::Due((METAINT / FRAME_BYTES + 1) as u64)));

        assert_eq!(blocks, vec![Some("System 1 - Talkgroup 100".into())]);
    }

    /// An Operator's label is free text, and a player finds the end of a title
    /// by looking for `';` — so a quote cannot be allowed to end one early, and
    /// nothing can make a block longer than its one length byte can say.
    #[rstest::rstest]
    #[case::apostrophe("O'Hare Ramp';", "O’Hare Ramp’;")]
    #[case::control_characters("Fire\nDispatch\0", "FireDispatch")]
    #[case::far_too_long(&"x".repeat(5_000), &"x".repeat(MAX_TITLE_CHARS))]
    fn a_label_cannot_break_the_block_it_rides_in(#[case] label: &str, #[case] said: &str) {
        let mut programme = Programme::new(everything(), AccessScope::All).announcing();
        programme.on(Event::Heard(Arc::new(StoredCall {
            system_label: Some("S".into()),
            talkgroup_label: Some(label.into()),
            ..call(1, 100)
        })));
        programme.on(Event::Prepared(1, Some(vec![frame(b'A')])));

        let (_, blocks) = announced(&programme.on(Event::Due((METAINT / FRAME_BYTES + 1) as u64)));

        assert_eq!(
            blocks,
            vec![Some(
                format!("S - {said}")
                    .chars()
                    .take(MAX_TITLE_CHARS)
                    .collect()
            )]
        );
    }

    /// One thing that can happen to a stream, for the property below.
    #[derive(Debug, Clone)]
    enum Step {
        /// A Call is heard, lasting this many seconds.
        Hear(u8),
        /// The outstanding read comes back with this many frames, or nothing.
        Answer(Option<u8>),
        /// This many frames fall due.
        Due(u8),
    }

    fn step() -> impl proptest::strategy::Strategy<Value = Step> {
        use proptest::prelude::*;
        prop_oneof![
            any::<u8>().prop_map(Step::Hear),
            proptest::option::of(0u8..40).prop_map(Step::Answer),
            (0u8..60).prop_map(Step::Due),
        ]
    }

    proptest::proptest! {
        /// **Whatever happens, the air is exactly as long as the clock says.**
        /// The stream is paced by counting frames, so `Due(n)` has to put
        /// exactly `n` frames on the air — never fewer because nothing was
        /// ready, never more because a Call was — and a player that asked for
        /// metadata has to be able to strip it back out at every interval.
        /// Calls go on the air in the order they were heard, each one whole.
        #[test]
        fn the_air_is_whole_frames_in_arrival_order(
            steps in proptest::collection::vec(step(), 0..120),
            announcing: bool,
        ) {
            let programme = Programme::new(everything(), AccessScope::All);
            let mut programme = match announcing {
                true => programme.announcing(),
                false => programme,
            };
            let (mut next_id, mut asked, mut due, mut all) = (1, None, 0usize, Vec::new());

            for step in steps {
                let actions = match step {
                    Step::Hear(seconds) => {
                        next_id += 1;
                        programme.on(Event::Heard(Arc::new(lasting(
                            Some(seconds as i64 * 1_000),
                            call(next_id, 100),
                        ))))
                    }
                    Step::Answer(frames) => match asked.take() {
                        // Each Call's frames carry its id, so the tape says
                        // which Call every frame of it came from.
                        Some(id) => programme.on(Event::Prepared(
                            id,
                            frames.map(|n| vec![frame(id as u8); n as usize]),
                        )),
                        None => Vec::new(),
                    },
                    Step::Due(frames) => {
                        due += frames as usize;
                        let actions = programme.on(Event::Due(frames as u64));
                        if !announcing {
                            proptest::prop_assert_eq!(
                                tape(&actions).len(),
                                frames as usize,
                            );
                        }
                        actions
                    }
                };
                for action in &actions {
                    if let Action::Prepare { id, .. } = action {
                        proptest::prop_assert!(asked.is_none(), "one read at a time");
                        asked = Some(*id);
                    }
                }
                all.extend(actions);
            }

            let audio = match announcing {
                true => announced(&all).0,
                false => all
                    .iter()
                    .filter_map(|action| match action {
                        Action::Send(bytes) => Some(bytes.to_vec()),
                        _ => None,
                    })
                    .flatten()
                    .collect(),
            };
            proptest::prop_assert_eq!(audio.len(), due * FRAME_BYTES);
            let aired: Vec<u8> = audio
                .chunks(FRAME_BYTES)
                .filter(|frame| *frame != silence().as_ref())
                .map(|frame| frame[0])
                .collect();
            proptest::prop_assert!(aired.windows(2).all(|pair| pair[0] <= pair[1]));
        }
    }

    fn code(expires_at_ms: Option<i64>) -> crate::access::CodeHeld {
        crate::access::CodeHeld {
            id: 4,
            label: Some("Engine 7".into()),
            expires_at_ms,
            max_connections: None,
        }
    }

    /// **A code that runs out takes its streams with it** — on the stream's own
    /// clock, as it takes its live sockets (#68). A smart speaker is exactly the
    /// listener nobody closes a tab on, so a stream that only checked when it
    /// opened would be a code that never expires.
    #[rstest::rstest]
    #[case::before_its_expiry(Some(code(Some(10_000))), 9_999, vec![])]
    #[case::at_its_expiry(
        Some(code(Some(10_000))),
        10_000,
        vec![Action::End(Some(crate::failure::Reason::AccessCodeExpired { code_id: 4 }))]
    )]
    #[case::a_code_that_never_expires(Some(code(None)), i64::MAX, vec![])]
    #[case::no_code_at_all(None, i64::MAX, vec![])]
    fn a_code_that_runs_out_ends_the_stream(
        #[case] held: Option<crate::access::CodeHeld>,
        #[case] now_ms: i64,
        #[case] expected: Vec<Action>,
    ) {
        let mut programme = Programme::new(everything(), AccessScope::All).holding(held);

        assert_eq!(programme.on(Event::Tick(now_ms)), expected);
    }

    fn gated(call: StoredCall) -> StoredCall {
        StoredCall {
            restricted: true,
            ..call
        }
    }

    /// **A narrowed scope reaches what is already queued, not only what comes
    /// next.** A stream is the listener nobody reconnects, so an Operator who
    /// revokes a code — or restricts a channel a stream was hearing openly —
    /// means *now*, and a queue two minutes deep would otherwise be two more
    /// minutes of exactly what they meant to stop. The Call being read is
    /// passed over when it comes back, the one held ready is let go, and the
    /// waiting ones are re-asked; what is already on the air finishes.
    #[test]
    fn a_narrowed_scope_passes_over_what_it_no_longer_reaches() {
        let mut programme = Programme::new(everything(), AccessScope::Granted(just_100()));
        programme.on(Event::Heard(Arc::new(gated(call(1, 100)))));
        programme.on(Event::Prepared(1, Some(vec![frame(b'A')])));
        programme.on(Event::Heard(Arc::new(gated(call(2, 100)))));
        programme.on(Event::Prepared(2, Some(vec![frame(b'B')])));
        programme.on(Event::Heard(Arc::new(gated(call(3, 100)))));
        programme.on(Event::Heard(Arc::new(gated(call(4, 100)))));
        programme.on(Event::Heard(Arc::new(call(5, 200))));

        let rescoped = programme.on(Event::Rescoped {
            scope: AccessScope::open(),
            held: None,
        });

        // 1 was on the air and finishes; 2 was ready and is let go, so 3 — the
        // next waiting — would be read, but it is gated too: 5 is read.
        assert_eq!(rescoped, vec![prepare(5)]);
        programme.on(Event::Prepared(5, Some(vec![frame(b'E')])));
        assert_eq!(
            tape(&programme.on(Event::Due(1 + GAP_FRAMES as u64 + 1))),
            format!("A{}E", silent(GAP_FRAMES))
        );
    }

    /// The Call being read when the scope narrows is checked again when it
    /// comes back, rather than played because it was asked for under the old
    /// one.
    #[test]
    fn a_call_read_under_the_old_scope_is_checked_again_when_it_is_ready() {
        let mut programme = Programme::new(everything(), AccessScope::Granted(just_100()));
        programme.on(Event::Heard(Arc::new(gated(call(1, 100)))));
        programme.on(Event::Rescoped {
            scope: AccessScope::open(),
            held: None,
        });

        programme.on(Event::Prepared(1, Some(vec![frame(b'A')])));

        assert_eq!(tape(&programme.on(Event::Due(2))), "..");
    }

    /// A rescope carries the code as it now stands — an expiry an Operator
    /// brought forward is the one the next tick ends the stream on.
    #[test]
    fn a_rescope_carries_the_code_as_it_now_stands() {
        let mut programme =
            Programme::new(everything(), AccessScope::All).holding(Some(code(Some(99_000))));

        programme.on(Event::Rescoped {
            scope: AccessScope::All,
            held: Some(code(Some(10_000))),
        });

        assert_eq!(
            programme.on(Event::Tick(10_000)),
            vec![Action::End(Some(
                crate::failure::Reason::AccessCodeExpired { code_id: 4 }
            ))]
        );
    }

    /// What the fanout hands a stream, as the programme hears it — the three
    /// answers a `broadcast` receiver has, one event each.
    #[test]
    fn the_fanout_is_heard_as_one_event_per_answer() {
        use tokio::sync::broadcast::error::RecvError;
        let emitted = crate::live::Emitted {
            seq: 1,
            call: std::sync::Arc::new(call(7, 100)),
        };

        assert!(matches!(
            Event::from_fanout(&Ok(emitted.clone())),
            Event::Heard(heard) if heard.id == 7
        ));
        assert!(matches!(
            Event::from_fanout(&Err(RecvError::Lagged(12))),
            Event::Lagged(12)
        ));
        assert!(matches!(
            Event::from_fanout(&Err(RecvError::Closed)),
            Event::Closed
        ));
    }

    /// A stream that fell behind the fanout says so and plays on: what it
    /// missed is missed, and the count is the measure of it — the live feed's
    /// own answer to the same thing.
    #[test]
    fn falling_behind_the_fanout_is_said_and_survived() {
        let capture = crate::testing::LogCapture::start();
        let mut programme = Programme::new(everything(), AccessScope::All);

        assert_eq!(programme.on(Event::Lagged(12)), vec![]);

        let logged = capture.text();
        assert!(
            logged.contains("station stream lagged behind the fanout"),
            "{logged}"
        );
        assert!(logged.contains("skipped=12"), "{logged}");
        assert_eq!(tape(&programme.on(Event::Due(1))), ".", "and it plays on");
    }

    /// A fanout with nobody left to send on it is an Instance shutting down,
    /// and a stream with nothing left to hear ends — with nothing refused.
    #[test]
    fn a_closed_fanout_ends_the_stream() {
        let mut programme = Programme::new(everything(), AccessScope::All);

        assert_eq!(programme.on(Event::Closed), vec![Action::End(None)]);
    }

    #[test]
    fn silence_until_a_call_is_ready_then_the_call() {
        let mut programme = Programme::new(everything(), AccessScope::All);
        programme.on(Event::Heard(Arc::new(call(7, 100))));

        assert_eq!(tape(&programme.on(Event::Due(3))), "...");
        programme.on(Event::Prepared(7, Some(vec![frame(b'A'), frame(b'A')])));
        assert_eq!(tape(&programme.on(Event::Due(3))), "AA.");
    }
}
