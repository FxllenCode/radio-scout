//! Turning audio into **quiet spans** — the whole of Catch-up's signal
//! processing (#59, spec US 23), and pure.
//!
//! # What a quiet span is, and what it deliberately is not
//!
//! Not digital silence, and not "below a level". A **Quiet span** (CONTEXT.md)
//! is a stretch of a Call where nobody is talking, measured *relative to how
//! loud this Call's speech is* — because the level a Call arrives at depends on
//! the recorder's gain, and enhancement, which would normalize it, is off by
//! default and off the ingest path anyway. That is [`crate::tone::detect`]'s
//! reasoning, and for its reason: a quiet recording must be trimmed exactly
//! like a loud one.
//!
//! The thing worth trimming is not the squelch hiss between syllables. A Trunk
//! Recorder call file spans the whole grant, so it routinely contains several
//! keyups with seconds of hang time between them, plus a tail after the last
//! word. **Those** are what a Listener draining a backlog is sitting through,
//! and they are why the floor is [`MIN_SPAN_MS`] rather than a frame.
//!
//! # Why a moving median, and not the loudest frame or a percentile
//!
//! The threshold is a fraction of how loud this Call's *speech* is, so
//! something has to stand for that. Two obvious answers are both wrong, and
//! both fail silently:
//!
//! - **The loudest frame.** One squelch pop is louder than every word in the
//!   Call. Referencing the peak puts the threshold above all of the speech and
//!   reports the whole Call as quiet — a Catch-up that skips a Call entirely.
//! - **A high percentile.** It survives the pop, and then fails on the Call
//!   this feature exists for: a Trunk Recorder file holding one keyup and nine
//!   seconds of hang time is *mostly* gap, so even a 90th percentile lands
//!   inside the silence and the Call reports no gaps at all.
//!
//! So the reference is the peak of a **moving median** of frame loudness. A
//! median discards anything shorter than half its window, which is exactly the
//! difference between a click and a syllable — and taking its peak asks only
//! that the Call contain a hundred milliseconds of speech *somewhere*, which is
//! the weakest thing that can be assumed about a Call worth playing.
//!
use super::{MIN_SPAN_MS, Span};

/// How much audio is measured at once, and the resolution of every `_ms` here.
///
/// 20 ms is a syllable's rise time: long enough that one glottal pulse does not
/// read as a gap, short enough that the edge of a span lands within a syllable
/// of the truth.
const FRAME_MS: i64 = 20;

/// How loud a frame may be, against [`peak`], and still be quiet.
///
/// 8% of amplitude — about −22 dB below the Call's own speech — and the number
/// is deliberately on the generous side, because the two failure directions are
/// not symmetric and only one of them is guarded.
///
/// Too strict misses real gaps and nothing catches it: an analogue channel
/// recorded with the squelch open leaves a noise floor perhaps 25 dB down, not
/// 40, so a threshold at −26 dB finds nothing between two keyups and Catch-up
/// quietly stops trimming. Too generous would trim inside speech — except that
/// it cannot, because [`super::MIN_SPAN_MS`] plus two [`GUARD_MS`] means a
/// stretch has to stay under this for **1.44 seconds** to be reported, and
/// nobody's syllable, fricative or inter-word pause is that long. A speaker who
/// really does stop for a second and a half has left a gap worth skipping.
const QUIET_RATIO: f32 = 0.08;

/// A floor beneath the relative one, so a Call that is *entirely* silence does
/// not have its own dither promoted to speech by being the loudest thing in it.
/// [`crate::tone::detect`]'s constant, for its reason.
const ABSOLUTE_FLOOR: f32 = 1e-4;

/// How many frames the median smooths over — 100 ms.
///
/// The whole of what makes this robust, in one number. Longer than any click,
/// squelch pop or dropout, and shorter than the shortest syllable anybody
/// utters: a median throws away what occupies less than half its window, so a
/// pop cannot raise the reference and a glottal stop cannot split a span.
///
/// Odd, so a median is one element and not two averaged.
const MEDIAN_FRAMES: usize = 5;

/// How much of a quiet span is given back to the speech on either side of it.
///
/// A seek lands on a sample, and a threshold crossing is not the moment a word
/// began — a plosive's onset is quieter than the vowel behind it. 120 ms is one
/// syllable of insurance, paid at both ends of every span that has speech
/// beside it, so that trimming never clips a word. Speech intact is the
/// acceptance criterion; a span that has to be shortened to keep it is cheap.
const GUARD_MS: i64 = 120;

/// Where in this Call nobody is talking.
///
/// Spans are in order, disjoint, and each is at least [`MIN_SPAN_MS`]
/// long *after* its guard has been paid — so what comes back is only the gaps a
/// Listener would notice sitting through, never the pauses inside a sentence.
///
/// An empty answer is the ordinary one: most Calls are somebody saying one
/// thing, and have nothing worth trimming.
pub fn spans(samples: &[f32], rate: u32) -> Vec<Span> {
    if rate == 0 || samples.is_empty() {
        return Vec::new();
    }
    let frame = ((rate as i64 * FRAME_MS) / 1_000).max(1) as usize;
    let loudness: Vec<f32> = samples.chunks(frame).map(rms).collect();
    let envelope = smoothed(&loudness);

    // `reference` is the peak of the envelope, so this is at most the peak and
    // the floor decides only for a Call that holds no speech at all.
    let threshold = (peak(&envelope) * QUIET_RATIO).max(ABSOLUTE_FLOOR);
    let total_ms = (samples.len() as i64 * 1_000) / rate as i64;
    let at = |index: usize| ((index * frame) as i64 * 1_000) / rate as i64;

    let mut found = Vec::new();
    let mut run: Option<usize> = None;
    // Classified on the **envelope**, not the raw frame: a click inside a gap
    // would otherwise split one span into two shorter ones, both of which the
    // floor might then discard — silence broken by a pop is still silence.
    for (index, &level) in envelope.iter().enumerate() {
        match (level <= threshold, run) {
            (true, None) => run = Some(index),
            (false, Some(from)) => {
                push(&mut found, at(from), at(index), total_ms);
                run = None;
            }
            _ => {}
        }
    }
    if let Some(from) = run {
        push(&mut found, at(from), total_ms, total_ms);
    }
    found
}

/// Give a raw span its guard back and keep it only if it is still worth a seek.
///
/// **The guard is paid only where there is speech to protect.** A span that
/// opens the Call has nothing before it and a span that closes one has nothing
/// after it — that trailing span is the squelch tail, which is the single
/// commonest thing worth trimming, and shortening it by a syllable that does
/// not exist would be paying for nothing.
///
/// **A span covering the whole Call is never reported.** Nothing distinguishes
/// a Call of digital silence from one recorded at a gain this cannot measure,
/// and the two want opposite answers — so the honest reading is that a Call with
/// nothing to trim has nothing to trim. Skipping a Call outright is a decision a
/// **Listener** makes from the queue sheet (#58); Catch-up shortens what they
/// hear and never decides they hear none of it.
fn push(found: &mut Vec<Span>, start_ms: i64, end_ms: i64, total_ms: i64) {
    if start_ms == 0 && end_ms >= total_ms {
        return;
    }
    let start_ms = if start_ms == 0 {
        0
    } else {
        start_ms + GUARD_MS
    };
    let end_ms = if end_ms >= total_ms {
        total_ms
    } else {
        end_ms - GUARD_MS
    };
    if end_ms - start_ms >= MIN_SPAN_MS {
        found.push(Span { start_ms, end_ms });
    }
}

/// Frame loudness with everything shorter than [`MEDIAN_FRAMES`] taken out of
/// it — see the module header for why a median and not a mean.
///
/// The window shortens at the two ends rather than being padded: padding with
/// zeros would drag the first and last frames toward silence, which is where a
/// Call's leading and trailing gaps are and exactly where the answer must not be
/// assumed.
fn smoothed(loudness: &[f32]) -> Vec<f32> {
    let reach = MEDIAN_FRAMES / 2;
    let mut window = Vec::with_capacity(MEDIAN_FRAMES);
    (0..loudness.len())
        .map(|index| {
            let from = index.saturating_sub(reach);
            let to = (index + reach + 1).min(loudness.len());
            window.clear();
            window.extend_from_slice(&loudness[from..to]);
            window.sort_by(f32::total_cmp);
            window[window.len() / 2]
        })
        .collect()
}

/// How loud this Call's speech is: the loudest the envelope ever gets.
///
/// The peak of a *median* is not the peak of the Call — see the module header.
/// It asks only that the Call hold a hundred milliseconds of speech somewhere,
/// which is the weakest assumption that can be made about a Call worth playing.
fn peak(envelope: &[f32]) -> f32 {
    envelope.iter().copied().fold(0.0, f32::max)
}

/// Root-mean-square of one frame — amplitude, so [`QUIET_RATIO`] reads as a
/// fraction of the speech rather than of its power.
fn rms(frame: &[f32]) -> f32 {
    let sum: f32 = frame.iter().map(|sample| sample * sample).sum();
    (sum / frame.len() as f32).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use rstest::rstest;

    /// The rate a P25 recorder writes, and therefore the rate most of this is
    /// ever asked to work at.
    const RATE: u32 = 8_000;

    /// A deterministic noise source.
    ///
    /// Speech is not a sinusoid and the detector does not care what it is — it
    /// measures energy — so band-agnostic noise is the honest stand-in: it has
    /// the one property that matters (a level held for a while) and none of the
    /// structure that would let an assertion pass for the wrong reason.
    ///
    /// Hand-rolled rather than a dependency because a test that cannot be
    /// replayed exactly is a test that reports a different answer on the day it
    /// fails.
    struct Noise(u32);

    impl Noise {
        fn next(&mut self) -> f32 {
            // Numerical Recipes' LCG. Any full-period generator would do; this
            // one is four characters of constant and no crate.
            self.0 = self.0.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (self.0 >> 8) as f32 / (1 << 23) as f32 - 1.0
        }
    }

    fn samples(ms: usize, amplitude: f32, noise: &mut Noise) -> Vec<f32> {
        (0..(RATE as usize * ms) / 1_000)
            .map(|_| noise.next() * amplitude)
            .collect()
    }

    /// Somebody talking.
    fn speech(ms: usize, noise: &mut Noise) -> Vec<f32> {
        samples(ms, 0.3, noise)
    }

    /// An open channel with nobody on it — the hiss a recorder writes between
    /// keyups. Deliberately **not** digital silence: a real gap has a noise
    /// floor, and a detector that only found exact zeroes would find nothing in
    /// any Call a radio ever produced.
    fn gap(ms: usize, noise: &mut Noise) -> Vec<f32> {
        samples(ms, 0.002, noise)
    }

    /// A squelch pop: one very loud click, shorter than a syllable.
    fn pop(noise: &mut Noise) -> Vec<f32> {
        samples(10, 4.0, noise)
    }

    fn call(parts: Vec<Vec<f32>>) -> Vec<f32> {
        parts.concat()
    }

    /// How far a reported edge may be from where it was synthesized.
    ///
    /// One frame of analysis, one of median smoothing, and the guard the
    /// detector deliberately pays back to the speech — asserted as a tolerance
    /// rather than pinned, because pinning the exact edge would fail the first
    /// time [`GUARD_MS`] is retuned and would say nothing about correctness.
    const SLACK_MS: i64 = GUARD_MS + 3 * FRAME_MS;

    fn near(found: i64, expected: i64) -> bool {
        (found - expected).abs() <= SLACK_MS
    }

    /// **The Call this feature exists for**: a Trunk Recorder file spanning a
    /// grant, holding two keyups with a long hang time between them and a tail
    /// after the last word.
    #[test]
    fn the_gap_between_two_keyups_is_found() {
        let mut noise = Noise(7);
        let audio = call(vec![
            speech(1_500, &mut noise),
            gap(3_000, &mut noise),
            speech(1_500, &mut noise),
            gap(2_000, &mut noise),
        ]);

        let found = spans(&audio, RATE);

        assert_eq!(found.len(), 2, "{found:?}");
        assert!(near(found[0].start_ms, 1_500 + GUARD_MS), "{found:?}");
        assert!(near(found[0].end_ms, 4_500 - GUARD_MS), "{found:?}");
        // The trailing gap runs to the end of the Call, and is **not** given a
        // guard on its right edge: there is no speech after it to protect, and
        // that tail is the single commonest thing worth trimming.
        assert!(near(found[1].start_ms, 6_000 + GUARD_MS), "{found:?}");
        assert_eq!(found[1].end_ms, 8_000, "the tail runs to the end");
    }

    /// **A Call that is mostly gap still reports it.** One keyup and nine
    /// seconds of hang time is what a kerchunk on a busy talkgroup looks like,
    /// and it is precisely the shape a percentile reference gets wrong: at any
    /// percentile below ~90 the reference lands *inside* the silence and the
    /// Call reports no gaps at all.
    #[test]
    fn a_call_that_is_mostly_gap_is_mostly_gap() {
        let mut noise = Noise(11);
        let audio = call(vec![speech(800, &mut noise), gap(9_000, &mut noise)]);

        let found = spans(&audio, RATE);

        assert_eq!(found.len(), 1, "{found:?}");
        assert!(near(found[0].start_ms, 800 + GUARD_MS), "{found:?}");
        assert_eq!(found[0].end_ms, 9_800);
    }

    /// **One squelch pop does not make a Call silent.** Referencing the loudest
    /// frame would put the threshold above every word and report the whole Call
    /// as skippable — which is Catch-up skipping a Call, not a gap in one.
    #[test]
    fn a_squelch_pop_does_not_silence_the_call() {
        let mut noise = Noise(13);
        let audio = call(vec![
            pop(&mut noise),
            speech(2_000, &mut noise),
            gap(2_500, &mut noise),
            speech(2_000, &mut noise),
        ]);

        let found = spans(&audio, RATE);

        assert_eq!(found.len(), 1, "only the real gap: {found:?}");
        assert!(near(found[0].start_ms, 2_010 + GUARD_MS), "{found:?}");
    }

    /// **A pop *inside* a gap does not break it in two.** Two 1.4-second halves
    /// would each fall under the floor and the whole gap would go unreported —
    /// which is why frames are classified on the smoothed envelope rather than
    /// on themselves.
    #[test]
    fn a_pop_inside_a_gap_does_not_break_it() {
        let mut noise = Noise(17);
        let audio = call(vec![
            speech(1_000, &mut noise),
            gap(1_400, &mut noise),
            pop(&mut noise),
            gap(1_400, &mut noise),
            speech(1_000, &mut noise),
        ]);

        let found = spans(&audio, RATE);

        assert_eq!(found.len(), 1, "one gap, interrupted: {found:?}");
        assert!(found[0].end_ms - found[0].start_ms >= 2_000, "{found:?}");
    }

    /// **The floor is the whole reason this is worth a seek.** The pauses inside
    /// a sentence are not gaps a Listener would notice being skipped, and a seek
    /// costs the element a re-buffer — so anything under [`MIN_SPAN_MS`],
    /// *after* the guard is paid, is not reported at all.
    #[rstest]
    // Comfortably over the floor once both guards are paid.
    #[case(2_000, 1)]
    // Exactly the floor plus both guards: the last length that survives.
    #[case(MIN_SPAN_MS as usize + 2 * GUARD_MS as usize, 1)]
    // A breath. Under the floor before the guard is even considered.
    #[case(600, 0)]
    // Over the floor, but not by both guards — the case a detector that forgot
    // to pay them back would wrongly report.
    #[case(MIN_SPAN_MS as usize + 20, 0)]
    fn only_a_gap_worth_seeking_over_is_reported(#[case] gap_ms: usize, #[case] expected: usize) {
        let mut noise = Noise(19);
        let audio = call(vec![
            speech(1_200, &mut noise),
            gap(gap_ms, &mut noise),
            speech(1_200, &mut noise),
        ]);

        assert_eq!(spans(&audio, RATE).len(), expected);
    }

    /// Somebody saying one thing has nothing to trim, which is most Calls —
    /// and is what keeps this key off most of the wire.
    #[test]
    fn continuous_speech_has_no_gaps() {
        let mut noise = Noise(23);

        assert_eq!(spans(&speech(6_000, &mut noise), RATE), Vec::new());
    }

    /// **A Call with nothing in it has nothing to trim.** Digital silence and a
    /// recording made at a gain this cannot measure are indistinguishable from
    /// here, and they want opposite answers — so a span covering the whole Call
    /// is never reported, whichever this is. Catch-up shortens what a Listener
    /// hears; deciding they hear none of a Call is theirs, from the queue sheet.
    ///
    /// Both arms, because they arrive by different routes: an open squelch is
    /// above [`ABSOLUTE_FLOOR`] and reports nothing because it is its own
    /// reference, and true silence is below it and reports nothing because of
    /// the rule.
    #[rstest]
    #[case::open_squelch(0.002)]
    #[case::digital_silence(0.0)]
    fn a_call_with_nothing_in_it_has_nothing_to_trim(#[case] amplitude: f32) {
        let mut noise = Noise(29);

        let audio = samples(5_000, amplitude, &mut noise);

        assert_eq!(spans(&audio, RATE), Vec::new());
    }

    /// **A leading gap keeps its left edge.** There is no speech before the
    /// start of a Call to protect, so paying a guard there would leave 120 ms of
    /// dead air at the head of every Call a recorder opened early.
    #[test]
    fn a_gap_that_opens_the_call_starts_at_zero() {
        let mut noise = Noise(31);
        let audio = call(vec![gap(2_500, &mut noise), speech(2_000, &mut noise)]);

        let found = spans(&audio, RATE);

        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].start_ms, 0);
        assert!(near(found[0].end_ms, 2_500 - GUARD_MS), "{found:?}");
    }

    /// Nothing to look at is no spans, rather than a panic on an empty slice or
    /// a division by a rate of zero. Both are reachable: a decode can hand back
    /// an empty buffer.
    #[rstest]
    #[case(Vec::new(), RATE)]
    #[case(vec![0.0; 100], 0)]
    fn audio_that_says_nothing_reports_nothing(#[case] audio: Vec<f32>, #[case] rate: u32) {
        assert_eq!(spans(&audio, rate), Vec::new());
    }

    proptest! {
        /// **Whatever the audio, the answer is a playable plan.** Every span is
        /// inside the Call, none is shorter than the floor, and no two overlap
        /// or touch — because a client seeks to `end_ms` and a span that ran
        /// past the Call, backwards, or into its neighbour would seek a
        /// Listener off the end of the audio or into a loop.
        #[test]
        fn every_span_is_one_a_player_could_seek_over(
            layout in proptest::collection::vec((0usize..2, 40usize..2_000), 1..12),
            seed in 1u32..10_000,
        ) {
            let mut noise = Noise(seed);
            let audio = call(
                layout
                    .iter()
                    .map(|(kind, ms)| match kind {
                        0 => speech(*ms, &mut noise),
                        _ => gap(*ms, &mut noise),
                    })
                    .collect(),
            );
            let total_ms = (audio.len() as i64 * 1_000) / RATE as i64;

            let found = spans(&audio, RATE);

            let mut previous = -1;
            for span in &found {
                prop_assert!(span.start_ms >= 0, "{found:?}");
                prop_assert!(span.end_ms <= total_ms, "{found:?} in {total_ms}ms");
                prop_assert!(span.end_ms - span.start_ms >= MIN_SPAN_MS, "{found:?}");
                prop_assert!(span.start_ms > previous, "{found:?}");
                previous = span.end_ms;
            }
        }
    }
}
