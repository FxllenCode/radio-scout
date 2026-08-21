//! **Tone-out detection** (#55, spec US 20): the page-out that marks the Call.
//!
//! A **Tone profile** (CONTEXT.md) is a station's paging sequence — Motorola
//! Quick Call II's two tones, a single long group tone, or an A-B-group run of
//! three — written down against a Talkgroup. When a Call on that channel holds
//! the sequence, the Call carries a **Mark**, the way the Recorder's
//! **Emergency** bit does (#42). It is *shown*, filtered and searched on, and
//! delivered to nobody except an Operator's own **Webhook**
//! ([ADR-0014](../../docs/adr/0014-no-notifications.md)).
//!
//! # Two halves, and the seam between them is the point
//!
//! - [`detect::runs`] turns audio into **steady-tone runs**, once per Call,
//!   whatever the profiles say.
//! - [`matches`] asks a [`Profile`] about those runs, and touches no audio.
//!
//! So a Talkgroup with twenty profiles decodes its Call once, and every rule
//! about ordering, tolerance and gaps is a test that constructs three structs.
//! The DSP tests are then confined to the one question that genuinely needs
//! synthesized audio — whether a tone is *found* — which is the split
//! [ADR-0009](../../docs/adr/0009-testing-strategy.md) calls the DSP unit seam.
//!
//! # No speech, ever
//!
//! This asks what *frequency* is present and never what was said
//! ([ADR-0013](../../docs/adr/0013-no-transcription.md)). A Tone profile is
//! signal, which is exactly why it survives a ban that took keyword alerts with
//! it.
//!
//! # Improving on rdio-scanner
//!
//! rdio-scanner has no tone detection of any kind, so there is nothing to be
//! compatible with and the whole design is ours. What the *field* does — the
//! Whisper-and-regex dashboards bolted onto Trunk Recorder — reaches this by
//! transcribing, which is banned here and is also the wrong tool: a page-out is
//! two sinusoids, and the machinery that turns speech into text cannot hear
//! them at all.

pub mod detect;
pub mod worker;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tracing::warn;

use crate::call::CallId;
use crate::worker::{Handoff, Meter, Ticket};

/// What this Worker is called on a status surface (#93, #70).
pub const WORKER: &str = "tone";

/// One tone in a sequence: a frequency, held for at least this long.
///
/// A *minimum* rather than a window, because every real-world error is on the
/// short side — a squelch opening late, a recorder starting mid-tone — and a
/// station whose B tone runs four seconds instead of three has not paged a
/// different station.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Step {
    pub hz: f64,
    pub min_ms: i64,
}

/// How far off a tone may be and still count, as a percentage of the tone.
///
/// Quick Call II is specified to ±1%; twice that is the room a recorder's
/// sample clock and an analogue path need, and is what every field tool
/// defaults to.
pub const DEFAULT_TOLERANCE_PCT: f64 = 2.0;

/// How long a silence between two tones may be before the sequence has been
/// broken. A console keys the tones back to back, so this is slack for the
/// path rather than a feature of the signal.
pub const DEFAULT_GAP_MAX_MS: i64 = 300;

/// A station's paging sequence, as an Operator wrote it down.
///
/// **An ordered sequence rather than a fixed A/B pair**, which costs nothing —
/// the matcher walks a list either way — and buys the two shapes that are not
/// Quick Call II: a single long tone (a group or all-call page) and an
/// A-B-then-long-group run of three. A form that could only spell two tones
/// would have to grow a column and a migration the first time somebody used
/// one.
#[derive(Debug, Clone, PartialEq)]
pub struct Profile {
    pub id: i64,
    /// What the match is *called* — "Station 12", "Squad 4". The reason a mark
    /// records which profile fired: an Operator with twelve stations on one
    /// dispatch channel is asking who was paged, not whether somebody was.
    pub label: String,
    pub tolerance_pct: f64,
    pub gap_max_ms: i64,
    pub steps: Vec<Step>,
}

/// A page-out found in a Call.
#[derive(Debug, Clone, PartialEq)]
pub struct ToneMatch {
    pub profile_id: i64,
    /// The profile's label **as it was when the page fired**. Snapshotted onto
    /// the Call rather than joined, so history says what happened: a profile
    /// renamed next year, or deleted, does not rewrite or erase the pages it
    /// already caught.
    pub label: String,
    /// How far into the Call the sequence began, in milliseconds.
    pub at_ms: i64,
}

/// How many tones one profile may name.
///
/// A ceiling rather than a rule about paging: the longest real sequence is three
/// (two tones and a group tone), and this exists so a form cannot be posted a
/// thousand steps that every Call on a busy channel then walks.
pub const MAX_STEPS: usize = 8;

/// The shortest tone worth asserting on.
///
/// Below one analysis window there is nothing to measure, and a step this short
/// would match essentially any transient — a squelch tail, a click, the leading
/// edge of a word. 100 ms is several windows and still well under the shortest
/// real page.
pub const MIN_STEP_MS: i64 = 100;

/// The widest tolerance that still means anything.
///
/// At 25% a 1000 Hz step matches everything from 750 to 1250, which on a Quick
/// Call set overlaps several of its own neighbours — so past here a profile
/// stops identifying a station and starts marking the channel. Refused rather
/// than clamped, because an Operator who typed it meant something and clamping
/// would leave them believing something else was configured.
pub const MAX_TOLERANCE_PCT: f64 = 25.0;

/// The longest gap between two tones a profile may allow.
///
/// Ten seconds is already far beyond a console's keying, and the ceiling exists
/// for the same reason as the tolerance one: past it, "the sequence" stops being
/// a sequence and becomes two tones that happened in the same Call.
pub const MAX_GAP_MS: i64 = 10_000;

/// **Why this profile could never page anything** — or `None` if it could.
///
/// Written here rather than in [`crate::curate::tones`] because three surfaces
/// ask it and a disagreement between them is invisible until an Operator's
/// station stops being paged: the admin form validating what was typed, the
/// configuration document (#51) validating what was imported, and
/// `client/src/lib/tone.ts` telling somebody *before* they submit. It is
/// [`crate::webhook::is_postable_url`]'s arrangement, for its reason.
///
/// The band is [`detect`]'s own, not a second opinion about it: a step outside
/// what the detector looks at is not a strict profile, it is a profile that can
/// never fire, and storing one silently is how an Operator ends up believing
/// their pager is being watched.
pub fn unusable(steps: &[Step], tolerance_pct: f64, gap_max_ms: i64) -> Option<String> {
    if steps.is_empty() {
        return Some(String::from("a tone profile needs at least one tone"));
    }
    if steps.len() > MAX_STEPS {
        return Some(format!("a tone profile may name at most {MAX_STEPS} tones"));
    }
    for step in steps {
        if !(detect::BAND_LOW_HZ..=detect::BAND_HIGH_HZ).contains(&step.hz) {
            return Some(format!(
                "{} Hz is outside the {}-{} Hz range tone-out detection listens to",
                step.hz,
                detect::BAND_LOW_HZ,
                detect::BAND_HIGH_HZ
            ));
        }
        if step.min_ms < MIN_STEP_MS {
            return Some(format!(
                "a tone must be held for at least {MIN_STEP_MS} ms (got {} ms)",
                step.min_ms
            ));
        }
    }
    if !(tolerance_pct.is_finite() && tolerance_pct > 0.0 && tolerance_pct <= MAX_TOLERANCE_PCT) {
        return Some(format!(
            "tolerance must be between 0 and {MAX_TOLERANCE_PCT}%, as a percentage of each tone"
        ));
    }
    if !(0..=MAX_GAP_MS).contains(&gap_max_ms) {
        return Some(format!(
            "the gap between tones must be between 0 and {MAX_GAP_MS} ms"
        ));
    }
    None
}

/// Whether these runs hold this profile's sequence, and where.
///
/// **Pure, and the whole matching policy.** Three rules, and each is one a field
/// tool gets wrong somewhere:
///
/// - **In order, and consecutively.** The next tone must be the next run —
///   something else in between means this was not the sequence, it was two of
///   its tones with a third thing between them.
/// - **Long enough**, per step, per [`Step::min_ms`].
/// - **Soon enough**, per [`Profile::gap_max_ms`], measured from the end of one
///   tone to the start of the next.
///
/// A profile with no steps matches **nothing**, which is the safe direction and
/// the [`crate::webhook::marks_of`] one: the alternative is a half-written row
/// marking every Call on the channel.
pub fn matches(runs: &[detect::Run], profile: &Profile) -> Option<ToneMatch> {
    if profile.steps.is_empty() {
        return None;
    }
    (0..runs.len())
        .find(|&start| sequence_at(runs, profile, start))
        .map(|start| ToneMatch {
            profile_id: profile.id,
            label: profile.label.clone(),
            at_ms: runs[start].start_ms,
        })
}

/// Does the sequence begin at `start`?
fn sequence_at(runs: &[detect::Run], profile: &Profile, start: usize) -> bool {
    if start + profile.steps.len() > runs.len() {
        return false;
    }
    profile.steps.iter().enumerate().all(|(offset, step)| {
        let run = runs[start + offset];
        let soon_enough =
            offset == 0 || run.start_ms - runs[start + offset - 1].end_ms() <= profile.gap_max_ms;
        soon_enough && run.duration_ms >= step.min_ms && within(run.hz, step.hz, profile)
    })
}

/// Is `measured` this step's tone, allowing the profile's tolerance?
///
/// A **percentage** rather than a fixed number of Hz, because that is how tone
/// sets are specified and how a recorder's error actually scales: ±2% is ±6 Hz
/// at 300 and ±49 Hz at 2468, and a flat window wide enough for the top of the
/// band would merge neighbouring tones at the bottom of it.
fn within(measured: f64, target: f64, profile: &Profile) -> bool {
    (measured - target).abs() <= target * profile.tolerance_pct.abs() / 100.0
}

/// Every profile on this channel that this Call pages, in the order the
/// profiles were given.
///
/// One walk of the audio, then a walk of the profiles — which is the whole
/// reason [`detect::Run`] exists as a value rather than the detector being
/// asked a question per profile.
pub fn matched(runs: &[detect::Run], profiles: &[Profile]) -> Vec<ToneMatch> {
    profiles
        .iter()
        .filter_map(|profile| matches(runs, profile))
        .collect()
}

/// **What a stored step list means, written once.**
///
/// A sequence is read whole and matched in memory — never joined, never
/// filtered on — so it is one JSON column rather than a child table, the
/// [`crate::webhook::marks_of`] shape. A list that will not parse is **no
/// steps**, which matches nothing: the other direction is a corrupt row marking
/// every Call on a county's dispatch channel.
pub fn steps_of(stored: &str) -> Vec<Step> {
    serde_json::from_str::<Vec<Step>>(stored).unwrap_or_default()
}

/// A step list as the column stores it.
pub fn steps_json(steps: &[Step]) -> String {
    serde_json::to_string(steps).unwrap_or_else(|_| String::from("[]"))
}

impl Profile {
    /// Read a stored row.
    pub fn from_row(row: &crate::db::entities::tone_profile::Model) -> Self {
        Profile {
            id: row.id,
            label: row.label.clone(),
            tolerance_pct: row.tolerance_pct,
            gap_max_ms: row.gap_max_ms,
            steps: steps_of(&row.steps),
        }
    }
}

// -- The detection queue and what an Instance holds ---------------------------

/// How many Calls may be waiting to be looked at.
///
/// [`crate::enhance::DEFAULT_QUEUE_DEPTH`]'s number, and deliberately the same
/// one: both queues hold Call ids behind an ingest path that must never block,
/// and both shed rather than push back. Detection is much the cheaper of the
/// two — a decode and a transform, where enhancement decodes, resamples twice,
/// filters, measures loudness and re-encodes — so a depth that holds
/// enhancement's backlog holds this one comfortably.
pub const DEFAULT_QUEUE_DEPTH: usize = 512;

/// How tone-out detection behaves — the `[tone]` section itself (ADR-0012, #87).
///
/// **Policy only, and there is no master switch**, which is the
/// [`crate::webhook::WebhookConfig`] shape rather than enhancement's: a Tone
/// profile is a **row**, so an Instance with none has an empty roster rather
/// than a feature switched off, and one written in the browser five minutes
/// from now must be detected against without a restart. The profiles themselves
/// are **Curation** and never live in `radio-scout.toml`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ToneConfig {
    /// How many Calls may be waiting to be looked at before the rest keep
    /// whatever they arrived as.
    pub queue_depth: usize,
}

impl Default for ToneConfig {
    fn default() -> Self {
        ToneConfig {
            queue_depth: DEFAULT_QUEUE_DEPTH,
        }
    }
}

/// The tone-out surface, cloned into every handler.
///
/// [`crate::enhance::Enhancer`]'s shape without its `None` arm, for the reason
/// [`ToneConfig`] gives: there is nothing to switch off, only a roster that may
/// be empty. What replaces the `None` arm is [`Tones::armed`] — a single bit
/// saying whether this Instance has *any* enabled profile — because an Instance
/// with none must not pay a statement per Call to rediscover it.
#[derive(Clone)]
pub struct Tones(Arc<Inner>);

struct Inner {
    /// Call ids, never audio: the object is written by the time anything lands
    /// here, and the worker reads it back rather than carrying megabytes
    /// through a channel. Each id travels with the [`Ticket`] that settles it.
    submissions: mpsc::Sender<(CallId, Ticket)>,
    /// Taken once, by [`worker::spawn`]. A second spawn finds it gone and starts
    /// no second worker.
    inbox: Handoff<mpsc::Receiver<(CallId, Ticket)>>,
    /// The queue depth an Operator reads (#93, #70).
    meter: Arc<Meter>,
    /// Whether any enabled **Tone profile** exists at all.
    ///
    /// **The gate that makes this feature free for the Instances not using it.**
    /// [`crate::webhook`] has the same problem and solves it with
    /// [`crate::webhook::Marks::is_empty`], which it can read off the Call in
    /// hand; there is no equivalent here — whether a Call *might* page can only
    /// be answered by asking the database — so the answer is cached as one bit
    /// and refreshed by [`Tones::rearm`] at boot and after every write that
    /// could change it.
    ///
    /// Being wrong in the two directions costs very different things, which is
    /// why this is safe to cache: stale-true wastes one statement per Call until
    /// the next write, and stale-false misses detection until one. Both are
    /// closed by `rearm` running on the same request that changed the roster,
    /// and `tests/tone.rs` asserts that end to end rather than trusting it.
    armed: AtomicBool,
}

impl Default for Tones {
    fn default() -> Self {
        Tones::new(ToneConfig::default())
    }
}

impl Tones {
    pub fn new(config: ToneConfig) -> Self {
        let (submissions, inbox) = mpsc::channel(config.queue_depth.max(1));
        Tones(Arc::new(Inner {
            submissions,
            inbox: Handoff::new(inbox),
            meter: Meter::new(),
            armed: AtomicBool::new(false),
        }))
    }

    /// Whether this Instance has any enabled **Tone profile** at all.
    pub fn is_armed(&self) -> bool {
        self.0.armed.load(Ordering::Relaxed)
    }

    /// Re-read whether anything is worth looking for.
    ///
    /// Called at boot and by every surface that writes a profile — which is what
    /// makes a profile written now apply to the very next Call rather than to
    /// the next restart.
    pub async fn rearm(&self, db: &crate::db::Db) {
        self.arm(crate::db::repo::any_tone_profiles(db).await);
    }

    /// [`Tones::rearm`]'s **decision**, separated from the read that answers it.
    ///
    /// A read that fails leaves the flag as it was and says so. Guessing `false`
    /// would silently stop detecting; guessing `true` would buy a statement per
    /// Call. Keeping the last known answer is right in both directions far more
    /// often than either, and the arm that says so is only reachable at all
    /// because the answer arrives as a value (#97's rule, at the smallest scale
    /// it applies to).
    pub fn arm(&self, answer: Result<bool, sea_orm::DbErr>) {
        match answer {
            Ok(armed) => self.0.armed.store(armed, Ordering::Relaxed),
            Err(error) => warn!(
                reason = %"roster-unreadable",
                %error,
                "could not re-read the tone profile roster; keeping the last answer"
            ),
        }
    }

    /// Offer a Call to the detection queue. `false` means it will not be looked
    /// at, and the caller leaves the row [`crate::db::entities::call::ToneState::NONE`].
    ///
    /// Never blocks and never waits — this is called from the ingest path, which
    /// has already answered the recorder. A full queue is a load signal, not an
    /// error: the Call is stored, playable and on the live feed, and the only
    /// thing lost is that it will not be checked for a page.
    pub fn submit(&self, call_id: CallId) -> bool {
        if !self.is_armed() {
            return false;
        }
        match self.0.submissions.try_send((call_id, self.0.meter.admit())) {
            Ok(()) => true,
            Err(mpsc::error::TrySendError::Full(_)) => {
                // WARN, because something was dropped (ADR-0011 rule 7) and a
                // queue that is persistently full means this hardware cannot
                // check for pages at the rate this System talks — which is an
                // Operator's to act on, since the pages it misses are the whole
                // reason the profiles exist.
                warn!(
                    reason = %"queue-full",
                    call_id, "tone-out detection skipped; this Call was not checked for a page"
                );
                false
            }
            // The worker is gone, which happens only as the process shuts down.
            Err(mpsc::error::TrySendError::Closed(_)) => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use detect::Run;
    use proptest::prelude::*;
    use rstest::rstest;

    fn run(hz: f64, start_ms: i64, duration_ms: i64) -> Run {
        Run {
            hz,
            start_ms,
            duration_ms,
        }
    }

    /// Motorola Quick Call II, as an Operator would write one down.
    fn station_12() -> Profile {
        Profile {
            id: 7,
            label: String::from("Station 12"),
            tolerance_pct: DEFAULT_TOLERANCE_PCT,
            gap_max_ms: DEFAULT_GAP_MAX_MS,
            steps: vec![
                Step {
                    hz: 1122.5,
                    min_ms: 800,
                },
                Step {
                    hz: 1465.6,
                    min_ms: 2000,
                },
            ],
        }
    }

    /// A page-out, as [`detect::runs`] reports one.
    fn page() -> Vec<Run> {
        vec![run(1122.5, 0, 1000), run(1465.6, 1000, 3000)]
    }

    /// The headline: the sequence is there, so the Call is marked — and the
    /// mark says which station, at what offset.
    #[test]
    fn a_page_matches_the_profile_it_pages_and_says_where() {
        assert_eq!(
            matches(&page(), &station_12()),
            Some(ToneMatch {
                profile_id: 7,
                label: String::from("Station 12"),
                at_ms: 0,
            })
        );
    }

    /// The page is rarely the first thing in the Call — a dispatcher keys up,
    /// says something, then pages — so the sequence is looked for everywhere in
    /// the Call rather than only at its start.
    #[test]
    fn a_page_part_way_through_a_call_is_still_a_page() {
        let runs = [vec![run(500.0, 0, 400)], page()].concat();

        assert_eq!(matches(&runs, &station_12()).map(|m| m.at_ms), Some(0));
    }

    /// **Every way a near miss can miss.** Each of these is a real recording
    /// that must not page a station: a neighbouring tone set, a tone the
    /// dispatcher cut short, a sequence run backwards, and two of the right
    /// tones with something else in between.
    #[rstest]
    #[case(page(), true, "the page itself")]
    #[case(vec![run(1122.5, 0, 1000), run(1499.0, 1000, 3000)], false, "the B tone 2.3% off")]
    #[case(vec![run(1160.0, 0, 1000), run(1465.6, 1000, 3000)], false, "the A tone 3.3% off")]
    #[case(vec![run(1122.5, 0, 1000), run(1465.6, 1000, 1200)], false, "the B tone cut short")]
    #[case(vec![run(1122.5, 0, 400), run(1465.6, 400, 3000)], false, "the A tone cut short")]
    #[case(vec![run(1465.6, 0, 3000), run(1122.5, 3000, 1000)], false, "the sequence backwards")]
    #[case(vec![run(1122.5, 0, 1000)], false, "only the A tone")]
    #[case(
        vec![run(1122.5, 0, 1000), run(900.0, 1000, 300), run(1465.6, 1300, 3000)],
        false,
        "a third tone in between",
    )]
    #[case(
        vec![run(1122.5, 0, 1000), run(1465.6, 1500, 3000)],
        false,
        "half a second of silence between the tones",
    )]
    fn only_the_sequence_pages_the_station(
        #[case] runs: Vec<Run>,
        #[case] expected: bool,
        #[case] what: &str,
    ) {
        assert_eq!(matches(&runs, &station_12()).is_some(), expected, "{what}");
    }

    /// Two tones keyed back to back overlap by the window that straddles them
    /// ([`Run::end_ms`]), so the gap is **signed** and a small negative one is
    /// what a real page looks like. Measuring a magnitude here would fail every
    /// Quick Call there is.
    #[test]
    fn tones_keyed_back_to_back_overlap_and_still_page() {
        let runs = vec![run(1122.5, 0, 1020), run(1465.6, 980, 3020)];

        assert!(matches(&runs, &station_12()).is_some(), "{runs:?}");
    }

    /// A single long tone is a group page, and a three-tone run is an A-B-group
    /// — the two shapes a fixed A/B pair could not spell, which is why a
    /// profile is a sequence.
    #[rstest]
    #[case(vec![Step { hz: 1050.0, min_ms: 6000 }], vec![run(1050.0, 0, 8000)], true, "a long group tone")]
    #[case(vec![Step { hz: 1050.0, min_ms: 6000 }], vec![run(1050.0, 0, 3000)], false, "a group tone cut short")]
    #[case(
        vec![
            Step { hz: 1122.5, min_ms: 800 },
            Step { hz: 1465.6, min_ms: 2000 },
            Step { hz: 1050.0, min_ms: 4000 },
        ],
        vec![run(1122.5, 0, 1000), run(1465.6, 1000, 3000), run(1050.0, 4000, 6000)],
        true,
        "a two-tone page followed by a group tone",
    )]
    fn a_sequence_spells_the_shapes_a_fixed_pair_could_not(
        #[case] steps: Vec<Step>,
        #[case] runs: Vec<Run>,
        #[case] expected: bool,
        #[case] what: &str,
    ) {
        let profile = Profile {
            steps,
            ..station_12()
        };

        assert_eq!(matches(&runs, &profile).is_some(), expected, "{what}");
    }

    /// **A profile that could never fire is refused, and says why.**
    ///
    /// Every one of these would otherwise be stored, never match, and produce no
    /// error anywhere — which is the failure an Operator cannot see, because a
    /// pager that is not being watched looks exactly like a pager that has not
    /// gone off. The band is [`detect`]'s own rather than a second opinion about
    /// it, so a step this accepts is one the detector really looks at.
    #[rstest]
    #[case(station_12().steps, DEFAULT_TOLERANCE_PCT, DEFAULT_GAP_MAX_MS, None)]
    #[case(
        Vec::new(),
        DEFAULT_TOLERANCE_PCT,
        DEFAULT_GAP_MAX_MS,
        Some("at least one tone")
    )]
    #[case(
        vec![Step { hz: 1122.5, min_ms: 800 }; MAX_STEPS + 1],
        DEFAULT_TOLERANCE_PCT,
        DEFAULT_GAP_MAX_MS,
        Some("at most"),
    )]
    #[case(
        vec![Step { hz: detect::BAND_LOW_HZ - 1.0, min_ms: 800 }],
        DEFAULT_TOLERANCE_PCT,
        DEFAULT_GAP_MAX_MS,
        Some("outside"),
    )]
    #[case(
        vec![Step { hz: detect::BAND_HIGH_HZ + 1.0, min_ms: 800 }],
        DEFAULT_TOLERANCE_PCT,
        DEFAULT_GAP_MAX_MS,
        Some("outside"),
    )]
    #[case(
        vec![Step { hz: 1122.5, min_ms: MIN_STEP_MS - 1 }],
        DEFAULT_TOLERANCE_PCT,
        DEFAULT_GAP_MAX_MS,
        Some("at least"),
    )]
    #[case(station_12().steps, 0.0, DEFAULT_GAP_MAX_MS, Some("tolerance"))]
    #[case(station_12().steps, f64::NAN, DEFAULT_GAP_MAX_MS, Some("tolerance"))]
    #[case(station_12().steps, MAX_TOLERANCE_PCT + 1.0, DEFAULT_GAP_MAX_MS, Some("tolerance"))]
    #[case(station_12().steps, DEFAULT_TOLERANCE_PCT, -1, Some("gap"))]
    #[case(station_12().steps, DEFAULT_TOLERANCE_PCT, MAX_GAP_MS + 1, Some("gap"))]
    // The edges of every control the form offers: an Operator choosing an
    // extreme must not be told it is out of range.
    #[case(
        vec![
            Step { hz: detect::BAND_LOW_HZ, min_ms: MIN_STEP_MS },
            Step { hz: detect::BAND_HIGH_HZ, min_ms: MIN_STEP_MS },
        ],
        MAX_TOLERANCE_PCT,
        MAX_GAP_MS,
        None,
    )]
    fn a_profile_that_could_never_fire_is_refused_with_a_reason(
        #[case] steps: Vec<Step>,
        #[case] tolerance_pct: f64,
        #[case] gap_max_ms: i64,
        #[case] expected: Option<&str>,
    ) {
        let refused = unusable(&steps, tolerance_pct, gap_max_ms);

        match expected {
            None => assert_eq!(refused, None, "{refused:?}"),
            Some(needle) => assert!(
                refused.as_deref().is_some_and(|it| it.contains(needle)),
                "{refused:?} should have mentioned {needle:?}"
            ),
        }
    }

    /// **A Call is offered only when there is something to look for.** On the
    /// Instances that will never write a profile — which is most of them — this
    /// one atomic load is the whole cost of the feature.
    #[test]
    fn an_unarmed_instance_offers_nothing() {
        let tones = Tones::default();

        assert!(!tones.is_armed());
        assert!(!tones.submit(1));

        tones.arm(Ok(true));
        assert!(tones.is_armed());
        assert!(tones.submit(1));
    }

    /// **A roster that cannot be read keeps the last answer.** Guessing `false`
    /// would silently stop detecting and guessing `true` would buy a statement
    /// per Call; neither is better than what was true a moment ago.
    #[test]
    fn a_roster_that_cannot_be_read_changes_nothing() {
        let tones = Tones::default();
        tones.arm(Ok(true));

        tones.arm(Err(sea_orm::DbErr::Custom(String::from("no"))));

        assert!(
            tones.is_armed(),
            "still armed, on the last answer that worked"
        );
    }

    /// A full queue **sheds**, and says so: the Call is already stored, playable
    /// and on the live feed, and the only thing lost is knowing whether it held
    /// a page. Blocking here would put a background convenience in front of an
    /// upload that has already been answered.
    #[test]
    fn a_full_queue_sheds_rather_than_waits() {
        let tones = Tones::new(ToneConfig { queue_depth: 1 });
        tones.arm(Ok(true));
        let capture = crate::testing::LogCapture::start();

        assert!(tones.submit(1), "the first fits");
        assert!(!tones.submit(2), "the second does not");

        let logged = capture.text();
        assert!(logged.contains("reason=queue-full"), "{logged}");
    }

    /// Once the worker has gone — which happens only as the process shuts down
    /// — an offer is refused rather than panicking on a closed channel.
    #[test]
    fn an_offer_after_the_worker_has_gone_is_refused() {
        let tones = Tones::new(ToneConfig { queue_depth: 8 });
        tones.arm(Ok(true));
        drop(tones.0.inbox.take().expect("the inbox"));

        assert!(!tones.submit(1));
    }

    /// A half-written profile pages **nothing**. The other direction marks every
    /// Call on a county's dispatch channel, which is the failure an Operator
    /// cannot take back once a Webhook has carried it.
    #[rstest]
    #[case(Vec::new(), "no steps at all")]
    #[case(steps_of("not a list"), "a step list that will not parse")]
    #[case(steps_of(r#"[{"hz":1122.5}]"#), "a step missing its duration")]
    fn a_profile_that_says_nothing_pages_nothing(#[case] steps: Vec<Step>, #[case] what: &str) {
        let profile = Profile {
            steps,
            ..station_12()
        };

        assert!(matches(&page(), &profile).is_none(), "{what}");
        assert!(matches(&[], &profile).is_none(), "{what}, no runs");
    }

    /// Every profile on the channel that fired, and only those — one walk of
    /// the audio, then a walk of the profiles.
    #[test]
    fn every_profile_the_call_pages_is_reported_and_no_others() {
        let station_4 = Profile {
            id: 9,
            label: String::from("Station 4"),
            steps: vec![Step {
                hz: 1122.5,
                min_ms: 800,
            }],
            ..station_12()
        };
        let elsewhere = Profile {
            id: 11,
            label: String::from("Squad 1"),
            steps: vec![Step {
                hz: 2468.2,
                min_ms: 800,
            }],
            ..station_12()
        };

        let fired = matched(&page(), &[station_12(), station_4, elsewhere]);

        assert_eq!(
            fired.iter().map(|m| m.profile_id).collect::<Vec<_>>(),
            vec![7, 9]
        );
        assert!(matched(&page(), &[]).is_empty());
    }

    /// The steps round-trip through the column, so what an Operator typed is
    /// what the matcher asks — including the `camelCase` the form sends.
    #[test]
    fn a_step_list_round_trips_through_the_column() {
        let steps = station_12().steps;

        assert_eq!(steps_of(&steps_json(&steps)), steps);
        assert_eq!(
            steps_json(&steps),
            r#"[{"hz":1122.5,"minMs":800},{"hz":1465.6,"minMs":2000}]"#
        );
    }

    /// A stored row is read into the value the matcher takes.
    #[test]
    fn a_stored_row_reads_back_as_the_profile_it_was_written_as() {
        let profile = station_12();

        assert_eq!(
            Profile::from_row(&crate::db::entities::tone_profile::Model {
                id: 7,
                talkgroup_id: 3,
                label: String::from("Station 12"),
                tolerance_pct: DEFAULT_TOLERANCE_PCT,
                gap_max_ms: DEFAULT_GAP_MAX_MS,
                steps: steps_json(&profile.steps),
                disabled: false,
                created_at_ms: 0,
            }),
            profile
        );
    }

    proptest! {
        /// **The tolerance is a percentage, and that is the whole point.** ±2%
        /// is ±6 Hz at 300 and ±49 Hz at 2468; a flat window wide enough for the
        /// top of the band would merge neighbouring tones at the bottom of it.
        /// So the boundary has to hold at every frequency, not at one.
        #[test]
        fn the_tolerance_holds_at_every_frequency_in_the_band(
            hz in 300.0f64..3_000.0,
            tolerance in 0.5f64..5.0,
            drift in -1.5f64..1.5,
        ) {
            let profile = Profile {
                tolerance_pct: tolerance,
                steps: vec![Step { hz, min_ms: 500 }],
                ..station_12()
            };
            let measured = hz * (1.0 + drift * tolerance / 100.0);

            prop_assert_eq!(
                matches(&[run(measured, 0, 1000)], &profile).is_some(),
                drift.abs() <= 1.0,
                "{} Hz measured as {} at ±{}%",
                hz,
                measured,
                tolerance
            );
        }
    }
}
