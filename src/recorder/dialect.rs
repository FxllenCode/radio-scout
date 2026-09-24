//! **What Trunk Recorder says down its status socket** (#71, spec US 50) —
//! parsed, and nothing else.
//!
//! The contract is `plugins/stat_socket/stat_socket.cc` plus
//! `docs/notes/STATUS-JSON.md`, and the shapes come from the three `get_stats()`
//! methods the recorder builds them with: `Call_impl::get_stats`,
//! `Recorder::get_stats` and `System_impl::get_stats`/`get_stats_current`.
//!
//! # Every scalar arrives as a string
//!
//! The recorder serialises through `boost::property_tree::write_json`, which has
//! no notion of a JSON number and quotes **everything**: `"freq": "410000000"`,
//! `"phase2": "false"`, `"decoderate": "39.333332"`. That is not a quirk to be
//! tolerated once — it is the whole wire, so every scalar here goes through
//! [`Num`], [`Real`] or [`Flag`], which take the string *or* the native form.
//! Anything else and a parser that worked against a hand-written fixture would
//! fail against every real recorder there is.
//!
//! # An unknown message is not an error
//!
//! [`Message::Other`] swallows a `type` this release has never heard of, because
//! the alternative is a recorder one version ahead closing the dashboard rather
//! than filling in most of it. The same goes for a field: nothing here is
//! `deny_unknown_fields`, which is the opposite of the configuration document's
//! rule (#51) and for the opposite reason — that document is *ours* and this
//! one is somebody else's.

use serde::Deserialize;

// ---------------------------------------------------------------------------
// Scalars, as a recorder writes them
// ---------------------------------------------------------------------------

/// A whole number, written as a JSON number **or** as the string boost wrote.
///
/// A value that is neither reads as zero rather than failing the whole message:
/// one unparseable field on a `calls_active` frame would otherwise take the
/// entire active-call list down with it, which on a busy county is the
/// dashboard going blank at exactly the moment it is being watched.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Num(pub i64);

/// A real number, on [`Num`]'s terms — the decode rate, and the lengths a
/// recorder measures in fractional seconds.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Real(pub f64);

/// A boolean. boost writes `"true"`/`"false"`; a hand-written client may send
/// the JSON literal, and `"1"`/`"0"` are what a C++ `int(bool)` would give.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Flag(pub bool);

impl<'de> Deserialize<'de> for Num {
    fn deserialize<D: serde::Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        Ok(Num(match serde_json::Value::deserialize(de)? {
            serde_json::Value::Number(n) => n.as_f64().unwrap_or(0.0) as i64,
            serde_json::Value::String(s) => s.trim().parse::<f64>().unwrap_or(0.0) as i64,
            _ => 0,
        }))
    }
}

impl<'de> Deserialize<'de> for Real {
    fn deserialize<D: serde::Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        Ok(Real(match serde_json::Value::deserialize(de)? {
            serde_json::Value::Number(n) => n.as_f64().unwrap_or(0.0),
            serde_json::Value::String(s) => s.trim().parse::<f64>().unwrap_or(0.0),
            _ => 0.0,
        }))
    }
}

impl<'de> Deserialize<'de> for Flag {
    fn deserialize<D: serde::Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        Ok(Flag(match serde_json::Value::deserialize(de)? {
            serde_json::Value::Bool(b) => b,
            serde_json::Value::Number(n) => n.as_f64().unwrap_or(0.0) != 0.0,
            // Anything that is not the recorder's own `false` counts as set —
            // the safe direction for `encrypted` and `emergency` alike, since a
            // flag shown that was not set is noticed and one silently dropped is
            // not.
            serde_json::Value::String(s) => !matches!(s.trim(), "false" | "0" | ""),
            _ => false,
        }))
    }
}

/// A string field that may be absent, and which is empty far more often than it
/// is useful — `instanceId` and `instanceKey` both default to `""`.
fn some(value: String) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

// ---------------------------------------------------------------------------
// The messages
// ---------------------------------------------------------------------------

/// One frame off the status socket.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type")]
pub enum Message {
    /// Sent once, the moment the socket opens: what this recorder *is*.
    #[serde(rename = "config")]
    Config(Config),
    /// Every three seconds, for every System: control-channel messages decoded
    /// per second. Zero on a conventional System, which has no control channel
    /// to decode.
    #[serde(rename = "rates")]
    Rates { rates: Vec<Rate> },
    /// The whole System list, on connect.
    #[serde(rename = "systems")]
    Systems { systems: Vec<SystemStats> },
    /// One System, when its sysid/wacn/nac first becomes known.
    #[serde(rename = "system")]
    System { system: SystemStats },
    /// Every call the recorder currently has in hand — sent on connect, and
    /// again whenever one starts or finishes. **Authoritative**: a call absent
    /// from it is over.
    #[serde(rename = "calls_active")]
    CallsActive { calls: Vec<CallStats> },
    #[serde(rename = "call_start")]
    CallStart { call: CallStats },
    /// Documented, and `stat_socket` returns before sending one — read anyway,
    /// because a fork or a later release that starts sending them should not
    /// leave a finished call on the screen forever.
    #[serde(rename = "call_end")]
    CallEnd { call: CallStats },
    /// Every demodulator, on connect.
    #[serde(rename = "recorders")]
    Recorders { recorders: Vec<RecorderStats> },
    /// One demodulator, when it changes state.
    #[serde(rename = "recorder")]
    Recorder { recorder: RecorderStats },
    /// A `type` this release has never heard of — see the module docs.
    #[serde(other)]
    Other,
}

impl Message {
    /// Read one frame, or say why it could not be read.
    ///
    /// The error is a **slug**, not the serde message: it reaches a log line and
    /// a refusal counter, and `serde_json`'s own text carries the offending
    /// bytes, which on this socket is a frame a stranger sent.
    pub fn read(frame: &str) -> Result<Message, Malformed> {
        serde_json::from_str(frame).map_err(|_| Malformed)
    }
}

/// A frame this endpoint could not read at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Malformed;

/// What the recorder is, sent once on connect.
#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub sources: Vec<SourceStats>,
    #[serde(default)]
    pub systems: Vec<ConfigSystem>,
    #[serde(rename = "captureDir", default)]
    pub capture_dir: String,
    #[serde(rename = "callTimeout", default)]
    pub call_timeout: Num,
    /// The recorder's own name for itself. Empty by default, which is what
    /// nearly every install sends.
    #[serde(rename = "instanceId", default)]
    pub instance_id: String,
}

impl Config {
    /// The name this recorder gave itself, if it gave one.
    pub fn name(&self) -> Option<String> {
        some(self.instance_id.clone())
    }

    /// Where it writes its audio, if it said.
    pub fn capture_dir(&self) -> Option<String> {
        some(self.capture_dir.clone())
    }
}

/// One SDR the recorder has open — the *device*, which is the thing an Operator
/// unplugs when it dies.
#[derive(Debug, Clone, Deserialize)]
pub struct SourceStats {
    #[serde(rename = "source_num", default)]
    pub source_num: Num,
    #[serde(default)]
    pub driver: String,
    #[serde(default)]
    pub device: String,
    #[serde(default)]
    pub antenna: String,
    #[serde(default)]
    pub center: Real,
    #[serde(default)]
    pub rate: Real,
    #[serde(default)]
    pub gain: Real,
    /// The **configured** tuning correction, in Hz — not a measurement. What the
    /// charts call drift is the recorder's own `freq_error` per Call (#71,
    /// [`crate::rf`]), which is a different number entirely.
    #[serde(default)]
    pub error: Real,
    #[serde(rename = "min_hz", default)]
    pub min_hz: Real,
    #[serde(rename = "max_hz", default)]
    pub max_hz: Real,
    #[serde(rename = "analog_recorders", default)]
    pub analog_recorders: Num,
    #[serde(rename = "digital_recorders", default)]
    pub digital_recorders: Num,
}

/// A System as the *configuration* describes it, which is a different shape from
/// the one its stats come in.
#[derive(Debug, Clone, Deserialize)]
pub struct ConfigSystem {
    #[serde(rename = "sysNum", default)]
    pub sys_num: Num,
    #[serde(rename = "shortName", default)]
    pub short_name: String,
    #[serde(rename = "systemType", default)]
    pub system_type: String,
    #[serde(default)]
    pub channels: Vec<Real>,
}

/// One System's decode rate.
#[derive(Debug, Clone, Deserialize)]
pub struct Rate {
    #[serde(default)]
    pub id: Num,
    #[serde(default)]
    pub decoderate: Real,
}

/// One System's identity, as `System_impl::get_stats` builds it.
#[derive(Debug, Clone, Deserialize)]
pub struct SystemStats {
    #[serde(default)]
    pub id: Num,
    #[serde(default)]
    pub name: String,
    #[serde(rename = "type", default)]
    pub system_type: String,
    #[serde(default)]
    pub sysid: String,
    #[serde(default)]
    pub wacn: String,
    #[serde(default)]
    pub nac: String,
}

/// One demodulator — TR's own `recorders` array, which is **not** the process
/// that dialed in. See [`crate::recorder`] on why this project calls them
/// demodulators.
#[derive(Debug, Clone, Deserialize)]
pub struct RecorderStats {
    /// `<srcNum>_<recNum>`, the recorder's own composite key.
    #[serde(default)]
    pub id: String,
    #[serde(rename = "type", default)]
    pub kind: String,
    #[serde(rename = "srcNum", default)]
    pub src_num: Num,
    #[serde(rename = "recNum", default)]
    pub rec_num: Num,
    /// How many calls this demodulator has taken since the recorder started.
    #[serde(default)]
    pub count: Num,
    /// And how many seconds of audio. A demodulator that has never recorded
    /// carries uninitialised memory here (`1.5e-314` in the recorder's own
    /// documentation), which is why it is read as a real and shown against the
    /// count rather than on its own.
    #[serde(default)]
    pub duration: Real,
    #[serde(default)]
    pub state: Num,
}

/// One call the recorder has in hand, as `Call_impl::get_stats` builds it.
#[derive(Debug, Clone, Deserialize)]
pub struct CallStats {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub freq: Real,
    #[serde(rename = "sysNum", default)]
    pub sys_num: Num,
    #[serde(rename = "shortName", default)]
    pub short_name: String,
    #[serde(default)]
    pub talkgroup: Num,
    #[serde(default)]
    pub talkgrouptag: String,
    #[serde(default)]
    pub elapsed: Real,
    #[serde(default)]
    pub length: Real,
    #[serde(default)]
    pub state: Num,
    /// **Why it is not being recorded** — the whole of spec US 50's third
    /// clause. `0` is `UNSPECIFIED`, which on a call in any state but
    /// `MONITORING` means nothing at all.
    #[serde(rename = "monState", default)]
    pub mon_state: Num,
    #[serde(default)]
    pub encrypted: Flag,
    #[serde(default)]
    pub emergency: Flag,
    #[serde(default)]
    pub conventional: Flag,
    #[serde(rename = "recNum", default)]
    pub rec_num: Num,
    #[serde(rename = "srcNum", default)]
    pub src_num: Num,
    #[serde(rename = "startTime", default)]
    pub start_time: Num,
    /// Present since the recorder started carrying milliseconds; where it is
    /// absent, `startTime` is unix **seconds**.
    #[serde(rename = "startTimeMs", default)]
    pub start_time_ms: Num,
}

impl CallStats {
    /// When this call started, in the unix milliseconds everything here stores.
    pub fn started_at_ms(&self) -> i64 {
        match self.start_time_ms.0 {
            0 => self.start_time.0.saturating_mul(1000),
            ms => ms,
        }
    }

    /// What this call is called, or nothing.
    pub fn talkgroup_label(&self) -> Option<String> {
        some(self.talkgrouptag.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Read a frame the test expects to be of one kind, and take its payload.
    macro_rules! read {
        ($frame:expr, $kind:pat => $payload:expr) => {
            match Message::read($frame).expect("a readable frame") {
                $kind => $payload,
                other => panic!("not the kind of frame expected: {other:?}"),
            }
        };
    }

    /// Every scalar reader takes the recorder's string, the native form, and
    /// anything else as the quiet default — never a failed frame.
    #[rstest::rstest]
    #[case::native_true("true", true)]
    #[case::native_false("false", false)]
    #[case::number_set("1", true)]
    #[case::number_clear("0", false)]
    #[case::null("null", false)]
    fn a_flag_reads_every_form_it_can_arrive_in(#[case] json: &str, #[case] expected: bool) {
        let Flag(flag) = serde_json::from_str(json).expect("a flag never fails");
        assert_eq!(flag, expected);
    }

    #[rstest::rstest]
    #[case::null("null")]
    #[case::boolean("true")]
    #[case::array("[1]")]
    fn a_number_that_is_neither_string_nor_number_is_zero(#[case] json: &str) {
        let Num(num) = serde_json::from_str(json).expect("a number never fails");
        assert_eq!(num, 0);
    }

    /// The exact bytes `docs/notes/STATUS-JSON.md` documents — every scalar a
    /// string, because `write_json` has no other mode.
    const RATES: &str = r#"{"rates":[{"id":"0","decoderate":"39.333332"},
        {"id":"1","decoderate":"0"}],"type":"rates","instanceId":"","instanceKey":""}"#;

    #[test]
    fn a_rates_frame_reads_its_strings_as_numbers() {
        let rates = read!(RATES, Message::Rates { rates } => rates);

        assert_eq!(rates.len(), 2);
        assert_eq!(rates[0].id, Num(0));
        assert!((rates[0].decoderate.0 - 39.333332).abs() < 1e-6);
        assert_eq!(rates[1].decoderate, Real(0.0));
    }

    /// The same frame a client that had never heard of boost would send. Both
    /// are accepted, because our own fake recorder in the harness is the second
    /// kind and a parser that only took one of them would be proving itself
    /// against itself.
    #[test]
    fn a_frame_written_with_real_json_numbers_reads_the_same() {
        let native = r#"{"rates":[{"id":0,"decoderate":39.333332}],"type":"rates"}"#;

        let rates = read!(native, Message::Rates { rates } => rates);

        assert_eq!(rates[0].id, Num(0));
    }

    #[test]
    fn a_calls_active_frame_carries_the_why_not_recorded_reason() {
        let frame = r#"{"calls":[{"id":"0_1001_1515574637","freq":"419000000",
            "sysNum":"0","shortName":"SYS 1","talkgroup":"1001",
            "talkgrouptag":"TG 77","elapsed":"35","length":"25.09","state":"0",
            "monState":"5","phase2":"false","conventional":"false",
            "encrypted":"true","emergency":"false","startTime":"1515574637",
            "recNum":"0","srcNum":"2","analog":"false"}],
            "type":"calls_active","instanceId":"","instanceKey":""}"#;

        let calls = read!(frame, Message::CallsActive { calls } => calls);

        assert_eq!(calls[0].talkgroup, Num(1001));
        assert_eq!(calls[0].mon_state, Num(5), "ENCRYPTED");
        assert_eq!(calls[0].encrypted, Flag(true));
        assert_eq!(calls[0].emergency, Flag(false), "boost writes \"false\"");
        assert_eq!(calls[0].src_num, Num(2));
        assert_eq!(
            calls[0].started_at_ms(),
            1_515_574_637_000,
            "`startTime` is unix seconds where there is no `startTimeMs`"
        );
    }

    /// A recorder new enough to send milliseconds is believed over its own
    /// rounded seconds — the two are in the same frame, and the factor of a
    /// thousand between them is exactly the sort of thing that hides.
    #[test]
    fn milliseconds_win_over_seconds_where_both_are_sent() {
        let frame = r#"{"call":{"id":"a","startTime":"1515574637",
            "startTimeMs":"1515574637250"},"type":"call_start"}"#;

        let call = read!(frame, Message::CallStart { call } => call);

        assert_eq!(call.started_at_ms(), 1_515_574_637_250);
    }

    #[test]
    fn a_config_frame_carries_the_sdrs_and_the_systems() {
        let frame = r#"{"sources":[{"source_num":"0","antenna":"","driver":"osmosdr",
            "device":"rtl=0","min_hz":"769000000","max_hz":"771000000",
            "center":"770000000","rate":"2400000","error":"0","gain":"40",
            "analog_recorders":"0","digital_recorders":"4"}],
            "systems":[{"audioArchive":"true","systemType":"p25",
            "shortName":"butco","sysNum":"0","channels":["770000000"]}],
            "captureDir":"/captures","callTimeout":"3","logFile":"false",
            "instanceId":"butco-pi","instanceKey":"","type":"config"}"#;

        let config = read!(frame, Message::Config(config) => config);

        assert_eq!(config.name().as_deref(), Some("butco-pi"));
        assert_eq!(config.capture_dir().as_deref(), Some("/captures"));
        assert_eq!(config.call_timeout, Num(3));
        assert_eq!(config.sources.len(), 1);
        assert_eq!(config.sources[0].device, "rtl=0");
        assert_eq!(config.sources[0].digital_recorders, Num(4));
        assert_eq!(config.systems[0].short_name, "butco");
        assert_eq!(config.systems[0].channels, vec![Real(770000000.0)]);
    }

    /// Nearly every install leaves `instanceId` at its default, and an empty
    /// string is not a name.
    #[test]
    fn an_unnamed_recorder_has_no_name() {
        let frame = r#"{"sources":[],"systems":[],"instanceId":"  ","type":"config"}"#;

        let config = read!(frame, Message::Config(config) => config);

        assert_eq!(config.name(), None);
    }

    /// A `type` this release has never heard of fills in most of the dashboard
    /// rather than closing it — and `signaling`, which a recorder sends only
    /// with `broadcast_signals` on, is one we deliberately do not read.
    #[test]
    fn an_unknown_message_type_is_not_an_error() {
        let signaling = r#"{"signal":{"unit_id":"4424000"},"type":"signaling"}"#;

        assert!(matches!(
            Message::read(signaling).expect("a frame"),
            Message::Other
        ));
    }

    /// A frame with no `type` at all cannot be anything, and a frame that is not
    /// JSON certainly cannot.
    #[test]
    fn a_frame_that_is_not_a_message_is_refused() {
        assert_eq!(Message::read("not json").unwrap_err(), Malformed);
        assert_eq!(Message::read(r#"{"rates":[]}"#).unwrap_err(), Malformed);
    }

    /// One unreadable field must not take a whole frame down: on a `calls_active`
    /// that is the entire active-call list, which on a busy county is the
    /// dashboard going blank at exactly the moment it is worth watching.
    #[test]
    fn a_scalar_that_is_not_a_number_reads_as_zero() {
        let frame = r#"{"call":{"id":"a","talkgroup":"not a number",
            "freq":null,"state":"1"},"type":"call_start"}"#;

        let call = read!(frame, Message::CallStart { call } => call);

        assert_eq!(call.talkgroup, Num(0));
        assert_eq!(call.freq, Real(0.0));
        assert_eq!(call.state, Num(1), "and the rest of the frame survives");
    }

    /// A demodulator that has never recorded carries uninitialised memory in its
    /// duration — the recorder's own documentation shows `1.5147569426240483e-314`
    /// — so it has to read as a real and not as a count of anything.
    #[test]
    fn an_idle_demodulators_nonsense_duration_still_parses() {
        let frame = r#"{"recorder":{"id":"0_2","type":"P25","srcNum":"0","recNum":"2",
            "count":"0","duration":"1.5147569426240483e-314","state":"7"},
            "type":"recorder"}"#;

        let recorder = read!(frame, Message::Recorder { recorder } => recorder);

        assert_eq!(recorder.count, Num(0));
        assert_eq!(recorder.state, Num(7));
        assert!(recorder.duration.0 < 1.0);
    }
}
