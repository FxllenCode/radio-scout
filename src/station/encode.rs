//! The Station stream's one encoder, behind one seam (#74).
//!
//! # Why MP3
//!
//! The stream is for players that cannot run the app, so its format is the one
//! every radio player there is will play: VLC, every browser including iOS
//! Safari, Sonos, smart speakers, a car's head unit. MP3's patents expired in
//! 2017. The alternatives each failed one of those players or one of this
//! project's rules — AAC is a patent pool's to license and
//! [`crate::enhance::Output`] already declined to publish an encoder for it;
//! Opus is unplayable in Safari before 18.4 and on most speakers; an endless WAV
//! is four times the bandwidth and is not a stream Safari or a Sonos will
//! follow.
//!
//! # Why this encoder
//!
//! `rusty_mp3` is pure Rust with no runtime dependencies, where LAME would add
//! an autotools build of vendored C to every release target. It is young
//! (2026), which is why it is held to this module alone and why every test here
//! judges its output with **our** decoder ([`crate::enhance::decode`], which is
//! symphonia) rather than with its own — a stream that only looks like MP3 to
//! the thing that wrote it fails here. Swapping it is a change to this file.
//!
//! # Why 16 kHz, 32 kbps, mono
//!
//! Voice-band radio audio has nothing above 4 kHz — a P25 vocoder never
//! produced any — so 16 kHz carries everything there is with room to spare, and
//! 32 kbps is 4 KB a second. 16 kHz is also **MPEG-2**, which is load-bearing
//! in two ways. Every frame is self-contained, because the encoder's bit
//! reservoir — a frame borrowing bits from the ones before it — is MPEG-1 only,
//! so a Call, the silence after it and the next Call can be three separate
//! encodes concatenated. And the encoder never holds frames back until the end
//! of its input, which in its MPEG-1 reservoir mode it does, and which in a
//! stream that never ends would be forever. 8 kHz would be MPEG-2.5, which is
//! not in the standard and not on every speaker.

use bytes::Bytes;
use rusty_mp3::{Mp3Encoder, Mp3EncoderConfig};

/// The rate the stream is encoded at.
pub const RATE: u32 = 16_000;

/// The constant bitrate, in kbps.
pub const BITRATE_KBPS: u32 = 32;

/// Samples in one MPEG-2 Layer III frame.
pub const FRAME_SAMPLES: usize = 576;

/// Bytes in one frame at [`RATE`] and 32 kbps.
pub const FRAME_BYTES: usize = 144;

/// How long one frame plays for — exactly 36 ms.
pub const FRAME: std::time::Duration =
    std::time::Duration::from_micros(FRAME_SAMPLES as u64 * 1_000_000 / RATE as u64);

/// Frames of silence pushed after a Call, so the encoder's own delay — about a
/// frame and a half — has carried the Call's last sample out before the encoder
/// is dropped.
const FLUSH_FRAMES: usize = 2;

/// One Call as a run of whole frames.
pub(crate) fn frames(pcm: &[f32]) -> Vec<Bytes> {
    let mut encoder = Mp3Encoder::new(Mp3EncoderConfig {
        bitrate_kbps: BITRATE_KBPS,
        vbr_quality: None,
    });
    let tail = pcm.len().div_ceil(FRAME_SAMPLES) * FRAME_SAMPLES - pcm.len()
        + FLUSH_FRAMES * FRAME_SAMPLES;
    // The one refusal a push has is a sample rate outside Layer III's nine,
    // and this one is a constant that is one of them.
    encoder
        .push_pcm_f32(pcm, 1, RATE)
        .and_then(|()| encoder.push_pcm_f32(&vec![0.0; tail], 1, RATE))
        .expect("16 kHz is a Layer III rate");
    std::iter::from_fn(|| encoder.next_packet().ok())
        .map(Bytes::from)
        .collect()
}

/// A stored Call's object as the stream carries it, or `None` when there is
/// nothing in it to play.
///
/// Every failure is one answer, for the stitched export's reason
/// ([`crate::export::stitch`]): the stream has to keep going, and which way a
/// Recorder's object was broken is not a fact anyone listening to a smart
/// speaker can act on.
///
/// CPU-bound — a decode, a resample and an encode — so a caller on the runtime
/// hands it to a blocking thread.
pub fn prepare(audio: &[u8]) -> Option<Vec<Bytes>> {
    let (samples, rate) = crate::enhance::decode(audio).ok()?;
    let samples = crate::enhance::resample(&samples, rate, RATE).ok()?;
    (!samples.is_empty()).then(|| frames(&samples))
}

/// One frame of silence, encoded once for the life of the process.
///
/// **What makes a quiet scanner cost nothing.** A stream spends most of its
/// life between transmissions, and every one of those frames is this one —
/// cloned, which for [`Bytes`] is a reference count, never an encode.
pub(crate) fn silence() -> Bytes {
    static SILENCE: std::sync::OnceLock<Bytes> = std::sync::OnceLock::new();
    SILENCE
        .get_or_init(|| {
            // The last of a few, so the frame is one the encoder made with
            // nothing but silence behind it as well as in it.
            frames(&[0.0; FRAME_SAMPLES * 2])
                .pop()
                .expect("silence encodes")
        })
        .clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(seconds: f32, hz: f32) -> Vec<f32> {
        (0..(seconds * RATE as f32) as usize)
            .map(|n| (n as f32 * hz * std::f32::consts::TAU / RATE as f32).sin() * 0.5)
            .collect()
    }

    /// Decode with the process's own decoder, which is not the encoder's.
    fn decoded(frames: &[Bytes]) -> (Vec<f32>, u32) {
        crate::enhance::decode(&frames.concat()).expect("our decoder reads it")
    }

    #[test]
    fn a_call_encodes_to_whole_frames_our_decoder_reads_back_at_the_stream_rate() {
        let call = tone(1.0, 1_000.0);

        let frames = frames(&call);

        assert!(!frames.is_empty());
        assert!(frames.iter().all(|frame| frame.len() == FRAME_BYTES));
        let (samples, rate) = decoded(&frames);
        assert_eq!(rate, RATE);
        assert_eq!(samples.len(), frames.len() * FRAME_SAMPLES);
        assert!(
            samples.len() >= call.len(),
            "nothing of the Call is cut off"
        );
    }

    /// How strongly `samples` carry `hz` — a single-bin DFT (Goertzel), as a
    /// share of the window's whole energy.
    fn strength(samples: &[f32], hz: f32) -> f32 {
        let w = std::f32::consts::TAU * hz / RATE as f32;
        let (re, im) = samples
            .iter()
            .enumerate()
            .fold((0f32, 0f32), |(re, im), (n, s)| {
                (re + s * (w * n as f32).cos(), im - s * (w * n as f32).sin())
            });
        let energy: f32 = samples.iter().map(|s| s * s).sum();
        2.0 * (re * re + im * im) / (samples.len() as f32 * energy.max(f32::EPSILON))
    }

    /// How many samples lie between the first and the last loud one, inclusive.
    fn loud_span(samples: &[f32]) -> usize {
        let first = samples.iter().position(|s| s.abs() > 0.1);
        let last = samples.iter().rposition(|s| s.abs() > 0.1);
        first.zip(last).map_or(0, |(first, last)| last - first + 1)
    }

    /// **The transmission is what comes out**: at its pitch, as long as it was,
    /// and nothing audible added around it.
    ///
    /// To the millisecond, because the encoder delays everything by about a
    /// frame and a half — and a Call whose tail was still inside the encoder
    /// when it was dropped is a Call that ends early by exactly that much.
    #[test]
    fn the_call_survives_at_its_pitch_and_its_length() {
        let call = tone(2.0, 1_000.0);

        let (samples, _) = decoded(&frames(&call));

        let span = loud_span(&samples);
        let (one_ms, wanted) = (RATE as usize / 1_000, call.len());
        assert!(span.abs_diff(wanted) <= one_ms, "{span} loud, for {wanted}");
        let middle = &samples[RATE as usize / 2..RATE as usize * 3 / 2];
        assert!(strength(middle, 1_000.0) > 0.9, "the tone is the tone");
        assert!(strength(middle, 2_000.0) < 0.01, "and not some other");
    }

    /// A run of them, because a run is how silence is sent — and because a
    /// decoder will not lock on to a stream from a single frame.
    #[test]
    fn the_silent_frame_repeats_into_nothing() {
        let (samples, rate) = decoded(&vec![silence(); 10]);

        assert_eq!(silence().len(), FRAME_BYTES);
        assert_eq!(rate, RATE);
        assert_eq!(samples.len(), 10 * FRAME_SAMPLES);
        assert!(samples.iter().all(|s| s.abs() < 1e-3));
    }

    /// **Every frame stands alone**, which is the property the whole stream is
    /// built on: two Calls and the silence between them are three encodes
    /// concatenated, and a decoder reads them as one stream — every sample
    /// accounted for, each Call where it was put.
    ///
    /// It holds because the stream is MPEG-2. The encoder's bit reservoir —
    /// main data borrowed from earlier frames — is MPEG-1 only, so at 16 kHz no
    /// frame refers to another, and a frame of one encode may follow a frame of
    /// any other.
    #[test]
    fn calls_and_silence_splice_into_one_stream() {
        let first = frames(&tone(1.0, 700.0));
        let quiet = vec![silence(); 30];
        let second = frames(&tone(1.0, 1_900.0));

        let spliced = [first.clone(), quiet.clone(), second.clone()].concat();
        let (samples, _) = decoded(&spliced);

        assert_eq!(samples.len(), spliced.len() * FRAME_SAMPLES);
        let at = |frame: usize| frame * FRAME_SAMPLES;
        let opening = &samples[at(5)..at(20)];
        let pause = &samples[at(first.len() + 5)..at(first.len() + 25)];
        let closing =
            &samples[at(first.len() + quiet.len() + 5)..at(first.len() + quiet.len() + 20)];
        assert!(strength(opening, 700.0) > 0.9);
        assert!(pause.iter().all(|s| s.abs() < 1e-3));
        assert!(strength(closing, 1_900.0) > 0.9);
    }

    /// What a Recorder uploads is not what the stream carries: Trunk Recorder
    /// writes 8 kHz, and the stream is 16 kHz whatever arrived.
    #[test]
    fn a_stored_object_is_prepared_at_the_stream_rate() {
        let eight_khz: Vec<f32> = (0..8_000)
            .map(|n| (n as f32 * 1_000.0 * std::f32::consts::TAU / 8_000.0).sin() * 0.5)
            .collect();
        let object = crate::enhance::encode_wav(&eight_khz, 8_000);

        let frames = prepare(&object).expect("a WAV prepares");

        let (samples, rate) = decoded(&frames);
        assert_eq!(rate, RATE);
        let span = loud_span(&samples);
        assert!(
            span.abs_diff(RATE as usize) <= RATE as usize / 100,
            "{span}"
        );
        assert!(strength(&samples[4_000..12_000], 1_000.0) > 0.9);
    }

    /// An object nothing can read — and one that reads as no audio at all —
    /// is a Call with nothing to play, which the stream skips.
    #[rstest::rstest]
    #[case::not_audio(b"this is not audio".to_vec())]
    #[case::empty_object(Vec::new())]
    #[case::no_samples(crate::enhance::encode_wav(&[], 8_000))]
    fn an_object_with_nothing_in_it_prepares_to_nothing(#[case] object: Vec<u8>) {
        assert_eq!(prepare(&object), None);
    }

    proptest::proptest! {
        // A debug-build encode is two orders of magnitude slower than a release
        // one, and the default 256 cases spent ten seconds of everybody's loop
        // proving a property 32 already finds a counterexample to.
        #![proptest_config(proptest::prelude::ProptestConfig::with_cases(32))]

        /// **The stream is paced by counting frames**, so a frame has to be one
        /// frame's worth of bytes and of time whatever went in — any length,
        /// any value, including the ones a broken decode can produce.
        #[test]
        fn every_frame_is_one_frame_whatever_is_pushed(
            pcm in proptest::collection::vec(
                proptest::prop_oneof![
                    -1.0f32..=1.0,
                    proptest::num::f32::ANY,
                ],
                0..(FRAME_SAMPLES * 12),
            )
        ) {
            let frames = frames(&pcm);

            proptest::prop_assert!(frames.iter().all(|frame| frame.len() == FRAME_BYTES));
            proptest::prop_assert!(frames.len() * FRAME_SAMPLES >= pcm.len());
        }
    }
}
