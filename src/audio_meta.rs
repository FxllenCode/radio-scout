//! What one pass over a Call's audio container says about it (#42, spec US 8;
//! #48, spec US 13).
//!
//! Two questions, one probe. **How long is this** comes from the container
//! header — no decode, no sample touched. **What did the Recorder write into
//! it** is the metadata SDRTrunk embeds in the MP3s it uploads, which is the
//! whole of #48's raw material.
//!
//! They share a pass because they read the same bytes at the same moment.
//! Ingest already probed for the duration; asking the probe for what it had
//! *already parsed on the way past* costs nothing, and it is what lets a new
//! Call be mined on the ingest path rather than by a worker reading the object
//! back afterwards. What it cost was a feature flag on a crate already in the
//! tree — `symphonia/id3v2`, which `mp3` does **not** imply, so under
//! `default-features = false` the tag ahead of every SDRTrunk MPEG frame was
//! being skipped over unread.
//!
//! This module is deliberately container vocabulary and nothing else: it hands
//! back ID3 frame ids verbatim and has no idea what a radio is. Interpreting
//! them is [`crate::mining`]'s, because that is one Recorder's dialect and this
//! is every Recorder's file format.

use symphonia::core::formats::TrackType;
use symphonia::core::formats::probe::Hint;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::units::Timestamp;

/// What an ID3v2 tag opens with.
///
/// Exposed so a caller can decide in three bytes whether probing for metadata
/// could possibly find any — which is what keeps a full container parse off
/// every Trunk Recorder upload, where there has never been a tag to find.
pub const ID3: &[u8] = b"ID3";

/// Everything one probe of a Call's audio can say about it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AudioFacts {
    /// The playing time in milliseconds, read from the container header alone,
    /// or `None` where the header says nothing.
    pub duration_ms: Option<i64>,
    /// The metadata the Recorder embedded, under the container's own name for
    /// each field — ID3v2 frame ids (`TPE1`, `COMM`) on an MP3.
    ///
    /// Kept as the container spelled them rather than mapped to a vocabulary of
    /// ours: what SDRTrunk means by `COMM` is SDRTrunk's business, and a
    /// normalizing layer here would have to guess at it twice.
    entries: Vec<(String, String)>,
}

impl AudioFacts {
    /// What the Recorder wrote under this field, or `None` if it wrote nothing.
    ///
    /// The first, where a container allows several: ID3 permits repeated
    /// `COMM` frames, SDRTrunk writes one, and picking a later duplicate over
    /// the first would be a coin toss dressed up as a rule.
    pub fn field(&self, key: &str) -> Option<&str> {
        self.entries
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value.as_str())
    }

    /// Facts stated outright, for the tests of whatever *interprets* them.
    ///
    /// [`crate::mining`] is a dialect and this is a file format; its cases are
    /// about what SDRTrunk wrote, not about whether an ID3 frame decodes, and
    /// hand-rolling an MPEG stream per case would test this module over and
    /// over instead. The two meet for real in `tests/ingest.rs`.
    #[cfg(test)]
    pub fn from_fields(entries: impl IntoIterator<Item = (String, String)>) -> Self {
        AudioFacts {
            duration_ms: None,
            entries: entries.into_iter().collect(),
        }
    }
}

/// Probe `audio` once, for its length and for what its Recorder embedded in it.
///
/// Never an error and never a panic: this runs on the ingest path over bytes an
/// unauthenticated caller could have sent, and audio that cannot be read is a
/// Call that says nothing about itself, not a failed ingest.
pub fn read(audio: &[u8]) -> AudioFacts {
    let source = MediaSourceStream::new(
        Box::new(std::io::Cursor::new(audio.to_vec())),
        <_>::default(),
    );
    let Ok(mut format) = symphonia::default::get_probe().probe(
        &Hint::new(),
        source,
        Default::default(),
        MetadataOptions::default(),
    ) else {
        return AudioFacts::default();
    };
    // Read before the borrow below: `metadata()` borrows the reader mutably,
    // and the track is behind an immutable one.
    let duration_ms = track_duration_ms(format.as_ref());
    let entries = format
        .metadata()
        .current()
        .map(|revision| {
            revision
                .media
                .tags
                .iter()
                .map(|tag| (tag.raw.key.to_string(), tag.raw.value.to_string()))
                .collect()
        })
        .unwrap_or_default();
    AudioFacts {
        duration_ms,
        entries,
    }
}

/// The playing time of the default audio track, in milliseconds.
fn track_duration_ms(format: &dyn symphonia::core::formats::FormatReader) -> Option<i64> {
    let track = format.default_track(TrackType::Audio)?;
    let ticks = Timestamp::new(i64::try_from(track.duration?.get()).ok()?);
    let millis = track.time_base?.calc_time(ticks)?.as_millis();
    i64::try_from(millis).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::id3::{
        comment, frame, mpeg, synchsafe, tagged_mp3, text, text_encoded, wav,
    };
    use proptest::prelude::*;
    use rstest::rstest;

    // -- The length half (#42) ----------------------------------------------

    /// The whole point: a Call's length, without decoding a single sample.
    ///
    /// 12 000 frames at 8 kHz is 1500 ms — a value neither the frame count nor
    /// the sample rate reaches on its own, so an assertion on it cannot be
    /// satisfied by returning either one.
    #[test]
    fn a_wav_header_gives_its_length() {
        assert_eq!(read(&wav(12_000, 8_000)).duration_ms, Some(1500));
    }

    /// SDRTrunk uploads MP3, and a live-recorded one carries no Xing header to
    /// read a frame count out of — so this is the case that decides whether
    /// half the recorders in the field get a duration at all.
    ///
    /// 40 Layer III frames is 40 × 1152 samples at 44 100 Hz = 1044.9 ms.
    #[test]
    fn an_mp3_without_a_xing_header_still_gives_its_length() {
        assert_eq!(read(&mpeg(40)).duration_ms, Some(1044));
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
    fn audio_that_cannot_be_read_says_nothing(#[case] bytes: &[u8]) {
        assert_eq!(read(bytes), AudioFacts::default());
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
        assert_eq!(read(&truncated).duration_ms, Some(40_000));
    }

    // -- The embedded-metadata half (#48) -----------------------------------

    /// The reason this module stopped being `duration_ms`: **one** probe
    /// answers both questions, so ingest can mine a Call without reading its
    /// audio object back afterwards.
    #[test]
    fn one_probe_gives_both_the_length_and_the_embedded_fields() {
        let facts = read(&tagged_mp3(vec![text(b"TPE1", "1234567 Engine 1")]));

        assert_eq!(facts.duration_ms, Some(1044));
        assert_eq!(facts.field("TPE1"), Some("1234567 Engine 1"));
    }

    /// Every frame SDRTrunk writes comes back under its own name, including
    /// the `COMM` whose body carries a language and a description ahead of the
    /// text — the one frame here that is not simply "encoding, then string".
    #[test]
    fn every_embedded_field_comes_back_under_its_container_name() {
        let facts = read(&tagged_mp3(vec![
            text(b"TCOM", "sdrtrunk v0.6.1"),
            text(b"TPE1", "1234567 Engine 1"),
            text(b"TIT2", "54241\"Fire Dispatch\""),
            comment("Date:2026-08-11 09:00:00.000;Site:Downtown;Decoder:P25 Phase 1;"),
        ]));

        assert_eq!(facts.field("TCOM"), Some("sdrtrunk v0.6.1"));
        assert_eq!(facts.field("TPE1"), Some("1234567 Engine 1"));
        assert_eq!(facts.field("TIT2"), Some("54241\"Fire Dispatch\""));
        assert_eq!(
            facts.field("COMM"),
            Some("Date:2026-08-11 09:00:00.000;Site:Downtown;Decoder:P25 Phase 1;")
        );
    }

    /// A field nobody wrote is absent rather than empty — the distinction the
    /// mining rules are built on, since "the operator configured no alias" and
    /// "the operator configured an empty one" must not be the same answer.
    #[test]
    fn a_field_the_recorder_did_not_write_is_absent() {
        let facts = read(&tagged_mp3(vec![text(b"TPE1", "1234567")]));

        assert_eq!(facts.field("COMM"), None);
        assert_eq!(facts.field("TCOM"), None);
    }

    /// Trunk Recorder's WAV and M4A carry no ID3 at all, and neither does an
    /// MP3 written by anything that doesn't bother — so the common case is a
    /// readable Call with nothing embedded, which must not look like a failure.
    #[test]
    fn audio_with_no_embedded_metadata_still_gives_its_length() {
        let facts = read(&wav(8_000, 8_000));

        assert_eq!(facts.duration_ms, Some(1000));
        assert_eq!(facts.field("TPE1"), None);
    }

    /// **The one thing the fixtures here cannot decide.** Every other test in
    /// this file writes the tag *and* reads it, so it can only ever prove the
    /// two agree — but which of ID3v2's four text encodings SDRTrunk's writer
    /// (mp3agic) picks is a fact about somebody else's library, and it may pick
    /// a different one for a name with an accent in it than for one without.
    ///
    /// So all four are covered rather than guessed at: whichever mp3agic
    /// chooses, the radio's name comes back as the same string.
    #[rstest]
    #[case::iso_8859_1(0x00)]
    #[case::utf_16_with_a_bom(0x01)]
    #[case::utf_16_big_endian(0x02)]
    #[case::utf_8(0x03)]
    fn every_text_encoding_id3_allows_comes_back_as_the_same_string(#[case] encoding: u8) {
        let facts = read(&tagged_mp3(vec![text_encoded(
            b"TPE1",
            encoding,
            "1234567 Engine 1",
        )]));

        assert_eq!(facts.field("TPE1"), Some("1234567 Engine 1"));
    }

    /// ID3 lets a text frame hold several null-separated strings, and a v2.4
    /// writer may use it for anything. Whatever comes back must be one string
    /// this module can hand on, not a panic and not a silently dropped tail.
    #[test]
    fn a_multi_string_field_comes_back_whole() {
        let mut body = vec![0x03];
        body.extend(b"Engine 1\0Engine 2");
        let facts = read(&tagged_mp3(vec![frame(b"TPE1", body)]));

        let value = facts.field("TPE1").expect("the frame is there");
        assert!(value.contains("Engine 1"), "{value:?}");
        assert!(value.contains("Engine 2"), "{value:?}");
    }

    proptest! {
        /// Whatever arrives on the ingest path — a truncated file, a
        /// mislabelled container, an outright hostile body — reading it must
        /// never panic and never hang. A panic here would be a 500 on a
        /// recorder's upload.
        #[test]
        fn arbitrary_bytes_never_panic(bytes in proptest::collection::vec(any::<u8>(), 0..2048)) {
            let _ = read(&bytes);
        }

        /// ...including bytes that got far enough to look like a RIFF file,
        /// which is where a length field is read and arithmetic happens.
        #[test]
        fn arbitrary_riff_bodies_never_panic(tail in proptest::collection::vec(any::<u8>(), 0..512)) {
            let mut bytes = b"RIFF".to_vec();
            bytes.extend(tail);
            let _ = read(&bytes);
        }

        /// ...and bytes that look like an ID3 tag, which is the new surface:
        /// a frame length is attacker-controlled and is used to slice.
        #[test]
        fn arbitrary_id3_bodies_never_panic(tail in proptest::collection::vec(any::<u8>(), 0..512)) {
            let mut bytes = b"ID3\x04\x00\x00".to_vec();
            bytes.extend(synchsafe(tail.len() as u32));
            bytes.extend(tail);
            bytes.extend(mpeg(4));
            let _ = read(&bytes);
        }
    }
}
