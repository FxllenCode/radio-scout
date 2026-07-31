//! How long is this, from the container header alone (#42, spec US 8).

use symphonia::core::formats::TrackType;
use symphonia::core::formats::probe::Hint;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::units::Timestamp;

/// The playing time of `audio`, in milliseconds, read from its container header.
pub fn duration_ms(audio: &[u8]) -> Option<i64> {
    let source = MediaSourceStream::new(
        Box::new(std::io::Cursor::new(audio.to_vec())),
        <_>::default(),
    );
    let format = symphonia::default::get_probe()
        .probe(
            &Hint::new(),
            source,
            Default::default(),
            MetadataOptions::default(),
        )
        .ok()?;
    let track = format.default_track(TrackType::Audio)?;
    let ticks = Timestamp::new(i64::try_from(track.duration?.get()).ok()?);
    let millis = track.time_base?.calc_time(ticks)?.as_millis();
    i64::try_from(millis).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use rstest::rstest;

    /// A mono 16-bit PCM WAV of exactly `samples` frames at `rate`, built by
    /// hand — a probe that shared its writer with the code under test could not
    /// tell a wrong header from a consistently wrong one.
    fn wav(samples: usize, rate: u32) -> Vec<u8> {
        let data = vec![0u8; samples * 2];
        let mut out = Vec::new();
        out.extend(b"RIFF");
        out.extend(((36 + data.len()) as u32).to_le_bytes());
        out.extend(b"WAVEfmt ");
        out.extend(16u32.to_le_bytes()); // PCM fmt chunk size
        out.extend(1u16.to_le_bytes()); // PCM
        out.extend(1u16.to_le_bytes()); // mono
        out.extend(rate.to_le_bytes());
        out.extend((rate * 2).to_le_bytes()); // byte rate
        out.extend(2u16.to_le_bytes()); // block align
        out.extend(16u16.to_le_bytes()); // bits
        out.extend(b"data");
        out.extend((data.len() as u32).to_le_bytes());
        out.extend(data);
        out
    }

    /// A constant-bitrate MPEG-1 Layer III stream of `frames` frames — mono,
    /// 44.1 kHz, 128 kbps, no CRC, no padding, and deliberately **no Xing
    /// header**, which is the shape a recorder writing a live stream produces.
    ///
    /// Frame size is `144 * 128000 / 44100 = 417` bytes; the four header bytes
    /// are followed by silence, because nothing here decodes a sample.
    fn mp3(frames: usize) -> Vec<u8> {
        let mut out = Vec::new();
        for _ in 0..frames {
            out.extend([0xFF, 0xFB, 0x90, 0xC0]);
            out.extend(std::iter::repeat_n(0u8, 417 - 4));
        }
        out
    }

    /// The whole point: a Call's length, without decoding a single sample.
    ///
    /// 12 000 frames at 8 kHz is 1500 ms — a value neither the frame count nor
    /// the sample rate reaches on its own, so an assertion on it cannot be
    /// satisfied by returning either one.
    #[test]
    fn a_wav_header_gives_its_length() {
        assert_eq!(duration_ms(&wav(12_000, 8_000)), Some(1500));
    }

    /// SDRTrunk uploads MP3, and a live-recorded one carries no Xing header to
    /// read a frame count out of — so this is the case that decides whether
    /// half the recorders in the field get a duration at all.
    ///
    /// 40 Layer III frames is 40 × 1152 samples at 44 100 Hz = 1044.9 ms.
    #[test]
    fn an_mp3_without_a_xing_header_still_gives_its_length() {
        assert_eq!(duration_ms(&mp3(40)), Some(1044));
    }

    /// Nothing a recorder can send is allowed to be an error here: a Call whose
    /// length cannot be read is a Call with no length, not a failed ingest. The
    /// second case is the harness's own default audio part, so every existing
    /// test in the suite goes through this arm.
    #[rstest]
    #[case::empty(&[])]
    #[case::not_audio_at_all(b"audio-bytes")]
    #[case::a_riff_and_nothing_else(b"RIFF")]
    #[case::a_wave_header_with_no_chunks(b"RIFF\x00\x00\x00\x00WAVE")]
    #[case::an_mp3_sync_word_and_nothing_after_it(&[0xFF, 0xFB])]
    fn audio_that_cannot_be_read_has_no_length(#[case] bytes: &[u8]) {
        assert_eq!(duration_ms(bytes), None);
    }

    /// A truncated upload — the recorder died mid-write, so the `data` chunk
    /// claims 40 s and the file holds 1 s.
    ///
    /// The header is the only thing read, so the header is what is believed:
    /// this is the number `#46`'s keep-best comparison will see, and pinning it
    /// is what makes that a decision rather than a surprise.
    #[test]
    fn a_wav_whose_header_lies_is_believed_at_its_word() {
        let mut truncated = wav(320_000, 8_000); // 40 s declared
        truncated.truncate(44 + 16_000); // ...1 s delivered
        assert_eq!(duration_ms(&truncated), Some(40_000));
    }

    proptest! {
        /// Whatever arrives on the ingest path — a truncated file, a
        /// mislabelled container, an outright hostile body — reading its length
        /// must never panic and never hang. A panic here would be a 500 on a
        /// recorder's upload.
        #[test]
        fn arbitrary_bytes_never_panic(bytes in proptest::collection::vec(any::<u8>(), 0..2048)) {
            let _ = duration_ms(&bytes);
        }

        /// ...including bytes that got far enough to look like a RIFF file,
        /// which is where a length field is read and arithmetic happens.
        #[test]
        fn arbitrary_riff_bodies_never_panic(tail in proptest::collection::vec(any::<u8>(), 0..512)) {
            let mut bytes = b"RIFF".to_vec();
            bytes.extend(tail);
            let _ = duration_ms(&bytes);
        }
    }
}
