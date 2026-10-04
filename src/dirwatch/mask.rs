//! rdio-scanner's **filename masks**: `cymx_#TG_#DATE_#TIME_#HZ`, read against
//! the name of a file a Recorder dropped (`dirwatch.go:parseMask`).
//!
//! The tokens and their character classes are rdio's, byte for byte, because an
//! Operator moving over copies their mask across and is owed the same answer.
//! What is not rdio's is everything around them, and each difference is a bug
//! rdio has:
//!
//! | rdio | Radio-Scout |
//! | --- | --- |
//! | the mask *is* a regular expression — the text between tokens is pasted in raw, so a `.` matches anything and a `(` panics `regexp.MustCompile` in a goroutine with no `recover`, which takes the **whole server** down | literal text is escaped, and a mask is compiled **once, when it is saved**, so a bad one is a refusal on a form rather than a crash at the first file |
//! | tokens are replaced in table order, so `#UNIT` is substituted inside `#UNITLBL` before `#UNITLBL` is looked for — and the value is then read back under the key `unitbl` — so `#UNITLBL` has never worked | **longest token first**, and every value is read under the name it was captured as |
//! | `#DATE` with no `#TIME` reads `20240101` as **unix seconds** (1970-08-23) | a date with no time says nothing about *when*, so the file's own modification time decides, as it does for every format |
//! | `#MHZ` `119.1` truncates `119.1 × 10⁶` to 119 099 999 Hz | rounded, so 119.1 MHz is 119 100 000 Hz |
//! | a token written twice keeps its second copy as literal `#TG` text | refused when saved |
//!
//! **Matched anywhere in the name, as rdio does**, rather than anchored: a mask
//! that worked against a filename with a prefix it never mentioned must still
//! work here, or a migration silently stops ingesting.

use chrono::{NaiveDate, NaiveDateTime, NaiveTime, TimeZone};
use regex::Regex;

use super::{Described, Named};

/// One thing a mask can read out of a filename.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Token {
    Date,
    Time,
    ZuluTime,
    Group,
    Hz,
    Khz,
    Mhz,
    Site,
    SiteLabel,
    System,
    SystemLabel,
    Tag,
    Talkgroup,
    TalkgroupAfs,
    TalkgroupHz,
    TalkgroupKhz,
    TalkgroupMhz,
    TalkgroupLabel,
    Unit,
    UnitLabel,
}

impl Token {
    /// Every token, **longest spelling first** — the order they are tried in, so
    /// `#TGLBL` is never read as `#TG` followed by the text `LBL`.
    pub const ALL: [Token; 20] = [
        Token::SiteLabel,
        Token::SystemLabel,
        Token::UnitLabel,
        Token::TalkgroupKhz,
        Token::TalkgroupLabel,
        Token::TalkgroupMhz,
        Token::TalkgroupAfs,
        Token::TalkgroupHz,
        Token::ZuluTime,
        Token::Group,
        Token::Date,
        Token::Site,
        Token::Time,
        Token::Unit,
        Token::Khz,
        Token::Mhz,
        Token::System,
        Token::Tag,
        Token::Hz,
        Token::Talkgroup,
    ];

    /// How it is written in a mask, without the `#`.
    pub fn spelling(self) -> &'static str {
        match self {
            Token::Date => "DATE",
            Token::Time => "TIME",
            Token::ZuluTime => "ZTIME",
            Token::Group => "GROUP",
            Token::Hz => "HZ",
            Token::Khz => "KHZ",
            Token::Mhz => "MHZ",
            Token::Site => "SITE",
            Token::SiteLabel => "SITELBL",
            Token::System => "SYS",
            Token::SystemLabel => "SYSLBL",
            Token::Tag => "TAG",
            Token::Talkgroup => "TG",
            Token::TalkgroupAfs => "TGAFS",
            Token::TalkgroupHz => "TGHZ",
            Token::TalkgroupKhz => "TGKHZ",
            Token::TalkgroupMhz => "TGMHZ",
            Token::TalkgroupLabel => "TGLBL",
            Token::Unit => "UNIT",
            Token::UnitLabel => "UNITLBL",
        }
    }

    /// What it matches — rdio's own classes (`dirwatch.go:parseMask`), so a
    /// mask reads the same filename the same way on both.
    fn pattern(self) -> &'static str {
        match self {
            Token::Date => r"\d{4}[-_]?\d{2}[-_]?\d{2}",
            Token::Time | Token::ZuluTime => r"\d{2}[-:]?\d{2}[-:]?\d{2}",
            Token::Group | Token::Tag => r"[a-zA-Z0-9\. -]+",
            Token::Hz
            | Token::Site
            | Token::System
            | Token::Talkgroup
            | Token::TalkgroupHz
            | Token::Unit => r"\d+",
            Token::Khz | Token::Mhz | Token::TalkgroupKhz | Token::TalkgroupMhz => r"[\d\.]+",
            Token::SiteLabel | Token::SystemLabel | Token::TalkgroupLabel | Token::UnitLabel => {
                r"[a-zA-Z0-9,\. -]+"
            }
            Token::TalkgroupAfs => r"\d{2}-\d{3}",
        }
    }
}

/// Why a mask cannot be saved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MaskError {
    /// `#` followed by capitals that spell no token — almost always a typo, and
    /// one that would otherwise match nothing for ever.
    UnknownToken(String),
    /// The same token twice. rdio silently keeps the second as literal text.
    Repeated(Token),
    /// A mask that reads nothing out of a name at all.
    NoTokens,
}

impl std::fmt::Display for MaskError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MaskError::UnknownToken(token) => write!(
                f,
                "#{token} is not a mask token (the tokens are {})",
                Token::ALL
                    .iter()
                    .map(|token| format!("#{}", token.spelling()))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            MaskError::Repeated(token) => {
                write!(f, "#{} appears more than once", token.spelling())
            }
            MaskError::NoTokens => write!(f, "a mask must contain at least one #TOKEN"),
        }
    }
}

/// A mask, compiled once.
#[derive(Debug, Clone)]
pub struct Mask {
    regex: Regex,
    /// Which token each capture group is, in order.
    tokens: Vec<Token>,
}

impl Mask {
    /// Compile `text`, or say why it cannot be.
    ///
    /// Never panics, whatever it is handed — it is an Operator's typing — which
    /// is the property test below.
    pub fn compile(text: &str) -> Result<Mask, MaskError> {
        let mut pattern = String::new();
        let mut tokens: Vec<Token> = Vec::new();
        let mut rest = text;
        while let Some(at) = rest.find('#') {
            pattern.push_str(&regex::escape(&rest[..at]));
            let after = &rest[at + 1..];
            match Token::ALL
                .iter()
                .find(|token| after.starts_with(token.spelling()))
            {
                Some(&token) => {
                    if tokens.contains(&token) {
                        return Err(MaskError::Repeated(token));
                    }
                    tokens.push(token);
                    pattern.push('(');
                    pattern.push_str(token.pattern());
                    pattern.push(')');
                    rest = &after[token.spelling().len()..];
                }
                None => {
                    let word: String = after.chars().take_while(char::is_ascii_uppercase).collect();
                    // `#` and then anything but a capital is a literal `#` —
                    // filenames are allowed one.
                    if !word.is_empty() {
                        return Err(MaskError::UnknownToken(word));
                    }
                    pattern.push_str(&regex::escape("#"));
                    rest = after;
                }
            }
        }
        pattern.push_str(&regex::escape(rest));
        if tokens.is_empty() {
            return Err(MaskError::NoTokens);
        }
        Ok(Mask {
            // Every piece above is either escaped text or a fixed class, so the
            // whole is a valid expression by construction.
            regex: Regex::new(&pattern).expect("an escaped mask compiles"),
            tokens,
        })
    }

    /// Whether this mask reads `token` at all — what the curation screen asks
    /// to decide whether a watch must name its own Talkgroup or System.
    pub fn reads(&self, token: Token) -> bool {
        self.tokens.contains(&token)
    }

    /// What `stem` — a filename without its extension — says, or `None` when
    /// the mask does not match it at all.
    pub fn read<Tz: TimeZone>(&self, stem: &str, zone: &Tz) -> Option<Described> {
        let captures = self.regex.captures(stem)?;
        let value = |wanted: Token| {
            self.tokens
                .iter()
                .position(|token| *token == wanted)
                .and_then(|index| captures.get(index + 1))
                .map(|found| found.as_str())
        };

        let mut described = Described {
            call_at_ms: when(
                value(Token::Date),
                value(Token::Time),
                value(Token::ZuluTime),
                zone,
            ),
            talkgroup_group: value(Token::Group).and_then(meaningful),
            talkgroup_tag: value(Token::Tag).and_then(meaningful),
            talkgroup_label: value(Token::TalkgroupLabel).and_then(meaningful),
            ..Described::default()
        };

        described.frequency = value(Token::Hz)
            .and_then(|hz| scaled(hz, 1.0))
            .or_else(|| value(Token::Khz).and_then(|khz| scaled(khz, 1e3)))
            .or_else(|| value(Token::Mhz).and_then(|mhz| scaled(mhz, 1e6)));

        described.system = value(Token::System)
            .and_then(positive)
            .map(Named::Ref)
            .or_else(|| {
                value(Token::SystemLabel)
                    .and_then(meaningful)
                    .map(Named::Label)
            });
        described.site_ref = value(Token::Site).and_then(positive);
        if described.site_ref.is_none() {
            described.site_label = value(Token::SiteLabel).and_then(meaningful);
        }

        // The Talkgroup, from the first token that names one — rdio's order.
        // The frequency-as-Talkgroup tokens are for conventional channels, where
        // the channel *is* its frequency: they set both.
        if let Some(talkgroup) = value(Token::Talkgroup).and_then(positive) {
            described.talkgroup_ref = Some(talkgroup);
        } else if let Some(afs) = value(Token::TalkgroupAfs) {
            described.talkgroup_ref = afs_ref(afs);
        } else if let Some((hz, talkgroup)) = value(Token::TalkgroupHz)
            .and_then(|hz| channel(hz, 1.0))
            .or_else(|| value(Token::TalkgroupKhz).and_then(|khz| channel(khz, 1e3)))
            .or_else(|| value(Token::TalkgroupMhz).and_then(|mhz| channel(mhz, 1e6)))
        {
            described.frequency.get_or_insert(hz);
            described.talkgroup_ref = Some(talkgroup);
        }

        described.unit = value(Token::Unit).and_then(positive).map(|unit_ref| {
            (
                unit_ref,
                value(Token::UnitLabel)
                    .and_then(meaningful)
                    // rdio's DSDPlus rule, applied here too: a "label" that is
                    // just the number again says nothing.
                    .filter(|label| *label != unit_ref.to_string()),
            )
        });
        Some(described)
    }
}

/// A value worth keeping: not empty, and not rdio's `-` placeholder.
fn meaningful(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty() && value != "-").then(|| value.to_owned())
}

/// A positive integer — the only kind of Ref there is.
fn positive(value: &str) -> Option<i64> {
    value.parse().ok().filter(|n| *n > 0)
}

/// A decimal in some unit, in hertz — **rounded**, never truncated.
fn scaled(value: &str, unit: f64) -> Option<i64> {
    let parsed: f64 = value.parse().ok()?;
    let hz = (parsed * unit).round();
    (hz.is_finite() && hz > 0.0 && hz < i64::MAX as f64).then_some(hz as i64)
}

/// A conventional channel named by its frequency: the frequency in hertz, and
/// the Talkgroup Ref rdio derives from it — the **whole** kilohertz, truncated
/// as rdio truncates, so a 12.5 kHz channel (155.0125 MHz → 155012) is the
/// Talkgroup an Operator migrating from rdio already has. From the hertz,
/// which are exact, rather than from rdio's float product, which turns 119.1
/// MHz into 119099.
fn channel(value: &str, unit: f64) -> Option<(i64, i64)> {
    let hz = scaled(value, unit)?;
    let talkgroup = hz / 1_000;
    (talkgroup > 0).then_some((hz, talkgroup))
}

/// EDACS agency-fleet-subfleet (`11-061`) packed into the Talkgroup Ref rdio
/// gives it: four bits of agency, four of fleet, three of subfleet.
///
/// Out-of-range parts are refused rather than packed, because rdio's shift
/// lets a subfleet of 9 spill into the fleet bits and name some *other*
/// Talkgroup.
fn afs_ref(afs: &str) -> Option<i64> {
    let (agency, rest) = afs.split_once('-')?;
    let (fleet, subfleet) = rest.split_at_checked(2)?;
    let (agency, fleet, subfleet): (i64, i64, i64) = (
        agency.parse().ok()?,
        fleet.parse().ok()?,
        subfleet.parse().ok()?,
    );
    let packed = (agency < 16 && fleet < 16 && subfleet < 8)
        .then_some(agency << 7 | fleet << 3 | subfleet)?;
    (packed > 0).then_some(packed)
}

/// When the file says the Call happened — a date **and** a time, or nothing.
///
/// `#TIME` is this Instance's wall clock and `#ZTIME` is UTC. A local time that
/// does not exist (the hour skipped in spring) is nothing, and one that exists
/// twice (the hour repeated in autumn) is the earlier — a Call is better a few
/// minutes early in the Archive than refused.
fn when<Tz: TimeZone>(
    date: Option<&str>,
    time: Option<&str>,
    zulu: Option<&str>,
    zone: &Tz,
) -> Option<i64> {
    let date = parse_date(date?)?;
    match (time.and_then(parse_time), zulu.and_then(parse_time)) {
        (Some(time), _) => local_instant(date.and_time(time), zone),
        (None, Some(time)) => Some(date.and_time(time).and_utc().timestamp_millis()),
        (None, None) => None,
    }
}

/// A wall-clock time in `zone`, as unix milliseconds.
pub(crate) fn local_instant<Tz: TimeZone>(local: NaiveDateTime, zone: &Tz) -> Option<i64> {
    zone.from_local_datetime(&local)
        .earliest()
        .map(|instant| instant.timestamp_millis())
}

fn digits(value: &str) -> String {
    value.chars().filter(char::is_ascii_digit).collect()
}

/// `20201231`, `2020-12-31` or `2020_12_31`.
pub(crate) fn parse_date(value: &str) -> Option<NaiveDate> {
    let digits = digits(value);
    NaiveDate::parse_from_str(&digits, "%Y%m%d").ok()
}

/// `083439`, `08-34-39` or `08:34:39`.
pub(crate) fn parse_time(value: &str) -> Option<NaiveTime> {
    let digits = digits(value);
    (digits.len() == 6)
        .then(|| NaiveTime::parse_from_str(&digits, "%H%M%S").ok())
        .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{FixedOffset, Utc};
    use proptest::prelude::*;
    use rstest::rstest;

    /// Five hours west of UTC — not this machine's zone, whatever it is, so a
    /// test that passes is not passing by coincidence.
    fn eastern() -> FixedOffset {
        FixedOffset::west_opt(5 * 3600).expect("an offset")
    }

    fn read(mask: &str, stem: &str) -> Option<Described> {
        Mask::compile(mask).expect("a mask").read(stem, &eastern())
    }

    /// rdio's own documented example, read the way rdio reads it.
    #[test]
    fn rdios_documented_example_reads_as_rdio_reads_it() {
        let described = read(
            "cymx_#TG_#DATE_#TIME_#HZ",
            "cymx_1457_20201231_083439_119100000",
        )
        .expect("a match");

        assert_eq!(described.talkgroup_ref, Some(1457));
        assert_eq!(described.frequency, Some(119_100_000));
        // 08:34:39 five hours west of UTC is 13:34:39 UTC.
        assert_eq!(
            described.call_at_ms,
            Some(
                Utc.with_ymd_and_hms(2020, 12, 31, 13, 34, 39)
                    .unwrap()
                    .timestamp_millis()
            )
        );
    }

    /// Every token, and what it puts where.
    #[rstest]
    #[case::group("#TG_#GROUP", "5_Fire Dept", |d: &Described| d.talkgroup_group.as_deref() == Some("Fire Dept"))]
    #[case::tag("#TG_#TAG", "5_Law Dispatch", |d: &Described| d.talkgroup_tag.as_deref() == Some("Law Dispatch"))]
    #[case::label("#TG_#TGLBL", "5_Fire, Main", |d: &Described| d.talkgroup_label.as_deref() == Some("Fire, Main"))]
    #[case::khz("#TG_#KHZ", "5_119100.5", |d: &Described| d.frequency == Some(119_100_500))]
    #[case::mhz("#TG_#MHZ", "5_119.1", |d: &Described| d.frequency == Some(119_100_000))]
    #[case::system("#SYS_#TG", "11_5", |d: &Described| d.system == Some(Named::Ref(11)))]
    #[case::system_label("#SYSLBL_#TG", "Fulton County_5", |d: &Described| d.system == Some(Named::Label("Fulton County".into())))]
    #[case::site("#TG_#SITE", "5_3", |d: &Described| d.site_ref == Some(3))]
    #[case::site_label("#TG_#SITELBL", "5_Downtown", |d: &Described| d.site_label.as_deref() == Some("Downtown"))]
    #[case::unit("#TG_#UNIT", "5_4424001", |d: &Described| d.unit == Some((4_424_001, None)))]
    #[case::unit_label("#TG_#UNIT_#UNITLBL", "5_4424001_Engine 1", |d: &Described| d.unit == Some((4_424_001, Some("Engine 1".into()))))]
    #[case::afs("#TGAFS", "11-061", |d: &Described| d.talkgroup_ref == Some(11 << 7 | 6 << 3 | 1))]
    #[case::tg_hz("#TGHZ", "119100000", |d: &Described| d.talkgroup_ref == Some(119_100) && d.frequency == Some(119_100_000))]
    #[case::tg_khz("#TGKHZ", "119100.000", |d: &Described| d.talkgroup_ref == Some(119_100) && d.frequency == Some(119_100_000))]
    #[case::tg_mhz("#TGMHZ", "119.1", |d: &Described| d.talkgroup_ref == Some(119_100) && d.frequency == Some(119_100_000))]
    #[case::tg_mhz_half_kilohertz("#TGMHZ", "155.0125", |d: &Described| d.talkgroup_ref == Some(155_012) && d.frequency == Some(155_012_500))]
    #[case::tg_khz_half_kilohertz("#TGKHZ", "155012.5", |d: &Described| d.talkgroup_ref == Some(155_012) && d.frequency == Some(155_012_500))]
    #[case::tg_hz_half_kilohertz("#TGHZ", "155012500", |d: &Described| d.talkgroup_ref == Some(155_012) && d.frequency == Some(155_012_500))]
    fn every_token_lands_where_it_says(
        #[case] mask: &str,
        #[case] stem: &str,
        #[case] check: fn(&Described) -> bool,
    ) {
        let described = read(mask, stem).expect("a match");
        assert!(check(&described), "{mask} over {stem}: {described:?}");
    }

    /// rdio's `#UNITLBL` has never worked: `#UNIT` is substituted inside it
    /// first, and the value is then looked up under a misspelled key. Longest
    /// token first, read under its own name.
    #[test]
    fn unitlbl_is_its_own_token_and_not_unit_followed_by_lbl() {
        let mask = Mask::compile("#UNIT-#UNITLBL").expect("a mask");

        assert_eq!(mask.tokens, vec![Token::Unit, Token::UnitLabel]);
    }

    /// rdio pastes the text between tokens into a regular expression raw, so a
    /// `.` matches anything and a bracket can panic its server.
    #[rstest]
    #[case::dot_is_a_dot("#TG.wav-call", "5.wav-call", true)]
    #[case::dot_is_not_anything("#TG.wav-call", "5xwav-call", false)]
    #[case::brackets_are_text("[#TG]", "[5]", true)]
    #[case::a_lone_paren_is_text("(#TG", "(5", true)]
    #[case::a_literal_hash("##TG", "#5", true)]
    fn literal_text_is_literal(#[case] mask: &str, #[case] stem: &str, #[case] matches: bool) {
        assert_eq!(read(mask, stem).is_some(), matches, "{mask} over {stem}");
    }

    /// Matched anywhere in the name, as rdio's `FindStringSubmatch` does, so a
    /// mask migrated from rdio reads the files it always read.
    #[test]
    fn a_mask_matches_inside_a_longer_name_as_rdios_does() {
        let described = read("TG#TG", "trunk_TG54241_extra").expect("a match");

        assert_eq!(described.talkgroup_ref, Some(54241));
    }

    #[rstest]
    #[case::typo("#TG_#TIEM", MaskError::UnknownToken("TIEM".into()))]
    #[case::lowercase_is_a_literal_but_then_nothing("#tg", MaskError::NoTokens)]
    #[case::repeated("#TG_#TG", MaskError::Repeated(Token::Talkgroup))]
    #[case::empty("", MaskError::NoTokens)]
    #[case::only_text("call_audio", MaskError::NoTokens)]
    fn a_mask_that_cannot_work_is_refused_when_saved(#[case] mask: &str, #[case] error: MaskError) {
        assert_eq!(Mask::compile(mask).expect_err("refused"), error);
    }

    #[rstest]
    #[case(MaskError::Repeated(Token::Talkgroup), "#TG appears more than once")]
    #[case(MaskError::NoTokens, "a mask must contain at least one #TOKEN")]
    fn a_refused_mask_says_why(#[case] error: MaskError, #[case] told: &str) {
        assert_eq!(error.to_string(), told);
    }

    /// The refusal an Operator reads lists what they could have written.
    #[test]
    fn an_unknown_token_is_told_what_the_tokens_are() {
        let told = MaskError::UnknownToken("TIEM".into()).to_string();

        for token in Token::ALL {
            assert!(told.contains(&format!("#{}", token.spelling())), "{told}");
        }
    }

    /// rdio reads a bare `#DATE` as unix seconds — 1970-08-23 for 2024-01-01.
    /// A date alone says nothing about when, so it says nothing.
    #[rstest]
    #[case::date_alone("#DATE_#TG", "20240101_5")]
    #[case::time_alone("#TIME_#TG", "083439_5")]
    #[case::impossible_date("#DATE_#TIME_#TG", "20241301_083439_5")]
    #[case::impossible_time("#DATE_#TIME_#TG", "20240101_253439_5")]
    fn an_incomplete_or_impossible_time_says_nothing(#[case] mask: &str, #[case] stem: &str) {
        assert_eq!(read(mask, stem).expect("a match").call_at_ms, None);
    }

    /// `#ZTIME` is UTC whatever this Instance's zone is.
    #[test]
    fn zulu_time_is_utc() {
        let described = read("#DATE_#ZTIME_#TG", "2020-12-31_04:34:39_5");

        assert_eq!(
            described.expect("a match").call_at_ms,
            Some(
                Utc.with_ymd_and_hms(2020, 12, 31, 4, 34, 39)
                    .unwrap()
                    .timestamp_millis()
            )
        );
    }

    /// rdio truncates `119.1 × 10⁶`, which is 119 099 999.99… in a float.
    #[test]
    fn megahertz_round_rather_than_truncate() {
        assert_eq!(
            read("#MHZ_#TG", "119.1_5").unwrap().frequency,
            Some(119_100_000)
        );
        assert_eq!(
            read("#MHZ_#TG", "851.0125_5").unwrap().frequency,
            Some(851_012_500)
        );
    }

    /// The first frequency token present wins, hertz first — rdio's order.
    #[test]
    fn hertz_outranks_kilohertz_outranks_megahertz() {
        let described = read("#HZ_#KHZ_#MHZ_#TG", "1000_2_3_5").expect("a match");

        assert_eq!(described.frequency, Some(1000));
    }

    /// A channel named by its frequency gets that frequency only where nothing
    /// more specific said one.
    #[test]
    fn a_frequency_token_outranks_the_one_a_channel_implies() {
        let described = read("#TGMHZ_#HZ", "119.1_119105000").expect("a match");

        assert_eq!(described.frequency, Some(119_105_000));
        assert_eq!(described.talkgroup_ref, Some(119_100));
    }

    #[rstest]
    #[case::zero_tg("#TG", "0", |d: &Described| d.talkgroup_ref.is_none())]
    #[case::placeholder_group("#TG_#GROUP", "5_-", |d: &Described| d.talkgroup_group.is_none())]
    #[case::placeholder_label("#TG_#TGLBL", "5_-", |d: &Described| d.talkgroup_label.is_none())]
    #[case::label_that_is_the_number("#UNIT_#UNITLBL", "77_77", |d: &Described| d.unit == Some((77, None)))]
    #[case::afs_subfleet_out_of_range("#TGAFS", "11-069", |d: &Described| d.talkgroup_ref.is_none())]
    #[case::afs_all_zero("#TGAFS", "00-000", |d: &Described| d.talkgroup_ref.is_none())]
    #[case::sys_zero_falls_to_nothing("#SYS_#TG", "0_5", |d: &Described| d.system.is_none())]
    #[case::site_ref_outranks_label("#SITE_#SITELBL_#TG", "2_North_5", |d: &Described| d.site_ref == Some(2) && d.site_label.is_none())]
    #[case::unparsable_khz("#KHZ_#TG", "1.2.3_5", |d: &Described| d.frequency.is_none())]
    #[case::unparsable_tg_khz("#TGKHZ", "1.2.3", |d: &Described| d.talkgroup_ref.is_none())]
    #[case::tiny_tg_hz("#TGHZ", "400", |d: &Described| d.talkgroup_ref.is_none())]
    fn values_that_name_nothing_are_nothing(
        #[case] mask: &str,
        #[case] stem: &str,
        #[case] check: fn(&Described) -> bool,
    ) {
        let described = read(mask, stem).expect("a match");
        assert!(check(&described), "{mask} over {stem}: {described:?}");
    }

    #[test]
    fn a_name_the_mask_does_not_describe_is_not_read() {
        assert_eq!(read("cymx_#TG", "other_5"), None);
    }

    #[test]
    fn a_mask_says_which_tokens_it_reads() {
        let mask = Mask::compile("#SYSLBL_#TGKHZ").expect("a mask");

        assert!(mask.reads(Token::SystemLabel));
        assert!(mask.reads(Token::TalkgroupKhz));
        assert!(!mask.reads(Token::Talkgroup));
    }

    /// A wall-clock time is read in the zone it is handed, not this machine's.
    #[test]
    fn a_local_time_is_read_in_the_zone_given() {
        let fixed = eastern();
        let local = NaiveDate::from_ymd_opt(2024, 3, 10)
            .unwrap()
            .and_hms_opt(2, 30, 0)
            .unwrap();
        assert_eq!(
            local_instant(local, &fixed),
            Some(local.and_utc().timestamp_millis() + 5 * 3_600_000)
        );
    }

    // -- Properties ----------------------------------------------------------

    proptest! {
        /// An Operator's typing never panics the compiler, whatever it is.
        #[test]
        fn compiling_any_text_never_panics(text in ".{0,64}") {
            let _ = Mask::compile(&text);
        }

        /// A compiled mask never panics over any filename, either.
        #[test]
        fn reading_any_name_never_panics(stem in ".{0,64}") {
            let mask = Mask::compile("#SYSLBL_#DATE_#TIME_#TGKHZ_#UNIT_#UNITLBL").expect("a mask");
            let _ = mask.read(&stem, &eastern());
        }

        /// **The round trip**: a filename written from known values, around
        /// arbitrary literal text, reads back as those values.
        #[test]
        fn a_name_written_from_values_reads_back_as_them(
            prefix in "[a-z]{0,6}",
            separator in "[_~+=]{1,2}",
            talkgroup in 1i64..=99_999_999,
            unit in 1i64..=99_999_999,
            label in "[A-Za-z][A-Za-z0-9 ]{0,10}[A-Za-z]",
            date in (2000i32..2100, 1u32..=12, 1u32..=28),
            time in (0u32..24, 0u32..60, 0u32..60),
        ) {
            let mask = format!("{prefix}#TG{separator}#DATE{separator}#ZTIME{separator}#UNIT{separator}#TGLBL");
            let stem = format!(
                "{prefix}{talkgroup}{separator}{:04}{:02}{:02}{separator}{:02}{:02}{:02}{separator}{unit}{separator}{label}",
                date.0, date.1, date.2, time.0, time.1, time.2
            );
            let described = Mask::compile(&mask).expect("a mask").read(&stem, &eastern()).expect("a match");

            prop_assert_eq!(described.talkgroup_ref, Some(talkgroup));
            prop_assert_eq!(described.unit.map(|(r, _)| r), Some(unit));
            prop_assert_eq!(described.talkgroup_label, Some(label));
            let expected = Utc
                .with_ymd_and_hms(date.0, date.1, date.2, time.0, time.1, time.2)
                .unwrap()
                .timestamp_millis();
            prop_assert_eq!(described.call_at_ms, Some(expected));
        }

        /// Whatever frequency a mask reads, it is the nearest whole hertz to
        /// what the name says.
        #[test]
        fn a_frequency_is_the_nearest_hertz(khz in 1u64..=10_000_000, fraction in 0u64..1000) {
            let stem = format!("{khz}.{fraction:03}_5");
            let described = read("#KHZ_#TG", &stem).expect("a match");

            prop_assert_eq!(described.frequency, Some((khz * 1000 + fraction) as i64));
        }
    }
}
