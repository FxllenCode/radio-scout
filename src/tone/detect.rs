//! Turning audio into **steady-tone runs** — the whole of tone-out detection's
//! signal processing (#55, spec US 20), and pure.
//!
//! # Why a Run, and not "does this Call match this profile?"
//!
//! A Talkgroup may carry twenty **Tone profiles** — a county dispatch channel
//! pages twenty stations off it. Asking the audio a question per profile would
//! decode and transform the same Call twenty times, on the hardware least able
//! to afford it. So the audio is asked exactly one question, once:
//!
//! > where in this Call is a steady tone, what frequency, and for how long?
//!
//! The answer is a handful of [`Run`]s — a value small enough to print — and
//! **matching a profile against it is [`super::matches`], which touches no
//! audio at all**. That is what lets every rule about ordering, tolerance and
//! gaps be a test that constructs three structs, and confines the part needing
//! synthesized audio to the question of whether a tone is *found*.
//!
//! # What it looks for
//!
//! Motorola Quick Call II and its relatives are **pure sinusoids held for a
//! long time** — 288 Hz to 2468 Hz, a second or more each. Two things
//! distinguish that from everything else on a scanner, and both are checked:
//!
//! - **Purity.** One frequency holds nearly all of the energy in the voice
//!   band. Speech never does: a vowel's energy is spread across a fundamental
//!   and its harmonics, so its strongest peak holds a fraction of the band.
//! - **Persistence at one frequency.** A voice sliding through a pitch crosses
//!   a tone's frequency for tens of milliseconds; a page-out sits on it for a
//!   second. The duration is what a profile asserts, and it is what makes
//!   "somebody sang the note" a non-problem.
//!
//! Deliberately **not** an energy threshold in absolute terms: the level a Call
//! arrives at depends on the recorder's gain, and enhancement (#20) — which
//! would normalize it — is off by default and, being off the ingest path, has
//! not run yet anyway. The floor here is relative to the loudest part of the
//! Call, so a quiet recording is detected exactly like a loud one.
//!
//! No speech recognition of any kind is involved, and none may be
//! ([ADR-0013](../../docs/adr/0013-no-transcription.md)): this asks what
//! *frequency* is present, never what was said.

use std::sync::Arc;

use realfft::{RealFftPlanner, RealToComplex};

/// A stretch of a Call holding one steady tone.
///
/// Times are milliseconds from the start of the Call, so a [`super::ToneMatch`]
/// can say *where* a page-out was — which is the difference between "this Call
/// contains a page" and "the page is 1.8 seconds in, after the dispatcher's
/// preamble".
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Run {
    /// The tone, in Hz — the median of the frames that make up the run, so one
    /// frame knocked off by a squelch tail cannot move it.
    pub hz: f64,
    pub start_ms: i64,
    pub duration_ms: i64,
}

impl Run {
    /// Where this run ends. Read by the matcher, which asks whether the next
    /// tone began soon enough after this one stopped.
    ///
    /// **Two adjacent runs may overlap by up to one analysis window**, and that
    /// is not an error: the window straddling the moment one tone becomes the
    /// next belongs to whichever of them fills more of it, so both runs claim
    /// part of it. It is why [`super::matches`] measures its gap as a signed
    /// difference and asks only that it not be *too large* — a page-out keyed
    /// back to back reads as a small negative gap, and treating that as a
    /// broken sequence would fail every real Quick Call there is.
    pub fn end_ms(self) -> i64 {
        self.start_ms + self.duration_ms
    }
}

/// How long a frame of audio is analysed at once.
///
/// 50 ms is the trade: long enough that a 300 Hz tone completes fifteen cycles
/// inside it (so its peak is sharp), short enough that the shortest tone worth
/// asserting on — a few hundred milliseconds — is still several frames.
const WINDOW_SECS: f64 = 0.050;

/// How far the window moves between frames. The resolution of every `_ms` this
/// module reports, and the size of a dropout it can step over.
const HOP_SECS: f64 = 0.010;

/// The band a paging tone can be in. Quick Call II runs 288.5–2468.2 Hz; the
/// margin above it covers the tone sets that go higher, and the floor keeps
/// mains hum and its harmonics out.
pub const BAND_LOW_HZ: f64 = 200.0;
pub const BAND_HIGH_HZ: f64 = 3_300.0;

/// How much of the band's energy the peak (and its two neighbours) must hold
/// for a frame to count as a tone.
///
/// A pure sinusoid under a Hann window puts essentially all of its energy in
/// three bins, so this is really a signal-to-noise floor: 0.5 is reached at
/// roughly 0 dB SNR, which is a page-out you can barely hear. Speech does not
/// approach it — a vowel's strongest three bins hold a fraction of the band,
/// because the rest is in the harmonics that make it a vowel.
const MIN_PURITY: f64 = 0.5;

/// How quiet a frame may be, relative to the loudest frame in the Call, and
/// still be looked at. Below this is the gap between transmissions, where noise
/// alone can look arbitrarily pure.
const SILENCE_FLOOR: f64 = 0.05;

/// Absolute floor beneath the relative one, so a Call that is *entirely*
/// digital silence does not have its own noise promoted to a tone by being the
/// loudest thing in it.
const ABSOLUTE_FLOOR: f64 = 1e-4;

/// How far a frame's frequency may drift from a run's and still belong to it.
///
/// Tighter than any tolerance a profile would use: this is asking whether two
/// adjacent frames are the *same* tone, not whether a tone is the one an
/// Operator configured.
const RUN_DRIFT: f64 = 0.015;

/// How many non-tonal frames a run survives. A page-out crossing a squelch
/// crackle, or a recorder dropping a packet, loses a frame or two in the
/// middle; without this the run splits and its halves are each too short to
/// assert on — the failure that reads as "detection is unreliable".
const RUN_GRACE_FRAMES: usize = 3;

/// Every steady tone in this audio, in the order they occur.
///
/// `samples` is mono at `rate`. Audio too short to fill one analysis window has
/// no runs, which is the right answer rather than an error: a 30 ms Call is a
/// kerchunk.
pub fn runs(samples: &[f32], rate: u32) -> Vec<Run> {
    let window = (rate as f64 * WINDOW_SECS).round() as usize;
    let hop = (rate as f64 * HOP_SECS).round() as usize;
    if rate == 0 || window == 0 || hop == 0 || samples.len() < window {
        return Vec::new();
    }
    let frames = analyse(samples, rate, window, hop);
    group(&frames, hop, window, rate)
}

/// What one analysis window turned out to be.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Frame {
    /// The dominant frequency, or `None` where the window held no tone — too
    /// quiet, or too much of a mess.
    hz: Option<f64>,
}

/// Walk the audio a window at a time, asking each window for its tone.
fn analyse(samples: &[f32], rate: u32, window: usize, hop: usize) -> Vec<Frame> {
    let fft_len = window.next_power_of_two();
    let mut planner = RealFftPlanner::<f32>::new();
    let fft = planner.plan_fft_forward(fft_len);
    let taper = hann(window);

    // The floor is relative to this Call, so a quiet recorder is read the same
    // as a loud one — see the module note.
    let levels: Vec<f64> = samples.windows(window).step_by(hop).map(rms).collect();
    let loudest = levels.iter().copied().fold(0.0, f64::max);
    let floor = (loudest * SILENCE_FLOOR).max(ABSOLUTE_FLOOR);

    let mut scratch = vec![0f32; fft_len];
    let mut spectrum = fft.make_output_vec();
    samples
        .windows(window)
        .step_by(hop)
        .zip(&levels)
        .map(|(frame, &level)| {
            if level < floor {
                return Frame { hz: None };
            }
            Frame {
                hz: peak(
                    &fft,
                    frame,
                    &taper,
                    &mut scratch,
                    &mut spectrum,
                    fft_len,
                    rate,
                ),
            }
        })
        .collect()
}

/// The dominant frequency of one window, or `None` if it holds no single tone.
///
/// Zero-padded to a power of two so the transform is fast, and the peak is
/// interpolated across its neighbours — a raw bin index at these sizes is
/// 15 Hz wide, which is coarser than the ±1% a tone set is specified to.
fn peak(
    fft: &Arc<dyn RealToComplex<f32>>,
    frame: &[f32],
    taper: &[f32],
    scratch: &mut [f32],
    spectrum: &mut [realfft::num_complex::Complex<f32>],
    fft_len: usize,
    rate: u32,
) -> Option<f64> {
    scratch.fill(0.0);
    for ((slot, sample), window) in scratch.iter_mut().zip(frame).zip(taper) {
        *slot = sample * window;
    }
    fft.process(scratch, spectrum).ok()?;

    let bin_hz = rate as f64 / fft_len as f64;
    let low = (BAND_LOW_HZ / bin_hz).ceil() as usize;
    let high = ((BAND_HIGH_HZ / bin_hz).floor() as usize).min(spectrum.len().saturating_sub(2));
    if low + 1 >= high {
        return None;
    }

    let power: Vec<f64> = spectrum.iter().map(|c| c.norm_sqr() as f64).collect();
    let total: f64 = power[low..=high].iter().sum();
    let (index, _) = power[low..=high]
        .iter()
        .enumerate()
        .max_by(|(_, a), (_, b)| a.total_cmp(b))?;
    let index = low + index;

    // The peak plus the two bins a windowed sinusoid always leaks into. Against
    // the whole band, this is how much of the sound *is* the one tone.
    //
    // **The floor under the divisor is load-bearing, not defensive.** A band
    // holding no energy at all would divide `0.0` by `0.0` to `NaN`, and
    // `NaN < 0.5` is *false* — so without it a frame with nothing in it would
    // pass, carrying whatever frequency the first bin happened to be. Written
    // this way rather than as a separate zero-check because there is then no
    // second branch to keep in step, and no arm no audio can reach.
    let purity =
        (power[index - 1] + power[index] + power[index + 1]) / total.max(f64::MIN_POSITIVE);
    if purity < MIN_PURITY {
        return None;
    }
    Some((index as f64 + interpolate(&power, index)) * bin_hz)
}

/// Where the true peak sits between bins — the vertex of the parabola through
/// the peak and its neighbours, in log magnitude.
///
/// Returns a fraction of a bin in `-0.5..=0.5`. Zero when the three are equal,
/// which is also what a divide-by-zero would have to mean.
fn interpolate(power: &[f64], index: usize) -> f64 {
    let db = |p: f64| (p.max(f64::MIN_POSITIVE)).log10();
    let (left, mid, right) = (db(power[index - 1]), db(power[index]), db(power[index + 1]));
    let denominator = left - 2.0 * mid + right;
    match denominator == 0.0 {
        true => 0.0,
        false => (0.5 * (left - right) / denominator).clamp(-0.5, 0.5),
    }
}

/// Collapse a run of frames holding the same tone into one [`Run`].
fn group(frames: &[Frame], hop: usize, window: usize, rate: u32) -> Vec<Run> {
    let ms = |samples: usize| (samples as f64 * 1000.0 / rate as f64).round() as i64;
    let mut runs = Vec::new();
    let mut current: Option<(usize, usize, Vec<f64>)> = None;
    let mut grace = 0usize;

    for (index, frame) in frames.iter().enumerate() {
        match (frame.hz, &mut current) {
            // A tone that continues the run in hand.
            (Some(hz), Some((_, last, seen)))
                if (hz - median(seen)).abs() <= median(seen) * RUN_DRIFT =>
            {
                *last = index;
                seen.push(hz);
                grace = 0;
            }
            // A tone, but a different one — or the first one seen.
            (Some(hz), slot) => {
                if let Some(run) = slot.take().map(|run| finish(run, hop, window, ms)) {
                    runs.push(run);
                }
                *slot = Some((index, index, vec![hz]));
                grace = 0;
            }
            // A frame holding nothing, inside a run: survivable, up to a point.
            (None, Some(_)) if grace < RUN_GRACE_FRAMES => grace += 1,
            (None, slot) => {
                if let Some(run) = slot.take().map(|run| finish(run, hop, window, ms)) {
                    runs.push(run);
                }
                grace = 0;
            }
        }
    }
    if let Some(run) = current.map(|run| finish(run, hop, window, ms)) {
        runs.push(run);
    }
    runs
}

/// One accumulated run, as a [`Run`].
///
/// A run of frames `first..=last` covers from the start of the first window to
/// the end of the last, which is `(last - first) * hop + window` samples — the
/// window's own length is why a tone reads as its real duration rather than as
/// the distance between the frames that saw it.
fn finish(
    (first, last, seen): (usize, usize, Vec<f64>),
    hop: usize,
    window: usize,
    ms: impl Fn(usize) -> i64,
) -> Run {
    Run {
        hz: median(&seen),
        start_ms: ms(first * hop),
        duration_ms: ms((last - first) * hop + window),
    }
}

/// The middle value — robust to the odd frame a squelch tail knocks sideways,
/// where a mean is not.
fn median(values: &[f64]) -> f64 {
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    match sorted.len() {
        0 => 0.0,
        n => sorted[n / 2],
    }
}

fn rms(frame: &[f32]) -> f64 {
    let sum: f64 = frame.iter().map(|s| (*s as f64) * (*s as f64)).sum();
    (sum / frame.len() as f64).sqrt()
}

/// A Hann window — the taper that makes a sinusoid's peak three bins wide
/// instead of smeared across the whole spectrum, which is what
/// [`peak`]'s purity measure is counting on.
fn hann(len: usize) -> Vec<f32> {
    (0..len)
        .map(|n| {
            let phase = std::f32::consts::TAU * n as f32 / len as f32;
            0.5 - 0.5 * phase.cos()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use rstest::rstest;

    /// The rate a P25 recorder writes, and therefore the rate most of this is
    /// ever asked to work at.
    const RATE: u32 = 8_000;

    /// A steady sinusoid — one **tone**, as a dispatch console generates it.
    ///
    /// Given an attack and decay, because a real console keys the tone through
    /// an audio path with a rise time, and a hard edge is a click whose energy
    /// is spread across the whole band. A synthesizer that omitted it would be
    /// testing against a signal cleaner than anything that exists.
    fn tone(hz: f64, ms: usize, amplitude: f32) -> Vec<f32> {
        let len = ms * RATE as usize / 1000;
        let ramp = (RATE as usize / 200).min(len / 2).max(1);
        (0..len)
            .map(|n| {
                let envelope = (n.min(len - n - 1) as f32 / ramp as f32).min(1.0);
                let phase = std::f64::consts::TAU * hz * n as f64 / RATE as f64;
                amplitude * envelope * phase.sin() as f32
            })
            .collect()
    }

    fn silence(ms: usize) -> Vec<f32> {
        vec![0.0; ms * RATE as usize / 1000]
    }

    /// Band-limited-ish hiss, deterministically — a seeded LCG rather than a
    /// random number generator, so a failure is a failure and not a Tuesday.
    fn noise(ms: usize, amplitude: f32) -> Vec<f32> {
        let mut state = 0x2545_F491_4F6C_DD1Du64;
        (0..ms * RATE as usize / 1000)
            .map(|_| {
                state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
                amplitude * ((state >> 40) as f32 / 8388608.0 - 1.0)
            })
            .collect()
    }

    fn mix(a: &[f32], b: &[f32]) -> Vec<f32> {
        a.iter()
            .zip(b)
            .map(|(x, y)| x + y)
            .chain(a.iter().skip(b.len()).copied())
            .collect()
    }

    /// Speech, near enough: a harmonic stack that *moves*, which is the thing a
    /// paging tone never does. Vowels are the hard negative — they are the only
    /// part of speech that holds a pitch at all.
    fn vowel(ms: usize, f0: f64) -> Vec<f32> {
        let len = ms * RATE as usize / 1000;
        (0..len)
            .map(|n| {
                let t = n as f64 / RATE as f64;
                // Vibrato: a human holding a note wobbles by a few percent, and
                // a human *talking* slides much further than that.
                let f = f0 * (1.0 + 0.06 * (std::f64::consts::TAU * 5.0 * t).sin());
                (1..=12)
                    .map(|h| {
                        let amp = 1.0 / h as f64;
                        amp * (std::f64::consts::TAU * f * h as f64 * t).sin()
                    })
                    .sum::<f64>() as f32
                    * 0.2
            })
            .collect()
    }

    /// A Motorola Quick Call II page: tone A for a second, tone B for three.
    fn quick_call(a: f64, b: f64) -> Vec<f32> {
        [tone(a, 1000, 0.5), tone(b, 3000, 0.5)].concat()
    }

    /// The headline: a two-tone page comes back as two runs, in order, at the
    /// frequencies and lengths it was generated with.
    #[test]
    fn a_two_tone_page_is_two_runs_at_the_tones_it_was_paged_with() {
        let found = runs(&quick_call(1122.5, 1465.6), RATE);

        assert_eq!(found.len(), 2, "{found:?}");
        assert!((found[0].hz - 1122.5).abs() < 11.0, "{:?}", found[0]);
        assert!((found[1].hz - 1465.6).abs() < 15.0, "{:?}", found[1]);
        assert!(found[0].duration_ms.abs_diff(1000) < 100, "{:?}", found[0]);
        assert!(found[1].duration_ms.abs_diff(3000) < 100, "{:?}", found[1]);
        assert!(found[0].start_ms < found[1].start_ms, "in order: {found:?}");
        // Keyed back to back, so the second tone starts where the first stops —
        // give or take the window that straddles the moment between them, which
        // is why this is a *magnitude* and why the matcher's gap is signed.
        assert!(
            (found[1].start_ms - found[0].end_ms()).abs() <= 50,
            "{found:?}"
        );
    }

    /// Where the page is, not merely that it happened — which is the whole
    /// reason a [`Run`] carries a time at all.
    #[test]
    fn a_run_says_where_in_the_call_the_tone_started() {
        let audio = [silence(1500), quick_call(1122.5, 1465.6)].concat();

        let found = runs(&audio, RATE);

        assert_eq!(found.len(), 2, "{found:?}");
        assert!(found[0].start_ms.abs_diff(1500) < 100, "{:?}", found[0]);
    }

    /// **Nothing that isn't a held tone is one at all.** Each of these is a real
    /// thing a scanner records, and every one of them used to be somebody's
    /// false page.
    ///
    /// Asserted as *no runs at all* rather than *no long runs*, which is what
    /// makes the silence floor and the purity threshold provable: a looser form
    /// let either be mutated away without a test noticing, because anything that
    /// got as far as being called tonal was still broken up by the run-length
    /// check and the negatives stayed green.
    #[rstest]
    #[case(silence(4000), "digital silence")]
    #[case(noise(4000, 0.3), "squelch hiss")]
    #[case(tone(1122.5, 40, 0.5), "a kerchunk shorter than one window")]
    fn nothing_that_is_not_a_held_tone_is_a_tone(#[case] audio: Vec<f32>, #[case] what: &str) {
        assert_eq!(runs(&audio, RATE), Vec::new(), "{what}");
    }

    /// **Speech is the one negative that is not asserted as silence**, and the
    /// honest claim about it is narrower: a vowel really does look like a tone
    /// for the fifty milliseconds its pitch takes to sweep through one, so the
    /// promise is not "never tonal" but "never held".
    ///
    /// The bound is [`super::MIN_STEP_MS`] — the shortest tone a **Tone
    /// profile** is allowed to name — because that is exactly the threshold
    /// that matters: a run shorter than the shortest configurable step cannot
    /// satisfy any profile, whatever an Operator types.
    #[test]
    fn a_voice_never_holds_a_pitch_long_enough_to_page_anything() {
        let found = runs(&vowel(6000, 220.0), RATE);

        let longest = found.iter().map(|run| run.duration_ms).max().unwrap_or(0);
        assert!(
            longest < super::super::MIN_STEP_MS,
            "the longest thing speech held was {longest} ms: {found:?}"
        );
    }

    /// **The silence floor is relative to this Call**, so the gap after a
    /// transmission is not a tone however pure its noise happens to look — and
    /// a quiet recording is still read, which is the other direction and the
    /// reason the floor is not an absolute number.
    #[test]
    fn the_quiet_after_a_transmission_is_not_a_tone() {
        let audio = [tone(1122.5, 1500, 0.6), noise(4000, 0.004)].concat();

        let found = runs(&audio, RATE);

        assert_eq!(found.len(), 1, "the tone, and nothing after it: {found:?}");
        assert!(found[0].end_ms() < 1700, "{found:?}");
    }

    /// A page-out is recorded off the air, not off a signal generator. It has
    /// to survive hiss, and it has to survive a recorder whose gain was set low.
    #[rstest]
    #[case(0.5, 0.05, "a clean page")]
    #[case(0.5, 0.15, "a noisy page")]
    #[case(0.03, 0.004, "a page recorded quietly")]
    fn a_page_survives_the_conditions_it_is_actually_recorded_in(
        #[case] amplitude: f32,
        #[case] hiss: f32,
        #[case] what: &str,
    ) {
        let audio = mix(
            &[tone(1122.5, 1000, amplitude), tone(1465.6, 3000, amplitude)].concat(),
            &noise(4000, hiss),
        );

        let found = runs(&audio, RATE);

        assert_eq!(found.len(), 2, "{what}: {found:?}");
        assert!((found[0].hz - 1122.5).abs() < 20.0, "{what}: {found:?}");
        assert!((found[1].hz - 1465.6).abs() < 20.0, "{what}: {found:?}");
    }

    /// **The grace frames earn their place here.** A packet lost mid-tone splits
    /// a four-second page into two two-second halves, each of which fails a
    /// profile asserting three seconds — which reads to an Operator as detection
    /// being unreliable rather than as their audio having a hole in it.
    #[test]
    fn a_momentary_dropout_does_not_split_a_tone_in_two() {
        let audio = [
            tone(1465.6, 1500, 0.5),
            silence(20),
            tone(1465.6, 1500, 0.5),
        ]
        .concat();

        let found = runs(&audio, RATE);

        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].duration_ms > 2800, "{found:?}");
    }

    /// A gap long enough to be a gap **is** one: this is what a profile's
    /// `gap_max_ms` is measured against, so the two tones have to arrive
    /// separately for it to mean anything.
    #[test]
    fn a_real_gap_between_two_tones_is_two_runs() {
        let audio = [
            tone(1122.5, 1000, 0.5),
            silence(200),
            tone(1465.6, 3000, 0.5),
        ]
        .concat();

        let found = runs(&audio, RATE);

        assert_eq!(found.len(), 2, "{found:?}");
        assert!(found[1].start_ms - found[0].end_ms() > 100, "{found:?}");
    }

    /// **The grace is bounded**, and this is the case that says so: two bursts
    /// of the *same* tone with a real silence between them are two
    /// transmissions, not one long tone.
    ///
    /// The pair above cannot prove it — the second tone is a different
    /// frequency, so the run closes on the frequency change whatever the grace
    /// does. Only the same tone twice can tell an unbounded grace from a
    /// three-frame one, which is exactly the difference between honouring a
    /// station's `min_ms` and inventing it.
    #[test]
    fn the_same_tone_twice_with_a_gap_between_is_two_runs() {
        let audio = [tone(1122.5, 800, 0.5), silence(300), tone(1122.5, 800, 0.5)].concat();

        let found = runs(&audio, RATE);

        assert_eq!(found.len(), 2, "{found:?}");
        assert!(
            found[0].duration_ms < 1000,
            "neither is the whole: {found:?}"
        );
    }

    /// Two frequencies close enough that a bin index could not tell them apart
    /// still come back as two tones — which is what the interpolation is for,
    /// and what makes a 2% tolerance mean anything at 300 Hz.
    #[test]
    fn two_tones_a_few_hz_apart_are_told_apart() {
        let found = runs(
            &[tone(600.0, 1000, 0.5), tone(620.0, 1000, 0.5)].concat(),
            RATE,
        );

        assert_eq!(found.len(), 2, "{found:?}");
        assert!((found[0].hz - 600.0).abs() < 6.0, "{found:?}");
        assert!((found[1].hz - 620.0).abs() < 6.0, "{found:?}");
    }

    /// A rate so low that the band this looks in does not fit under Nyquist has
    /// no tones rather than a panic — there is nothing there to find, and a
    /// slice index is not the way to say so.
    #[test]
    fn a_rate_too_low_to_hold_the_band_has_no_tones() {
        let samples: Vec<f32> = (0..400)
            .map(|n| 0.5 * (std::f64::consts::TAU * 100.0 * n as f64 / 400.0).sin() as f32)
            .collect();

        assert!(runs(&samples, 400).is_empty());
    }

    /// Three equal bins have no parabola through them, so the peak is where the
    /// bin index says and nowhere else. Reached directly because a real
    /// spectrum with three exactly-equal `f64` bins is not something audio
    /// produces — and a divide-by-zero here would put a `NaN` frequency into a
    /// Run, which every comparison downstream would answer `false` to.
    #[rstest]
    #[case(&[1.0, 1.0, 1.0], 0.0)]
    #[case(&[0.0, 0.0, 0.0], 0.0)]
    #[case(&[1.0, 4.0, 1.0], 0.0)]
    #[case(&[1.0, 4.0, 2.0], 0.1)]
    fn a_peak_between_bins_is_interpolated_and_never_divides_by_zero(
        #[case] power: &[f64],
        #[case] expected: f64,
    ) {
        let offset = interpolate(power, 1);

        assert!(offset.is_finite(), "{offset}");
        assert!((offset - expected).abs() < 0.2, "{offset} vs {expected}");
    }

    /// The middle of nothing is zero rather than a panic — [`finish`] never
    /// hands this an empty run, and an indexing `unwrap` would be a crash
    /// waiting for the day something does.
    #[test]
    fn the_middle_of_nothing_is_nothing() {
        assert_eq!(median(&[]), 0.0);
        assert_eq!(median(&[3.0, 1.0, 2.0]), 2.0);
    }

    /// Audio shorter than a single analysis window has no runs rather than a
    /// panic — a Call can be 30 milliseconds long, and several are.
    #[rstest]
    #[case(&[], 8_000)]
    #[case(&[0.0; 10], 8_000)]
    #[case(&[0.0; 10_000], 0)]
    fn audio_too_short_or_rateless_has_no_tones(#[case] samples: &[f32], #[case] rate: u32) {
        assert!(runs(samples, rate).is_empty());
    }

    /// A recorder writes at whatever rate it likes — 8 kHz from Trunk Recorder,
    /// 22.05 kHz from an SDRTrunk MP3 — and the answer is the same tone.
    #[rstest]
    #[case(8_000)]
    #[case(16_000)]
    #[case(22_050)]
    #[case(44_100)]
    fn the_same_tone_reads_the_same_at_every_rate_a_recorder_writes(#[case] rate: u32) {
        let len = rate as usize * 2;
        let samples: Vec<f32> = (0..len)
            .map(|n| 0.5 * (std::f64::consts::TAU * 1122.5 * n as f64 / rate as f64).sin() as f32)
            .collect();

        let found = runs(&samples, rate);

        assert_eq!(found.len(), 1, "{rate}: {found:?}");
        assert!((found[0].hz - 1122.5).abs() < 15.0, "{rate}: {found:?}");
        assert!(
            found[0].duration_ms.abs_diff(2000) < 100,
            "{rate}: {found:?}"
        );
    }

    proptest! {
        /// **Any** tone in the band is measured accurately enough for a profile
        /// to assert a 2% tolerance against it — which is the number an Operator
        /// is going to type, and the reason the peak is interpolated rather than
        /// read off a bin index 15 Hz wide.
        #[test]
        fn any_tone_in_the_band_is_measured_within_one_percent(hz in 300.0f64..3_000.0) {
            let found = runs(&tone(hz, 1200, 0.5), RATE);

            prop_assert_eq!(found.len(), 1, "{:?}", found);
            prop_assert!(
                (found[0].hz - hz).abs() <= hz * 0.01,
                "{} measured as {}",
                hz,
                found[0].hz
            );
        }
    }
}
