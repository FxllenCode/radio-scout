//! The rdio upload dialect, written rather than read (#52).
//!
//! [`crate::ingest::call_upload`] is the receiving half of this contract and has
//! been since #5; this is the first time Radio-Scout speaks it. The two are held
//! together by more than symmetry: the integration suite forwards from one
//! Instance to a second and compares the Calls, so a field this module spells
//! differently from the one that reads it fails the suite rather than quietly
//! arriving as `NULL`.
//!
//! **Pure, and built by hand.** A `reqwest::multipart::Form` would be shorter
//! and would put the bytes out of reach: the boundary would be random, the field
//! order the builder's, and the only thing assertable about a delivery would be
//! that one happened. Here the body is a value, so it is snapshot-pinned — which
//! is the acceptance criterion ("correct rdio dialect, fixture-pinned") and is
//! also what makes the field *order* testable, and the order is load-bearing:
//! rdio's parser derives a MIME type from `audioFilename`'s extension and lets a
//! later `audioMime` override it (`parsers.go:254`), so the two have to arrive
//! in that order to mean what we intend.

use serde::Serialize;

/// Everything the rdio dialect can say about one Call, read out of the Archive.
///
/// Deliberately not [`crate::call::StoredCall`]: that is the denormalized view a
/// Listener is shown, and it carries the *first* radio heard and no per-source
/// offsets, no frequency detail and no audio filename. A forward has to carry
/// what the recorder said, because the peer is going to store it as if it were
/// the recorder.
#[derive(Debug, Clone, PartialEq)]
pub struct Forwardable {
    pub system_ref: i64,
    pub system_label: Option<String>,
    pub talkgroup_ref: i64,
    pub talkgroup_label: Option<String>,
    pub talkgroup_name: Option<String>,
    pub talkgroup_tag: Option<String>,
    pub talkgroup_groups: Vec<String>,
    /// When the transmission started, unix milliseconds.
    pub call_at_ms: i64,
    pub frequency: Option<i64>,
    pub site_ref: Option<i64>,
    pub audio_name: Option<String>,
    pub audio_mime: Option<String>,
    /// Canonical Talkgroup Refs this Call is patched to.
    pub patches: Vec<i64>,
    pub units: Vec<ForwardUnit>,
    pub frequencies: Vec<ForwardFrequency>,
    /// Where the audio is. Not sent — read, to become the `audio` part.
    pub object_key: String,
}

/// One radio heard, in the shape rdio's own parser reads (`parsers.go:433`).
///
/// Lowercase `id`/`label`/`offset`, and seconds rather than milliseconds — which
/// is what rdio's *forwarder* fails to send, because `CallUnit` carries no JSON
/// tags and Go marshals it as `{"Id":…,"CallId":…,"Offset":…,"UnitRef":…}`. Its
/// own ingest finds none of those keys, so every radio on every forwarded Call
/// is dropped in silence.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ForwardUnit {
    pub id: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Seconds from the start of the Call, as the wire spells it.
    pub offset: f64,
}

/// One frequency segment, in the shape rdio's parser reads (`parsers.go:272`).
///
/// `len` is ours and rdio ignores it, which is the whole bargain of this module:
/// its parser is a `switch` with no error default, so a field it does not know
/// costs it nothing and a Radio-Scout peer keeps the detail.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ForwardFrequency {
    pub freq: i64,
    pub pos: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub len: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dbm: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_count: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spike_count: Option<i32>,
}

/// Milliseconds as the seconds-with-fraction the dialect uses.
pub fn seconds(ms: i64) -> f64 {
    ms as f64 / 1000.0
}

/// A body ready to POST: the bytes, and the `Content-Type` that describes them.
#[derive(Debug, Clone, PartialEq)]
pub struct Body {
    pub content_type: String,
    pub bytes: Vec<u8>,
}

/// The `audio` part's filename when the Call carries none. rdio derives a MIME
/// type from this extension, so a Call whose recorder sent no filename must
/// still be given one that does not lie about the bytes.
const FALLBACK_AUDIO_NAME: &str = "audio.wav";

/// Build the multipart body one delivery POSTs.
///
/// `boundary` is a parameter rather than generated here so the whole body is a
/// pure function of its inputs and can be snapshot-tested. The sender passes a
/// fresh random one per request.
///
/// **Absent fields are omitted rather than sent empty.** rdio sends all fourteen
/// of its fields unconditionally, including empty strings; both parsers treat an
/// empty label as no label, so this costs nothing and keeps a Pi's uplink
/// carrying audio rather than padding.
pub fn body(call: &Forwardable, api_key: &str, audio: &[u8], boundary: &str) -> Body {
    let mut out = Vec::with_capacity(audio.len() + 1024);
    let audio_name = call
        .audio_name
        .as_deref()
        .filter(|name| !name.is_empty())
        .unwrap_or(FALLBACK_AUDIO_NAME);

    // The `audio` part first, then the two fields that describe it — the order
    // rdio's own forwarder uses and the order its parser needs.
    file_part(
        &mut out,
        boundary,
        "audio",
        audio_name,
        call.audio_mime.as_deref(),
        audio,
    );
    text_part(&mut out, boundary, "audioFilename", audio_name);
    if let Some(mime) = &call.audio_mime {
        text_part(&mut out, boundary, "audioMime", mime);
    }

    // `dateTime` is rdio's pre-v7 spelling of the same instant and `timestamp`
    // is the modern one. Both are sent for the same reason rdio sends both: a
    // peer may be either.
    if let Some(iso) = rfc3339(call.call_at_ms) {
        text_part(&mut out, boundary, "dateTime", &iso);
    }
    // **`frequency` before `frequencies`, and this order is load-bearing.**
    // rdio's parser *replaces* the whole array when it sees the singular field
    // — `call.Frequencies = []CallFrequency{{…}}` (`parsers.go:318`), an
    // assignment, not an append — and `api.go` feeds the parts through it in
    // the order they arrive. Sent the other way round, an rdio peer parses every
    // frequency sample and then throws all of them away for one entry at offset
    // zero, which is the exact opposite of what sending the field at all is for.
    // Our own ingest keeps the two in separate columns and cannot see the
    // difference, which is why this is pinned by the snapshot rather than by the
    // two-Instance forward.
    if let Some(frequency) = call.frequency {
        text_part(&mut out, boundary, "frequency", &frequency.to_string());
    }
    if !call.frequencies.is_empty() {
        json_part(&mut out, boundary, "frequencies", &call.frequencies);
    }
    text_part(&mut out, boundary, "key", api_key);
    if !call.patches.is_empty() {
        json_part(&mut out, boundary, "patches", &call.patches);
    }
    text_part(&mut out, boundary, "system", &call.system_ref.to_string());
    if let Some(label) = &call.system_label {
        text_part(&mut out, boundary, "systemLabel", label);
    }
    text_part(
        &mut out,
        boundary,
        "talkgroup",
        &call.talkgroup_ref.to_string(),
    );
    if !call.talkgroup_groups.is_empty() {
        // Comma-joined, which is the plural field's own encoding on both sides
        // (`ingest::parse_groups`, rdio `parsers.go`).
        text_part(
            &mut out,
            boundary,
            "talkgroupGroups",
            &call.talkgroup_groups.join(","),
        );
    }
    if let Some(label) = &call.talkgroup_label {
        text_part(&mut out, boundary, "talkgroupLabel", label);
    }
    if let Some(name) = &call.talkgroup_name {
        text_part(&mut out, boundary, "talkgroupName", name);
    }
    if let Some(tag) = &call.talkgroup_tag {
        text_part(&mut out, boundary, "talkgroupTag", tag);
    }
    text_part(
        &mut out,
        boundary,
        "timestamp",
        &call.call_at_ms.to_string(),
    );
    if !call.units.is_empty() {
        json_part(&mut out, boundary, "units", &call.units);
    }

    // Ours: a field rdio's *parser* accepts while its forwarder never sends it,
    // so an rdio peer gains the tower it would not have got from another rdio.
    // (`frequency` is the same kind of gift and is sent above, for the ordering
    // reason written there.)
    if let Some(site_ref) = call.site_ref {
        text_part(&mut out, boundary, "site", &site_ref.to_string());
    }

    out.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    Body {
        content_type: format!("multipart/form-data; boundary={boundary}"),
        bytes: out,
    }
}

fn text_part(out: &mut Vec<u8>, boundary: &str, name: &str, value: &str) {
    out.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n"
        )
        .as_bytes(),
    );
}

fn json_part<T: Serialize>(out: &mut Vec<u8>, boundary: &str, name: &str, value: &T) {
    // A `Vec` of plain numbers and structs of numbers and strings: nothing here
    // can fail to serialize, and an empty array would be a wrong-but-harmless
    // answer rather than a reason to abandon a Call.
    let json = serde_json::to_string(value).unwrap_or_else(|_| String::from("[]"));
    text_part(out, boundary, name, &json);
}

fn file_part(
    out: &mut Vec<u8>,
    boundary: &str,
    name: &str,
    filename: &str,
    mime: Option<&str>,
    bytes: &[u8],
) {
    out.extend_from_slice(
        format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"; filename=\"{filename}\"\r\n")
            .as_bytes(),
    );
    // Only when the Call carries one: axum's multipart reads the part's own
    // `Content-Type`, and inventing `application/octet-stream` for a Call whose
    // recorder said nothing would put a wrong MIME on the peer's row where a
    // missing one leaves it honest.
    if let Some(mime) = mime {
        out.extend_from_slice(format!("Content-Type: {mime}\r\n").as_bytes());
    }
    out.extend_from_slice(b"\r\n");
    out.extend_from_slice(bytes);
    out.extend_from_slice(b"\r\n");
}

/// The instant as rdio's `dateTime` spells it, or `None` for a timestamp no
/// calendar can represent — which a Recorder is entitled to send and which must
/// cost the field rather than the Call.
fn rfc3339(at_ms: i64) -> Option<String> {
    use time::format_description::well_known::Rfc3339;
    time::OffsetDateTime::from_unix_timestamp_nanos(at_ms as i128 * 1_000_000)
        .ok()?
        .format(&Rfc3339)
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    const BOUNDARY: &str = "rsboundary";

    fn full() -> Forwardable {
        Forwardable {
            system_ref: 11,
            system_label: Some(String::from("Fulton County")),
            talkgroup_ref: 54241,
            talkgroup_label: Some(String::from("FD Disp")),
            talkgroup_name: Some(String::from("Fire Dispatch")),
            talkgroup_tag: Some(String::from("Fire Dispatch")),
            talkgroup_groups: vec![String::from("Fire"), String::from("EMS")],
            call_at_ms: 1_700_000_000_500,
            frequency: Some(853_712_500),
            site_ref: Some(3),
            audio_name: Some(String::from("54241-1700000000.wav")),
            audio_mime: Some(String::from("audio/wav")),
            patches: vec![54242, 54243],
            units: vec![
                ForwardUnit {
                    id: 4_424_000,
                    label: Some(String::from("MEDIC 7")),
                    offset: 0.0,
                },
                ForwardUnit {
                    id: 4_424_001,
                    label: None,
                    offset: 1.5,
                },
            ],
            frequencies: vec![ForwardFrequency {
                freq: 853_712_500,
                pos: 0.0,
                len: Some(4.25),
                dbm: Some(-72.5),
                error_count: Some(2),
                spike_count: Some(0),
            }],
            object_key: String::from("calls/2023/11/14/1.wav"),
        }
    }

    fn minimal() -> Forwardable {
        Forwardable {
            system_ref: 11,
            system_label: None,
            talkgroup_ref: 100,
            talkgroup_label: None,
            talkgroup_name: None,
            talkgroup_tag: None,
            talkgroup_groups: vec![],
            call_at_ms: 0,
            frequency: None,
            site_ref: None,
            audio_name: None,
            audio_mime: None,
            patches: vec![],
            units: vec![],
            frequencies: vec![],
            object_key: String::from("k"),
        }
    }

    /// Rendered with the audio replaced by a marker, so the snapshot is the
    /// *contract* rather than a few kilobytes of PCM — and so a change to the
    /// synthetic audio a test happens to use cannot churn it.
    fn rendered(call: &Forwardable, key: &str) -> String {
        let body = body(call, key, b"<AUDIO>", BOUNDARY);
        String::from_utf8(body.bytes).expect("the dialect is text but for the audio part")
    }

    /// **The fixture** (#52's first acceptance criterion). Every field, in the
    /// order they go on the wire — which is itself part of the contract, because
    /// rdio derives a MIME type from `audioFilename` and lets a later
    /// `audioMime` override it.
    #[test]
    fn a_full_call_is_pinned_field_by_field() {
        insta::assert_snapshot!(rendered(&full(), "peer-key"));
    }

    /// A Call whose recorder said almost nothing: the mandatory fields, and no
    /// empty ones padding the uplink. rdio sends all fourteen regardless.
    #[test]
    fn a_bare_call_sends_only_what_it_has() {
        insta::assert_snapshot!(rendered(&minimal(), "peer-key"));
    }

    /// The `units` and `frequencies` shapes are the ones the *receiving* parser
    /// reads — lowercase, seconds, camelCase counts. This is the assertion rdio
    /// does not have and the reason its own forwarder loses every radio and
    /// every frequency sample it sends.
    #[test]
    fn the_arrays_use_the_keys_the_receiving_parser_looks_for() {
        let rendered = rendered(&full(), "peer-key");

        assert!(
            rendered.contains(
                r#"[{"id":4424000,"label":"MEDIC 7","offset":0.0},{"id":4424001,"offset":1.5}]"#
            ),
            "{rendered}"
        );
        assert!(
            rendered.contains(
                r#""freq":853712500,"pos":0.0,"len":4.25,"dbm":-72.5,"errorCount":2,"spikeCount":0"#
            ),
            "{rendered}"
        );
    }

    /// **`frequency` must arrive before `frequencies`**, because rdio's parser
    /// *replaces* the array when it sees the singular field (`parsers.go:318` is
    /// an assignment) and reads the parts in the order they arrive.
    ///
    /// Sent the other way round — which is how this shipped until `/code-review`
    /// caught it — an rdio peer parses every frequency sample and then discards
    /// all of them for one entry at offset zero. The two-Instance forward cannot
    /// see it: our own ingest keeps `frequency` and `frequencies` in separate
    /// columns, so both survive whatever order they come in. Only a peer that
    /// conflates them is harmed, and only this assertion notices.
    #[test]
    fn the_single_frequency_cannot_overwrite_the_array_it_precedes() {
        let rendered = rendered(&full(), "peer-key");

        let singular = rendered.find(r#"name="frequency""#).expect("frequency");
        let plural = rendered.find(r#"name="frequencies""#).expect("frequencies");
        assert!(
            singular < plural,
            "`frequency` is parsed as a replacement for `frequencies`, so it has \
             to be overwritten by the array rather than overwrite it"
        );
    }

    /// The audio part carries the Call's own filename and MIME. A Call with
    /// neither still needs a filename — rdio types the bytes off its extension —
    /// but must not be given a `Content-Type` nobody claimed.
    #[test]
    fn a_call_with_no_filename_is_given_one_that_does_not_lie() {
        let rendered = rendered(&minimal(), "k");

        assert!(rendered.contains(r#"filename="audio.wav""#), "{rendered}");
        assert!(!rendered.contains("Content-Type:"), "{rendered}");
        assert!(!rendered.contains("audioMime"), "{rendered}");
    }

    /// The audio goes through byte for byte — the one part that is not text, and
    /// the one whose corruption would be inaudible until somebody played it.
    #[test]
    fn the_audio_bytes_are_carried_verbatim() {
        let audio: Vec<u8> = (0u8..=255).collect();

        let body = body(&full(), "k", &audio, BOUNDARY);

        let start = body
            .bytes
            .windows(audio.len())
            .position(|window| window == audio)
            .expect("the audio bytes appear in the body unchanged");
        assert_eq!(
            &body.bytes[start + audio.len()..start + audio.len() + 2],
            b"\r\n",
            "the part is terminated rather than run into the next boundary"
        );
        assert_eq!(
            body.content_type,
            format!("multipart/form-data; boundary={BOUNDARY}")
        );
    }

    /// A Recorder is entitled to send a timestamp no calendar can represent, and
    /// #46 stores whatever it said. It costs the field, never the Call — the
    /// same bargain `ingest`'s own saturating arithmetic makes.
    #[test]
    fn an_impossible_instant_costs_the_field_and_not_the_call() {
        let call = Forwardable {
            call_at_ms: i64::MAX,
            ..minimal()
        };

        let rendered = rendered(&call, "k");

        assert!(!rendered.contains("dateTime"), "{rendered}");
        assert!(
            rendered.contains(&format!("\r\n\r\n{}\r\n", i64::MAX)),
            "the numeric timestamp still goes, which is the one both parsers \
             prefer: {rendered}"
        );
    }

    /// Milliseconds are seconds on this wire, and the conversion is written once
    /// because two arrays use it.
    #[test]
    fn offsets_are_seconds_on_the_wire() {
        assert_eq!(seconds(1_500), 1.5);
        assert_eq!(seconds(0), 0.0);
    }
}
