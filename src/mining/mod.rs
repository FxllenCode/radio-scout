//! **Mining** (CONTEXT.md, #48, spec US 13): reading what a Recorder wrote
//! *inside* a Call's audio, and folding it into the Archive.
//!
//! SDRTrunk cannot have a plugin — its broadcast formats are compiled-in
//! classes with no extension point (CLAUDE.md) — so the usual answer to "teach
//! the recorder to send more" is closed. It turns out not to be needed: every
//! MP3 SDRTrunk uploads *already* carries an ID3v2.4 tag, written by
//! `AudioSegmentRecorder.recordMP3` before the first MPEG frame and uploaded
//! verbatim by `RdioScannerBroadcaster` (`Files.readAllBytes` of the temp file
//! it just wrote). Nothing has to change on the recorder at all; the facts were
//! being stored and thrown away.
//!
//! What that tag holds that the wire does not:
//!
//! | Frame | SDRTrunk source | Worth |
//! | --- | --- | --- |
//! | `TCOM` | the application name | says this is SDRTrunk's, and nothing else does |
//! | `TPE1` | the FROM radio, then `aliasList.getAliases(from)` | **the configured radio alias — sent on no wire field at all** |
//! | `COMM` | `Site:`, `Decoder:`, `Frequency:` … | the tower's name, which the rdio dialect has no field for |
//!
//! `TIT2`, `TALB` and `TIT1` are deliberately ignored: they are the Talkgroup
//! and System labels, which SDRTrunk already sends as `talkgroupLabel` and
//! `systemLabel`. Mining them would be re-deriving a wire field from a worse
//! copy of itself.
//!
//! **The alias here is the *configured* one, not the OTA alias.**
//! `aliasList.getAliases(from)` is SDRTrunk's own alias list — a name an
//! Operator wrote down — where `talkerAlias`, which the wire *does* carry, is
//! what the radio broadcast about itself. CONTEXT.md keeps those apart on
//! purpose, so this fills `label` and never `tag_ota`.
//!
//! Everything in this module is pure: bytes in, a value out, nothing awaited
//! and nothing read. Both callers — [`crate::ingest`] mining a Call as it
//! arrives, and [`sweep`] mining one already stored — decide what to *do* with
//! the result.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::audio_meta::AudioFacts;

pub mod sweep;

/// How often the sweep runs, by default.
///
/// Short, because the sweep it schedules is bounded: the cost of a tick with
/// nothing to do is one indexed query that finds no rows, which is what every
/// tick looks like for the whole life of an Instance once the Archive has been
/// walked once.
const DEFAULT_INTERVAL: Duration = Duration::from_secs(30);

/// How many Calls one sweep reads, by default.
///
/// With [`DEFAULT_INTERVAL`] this is 12 000 Calls an hour — a hundred thousand
/// in a little under nine hours, which is the shape of archive this exists for
/// and slow enough that a Pi taking a Call a second never notices.
const DEFAULT_BATCH_SIZE: u64 = 100;

/// The `[mining]` section (ADR-0012, #87) — one type, so the file's defaults
/// and the sweep's are the same values.
///
/// It governs the [`sweep`] alone. Mining a Call **as it arrives** is not a
/// setting and never has been: it is a header read inside a probe ingest
/// already makes, and there is no configuration in which an Operator would want
/// their Recorder's own facts thrown away. Which is why the switch below is
/// spelled `sweep` rather than `enabled` — turning it off leaves new Calls
/// mined and stops the walk over the old ones.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MiningConfig {
    /// Whether the sweep over the existing Archive runs at all.
    ///
    /// **On**, unlike Enhancement's master switch. The two are different kinds
    /// of feature: enhancement rewrites audio and costs CPU per Call forever,
    /// where this reads each Call once and then has nothing left to do. An
    /// Operator upgrading into #48 should get their archive's names without
    /// having to find a setting — and the one who does not want the object-store
    /// reads has this.
    pub sweep: bool,
    /// How long between sweeps.
    #[serde(rename = "interval_secs", with = "crate::config::secs")]
    pub interval: Duration,
    /// How many Calls one sweep reads.
    pub batch_size: u64,
}

impl Default for MiningConfig {
    fn default() -> Self {
        MiningConfig {
            sweep: true,
            interval: DEFAULT_INTERVAL,
            batch_size: DEFAULT_BATCH_SIZE,
        }
    }
}

impl MiningConfig {
    /// The cadence to run at, reading a zero as "unset" — the reading
    /// [`crate::retention::RetentionConfig`] gives its own interval.
    pub fn effective_interval(&self) -> Duration {
        match self.interval.is_zero() {
            true => DEFAULT_INTERVAL,
            false => self.interval,
        }
    }
}

/// The ID3 frame naming the program that wrote the file.
const COMPOSER: &str = "TCOM";
/// The ID3 frame holding the FROM radio and its configured aliases.
const ARTIST: &str = "TPE1";
/// The ID3 frame holding SDRTrunk's `Key:Value;` run.
const COMMENT: &str = "COMM";

/// How SDRTrunk names itself in [`COMPOSER`].
///
/// A prefix, not the whole value: `SystemProperties.getApplicationName()` is
/// `"sdrtrunk"` plus whatever the build manifest adds — `"sdrtrunk v0.6.1"`,
/// `"sdrtrunk nightly - <timestamp>"` — so matching the whole string would mean
/// mining only the one release somebody happened to test against.
const SDRTRUNK: &str = "sdrtrunk";

/// The tower this channel was configured to receive, as a name.
const SITE_KEY: &str = "Site";
/// What the channel was being demodulated as (`P25 Phase 1`, `DMR`).
const DECODER_KEY: &str = "Decoder";
/// The channel's centre frequency, in hertz.
const FREQUENCY_KEY: &str = "Frequency";

/// What SDRTrunk said about a Call, inside the Call.
///
/// Every field is optional and independently so: an Operator who configured
/// site names but no radio aliases gets the sites, which is most of them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Mined {
    /// The radio that keyed, and what its Operator calls it.
    pub unit: Option<MinedUnit>,
    /// The **Site** the channel was tuned to, by name. The rdio upload dialect
    /// has no field for this and SDRTrunk sends none, so for an SDRTrunk Call
    /// this is the *only* way a tower is ever named.
    pub site: Option<String>,
    /// What the recorder was demodulating, in SDRTrunk's vocabulary — the same
    /// question `calls.audio_type` records for Trunk Recorder.
    pub decoder: Option<String>,
    /// The channel's frequency in hertz. Usually redundant — SDRTrunk sends
    /// `frequency` on the wire from the same identifier — which is exactly why
    /// mining only ever *fills* and never overwrites: on a Call that already
    /// has one this costs nothing and changes nothing.
    pub frequency: Option<i64>,
}

impl Mined {
    /// Whether this is worth writing anything for.
    ///
    /// A [`MinedUnit`] only exists when it carries a name, so there is no arm
    /// here for "a radio Ref and nothing to call it" — that is not something
    /// the Archive can be any richer for.
    pub fn is_empty(&self) -> bool {
        self.unit.is_none()
            && self.site.is_none()
            && self.decoder.is_none()
            && self.frequency.is_none()
    }
}

/// A radio SDRTrunk had a name for.
///
/// Carries the Ref as well as the name because the name has to be *checked*
/// before it is believed: the tag is the Recorder's snapshot of one moment, and
/// applying it to whichever radio the Call happens to list would put one
/// apparatus's name on another's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MinedUnit {
    pub unit_ref: i64,
    pub label: String,
}

/// What SDRTrunk embedded in this audio, or `None` if SDRTrunk did not write it.
///
/// The gate is [`COMPOSER`], and it is deliberately the *only* gate. Without
/// it, `TPE1` on any MP3 ever uploaded is read as a radio id followed by a
/// name — and "50 Cent" is a perfectly good radio 50 called "Cent". Requiring
/// the recorder to have identified itself makes that unreachable rather than
/// merely unlikely.
pub fn mine(facts: &AudioFacts) -> Option<Mined> {
    let composer = facts.field(COMPOSER)?;
    if !composer
        .trim_start()
        .to_ascii_lowercase()
        .starts_with(SDRTRUNK)
    {
        return None;
    }
    let comment = facts.field(COMMENT).unwrap_or_default();
    Some(Mined {
        unit: facts.field(ARTIST).and_then(mine_unit),
        site: comment_value(comment, SITE_KEY).map(str::to_string),
        decoder: comment_value(comment, DECODER_KEY).map(str::to_string),
        // `0` is SDRTrunk's "no frequency" — `getFrequency` returns a boxed
        // zero rather than nothing — so it is absence, not a tuning of 0 Hz.
        frequency: comment_value(comment, FREQUENCY_KEY)
            .and_then(|value| value.parse().ok())
            .filter(|hz| *hz > 0),
    })
}

/// The radio and alias inside `TPE1`.
///
/// SDRTrunk builds this as the FROM identifier's own rendering followed by each
/// configured alias, space-separated (`AudioMetadataUtils.getMetadataMap`), so
/// `1234567 Engine 1` is radio 1234567 called "Engine 1".
///
/// The leading token has to be **entirely** a Ref, optionally trailed by a
/// parenthesised fully-qualified address (`1234567(9BF00.1.7777)`, a roaming
/// radio SDRTrunk knows locally). Anything else — a bare `9BF00.1.7777` for a
/// radio it has *no* local number for, or a `TalkerAliasIdentifier` that stood
/// in because there was no radio at all — yields nothing, because there is no
/// honest Ref to hang the name on and a leading-digits guess would invent one.
fn mine_unit(artist: &str) -> Option<MinedUnit> {
    let (token, rest) = match artist.trim().split_once(char::is_whitespace) {
        Some((token, rest)) => (token, rest.trim()),
        None => (artist.trim(), ""),
    };
    // A closed parenthesised remainder is allowed and nothing else is: a token
    // of `1234567x` is not a Ref that happens to have a suffix, it is a value
    // this parser does not understand.
    //
    // Written as a match on the split rather than a chain of guards because the
    // guards overlapped: an `is_empty()` check here was unkillable by
    // construction, since an empty string is exactly what the parse below
    // refuses. Two ways to say the same "no" is one way too many (#83).
    let digits = match token.split_once('(') {
        Some((digits, qualified)) if qualified.ends_with(')') => digits,
        Some(_) => return None,
        None => token,
    };
    let unit_ref: i64 = digits.parse().ok().filter(|value| *value > 0)?;
    (!rest.is_empty()).then(|| MinedUnit {
        unit_ref,
        label: rest.to_string(),
    })
}

/// The value SDRTrunk wrote for `key` in its comment run.
///
/// The format is `Key:Value;Key:Value;` (`COMMENT_SEPARATOR` in
/// `AudioMetadataUtils`), so a value keeps every colon after the first — which
/// matters, because the first entry is a timestamp full of them.
///
/// A value containing a `;` is truncated at it, and deliberately not worked
/// around: SDRTrunk escapes nothing, so the separator genuinely is ambiguous in
/// its own format, and a parser that guessed would be guessing.
fn comment_value<'a>(comment: &'a str, key: &str) -> Option<&'a str> {
    comment
        .split(';')
        .filter_map(|entry| entry.split_once(':'))
        .find(|(name, _)| name.trim() == key)
        .map(|(_, value)| value.trim())
        .filter(|value| !value.is_empty())
}

// -- Folding it into a Call ---------------------------------------------------

/// Fold what was mined into a Call an ingest is about to store. `true` if it
/// added anything.
///
/// **Fills and never overwrites**, in every field. The wire is the Recorder
/// speaking now and the container is a snapshot it wrote earlier; where both
/// answer, the live one is believed. It is also the auto-populate rule (#8)
/// applied one layer up — a curated name must survive the next upload, or every
/// Call would quietly undo the admin surface.
///
/// **A name is hung only on a radio this Call actually heard.** The tag names
/// one radio at one moment, and applying it to whichever Unit the Call happens
/// to list would put an apparatus's name on a different apparatus — a wrong
/// answer that looks exactly like a right one.
///
/// The stored-Call counterpart is [`crate::db::repo::apply_mined`], and the two
/// are held to landing the identical Call by `tests/mining.rs`, the way #44
/// holds the upload script and the plugin together.
pub fn apply(call: &mut crate::db::repo::NewCall, mined: &Mined) -> bool {
    let mut applied = fill(&mut call.frequency, mined.frequency);
    applied |= fill(&mut call.audio_type, mined.decoder.clone());
    applied |= fill(&mut call.site_label, mined.site.clone());
    if let Some(unit) = &mined.unit
        && let Some(heard) = call
            .units
            .iter_mut()
            .find(|heard| heard.unit_ref == unit.unit_ref)
    {
        applied |= fill(&mut heard.label, Some(unit.label.clone()));
    }
    applied
}

/// Put `value` in `target` if `target` is empty. `true` if it went in.
///
/// One function so "fills and never overwrites" is a single statement per
/// field rather than a per-field `if` an edit could get backwards — and so the
/// two application paths can be read side by side and seen to agree.
pub(crate) fn fill<T>(target: &mut Option<T>, value: Option<T>) -> bool {
    match (&target, value) {
        (None, Some(value)) => {
            *target = Some(value);
            true
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::repo::{NewCall, NewCallUnit};
    use proptest::prelude::*;
    use rstest::rstest;

    /// The facts a probe would hand back for an SDRTrunk upload with these
    /// frames. Built as pairs rather than through [`crate::audio_meta::read`]
    /// so a failure here is this module's and not the probe's — the two meet
    /// for real in `tests/ingest.rs`.
    fn facts(entries: &[(&str, &str)]) -> AudioFacts {
        AudioFacts::from_fields(entries.iter().map(|(k, v)| (k.to_string(), v.to_string())))
    }

    /// A complete SDRTrunk tag, as `getMetadataMap` assembles one for a P25
    /// call from a radio the operator has named.
    fn sdrtrunk_call() -> AudioFacts {
        facts(&[
            ("TCOM", "sdrtrunk v0.6.1"),
            ("TPE1", "1234567 Engine 1"),
            ("TIT2", "54241\"Fire Dispatch\""),
            ("TALB", "Fulton Control 1"),
            (
                "COMM",
                "Date:2026-08-11 09:00:00.000;System:Fulton;Site:Downtown;\
                 Name:Control 1;Decoder:P25 Phase 1;Frequency:851012500;",
            ),
        ])
    }

    /// The whole ticket in one assertion: everything SDRTrunk buried in the
    /// audio, out.
    #[test]
    fn a_complete_sdrtrunk_tag_gives_up_everything_it_holds() {
        assert_eq!(
            mine(&sdrtrunk_call()),
            Some(Mined {
                unit: Some(MinedUnit {
                    unit_ref: 1234567,
                    label: "Engine 1".into()
                }),
                site: Some("Downtown".into()),
                decoder: Some("P25 Phase 1".into()),
                frequency: Some(851012500),
            })
        );
    }

    /// The gate. Without it, every MP3 in the archive is read as though its
    /// artist were a radio id — and plenty of them parse.
    #[rstest]
    #[case::nothing_at_all(&[])]
    #[case::no_composer(&[("TPE1", "1234567 Engine 1")])]
    #[case::somebody_elses_encoder(&[("TCOM", "LAME 3.100"), ("TPE1", "50 Cent")])]
    #[case::a_recorder_that_only_looks_similar(&[("TCOM", "trunk-recorder"), ("TPE1", "1 A")])]
    fn audio_no_sdrtrunk_wrote_is_not_mined(#[case] entries: &[(&str, &str)]) {
        assert_eq!(mine(&facts(entries)), None);
    }

    /// The application name carries a build stamp, so the gate is a prefix —
    /// and matching the whole string would mine exactly one release.
    #[rstest]
    #[case::bare("sdrtrunk")]
    #[case::released("sdrtrunk v0.6.1")]
    #[case::nightly("sdrtrunk nightly - 2026-08-01")]
    #[case::shouted("SDRTrunk v0.6.1")]
    fn every_spelling_of_sdrtrunks_own_name_is_recognised(#[case] composer: &str) {
        let mined = mine(&facts(&[("TCOM", composer), ("TPE1", "7 Car 7")]));

        assert_eq!(mined.and_then(|m| m.unit).map(|u| u.unit_ref), Some(7));
    }

    /// A radio's Ref and its name, out of one space-separated string. The
    /// multi-word case is the normal one — apparatus names have spaces — and
    /// the multi-alias case is SDRTrunk joining several aliases with more.
    #[rstest]
    #[case::one_word("1234567 Engine", 1234567, "Engine")]
    #[case::a_name_with_spaces("1234567 Engine 1", 1234567, "Engine 1")]
    #[case::several_aliases("42 Engine 1 Battalion 3", 42, "Engine 1 Battalion 3")]
    #[case::a_roaming_radio_known_locally("1234567(9BF00.1.7777) Medic 4", 1234567, "Medic 4")]
    #[case::padded("  99   Ladder 9  ", 99, "Ladder 9")]
    fn a_radio_names_itself_in_the_artist_field(
        #[case] artist: &str,
        #[case] unit_ref: i64,
        #[case] label: &str,
    ) {
        assert_eq!(
            mine_unit(artist),
            Some(MinedUnit {
                unit_ref,
                label: label.into()
            })
        );
    }

    /// Everything that is *not* a Ref followed by a name. Each of these would
    /// otherwise put a name on a radio that never keyed:
    ///
    /// - a bare Ref is SDRTrunk saying it has no alias configured, which is the
    ///   overwhelmingly common case and must cost nothing;
    /// - a fully-qualified address with no local number is a roaming radio we
    ///   cannot number, so there is nothing to hang the name on;
    /// - a `TalkerAliasIdentifier` stands in when there is no radio at all, and
    ///   its text is a name with no Ref in front of it;
    /// - `0` is `getFrom`'s own "nobody".
    #[rstest]
    #[case::a_bare_ref("1234567")]
    #[case::a_bare_ref_padded("  1234567  ")]
    #[case::an_unnumbered_roaming_radio("9BF00.1.7777 Medic 4")]
    #[case::a_talker_alias_standing_in("ENGINE ONE")]
    #[case::nobody("0 Engine 1")]
    #[case::empty("")]
    #[case::a_ref_with_a_suffix("1234567x Engine 1")]
    #[case::an_unclosed_qualifier("1234567(9BF00 Medic 4")]
    #[case::negative("-5 Engine 1")]
    fn an_artist_field_that_names_no_radio_yields_none(#[case] artist: &str) {
        assert_eq!(mine_unit(artist), None);
    }

    /// SDRTrunk's comment run is `Key:Value;` pairs, and the first value is a
    /// timestamp — so a value keeps every colon after the first. A parser that
    /// split on *every* colon would truncate the one entry that proves it.
    #[test]
    fn a_comment_value_keeps_the_colons_inside_it() {
        assert_eq!(
            comment_value("Date:2026-08-11 09:00:00.000;Site:Downtown;", "Date"),
            Some("2026-08-11 09:00:00.000")
        );
    }

    /// Every field is independently optional: SDRTrunk writes a `Site:` entry
    /// only for a channel configured with one, a `Decoder:` only where the
    /// decoder type is known, and so on. An Operator who named their towers but
    /// not their radios must still get their towers.
    #[rstest]
    #[case::site_only("Site:Downtown;", Some("Downtown"), None, None)]
    #[case::decoder_only("Decoder:DMR;", None, Some("DMR"), None)]
    #[case::frequency_only("Frequency:851012500;", None, None, Some(851012500))]
    #[case::none_of_them("Date:2026-08-11 09:00:00.000;System:Fulton;", None, None, None)]
    #[case::empty("", None, None, None)]
    fn each_comment_entry_is_optional_on_its_own(
        #[case] comment: &str,
        #[case] site: Option<&str>,
        #[case] decoder: Option<&str>,
        #[case] frequency: Option<i64>,
    ) {
        let mined = mine(&facts(&[("TCOM", "sdrtrunk"), ("COMM", comment)])).expect("sdrtrunk's");

        assert_eq!(mined.site.as_deref(), site);
        assert_eq!(mined.decoder.as_deref(), decoder);
        assert_eq!(mined.frequency, frequency);
    }

    /// `getFrequency` returns a boxed zero when the channel has no frequency
    /// identifier, so `0` is absence. Stored as a tuning it would show a
    /// Listener a Call heard at 0 Hz.
    #[rstest]
    #[case::zero("Frequency:0;")]
    #[case::not_a_number("Frequency:wideband;")]
    #[case::empty("Frequency:;")]
    #[case::negative("Frequency:-1;")]
    fn a_frequency_that_is_not_a_tuning_is_absent(#[case] comment: &str) {
        let mined = mine(&facts(&[("TCOM", "sdrtrunk"), ("COMM", comment)])).expect("sdrtrunk's");

        assert_eq!(mined.frequency, None);
    }

    /// An SDRTrunk Call with nothing configured is the zero-configuration
    /// install, and it is a *skip*, not a failure — the sweep has to be able
    /// to tell "I looked and there was nothing" from "I could not look".
    #[test]
    fn an_sdrtrunk_tag_with_nothing_configured_is_empty_but_still_sdrtrunks() {
        let mined = mine(&facts(&[
            ("TCOM", "sdrtrunk"),
            ("TPE1", "1234567"),
            ("COMM", "Date:2026-08-11 09:00:00.000;"),
        ]))
        .expect("sdrtrunk wrote it");

        assert!(mined.is_empty());
    }

    /// ...and one that yields anything at all is not empty, so a Call is never
    /// skipped as barren while holding a name.
    #[rstest]
    #[case::a_named_radio(&[("TPE1", "1 Engine 1")])]
    #[case::a_tower(&[("COMM", "Site:Downtown;")])]
    #[case::a_decoder(&[("COMM", "Decoder:DMR;")])]
    #[case::a_frequency(&[("COMM", "Frequency:851012500;")])]
    fn anything_worth_writing_makes_it_not_empty(#[case] extra: &[(&str, &str)]) {
        let mut entries = vec![("TCOM", "sdrtrunk")];
        entries.extend_from_slice(extra);

        assert!(!mine(&facts(&entries)).expect("sdrtrunk's").is_empty());
    }

    proptest! {
        /// The archive is full of files nobody here wrote, and the sweep
        /// reads every one of them. Nothing in this module may panic on any of
        /// it — a panic in the worker is an Instance that stops mining.
        #[test]
        fn arbitrary_fields_never_panic(
            composer in "\\PC{0,32}",
            artist in "\\PC{0,64}",
            comment in "\\PC{0,128}",
        ) {
            let _ = mine(&facts(&[
                ("TCOM", &composer),
                ("TPE1", &artist),
                ("COMM", &comment),
            ]));
        }

        /// A mined Ref is always one a radio could really have: positive, and
        /// exactly the digits that were written down. Everything downstream
        /// compares it against the Call's own radio, and a `0` or a negative
        /// would compare equal to something eventually.
        #[test]
        fn a_mined_ref_is_always_a_ref_a_radio_could_have(
            unit_ref in 1i64..i64::MAX,
            label in "[a-zA-Z][a-zA-Z0-9 ]{0,20}",
        ) {
            let mined = mine_unit(&format!("{unit_ref} {label}"));

            prop_assert_eq!(mined.map(|u| u.unit_ref), Some(unit_ref));
        }
    }

    // -- Folding it into a Call -------------------------------------------

    /// A Call as an rdio-dialect upload from SDRTrunk builds one: one radio
    /// heard, no tower, no decoder, and whatever the wire said about frequency.
    fn arriving(frequency: Option<i64>, heard: i64) -> NewCall {
        NewCall {
            frequency,
            units: vec![NewCallUnit {
                unit_ref: heard,
                ..Default::default()
            }],
            ..NewCall::new(11, 54241, 1_000)
        }
    }

    /// Everything mined lands on the Call, in the columns that hold it.
    #[test]
    fn what_was_mined_lands_on_the_call() {
        let mut call = arriving(None, 1234567);

        let applied = apply(
            &mut call,
            &Mined {
                unit: Some(MinedUnit {
                    unit_ref: 1234567,
                    label: "Engine 1".into(),
                }),
                site: Some("Downtown".into()),
                decoder: Some("P25 Phase 1".into()),
                frequency: Some(851012500),
            },
        );

        assert!(applied);
        assert_eq!(call.units[0].label.as_deref(), Some("Engine 1"));
        assert_eq!(call.site_label.as_deref(), Some("Downtown"));
        assert_eq!(call.audio_type.as_deref(), Some("P25 Phase 1"));
        assert_eq!(call.frequency, Some(851012500));
    }

    /// Nothing the Recorder already said is touched. Each case fills exactly
    /// one field on the arriving Call and mines a *different* value for it.
    #[test]
    fn what_the_recorder_already_said_is_never_overwritten() {
        let mut call = NewCall {
            audio_type: Some("digital".into()),
            ..arriving(Some(851012500), 1234567)
        };
        call.units[0].label = Some("Battalion 3".into());

        let applied = apply(
            &mut call,
            &Mined {
                unit: Some(MinedUnit {
                    unit_ref: 1234567,
                    label: "Engine 1".into(),
                }),
                site: None,
                decoder: Some("P25 Phase 1".into()),
                frequency: Some(99_000_000),
            },
        );

        assert!(!applied, "there was nothing left to fill");
        assert_eq!(call.units[0].label.as_deref(), Some("Battalion 3"));
        assert_eq!(call.audio_type.as_deref(), Some("digital"));
        assert_eq!(call.frequency, Some(851012500));
    }

    /// The name goes on the radio the tag named, and on no other. A Call that
    /// heard a different radio — or heard none — gains nothing, because a name
    /// on the wrong apparatus is worse than no name.
    #[rstest]
    #[case::a_different_radio(vec![7_654_321])]
    #[case::several_others(vec![1, 2, 3])]
    #[case::nobody(vec![])]
    fn a_name_never_lands_on_a_radio_the_call_did_not_hear(#[case] heard: Vec<i64>) {
        let mut call = NewCall {
            units: heard
                .into_iter()
                .map(|unit_ref| NewCallUnit {
                    unit_ref,
                    ..Default::default()
                })
                .collect(),
            ..NewCall::new(11, 54241, 1_000)
        };

        let applied = apply(
            &mut call,
            &Mined {
                unit: Some(MinedUnit {
                    unit_ref: 1234567,
                    label: "Engine 1".into(),
                }),
                ..Default::default()
            },
        );

        assert!(!applied);
        assert!(call.units.iter().all(|heard| heard.label.is_none()));
    }

    /// ...and where the Call heard several radios, it lands on the one named
    /// rather than on the first.
    #[test]
    fn a_name_lands_on_the_radio_it_names_and_not_the_first_one() {
        let mut call = NewCall {
            units: [11, 1234567, 22]
                .into_iter()
                .map(|unit_ref| NewCallUnit {
                    unit_ref,
                    ..Default::default()
                })
                .collect(),
            ..NewCall::new(11, 54241, 1_000)
        };

        apply(
            &mut call,
            &Mined {
                unit: Some(MinedUnit {
                    unit_ref: 1234567,
                    label: "Engine 1".into(),
                }),
                ..Default::default()
            },
        );

        let named: Vec<_> = call
            .units
            .iter()
            .map(|heard| (heard.unit_ref, heard.label.clone()))
            .collect();
        assert_eq!(
            named,
            vec![(11, None), (1234567, Some("Engine 1".into())), (22, None),]
        );
    }

    /// **Any one field on its own makes this an application**, and each is
    /// folded in by its own statement.
    ///
    /// A table over *which single thing is new* rather than a case or two,
    /// because the fold is four `|=` in a row and a test that fills several at
    /// once cannot tell them apart: with everything new, an `&=` reports `true`
    /// just the same. What that would cost is a Call reported as `nothing-new`
    /// after gaining a tower — the sweep would stamp it and move on, correctly,
    /// but its own log would say it had found nothing.
    ///
    /// The Call here already knows all four, so each case takes exactly one
    /// away and offers a mined value for it.
    #[rstest]
    #[case::only_the_frequency(true, false, false, false)]
    #[case::only_the_decoder(false, true, false, false)]
    #[case::only_the_tower(false, false, true, false)]
    #[case::only_the_radios_name(false, false, false, true)]
    fn any_one_field_on_its_own_is_an_application(
        #[case] wants_frequency: bool,
        #[case] wants_decoder: bool,
        #[case] wants_site: bool,
        #[case] wants_unit: bool,
    ) {
        // Everything the Call is *not* missing, it already knows — so exactly
        // one `fill` below can return true.
        let mut call = NewCall {
            frequency: (!wants_frequency).then_some(851012500),
            audio_type: (!wants_decoder).then(|| "digital".to_string()),
            site_label: (!wants_site).then(|| "Courthouse".to_string()),
            units: vec![NewCallUnit {
                unit_ref: 1234567,
                label: (!wants_unit).then(|| "Battalion 3".to_string()),
                ..Default::default()
            }],
            ..NewCall::new(11, 54241, 1_000)
        };

        let applied = apply(
            &mut call,
            &Mined {
                unit: Some(MinedUnit {
                    unit_ref: 1234567,
                    label: "Engine 1".into(),
                }),
                site: Some("Downtown".into()),
                decoder: Some("P25 Phase 1".into()),
                frequency: Some(99_000_000),
            },
        );

        assert!(applied, "the one thing the Call did not know was new");
        // ...and only that one moved: everything else kept what it had.
        assert_eq!(
            call.frequency,
            Some(if wants_frequency {
                99_000_000
            } else {
                851012500
            })
        );
        assert_eq!(
            call.audio_type.as_deref(),
            Some(if wants_decoder {
                "P25 Phase 1"
            } else {
                "digital"
            })
        );
        assert_eq!(
            call.site_label.as_deref(),
            Some(if wants_site { "Downtown" } else { "Courthouse" })
        );
        assert_eq!(
            call.units[0].label.as_deref(),
            Some(if wants_unit {
                "Engine 1"
            } else {
                "Battalion 3"
            })
        );
    }

    /// Nothing mined is nothing applied — which is what lets the sweep mark
    /// a Call looked-at without writing to it.
    #[test]
    fn an_empty_mining_changes_nothing() {
        let mut call = arriving(None, 1234567);
        let before = format!("{call:?}");

        assert!(!apply(&mut call, &Mined::default()));
        assert_eq!(format!("{call:?}"), before);
    }

    // -- The `[mining]` section -------------------------------------------

    /// A zero interval is "unset" rather than a spin — the reading
    /// [`crate::retention::RetentionConfig`] gives its own.
    #[rstest]
    #[case::unset(Duration::ZERO, DEFAULT_INTERVAL)]
    #[case::configured(Duration::from_secs(600), Duration::from_secs(600))]
    fn a_zero_interval_falls_back_to_the_default(
        #[case] configured: Duration,
        #[case] expected: Duration,
    ) {
        let config = MiningConfig {
            interval: configured,
            ..MiningConfig::default()
        };

        assert_eq!(config.effective_interval(), expected);
    }

    /// The shipped posture, stated outright: on, because it reads each Call
    /// once and then has nothing left to do.
    #[test]
    fn the_shipped_default_mines() {
        assert!(MiningConfig::default().sweep);
    }
}
