//! One range of the Archive as one playable file (#65, spec US 33).
//!
//! # Why the timeline is *declared* rather than discovered
//!
//! A WAV states its length in its first 44 bytes, and an export cannot know the
//! length of decoded audio without decoding it — which is the one thing it must
//! not do twice and must not hold whole. So the length comes from what the
//! Archive already wrote down: `calls.duration_ms`, summed in one statement
//! before a byte is read, and each Call is then **fitted** into the room that
//! sum reserved for it. Reality is made to match the header rather than the
//! header waiting on reality, which is what buys a single decode pass, an exact
//! `Content-Length`, and a memory cost of one Call.
//!
//! Three consequences, and each is load-bearing:
//!
//! - **A Call whose length was never measured cannot be placed on a declared
//!   timeline**, so it is not in the stitched file at all. That is the rule
//!   [`crate::archive::CallSearch::min_duration_ms`] already applies for its own
//!   reason — a threshold cannot be tested against an unknown — and an
//!   **Encrypted Call** is out for the older reason that there is no audio.
//!   The zip export carries both, in its manifest.
//! - **What cannot be read is still the length it claimed.** An object
//!   retention pruned between the pre-pass and the read, or one no decoder will
//!   open, becomes silence of exactly its declared length. The alternative is a
//!   valid header over a body that came up short, which does not fail — it
//!   plays as every Call after the gap being the wrong one.
//! - **The fit is a few milliseconds either way.** A container's own idea of
//!   its length differs from a recorder's by an encoder delay or a truncated
//!   final frame; padding that with silence at a transmission boundary is
//!   inaudible, and so is losing it.
//!
//! No FFmpeg and no C: [`crate::enhance::decode`] is the one decoder this
//! process has, [`crate::enhance::resample`] the one resampler, and the header
//! is written out by hand for [`crate::enhance`]'s own reason — this is the
//! entire format, and it is the one encoding that plays everywhere.

use bytes::Bytes;

/// The rate a stitched export is written at — enhancement's own output rate,
/// because it is the same argument: nothing a P25 vocoder or an analogue
/// channel produces survives above 4 kHz, and writing 48 kHz would quadruple a
/// county's night for content that is not in it.
pub const RATE: u32 = crate::enhance::OUTPUT_RATE;

/// The most samples a canonical WAV can state, since both of its lengths are
/// 32-bit. What refuses an export this long is [`crate::export::Extent`], one
/// statement before anything is written; this is the number it asks against.
pub const MAX_SAMPLES: u64 = ((u32::MAX as u64) - 36) / 2;

/// How many samples the stitched file gives a Call of `duration_ms`.
///
/// **The one place this arithmetic is written.** The pre-pass sums it to build
/// the header and the stream applies it per Call, so a rounding difference
/// between the two would leave the file a sample short per Call and end it
/// mid-transmission.
pub fn samples_for(duration_ms: i64) -> u64 {
    (duration_ms.max(0) as u64).saturating_mul(RATE as u64) / 1000
}

/// The header a stitched file opens with: mono 16-bit PCM at [`RATE`].
pub fn header(total_samples: u64) -> Bytes {
    let data = (total_samples.min(MAX_SAMPLES) * 2) as u32;
    let mut out = Vec::with_capacity(44);
    out.extend(b"RIFF");
    out.extend((36 + data).to_le_bytes());
    out.extend(b"WAVEfmt ");
    out.extend(16u32.to_le_bytes()); // PCM format chunk size
    out.extend(1u16.to_le_bytes()); // PCM
    out.extend(1u16.to_le_bytes()); // mono
    out.extend(RATE.to_le_bytes());
    out.extend((RATE * 2).to_le_bytes()); // byte rate: rate * channels * 2
    out.extend(2u16.to_le_bytes()); // block align
    out.extend(16u16.to_le_bytes()); // bits per sample
    out.extend(b"data");
    out.extend(data.to_le_bytes());
    Bytes::from(out)
}

/// The most samples written in one piece — four seconds, 64 KB.
///
/// **The export's memory bound, stated as a number.** Everything else here is
/// bounded by an object a Recorder really uploaded; `want` is not — it comes
/// from `calls.duration_ms`, which is a number a Recorder *said*, and the
/// silence that fills an unread Call is bounded by nothing at all. A single row
/// claiming seventy-four hours would otherwise be four gigabytes of zeroes in
/// one allocation, on a Pi.
const CHUNK_SAMPLES: u64 = RATE as u64 * 4;

/// One Call's place in the stitched file, filled with whatever its audio turned
/// out to hold — padded with silence where it fell short, trimmed where it ran
/// over, and silent throughout where there was nothing readable to put in it.
///
/// Exactly `want * 2` bytes in total, always. That is the promise the header
/// made — but they arrive in [`CHUNK_SAMPLES`]-sized pieces, because the total
/// is a number this module does not get to choose.
pub fn segment(audio: Option<&[u8]>, want: u64) -> Segment {
    Segment {
        voice: audio.and_then(decoded).unwrap_or_default(),
        left: want,
        at: 0,
    }
}

/// One Call's place, a piece at a time.
pub struct Segment {
    /// The decoded audio, bounded by the object it came out of — which is one
    /// Call, and is the one thing here that is genuinely a Call's worth.
    voice: Vec<f32>,
    /// Samples still owed.
    left: u64,
    /// How far into `voice` the next piece starts.
    at: usize,
}

impl Iterator for Segment {
    type Item = Bytes;

    fn next(&mut self) -> Option<Bytes> {
        if self.left == 0 {
            return None;
        }
        let samples = self.left.min(CHUNK_SAMPLES) as usize;
        self.left -= samples as u64;

        let mut out = Vec::with_capacity(samples * 2);
        for sample in self.voice.iter().skip(self.at).take(samples) {
            out.extend(((sample.clamp(-1.0, 1.0) * i16::MAX as f32) as i16).to_le_bytes());
        }
        // Whatever the audio did not fill, silence does. `resize` shortens
        // nothing: the loop above already stopped at `samples`.
        out.resize(samples * 2, 0);
        self.at += samples;
        Some(Bytes::from(out))
    }
}

/// This Call's audio at the export's rate, or `None` if nothing could be read
/// out of it. Every failure is one answer on purpose — the file has to keep
/// going, and which way a stranger's object was broken is not a fact the
/// listener of a stitched export can act on.
fn decoded(audio: &[u8]) -> Option<Vec<f32>> {
    let (samples, rate) = crate::enhance::decode(audio).ok()?;
    crate::enhance::resample(&samples, rate, RATE).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    /// A mono 16-bit WAV at `rate`, the shape a recorder uploads.
    fn wav(samples: &[f32], rate: u32) -> Vec<u8> {
        let data: Vec<u8> = samples
            .iter()
            .flat_map(|s| ((s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16).to_le_bytes())
            .collect();
        let mut out = Vec::new();
        out.extend(b"RIFF");
        out.extend(((36 + data.len()) as u32).to_le_bytes());
        out.extend(b"WAVEfmt ");
        out.extend(16u32.to_le_bytes());
        out.extend(1u16.to_le_bytes());
        out.extend(1u16.to_le_bytes());
        out.extend(rate.to_le_bytes());
        out.extend((rate * 2).to_le_bytes());
        out.extend(2u16.to_le_bytes());
        out.extend(16u16.to_le_bytes());
        out.extend(b"data");
        out.extend((data.len() as u32).to_le_bytes());
        out.extend(data);
        out
    }

    fn tone(seconds: f32, rate: u32) -> Vec<f32> {
        (0..(seconds * rate as f32) as usize)
            .map(|n| (n as f32 * 1000.0 * std::f32::consts::TAU / rate as f32).sin() * 0.5)
            .collect()
    }

    /// One Call's whole place, collected — what every assertion below is about,
    /// since the piece boundaries are the export's business and not the file's.
    fn whole(audio: Option<&[u8]>, want: u64) -> Vec<u8> {
        segment(audio, want)
            .flat_map(|piece| piece.to_vec())
            .collect()
    }

    fn u32_at(bytes: &[u8], at: usize) -> u32 {
        u32::from_le_bytes(bytes[at..at + 4].try_into().expect("four bytes"))
    }

    fn u16_at(bytes: &[u8], at: usize) -> u16 {
        u16::from_le_bytes(bytes[at..at + 2].try_into().expect("two bytes"))
    }

    proptest::proptest! {
        /// **The header would lie if this were not true.** The length is
        /// declared from one `SUM(duration_ms)` and then written a Call at a
        /// time, so the sum of the parts has to *be* the whole — a per-Call
        /// rounding loss of one sample would leave an hour's export ending
        /// mid-transmission, and would look exactly like a decoder problem.
        ///
        /// It holds because [`RATE`] divides evenly into a second. That is a
        /// property of the number rather than of the code, which is precisely
        /// why it is asserted here rather than assumed: a future rate of
        /// 11,025 Hz would break the whole design and nothing else would say
        /// so.
        #[test]
        fn a_calls_place_and_the_whole_are_the_same_arithmetic(
            durations in proptest::collection::vec(0i64..600_000, 0..40),
        ) {
            let whole: i64 = durations.iter().sum();
            let parts: u64 = durations.iter().copied().map(samples_for).sum();

            proptest::prop_assert_eq!(parts, samples_for(whole));
        }
    }

    /// **The header is the whole reason the timeline is declared rather than
    /// discovered**: a WAV states its length before its audio, and an export
    /// that had to decode a county's night before it could write byte 0 would
    /// buffer the thing this feature exists not to buffer.
    #[test]
    fn the_header_states_the_length_before_a_sample_is_written() {
        let total = 8_000 * 42;

        let head = header(total);

        assert_eq!(head.len(), 44, "a canonical WAV header");
        assert_eq!(&head[..4], b"RIFF");
        assert_eq!(u32_at(&head, 4), 36 + total as u32 * 2, "RIFF chunk size");
        assert_eq!(&head[8..12], b"WAVE");
        assert_eq!(u16_at(&head, 22), 1, "mono");
        assert_eq!(u32_at(&head, 24), RATE, "the export's own rate");
        assert_eq!(u16_at(&head, 34), 16, "16-bit");
        assert_eq!(&head[36..40], b"data");
        assert_eq!(u32_at(&head, 40), total as u32 * 2, "data chunk size");
    }

    /// Both of a WAV's lengths are 32-bit, so there is a range this format
    /// cannot describe. [`crate::export::Extent`] refuses one that long before
    /// anything is written; this pins that the header saturates rather than
    /// wrapping, so the failure could only ever be a short file and never a
    /// header claiming four gigabytes fewer than it has.
    #[test]
    fn a_length_the_format_cannot_state_is_clamped_rather_than_wrapped() {
        let head = header(MAX_SAMPLES + 1_000_000);

        assert_eq!(
            u32_at(&head, 40),
            (MAX_SAMPLES * 2) as u32,
            "data chunk size"
        );
        assert_eq!(
            u32_at(&head, 4),
            u32::MAX - 1,
            "and the RIFF size still fits"
        );
    }

    /// The declaration and the audio have to be the same arithmetic, or the
    /// file drifts by a sample per Call and ends short.
    #[rstest]
    #[case::a_second(1_000, 8_000)]
    #[case::a_typical_transmission(4_500, 36_000)]
    #[case::a_kerchunk(120, 960)]
    #[case::nothing(0, 0)]
    fn a_calls_place_is_its_declared_length(#[case] duration_ms: i64, #[case] samples: u64) {
        assert_eq!(samples_for(duration_ms), samples);

        assert_eq!(
            whole(None, samples_for(duration_ms)).len() as u64,
            samples * 2,
            "the bytes written must be the bytes declared"
        );
    }

    /// A Call whose audio decodes to exactly what it claimed: the common case,
    /// and the one where nothing is padded or thrown away.
    #[test]
    fn a_call_that_is_as_long_as_it_said_is_written_whole() {
        let audio = wav(&tone(1.0, 8_000), 8_000);

        let out = whole(Some(&audio), samples_for(1_000));

        assert_eq!(out.len(), 8_000 * 2);
        assert!(
            out.chunks(2)
                .any(|s| i16::from_le_bytes([s[0], s[1]]).abs() > 8_000),
            "the tone should have survived"
        );
    }

    /// Fitting, both ways. A recorder's stated length and its container's own
    /// idea of it differ by an encoder's delay or a truncated final frame, and
    /// the declared timeline is what the header promised — so the audio is made
    /// to fit it rather than the other way round.
    #[rstest]
    #[case::shorter_than_it_claimed(0.5, 1_000)]
    #[case::longer_than_it_claimed(2.0, 1_000)]
    fn audio_is_fitted_to_the_place_declared_for_it(
        #[case] seconds: f32,
        #[case] duration_ms: i64,
    ) {
        let audio = wav(&tone(seconds, 8_000), 8_000);

        let out = whole(Some(&audio), samples_for(duration_ms));

        assert_eq!(out.len() as u64, samples_for(duration_ms) * 2);
    }

    /// Audio at a rate that is not the export's — SDRTrunk writes MP3s at
    /// 16 kHz and a scanner feed can be 44.1 — still fills exactly its place.
    #[rstest]
    #[case::sdrtrunk(16_000)]
    #[case::a_desktop_recorder(44_100)]
    #[case::trunk_recorder(8_000)]
    fn audio_at_any_rate_fills_exactly_its_place(#[case] rate: u32) {
        let audio = wav(&tone(1.0, rate), rate);

        let out = whole(Some(&audio), samples_for(1_000));

        assert_eq!(out.len(), 8_000 * 2);
    }

    /// **No piece is bigger than the bound, however long the place is.** The
    /// number that decides it comes from `calls.duration_ms` — a Recorder's
    /// word, not ours — so a row claiming seventy-four hours must cost this
    /// process a chunk, not four gigabytes.
    #[test]
    fn a_long_place_is_written_in_bounded_pieces() {
        let hours = samples_for(74 * 60 * 60 * 1_000);

        let mut pieces = segment(None, hours);

        assert!(
            pieces
                .by_ref()
                .take(3)
                .all(|piece| piece.len() as u64 == CHUNK_SAMPLES * 2),
            "each piece is the bound"
        );
        assert_eq!(
            segment(None, CHUNK_SAMPLES + 1).count(),
            2,
            "and the last one is only what is left"
        );
    }

    /// **The header cannot be taken back.** An object retention pruned between
    /// the pre-pass and the read, or one no decoder will open, must still take
    /// up the room the file said it would — the alternative is a stitched
    /// export that is a valid header over a truncated body, which plays as
    /// every later Call being the wrong one.
    #[rstest]
    #[case::retention_took_it(None)]
    #[case::not_audio_at_all(Some(b"<html>404</html>".as_slice()))]
    #[case::a_truncated_upload(Some(b"RIFF\0\0\0\0WAVE".as_slice()))]
    fn what_cannot_be_read_is_still_the_length_it_claimed(#[case] audio: Option<&[u8]>) {
        let out = whole(audio, samples_for(3_000));

        assert_eq!(out.len(), 24_000 * 2);
        assert!(out.iter().all(|byte| *byte == 0), "silence, not garbage");
    }
}
