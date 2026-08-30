//! **Quiet spans** (#59, spec US 23): where nobody is talking, so **Catch-up**
//! can skip it.
//!
//! A Listener forty Calls behind is sitting through two things — the words, and
//! the gaps between them. Raising the rate shortens both by the same fraction;
//! only knowing *where the gaps are* shortens the backlog by more than the rate
//! alone. That is the whole of what this produces: a handful of
//! `[start_ms, end_ms]` pairs per Call, small enough to ride on the Call itself.
//!
//! # Why the server has to answer this
//!
//! The client cannot. [ADR-0005](../../docs/adr/0005-client-audio-media-session-background.md)
//! gives Radio-Scout exactly one `<audio>` element and forbids WebAudio — iOS
//! classifies WebAudio output as ambient and mutes it in the background, which
//! is precisely why rdio-scanner has no working background audio — so there is
//! no path by which a browser could look at these samples. The levers a player
//! has are `playbackRate` and `currentTime`, and `currentTime` is worth nothing
//! without somewhere to point it.
//!
//! # Two halves, and the seam between them is the point
//!
//! - [`detect::spans`] turns audio into quiet spans, and touches no database.
//! - [`worker`] reads a Call's object, asks that question once, and writes the
//!   answer down.
//!
//! [`crate::tone`]'s split, for its reason: every rule about what counts as a
//! gap is then a test over a synthesized waveform with no Instance behind it,
//! and every rule about failure is a test over a substituted [`worker::Archive`]
//! with no audio behind it.
//!
//! # The one thing that is different from every other worker here
//!
//! **This one is on by default and looks at every Call.** Enhancement ships
//! `off`; tone-out detection skips any Call whose channel has no **Tone
//! profile**, which is every Call on nearly every Instance. Catch-up has no
//! equivalent gate — whether a Call has a gap in it can only be answered by
//! looking — so this is the first worker in the process that decodes every Call
//! there is.
//!
//! Three things keep that affordable, and they are why it is defensible rather
//! than merely convenient. It **decodes and scans**, where enhancement decodes,
//! resamples twice, filters, measures loudness and re-encodes — the cheap half
//! of the cheaper worker. It sheds rather than waits, so a burst costs the
//! Calls at the back their spans and costs ingest nothing. And it is
//! **`[quiet] enabled`**, which an Operator on constrained hardware, or one who
//! does not care about Catch-up, can switch off in one line — unlike the Mining
//! sweep, which is on unconditionally because it finishes.
//!
//! # Nothing goes back over the Archive
//!
//! [`crate::db::entities::call::QuietState::NONE`] is never re-queued, which is
//! enhancement's rule and tone-out detection's: an Operator upgrading must not
//! find their whole Archive being read back on the next boot. What a restart
//! *does* pick up is `pending` — work this Instance already accepted and had not
//! finished.

pub mod detect;
pub mod worker;

use std::collections::BTreeMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tracing::warn;

use crate::call::CallId;
use crate::worker::{Handoff, Meter, Ticket};

/// What this Worker is called on a status surface (#93, #70).
pub const WORKER: &str = "quiet";

/// The shortest gap worth telling a client about.
///
/// A seek is not free — it costs the element a re-buffer, and on iOS it is the
/// one operation that can stall a backgrounded page — so a span has to save
/// more than it costs. At Catch-up's rate 1.2 s of audio is 0.8 s of a
/// Listener's life, which is the shortest gap anybody would notice being
/// skipped. Below it, the rate alone is the better answer.
///
/// It is also what keeps this off the wire on most Calls: somebody saying one
/// thing has no 1.2-second hole in it, so [`crate::call::StoredCall`] carries no
/// key at all for them.
pub const MIN_SPAN_MS: i64 = 1_200;

/// A stretch of a Call where nobody is talking.
///
/// Milliseconds from the start of the Call, so it means the same thing to the
/// detector, the column, the wire and a `currentTime`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub start_ms: i64,
    pub end_ms: i64,
}

/// How a span goes on the wire: a pair, because it is two numbers and there
/// will never be a third.
///
/// Written once and read by both senders — [`QuietWindow::of`] and
/// [`crate::archive::stored_calls`] — because the same span reaching a Listener
/// two ways has to be the same two numbers in the same order.
impl From<Span> for [i64; 2] {
    fn from(span: Span) -> Self {
        [span.start_ms, span.end_ms]
    }
}

/// How the spans are written down.
///
/// `"3200-5100,9000-11400"` — one column, no JSON, no child table. A child
/// table would be [`crate::tone`]'s `call_tones`, and the difference is what
/// each is *for*: a page-out is an event an Operator searches and filters on, so
/// it earns rows; a quiet span is a playback hint read only by whoever is
/// playing that exact Call, and giving it a table would put a `RESTRICT` foreign
/// key on the retention sweeper's path for something nothing ever joins to.
///
/// Text rather than JSON for the same reason it is not a table: this is never
/// queried, only round-tripped, and a packed string is a third the bytes and
/// needs no parser to fail.
pub fn pack(spans: &[Span]) -> String {
    spans
        .iter()
        .map(|span| format!("{}-{}", span.start_ms, span.end_ms))
        .collect::<Vec<_>>()
        .join(",")
}

/// Read [`pack`]'s column back.
///
/// **Anything unreadable is no span rather than an error.** The column is a
/// hint: a row hand-edited into nonsense, or written by a future release that
/// spells this differently, should cost a Listener a trim and never a 500 on a
/// search page. Malformed entries are dropped individually, so one bad pair does
/// not discard the Call's other gaps.
pub fn unpack(packed: &str) -> Vec<Span> {
    packed
        .split(',')
        .filter_map(|pair| {
            let (start, end) = pair.split_once('-')?;
            let start_ms = start.trim().parse().ok()?;
            let end_ms = end.trim().parse().ok()?;
            (end_ms > start_ms).then_some(Span { start_ms, end_ms })
        })
        .collect()
}

/// What `GET /api/calls/quiet` answers with: the spans of the Calls that have
/// any, by Call id.
///
/// **Keyed, and sparse.** A Listener asks about the window of their queue they
/// are about to play; most of those Calls have nothing to trim and are simply
/// absent, so the ordinary answer to a twenty-id question is a handful of
/// entries. A list of `{id, quiet}` objects would have to carry the empties to
/// mean the same thing, or mean something subtly different.
///
/// The ids are strings because JSON object keys are. `BTreeMap` rather than a
/// `HashMap` so the bytes are a function of the answer and nothing else — the
/// same reason [`crate::curate::document`] orders everything it exports.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct QuietWindow(pub BTreeMap<String, Vec<[i64; 2]>>);

impl QuietWindow {
    /// Build the answer from what the Archive holds.
    pub fn of(found: Vec<(CallId, Vec<Span>)>) -> Self {
        QuietWindow(
            found
                .into_iter()
                .map(|(id, spans)| {
                    (
                        id.to_string(),
                        spans.into_iter().map(<[i64; 2]>::from).collect(),
                    )
                })
                .collect(),
        )
    }
}

crate::answers_json!(QuietWindow);

/// How many Calls one request may ask about.
///
/// The client's own queue holds a hundred, and **Catch-up** asks about the few
/// it is about to play rather than all of them — so this is a ceiling on a
/// mistake, not a limit anything legitimate reaches. It exists because the
/// parameter is a list an anonymous caller writes: without it, one request could
/// name every Call in a county's Archive.
pub const MAX_WINDOW: usize = 250;

/// `[quiet]` — what looking for gaps costs, and whether to do it at all.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "snake_case", deny_unknown_fields)]
pub struct QuietConfig {
    /// Look at all. **On**, unlike `[enhancement] mode`: Catch-up is a Listener
    /// feature and an Operator should not have to find a setting before the
    /// queue can be drained. Off is for the Instance that will never have one —
    /// a headless forwarder — or for hardware that cannot spare the decode.
    pub enabled: bool,
    /// How many Calls may be waiting to be looked at before the rest keep
    /// whatever they arrived as. [`crate::tone::ToneConfig`]'s knob and its
    /// bargain: the Call is already stored, answered and on the live feed, so
    /// shedding costs a Listener a trim and never a Call.
    pub queue_depth: usize,
}

/// How many Calls may be waiting to be scanned.
///
/// [`crate::tone`]'s number, and this one is likelier to reach it: that worker
/// skips every Call whose channel has no profile, where this looks at all of
/// them. A scan is a decode and nothing else, so the queue drains far faster
/// than a county's traffic arrives — but a burst after a restart is real, and
/// the cost of a deep queue is a `usize` per waiting id.
const DEFAULT_QUEUE_DEPTH: usize = 512;

impl Default for QuietConfig {
    fn default() -> Self {
        QuietConfig {
            enabled: true,
            queue_depth: DEFAULT_QUEUE_DEPTH,
        }
    }
}

/// The handle ingest offers a Call to, and the worker drains.
///
/// [`crate::tone::Tones`]'s shape minus its `armed` bit, which has no
/// counterpart here: whether a Call *might* hold a page is a question about a
/// roster, and whether it holds a gap is a question about the audio. There is
/// nothing to cache, so the gate is the configuration and nothing else.
#[derive(Clone)]
pub struct Quiet(Arc<Inner>);

struct Inner {
    /// Call ids, never audio: the object is written by the time anything lands
    /// here, and the worker reads it back rather than carrying megabytes through
    /// a channel.
    submissions: mpsc::Sender<(CallId, Ticket)>,
    /// Taken once, by [`worker::spawn`]. A second spawn finds it gone and starts
    /// no second worker.
    inbox: Handoff<mpsc::Receiver<(CallId, Ticket)>>,
    /// The queue depth an Operator reads (#93, #70).
    meter: Arc<Meter>,
    enabled: bool,
}

impl Default for Quiet {
    fn default() -> Self {
        Quiet::new(QuietConfig::default())
    }
}

impl Quiet {
    pub fn new(config: QuietConfig) -> Self {
        let (submissions, inbox) = mpsc::channel(config.queue_depth.max(1));
        Quiet(Arc::new(Inner {
            submissions,
            inbox: Handoff::new(inbox),
            meter: Arc::new(Meter::default()),
            enabled: config.enabled,
        }))
    }

    /// An Instance that never looks — `[quiet] enabled = false`.
    ///
    /// Read by ingest before anything else, so a switched-off Instance spends no
    /// statement per upload marking a row `pending` for a worker that will never
    /// come for it.
    pub fn is_enabled(&self) -> bool {
        self.0.enabled
    }

    /// Offer a stored Call. `false` means it will keep whatever it arrived as.
    ///
    /// [`crate::tone::Tones::submit`]'s contract: a full queue **sheds**, and
    /// says so once, because blocking here would put a background convenience in
    /// front of an upload that has already been answered.
    pub fn submit(&self, call_id: CallId) -> bool {
        if !self.0.enabled {
            return false;
        }
        let ticket = self.0.meter.admit();
        match self.0.submissions.try_send((call_id, ticket)) {
            Ok(()) => true,
            Err(mpsc::error::TrySendError::Full(_)) => {
                warn!(
                    reason = %"queue-full",
                    "this Call will not be scanned for quiet spans"
                );
                false
            }
            // The worker has gone, which happens only as the process shuts down.
            Err(mpsc::error::TrySendError::Closed(_)) => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use rstest::rstest;

    fn span(start_ms: i64, end_ms: i64) -> Span {
        Span { start_ms, end_ms }
    }

    /// What the column looks like, said out loud once — so a change to the
    /// packing is a change to this string and not a silent change to what every
    /// stored row means.
    #[test]
    fn the_column_is_the_pairs_it_says_it_is() {
        assert_eq!(
            pack(&[span(3_200, 5_100), span(9_000, 11_400)]),
            "3200-5100,9000-11400"
        );
        assert_eq!(pack(&[]), "");
    }

    /// **Anything unreadable is no span rather than an error**, and one bad pair
    /// does not discard the Call's other gaps. The column is a hint: a row
    /// hand-edited into nonsense, or written by a release that spells this
    /// differently, must cost a Listener a trim and never a 500 on a search page.
    #[rstest]
    #[case::empty("", vec![])]
    #[case::not_a_pair("hello", vec![])]
    #[case::not_numbers("a-b", vec![])]
    #[case::backwards("5000-1000", vec![])]
    #[case::zero_length("1000-1000", vec![])]
    #[case::one_bad_pair_of_three("100-200,oops,300-4000", vec![span(100, 200), span(300, 4_000)])]
    #[case::whitespace(" 100 - 200 ", vec![span(100, 200)])]
    fn a_column_that_cannot_be_read_costs_a_trim_and_nothing_else(
        #[case] packed: &str,
        #[case] expected: Vec<Span>,
    ) {
        assert_eq!(unpack(packed), expected);
    }

    /// **An Instance that does not scan offers nothing**, and the switch is read
    /// before the queue: `[quiet] enabled = false` must cost an upload nothing at
    /// all, not a ticket admitted and immediately dropped.
    #[test]
    fn a_disabled_instance_takes_no_offers() {
        let quiet = Quiet::new(QuietConfig {
            enabled: false,
            queue_depth: 8,
        });

        assert!(!quiet.is_enabled());
        assert!(!quiet.submit(1));
    }

    /// A full queue **sheds**, and says so: the Call is already stored, playable
    /// and on the live feed, and the only thing lost is a trim. Blocking here
    /// would put a background convenience in front of an upload that has already
    /// been answered.
    #[test]
    fn a_full_queue_sheds_rather_than_waits() {
        let quiet = Quiet::new(QuietConfig {
            enabled: true,
            queue_depth: 1,
        });
        let capture = crate::testing::LogCapture::start();

        assert!(quiet.submit(1), "the first fits");
        assert!(!quiet.submit(2), "the second does not");

        let logged = capture.text();
        assert!(logged.contains("reason=queue-full"), "{logged}");
    }

    /// Once the worker has gone — which happens only as the process shuts down —
    /// an offer is refused rather than panicking on a closed channel.
    #[test]
    fn an_offer_after_the_worker_has_gone_is_refused() {
        let quiet = Quiet::default();
        drop(quiet.0.inbox.take().expect("the inbox"));

        assert!(!quiet.submit(1));
    }

    /// The answer is keyed by Call id and **carries only the Calls that have
    /// something**, which is what keeps a twenty-id question a small reply.
    #[test]
    fn the_window_is_keyed_and_sparse() {
        let window = QuietWindow::of(vec![(91, vec![span(3_200, 5_100)]), (94, Vec::new())]);

        assert_eq!(
            serde_json::to_string(&window).expect("json"),
            r#"{"91":[[3200,5100]],"94":[]}"#
        );
        assert_eq!(
            serde_json::to_string(&QuietWindow::default()).expect("json"),
            "{}"
        );
    }

    proptest! {
        /// **Every span a detector could produce survives the column.** The
        /// packing is the one place the answer stops being a value and becomes a
        /// string, and a pair that did not round-trip would be a Listener seeking
        /// somewhere the audio is not.
        #[test]
        fn every_span_round_trips_through_the_column(
            pairs in proptest::collection::vec((0i64..3_600_000, 1i64..3_600_000), 0..8),
        ) {
            let spans: Vec<Span> = pairs
                .into_iter()
                .map(|(start_ms, length)| span(start_ms, start_ms + length))
                .collect();

            prop_assert_eq!(unpack(&pack(&spans)), spans);
        }
    }
}
