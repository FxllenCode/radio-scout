//! **SDRTrunk**'s recordings, read from the ID3 tag it writes into every MP3
//! (`AudioMetadataUtils.getMetadataMap`; rdio's `parsers.go:ParseSdrTrunkMeta`).
//!
//! A dropped SDRTrunk file has no wire fields at all, so the tag is the whole of
//! what is known — which is why this reads the frames **Mining** (#48)
//! deliberately skips. On an upload `TIT2` and `TALB` are worse copies of
//! `talkgroupLabel` and `systemLabel`; here they are the only copies there are.
//!
//! | Frame | SDRTrunk writes | Read as |
//! | --- | --- | --- |
//! | `TIT2` | the TO identifier, then its aliases quoted: `54241"Fire Dispatch"`, or `P:1234 [54241, 54242]` for a patch | Talkgroup Ref, label, **Patches** |
//! | `TPE1` | the FROM radio, then its aliases | the radio's Ref (its name is Mining's, applied at ingest as for every SDRTrunk Call) |
//! | `COMM` | `Date:…;System:…;Site:…;Frequency:…;` | when, and which System by name |
//! | `TIT1` | the System's name again | the System, when `COMM` has none |
//!
//! **When is the tag's `Date:` minus the Call's length.** SDRTrunk stamps the
//! moment it *wrote the file* — the end of the transmission — where its own
//! HTTP upload sends the start (`RdioScannerBroadcaster`: `getStartTime()`).
//! rdio files a dropped Call at its end, so the same transmission arrives seconds
//! apart by the two routes and an archive sorted by time is out of order by
//! every Call's length.

use chrono::{NaiveDateTime, TimeZone};

use super::mask::local_instant;
use super::{Described, Named};
use crate::audio_meta::AudioFacts;
use crate::mining::{ARTIST, COMMENT, comment_value, radio};

/// The frame holding the TO identifier and its aliases.
const TITLE: &str = "TIT2";
/// The frame SDRTrunk writes the System's name into a second time.
const GROUPING: &str = "TIT1";

/// What SDRTrunk's tag says about the recording `facts` were read from.
pub fn read<Tz: TimeZone>(facts: &AudioFacts, zone: &Tz) -> Described {
    let title = facts.field(TITLE).unwrap_or_default();
    let comment = facts.field(COMMENT).unwrap_or_default();

    Described {
        talkgroup_ref: first_number(title),
        talkgroup_label: quoted(title),
        patches: bracketed(title),
        unit: facts
            .field(ARTIST)
            .and_then(radio)
            .map(|(unit_ref, _)| (unit_ref, None)),
        system: comment_value(comment, "System")
            .or_else(|| facts.field(GROUPING).map(str::trim))
            .filter(|label| !label.is_empty())
            .map(|label| Named::Label(label.to_owned())),
        call_at_ms: comment_value(comment, "Date")
            .and_then(|date| NaiveDateTime::parse_from_str(date, "%Y-%m-%d %H:%M:%S%.f").ok())
            .and_then(|written| local_instant(written, zone))
            .map(|written| written - facts.duration_ms.unwrap_or_default()),
        ..Described::default()
    }
}

/// The first run of digits — the TO identifier's own number, after any prefix
/// SDRTrunk's rendering of it carries (`P:`, `TG:`).
fn first_number(title: &str) -> Option<i64> {
    let start = title.find(|c: char| c.is_ascii_digit())?;
    let digits: String = title[start..]
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    digits.parse().ok().filter(|n| *n > 0)
}

/// The first alias SDRTrunk quoted after the identifier.
fn quoted(title: &str) -> Option<String> {
    let (_, rest) = title.split_once('"')?;
    let (alias, _) = rest.split_once('"')?;
    let alias = alias.trim();
    (!alias.is_empty()).then(|| alias.to_owned())
}

/// A patch group's members — Java's `List.toString`, `[54241, 54242]`.
fn bracketed(title: &str) -> Vec<i64> {
    let Some((_, rest)) = title.split_once('[') else {
        return Vec::new();
    };
    let Some((list, _)) = rest.split_once(']') else {
        return Vec::new();
    };
    list.split(',')
        .filter_map(|member| member.trim().parse().ok())
        .filter(|member: &i64| *member > 0)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{FixedOffset, Utc};
    use proptest::prelude::*;
    use rstest::rstest;

    fn eastern() -> FixedOffset {
        FixedOffset::west_opt(5 * 3600).expect("an offset")
    }

    fn facts(entries: &[(&str, &str)]) -> AudioFacts {
        AudioFacts::from_fields(entries.iter().map(|(k, v)| (k.to_string(), v.to_string())))
    }

    /// A whole tag, as `getMetadataMap` builds one for a P25 Call from a radio
    /// the Operator has named.
    #[test]
    fn a_complete_tag_says_everything_a_dropped_file_can() {
        let described = read(
            &facts(&[
                ("TCOM", "sdrtrunk v0.6.1"),
                ("TPE1", "1234567 Engine 1"),
                ("TIT2", "54241\"Fire Dispatch\""),
                ("TIT1", "Fulton"),
                (
                    "COMM",
                    "Date:2026-08-11 09:00:00.000;System:Fulton;Site:Downtown;Frequency:851012500;",
                ),
            ]),
            &eastern(),
        );

        assert_eq!(described.talkgroup_ref, Some(54241));
        assert_eq!(described.talkgroup_label.as_deref(), Some("Fire Dispatch"));
        assert_eq!(described.unit, Some((1_234_567, None)));
        assert_eq!(described.system, Some(Named::Label("Fulton".into())));
        assert_eq!(
            described.call_at_ms,
            Some(
                Utc.with_ymd_and_hms(2026, 8, 11, 14, 0, 0)
                    .unwrap()
                    .timestamp_millis()
            )
        );
    }

    /// The tag is stamped when the file was written, so the Call began its own
    /// length earlier.
    #[test]
    fn the_call_began_its_length_before_the_tag_was_written() {
        let mut tagged = facts(&[("COMM", "Date:2026-08-11 09:00:10.500;")]);
        tagged.duration_ms = Some(10_500);

        assert_eq!(
            read(&tagged, &eastern()).call_at_ms,
            Some(
                Utc.with_ymd_and_hms(2026, 8, 11, 14, 0, 0)
                    .unwrap()
                    .timestamp_millis()
            )
        );
    }

    /// A patch: the group's own number, and the Talkgroups it joined.
    #[test]
    fn a_patch_group_names_its_members() {
        let described = read(
            &facts(&[("TIT2", "P:1234 [54241, 54242]\"Mutual Aid\"")]),
            &eastern(),
        );

        assert_eq!(described.talkgroup_ref, Some(1234));
        assert_eq!(described.patches, vec![54241, 54242]);
        assert_eq!(described.talkgroup_label.as_deref(), Some("Mutual Aid"));
    }

    #[rstest]
    #[case::no_title(&[])]
    #[case::title_without_a_number(&[("TIT2", "\"Fire\"")])]
    #[case::zero(&[("TIT2", "0")])]
    fn a_tag_naming_no_talkgroup_names_none(#[case] entries: &[(&str, &str)]) {
        assert_eq!(read(&facts(entries), &eastern()).talkgroup_ref, None);
    }

    #[rstest]
    #[case::grouping_when_no_comment_system(&[("TIT1", " Fulton ")], Some("Fulton"))]
    #[case::comment_outranks_grouping(&[("TIT1", "Other"), ("COMM", "System:Fulton;")], Some("Fulton"))]
    #[case::blank_grouping(&[("TIT1", "  ")], None)]
    #[case::nothing(&[], None)]
    fn the_system_is_named_by_the_comment_then_the_grouping(
        #[case] entries: &[(&str, &str)],
        #[case] expected: Option<&str>,
    ) {
        assert_eq!(
            read(&facts(entries), &eastern()).system,
            expected.map(|label| Named::Label(label.into()))
        );
    }

    #[rstest]
    #[case::unparsable_date("Date:yesterday;")]
    #[case::no_date("System:Fulton;")]
    fn a_tag_without_a_readable_date_says_nothing_about_when(#[case] comment: &str) {
        assert_eq!(
            read(&facts(&[("COMM", comment)]), &eastern()).call_at_ms,
            None
        );
    }

    #[rstest]
    #[case::unclosed_quote("54241\"Fire", None)]
    #[case::empty_quotes("54241\"\"", None)]
    #[case::second_alias_ignored("54241\"Fire\",\"Other\"", Some("Fire"))]
    fn only_a_whole_quoted_alias_is_a_label(#[case] title: &str, #[case] expected: Option<&str>) {
        assert_eq!(quoted(title).as_deref(), expected);
    }

    #[rstest]
    #[case::unclosed("P:1 [2, 3", vec![])]
    #[case::not_numbers("P:1 [a, b]", vec![])]
    #[case::zero_dropped("P:1 [0, 5]", vec![5])]
    fn only_a_closed_list_of_numbers_is_a_patch(#[case] title: &str, #[case] expected: Vec<i64>) {
        assert_eq!(bracketed(title), expected);
    }

    proptest! {
        /// Whatever an MP3's tag says, reading it never panics.
        #[test]
        fn any_tag_reads_without_panicking(title in ".{0,48}", artist in ".{0,48}", comment in ".{0,96}") {
            let _ = read(
                &facts(&[("TIT2", &title), ("TPE1", &artist), ("COMM", &comment)]),
                &eastern(),
            );
        }
    }
}
