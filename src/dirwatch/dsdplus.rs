//! **DSDPlus Fast Lane**'s recordings, read from where it puts them and what it
//! calls them (`parsers.go:ParseDSDPlusMeta`).
//!
//! DSDPlus writes no metadata anywhere but the path:
//! `…\1R-Record#6\20220809\153120_001_DMR(BS)_1-899_DCC2_Slot1_GC_750[Ram_Muni]_52.wav`
//! — the folder is the date, and the name is the time, a sequence number, the
//! protocol, the network, and (always last) the Talkgroup and the radio, each
//! optionally followed by its alias in brackets. The fixtures below are real
//! names from rdio-scanner discussion #244.
//!
//! rdio's parser is followed field for field, with three of its bugs fixed:
//!
//! - **A name with fewer than two fields panics rdio**: `meta[len(meta)-2]` on a
//!   one-element slice is an index of −1. Here it is a file that names no
//!   Talkgroup.
//! - **`P25(BS)` never yields a System in rdio**: its pattern wants a decimal
//!   network (`1-899`), and a P25 base-station name carries `WACN.SYSID-RFSS.SITE`
//!   in hex (`BEE00.37D-1.5`). Both P25 forms read the hex SYSID here, which is
//!   what the plain `P25` arm always did.
//! - **An alias that is only punctuation** (`[---]`, `[__\_______]`, DSDPlus's
//!   way of saying it has none) is a name in rdio whenever it carries a
//!   character outside `.- ,_`. Here an alias must contain a letter or a digit.

use chrono::{NaiveDate, TimeZone};

use super::mask::{local_instant, parse_time};
use super::{Described, Named};

/// What a DSDPlus recording's folder and name say about it.
///
/// `folder` is the name of the directory the file is in (its date), and `stem`
/// is the file's name without its extension. Pure, and never a panic, over any
/// strings at all.
pub fn read<Tz: TimeZone>(folder: &str, stem: &str, zone: &Tz) -> Described {
    let fields = fields(stem);
    let mut described = Described {
        call_at_ms: when(folder, stem, zone),
        system: fields
            .get(2)
            .and_then(|protocol| system(protocol, fields.get(3).copied(), fields.get(4).copied())),
        ..Described::default()
    };

    // The last two fields are always the Talkgroup and the radio — and a name
    // with fewer than two fields names neither.
    if let [.., talkgroup, radio] = fields.as_slice() {
        let (talkgroup_ref, label) = numbered(talkgroup);
        described.talkgroup_ref = talkgroup_ref;
        described.talkgroup_label = label.filter(|_| talkgroup_ref.is_some());
        let (unit_ref, alias) = numbered(radio);
        described.unit = unit_ref.map(|unit_ref| {
            (
                unit_ref,
                alias.filter(|alias| *alias != unit_ref.to_string()),
            )
        });
    }
    described
}

/// The name split on `_`, except inside an alias's brackets — aliases are full
/// of underscores (`750[Ram_Muni]`).
fn fields(stem: &str) -> Vec<&str> {
    let mut fields = Vec::new();
    let mut depth = 0usize;
    let mut start = 0;
    for (at, character) in stem.char_indices() {
        match character {
            '[' => depth += 1,
            ']' => depth = depth.saturating_sub(1),
            '_' if depth == 0 => {
                fields.push(&stem[start..at]);
                start = at + 1;
            }
            _ => {}
        }
    }
    fields.push(&stem[start..]);
    fields
}

/// A number, and the alias bracketed after it: `750[Ram_Muni]`.
fn numbered(field: &str) -> (Option<i64>, Option<String>) {
    let mut parts = field
        .split(['[', ']'])
        .map(str::trim)
        .filter(|part| !part.is_empty());
    let number = parts
        .next()
        .and_then(|number| number.parse().ok())
        .filter(|n: &i64| *n > 0);
    let alias = parts
        .next()
        .filter(|alias| alias.chars().any(char::is_alphanumeric))
        .map(str::to_owned);
    (number, alias)
}

/// When it happened: the folder's `YYYYMMDD` and the name's leading `HHMMSS`,
/// on this Instance's wall clock — DSDPlus writes local time and says so
/// nowhere.
fn when<Tz: TimeZone>(folder: &str, stem: &str, zone: &Tz) -> Option<i64> {
    let date_digits: String = trailing_digits(folder);
    let date = (date_digits.len() == 8)
        .then(|| NaiveDate::parse_from_str(&date_digits, "%Y%m%d").ok())
        .flatten()?;
    let time_digits: String = stem.chars().take_while(char::is_ascii_digit).collect();
    let time = parse_time(&time_digits)?;
    local_instant(date.and_time(time), zone)
}

fn trailing_digits(text: &str) -> String {
    let digits: Vec<char> = text
        .chars()
        .rev()
        .take_while(char::is_ascii_digit)
        .collect();
    digits.into_iter().rev().collect()
}

/// The System a protocol's network field names, where it names one.
fn system(protocol: &str, network: Option<&str>, after: Option<&str>) -> Option<Named> {
    let network = network?;
    let found = match protocol {
        "ConP(BS)" | "DMR(BS)" => leading_decimal_before_dash(network),
        "P25" | "P25(BS)" => p25_sysid(network),
        "NEXEDGE48(CB)" | "NEXEDGE48(CS)" | "NEXEDGE48(TB)" | "NEXEDGE96(CB)" | "NEXEDGE96(CS)"
        | "NEXEDGE96(TB)" => nxdn_site(network).or_else(|| after.and_then(ran)),
        _ => None,
    };
    found.filter(|n| *n > 0).map(Named::Ref)
}

/// `1-899` → 1.
fn leading_decimal_before_dash(network: &str) -> Option<i64> {
    let (number, rest) = network.split_once('-')?;
    (!rest.is_empty()).then(|| number.parse().ok()).flatten()
}

/// `BEE00.37D-1.5` → 0x37D: `WACN.SYSID-RFSS.SITE`, in hex.
fn p25_sysid(network: &str) -> Option<i64> {
    let (_, rest) = network.split_once('.')?;
    let sysid = rest.split('-').next()?;
    i64::from_str_radix(sysid, 16).ok()
}

/// NXDN's `<letter><site>-<number>`.
fn nxdn_site(network: &str) -> Option<i64> {
    let mut characters = network.chars();
    characters.next()?;
    let (site, number) = characters.as_str().split_once('-')?;
    number
        .chars()
        .all(|c| c.is_ascii_digit())
        .then(|| site.parse().ok())
        .flatten()
        .filter(|_| !number.is_empty())
}

/// NXDN's `RAN5`, which names the System when the network field is empty.
fn ran(field: &str) -> Option<i64> {
    let at = field.find("RAN")?;
    let digits: String = field[at + 3..]
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    digits.parse().ok()
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

    fn read_name(stem: &str) -> Described {
        read("20220809", stem, &eastern())
    }

    /// Real names from rdio-scanner discussion #244, one per protocol it lists.
    #[rstest]
    #[case::dmr_named_talkgroup_and_punctuation_radio(
        "153120_001_DMR(BS)_1-899_DCC2_Slot1_GC_750[Ram_Muni]_52[__\\_______]",
        Some(Named::Ref(1)), Some(750), Some("Ram_Muni"), Some((52, None))
    )]
    #[case::dmr_bare_radio(
        "153128_001_DMR(BS)_1-899_DCC2_Slot1_GC_750[Ram_Muni]_1",
        Some(Named::Ref(1)), Some(750), Some("Ram_Muni"), Some((1, None))
    )]
    #[case::p25_base_station(
        "152124_000_P25(BS)_BEE00.37D-1.5_GC_1285[Reh_5016]_3150006",
        Some(Named::Ref(0x37D)), Some(1285), Some("Reh_5016"), Some((3_150_006, None))
    )]
    #[case::nxdn_ran(
        "152142_001_NEXEDGE48(CB)__RAN5_GC_0_235",
        Some(Named::Ref(5)), None, None, Some((235, None))
    )]
    #[case::nxdn_dashes_alias(
        "152144_002_NEXEDGE48(CB)__RAN5_GC_0_97[---]",
        Some(Named::Ref(5)), None, None, Some((97, None))
    )]
    #[case::dcdm_names_no_system(
        "151405_014_DCDM(D2)__DCC9_Slot2_GC_292_5",
        None, Some(292), None, Some((5, None))
    )]
    fn real_dsdplus_names_read_as_they_should(
        #[case] stem: &str,
        #[case] system: Option<Named>,
        #[case] talkgroup: Option<i64>,
        #[case] label: Option<&str>,
        #[case] unit: Option<(i64, Option<String>)>,
    ) {
        let described = read_name(stem);

        assert_eq!(described.system, system, "{stem}");
        assert_eq!(described.talkgroup_ref, talkgroup, "{stem}");
        assert_eq!(described.talkgroup_label.as_deref(), label, "{stem}");
        assert_eq!(described.unit, unit, "{stem}");
    }

    /// The folder is the date and the name is the time, on the wall clock.
    #[test]
    fn the_folder_and_the_name_together_say_when() {
        let described = read_name("153120_001_DMR(BS)_1-899_DCC2_Slot1_GC_750_52");

        assert_eq!(
            described.call_at_ms,
            Some(
                Utc.with_ymd_and_hms(2022, 8, 9, 20, 31, 20)
                    .unwrap()
                    .timestamp_millis()
            )
        );
    }

    #[rstest]
    #[case::folder_is_not_a_date("1R-Record#6", "153120_001_P25_X_1_2")]
    #[case::folder_too_short("2022089", "153120_001_P25_X_1_2")]
    #[case::impossible_date("20221399", "153120_001_P25_X_1_2")]
    #[case::name_has_no_time("20220809", "rec_001_P25_X_1_2")]
    fn a_folder_or_name_that_is_not_a_time_says_nothing(#[case] folder: &str, #[case] stem: &str) {
        assert_eq!(read(folder, stem, &eastern()).call_at_ms, None);
    }

    /// rdio indexes `meta[len-2]` unconditionally and panics on a name with
    /// fewer than two fields.
    #[rstest]
    #[case::one_field("153120")]
    #[case::empty("")]
    fn a_name_too_short_to_hold_a_talkgroup_names_none(#[case] stem: &str) {
        let described = read_name(stem);

        assert_eq!(described.talkgroup_ref, None);
        assert_eq!(described.unit, None);
    }

    #[rstest]
    #[case::conp("ConP(BS)", "3-12", None, Some(3))]
    #[case::dmr_no_dash("DMR(BS)", "899", None, None)]
    #[case::dmr_trailing_dash("DMR(BS)", "1-", None, None)]
    #[case::p25_plain("P25", "BEE00.1A2-1.1", None, Some(0x1A2))]
    #[case::p25_not_hex("P25", "BEE00.ZZZ-1", None, None)]
    #[case::p25_no_dot("P25", "BEE00", None, None)]
    #[case::nxdn_site("NEXEDGE96(CS)", "S12-3", None, Some(12))]
    #[case::nxdn_site_zero_falls_to_nothing("NEXEDGE96(TB)", "S0-3", None, None)]
    #[case::nxdn_bad_number("NEXEDGE48(TB)", "S12-x", Some("RAN7"), Some(7))]
    #[case::nxdn_empty_number("NEXEDGE48(CS)", "S12-", None, None)]
    #[case::nxdn_no_ran("NEXEDGE48(CB)", "", Some("GC"), None)]
    #[case::unknown_protocol("YSF", "1-2", None, None)]
    fn a_protocols_network_field_names_its_system(
        #[case] protocol: &str,
        #[case] network: &str,
        #[case] after: Option<&str>,
        #[case] expected: Option<i64>,
    ) {
        assert_eq!(
            system(protocol, Some(network), after),
            expected.map(Named::Ref)
        );
    }

    proptest! {
        /// DSDPlus names are whatever a Windows filesystem allows; none of them
        /// may panic the reader.
        #[test]
        fn any_folder_and_name_reads_without_panicking(folder in ".{0,24}", stem in ".{0,64}") {
            let _ = read(&folder, &stem, &eastern());
        }

        /// The Talkgroup and the radio are always the last two fields, whatever
        /// came before them and whatever aliases they carry.
        #[test]
        fn the_last_two_fields_are_the_talkgroup_and_the_radio(
            leading in proptest::collection::vec("[A-Za-z0-9()\\.-]{0,8}", 0..6),
            talkgroup in 1i64..=16_777_215,
            radio in 1i64..=16_777_215,
            alias in "[A-Za-z][A-Za-z0-9_ ]{0,10}",
        ) {
            let mut stem = leading.join("_");
            if !stem.is_empty() {
                stem.push('_');
            }
            stem.push_str(&format!("{talkgroup}[{alias}]_{radio}"));
            let described = read_name(&stem);

            prop_assert_eq!(described.talkgroup_ref, Some(talkgroup));
            prop_assert_eq!(described.talkgroup_label.as_deref(), Some(alias.trim()));
            prop_assert_eq!(described.unit, Some((radio, None)));
        }
    }
}
