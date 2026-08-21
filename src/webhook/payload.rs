//! What a **Webhook** delivery's body is (#54, spec US 21) — **pure**, so every
//! shape is a value a test constructs and a snapshot pins.
//!
//! Two formats, and the split is not cosmetic. [`Format::RadioScout`] is for an
//! automation: it carries the Call exactly as the live feed and the search page
//! describe it ([`StoredCall`]), so a script that has parsed one Radio-Scout Call
//! has parsed this one. [`Format::Discord`] is for a human reading a channel:
//! Discord will not render an arbitrary document, so this is *its* schema, and
//! the constraints below are Discord's rather than ours.
//!
//! # The URL problem, and what `[server] public_url` is for
//!
//! A [`StoredCall`]'s `audio_url` is a **path** — `/api/call/1/audio` — because
//! every other consumer of it is a browser already on this origin. A Discord
//! message is not: a relative URL there is not merely useless, it is a `400`
//! ("Not a well formed URL"), and a `400` is [`crate::delivery::Verdict::Abandon`]
//! — so the Emergency this webhook existed to carry would be *dropped*, not
//! retried, for a configuration mistake.
//!
//! So an Instance that has not been told where it lives sends **no URL at all**
//! rather than a broken one. `[server] public_url` is the Operator saying it, and
//! it is a parameter here rather than something this module reaches for, because
//! that is what keeps the render pure.

use serde::Serialize;

use super::{Format, Marks};
use crate::call::StoredCall;

/// A rendered body, ready to POST.
///
/// The [`crate::downstream::dialect::Body`] shape, and for its reason: what goes
/// on the wire is decided here, and the sender only sends it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Body {
    pub content_type: String,
    pub bytes: Vec<u8>,
}

/// Radio-Scout's own shape: what fired, and the Call it fired about.
///
/// `call` is a [`StoredCall`] verbatim, which is the whole point — a field added
/// to the live feed arrives here, and an automation written against
/// `GET /api/calls` needs no second parser.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct Marked<'a> {
    /// Which marks this Call carries **that this webhook asked for** — not every
    /// mark on the Call. A webhook watching only for tone-outs should not have
    /// to work out why it was sent something.
    marks: &'a Marks,
    call: &'a StoredCall,
}

/// What Discord accepts: a message with one embed.
///
/// Only the fields we send, so a snapshot of this is a snapshot of the request.
/// Everything optional is omitted rather than sent null — Discord validates the
/// document and answers `400` for a field it does not like, which this queue
/// treats as permanent.
#[derive(Debug, Serialize)]
struct DiscordMessage {
    embeds: Vec<DiscordEmbed>,
}

/// **No `rename_all` here, deliberately.** Discord's embed schema is
/// *snake_case* (`icon_url`, `proxy_url`), which is the opposite of every other
/// document in this codebase — so a blanket camelCase rule would be inert today
/// (every field below is one word) and quietly wrong the first time somebody
/// adds a footer icon or a thumbnail, renaming a field Discord would then ignore
/// or refuse. The snapshot would pin the wrong name just as happily.
#[derive(Debug, Serialize)]
struct DiscordEmbed {
    title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    color: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    timestamp: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    fields: Vec<DiscordField>,
    footer: DiscordFooter,
}

#[derive(Debug, Serialize)]
struct DiscordField {
    name: String,
    value: String,
    inline: bool,
}

#[derive(Debug, Serialize)]
struct DiscordFooter {
    text: String,
}

/// Discord's own red (`#ED4245`), as the integer its API takes.
///
/// One colour for every **Mark** rather than a palette per mark: they all mean
/// "look at this", and a channel where some marked Calls are red and others are
/// amber is one where the amber ones stop being read.
const MARKED_COLOR: u32 = 0xED_42_45;

/// Discord's documented limits on the fields we use. Exceeding one is a `400`,
/// which this queue abandons — so an Operator with a very long Talkgroup label
/// must lose the end of a *title*, never the Call.
const TITLE_LIMIT: usize = 256;
const FIELD_VALUE_LIMIT: usize = 1024;

/// Build the body one delivery POSTs.
///
/// `public_url` is where this Instance can be reached from outside, if an
/// Operator has said — see the module note for why its absence costs a link
/// rather than the delivery.
pub fn body(format: Format, call: &StoredCall, marks: &Marks, public_url: Option<&str>) -> Body {
    let json = match format {
        Format::RadioScout => serde_json::to_vec(&Marked {
            marks,
            call: &absolute(call, public_url),
        }),
        Format::Discord => serde_json::to_vec(&discord(call, marks, public_url)),
    };
    Body {
        content_type: String::from("application/json"),
        // Neither document can fail to serialize — both are plain structs over
        // owned strings and numbers, with no map keys and no non-finite floats.
        // An empty body would be refused by any sink, which is the honest
        // outcome of an impossible case and costs nothing to spell.
        bytes: json.unwrap_or_default(),
    }
}

/// The Call with its `audio_url` made absolute, or with none at all.
///
/// A clone rather than a mutation of the caller's Call, because [`body`] is pure
/// and a renderer that edited what it was shown would be a surprise waiting for
/// the second caller.
fn absolute(call: &StoredCall, public_url: Option<&str>) -> StoredCall {
    StoredCall {
        audio_url: call
            .audio_url
            .as_deref()
            .and_then(|path| audio_url(path, public_url)),
        ..call.clone()
    }
}

/// A Call's audio as somewhere else can fetch it, or `None` if this Instance has
/// not been told where it lives.
///
/// The trailing slash is trimmed off the base rather than assumed absent: an
/// Operator pastes what their browser shows them, and `https://scan.example/`
/// with `/api/call/1/audio` would otherwise produce a double slash — which most
/// servers tolerate and some proxies do not.
fn audio_url(path: &str, public_url: Option<&str>) -> Option<String> {
    Some(format!("{}{path}", public_url?.trim_end_matches('/')))
}

/// The Discord message for one marked Call.
fn discord(call: &StoredCall, marks: &Marks, public_url: Option<&str>) -> DiscordMessage {
    let mut fields = Vec::new();
    // **First**, because on a paging channel it is the whole message: an
    // Operator reading a chat room wants "Station 12", and everything below is
    // context for it.
    if let Some(paged) = paged_stations(call) {
        fields.push(field("Paged", paged));
    }
    if let Some(system) = system_name(call) {
        fields.push(field("System", system));
    }
    if let Some(unit) = unit_name(call) {
        fields.push(field("Unit", unit));
    }
    if let Some(duration) = call.duration_ms.map(human_duration) {
        fields.push(field("Duration", duration));
    }

    DiscordMessage {
        embeds: vec![DiscordEmbed {
            title: truncate(&talkgroup_name(call), TITLE_LIMIT),
            // Omitted rather than relative — see the module note. This is the
            // one field whose absence is a design decision rather than a
            // missing fact.
            url: call
                .audio_url
                .as_deref()
                .and_then(|path| audio_url(path, public_url)),
            description: marks_sentence(marks),
            color: MARKED_COLOR,
            timestamp: call.timestamp.and_then(rfc3339),
            fields,
            footer: DiscordFooter {
                text: String::from("Radio-Scout"),
            },
        }],
    }
}

/// What to call the channel: the label an Operator curated, else the Ref.
///
/// The Ref fallback is [`crate::call::StoredCall`]'s own rule one layer up (a
/// bare number still says *which* channel, and two Calls carrying the same one
/// are the same channel), and it matters more here than on a screen: a Discord
/// embed with an empty title is refused with a `400`.
fn talkgroup_name(call: &StoredCall) -> String {
    said(&call.talkgroup_label).unwrap_or_else(|| format!("Talkgroup {}", call.talkgroup_ref))
}

fn system_name(call: &StoredCall) -> Option<String> {
    Some(said(&call.system_label).unwrap_or_else(|| format!("System {}", call.system_ref)))
}

/// A stored label that actually **says** something.
///
/// Blank counts as absent, and that is not tidiness: Discord refuses an embed
/// whose title or field value is the empty string (`BASE_TYPE_REQUIRED`) with a
/// `400`, which this queue abandons — so a Call whose Talkgroup carries `""`
/// would be *dropped* rather than rendered under its Ref. The admin form clears
/// blanks on the way in ([`crate::curate::optional_text`]), so this is the
/// belt to that braces: a label can also arrive from a CSV, a configuration
/// document, or **Mining**.
fn said(label: &Option<String>) -> Option<String> {
    label.as_deref().and_then(named)
}

/// [`said`] over a string that is always there — same rule, one fewer `Option`
/// to build in order to ask it.
fn named(label: &str) -> Option<String> {
    match label.trim() {
        "" => None,
        text => Some(text.to_owned()),
    }
}

/// The radio, named the way every other surface names it — the curated alias if
/// there is one, else the bare Ref, else nothing at all.
///
/// Nothing at all rather than a dash: a Discord field with a placeholder in it
/// is a row of noise on every Call from a recorder that sends no source list.
fn unit_name(call: &StoredCall) -> Option<String> {
    match (said(&call.unit_label), call.unit_ref) {
        (Some(label), _) => Some(label),
        (None, Some(unit_ref)) => Some(unit_ref.to_string()),
        (None, None) => None,
    }
}

/// The marks, as a human reads them. `None` cannot arise from a real delivery —
/// a Call with no marks reaches no webhook — and costs the line rather than the
/// message if it ever did.
fn marks_sentence(marks: &Marks) -> Option<String> {
    let words: Vec<&str> = marks
        .iter()
        .map(|mark| match mark {
            super::Mark::Emergency => "Emergency",
            super::Mark::Tone => "Tone-out",
        })
        .collect();
    match words.is_empty() {
        true => None,
        false => Some(words.join(", ")),
    }
}

/// Which stations were paged, as a human reads them.
///
/// A blank profile label is dropped for [`said`]'s reason — Discord refuses an
/// empty field value with a `400`, which this queue abandons — and a set that is
/// *entirely* blank yields no field at all rather than an empty one. The mark
/// itself still rides in the description, so the message never silently loses
/// the fact that a page happened.
fn paged_stations(call: &StoredCall) -> Option<String> {
    let named: Vec<String> = call
        .tones
        .iter()
        .filter_map(|page| named(&page.label))
        .collect();
    match named.is_empty() {
        true => None,
        false => Some(named.join(", ")),
    }
}

fn field(name: &str, value: String) -> DiscordField {
    DiscordField {
        name: name.to_owned(),
        value: truncate(&value, FIELD_VALUE_LIMIT),
        inline: true,
    }
}

/// How long the transmission was, for somebody reading a channel.
///
/// Whole seconds under a minute and `m:ss` above it — a scanner Call is
/// typically four seconds long, and "4s" is what a human wants where "4.217s"
/// is what a machine does. The machine has [`Format::RadioScout`], which carries
/// the milliseconds untouched.
fn human_duration(ms: i64) -> String {
    let seconds = ms.max(0) / 1_000;
    match seconds < 60 {
        true => format!("{seconds}s"),
        false => format!("{}m {:02}s", seconds / 60, seconds % 60),
    }
}

/// `at_ms` as Discord's `timestamp` wants it, or `None` for an instant no
/// calendar can represent — which costs the field rather than the delivery,
/// [`crate::downstream::dialect`]'s rule for the same problem.
fn rfc3339(at_ms: i64) -> Option<String> {
    use time::format_description::well_known::Rfc3339;
    time::OffsetDateTime::from_unix_timestamp_nanos(at_ms as i128 * 1_000_000)
        .ok()?
        .format(&Rfc3339)
        .ok()
}

/// `text`, cut to `limit` **characters**.
///
/// Characters rather than bytes, because cutting a UTF-8 string mid-codepoint
/// panics — and a Talkgroup label is exactly the field an Operator would put a
/// non-ASCII character in.
///
/// **No length comparison**, deliberately: `take` already stops early on a short
/// string, so a `text.chars().count() > limit` guard around this would be a
/// branch whose two arms return the same string at the boundary — an equivalent
/// mutant bought with a redundant comparison, which is the shape #83's rule says
/// to remove rather than exclude. The cost is one allocation on a string that
/// did not need one, on a path that already serializes a JSON document.
fn truncate(text: &str, limit: usize) -> String {
    text.chars().take(limit).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    fn a_call() -> StoredCall {
        StoredCall {
            id: 42,
            system_ref: 11,
            system_label: Some(String::from("Fulton County")),
            talkgroup_ref: 54241,
            talkgroup_label: Some(String::from("Fire Dispatch")),
            talkgroup_group: Some(String::from("Fire")),
            talkgroup_tag: Some(String::from("Fire Dispatch")),
            led: Some(String::from("red")),
            patches: vec![],
            frequency: Some(853_500_000),
            unit_ref: Some(1234),
            unit_label: Some(String::from("Engine 1")),
            timestamp: Some(1_700_000_000_000),
            audio_mime: Some(String::from("audio/wav")),
            duration_ms: Some(7_400),
            emergency: true,
            encrypted: false,
            tone: false,
            tones: Vec::new(),
            site_ref: Some(3),
            site_label: Some(String::from("North Tower")),
            object_key: String::from("calls/42.wav"),
            audio_url: Some(String::from("/api/call/42/audio")),
        }
    }

    fn rendered(body: &Body) -> serde_json::Value {
        serde_json::from_slice(&body.bytes).expect("a JSON body")
    }

    /// Our own shape carries the Call **verbatim**, which is the promise: an
    /// automation that has parsed a search result has parsed this.
    #[test]
    fn our_own_shape_is_the_call_a_listener_would_see() {
        let body = body(
            Format::RadioScout,
            &a_call(),
            &Marks::on_call(true, false),
            Some("https://scan.example"),
        );

        assert_eq!(body.content_type, "application/json");
        let json = rendered(&body);
        assert_eq!(json["marks"], serde_json::json!(["emergency"]));
        assert_eq!(json["call"]["id"], 42);
        assert_eq!(json["call"]["talkgroupLabel"], "Fire Dispatch");
        assert_eq!(json["call"]["emergency"], true);
        assert_eq!(json["call"]["durationMs"], 7_400);
    }

    /// **The audio URL is absolute or absent, never relative.** Discord answers
    /// `400` for a relative one, and a `400` abandons the delivery — so the
    /// Emergency this webhook existed to carry would be dropped rather than
    /// retried, over a setting nobody filled in.
    #[rstest]
    #[case::configured(
        Some("https://scan.example"),
        Some("https://scan.example/api/call/42/audio")
    )]
    #[case::trailing_slash(
        Some("https://scan.example/"),
        Some("https://scan.example/api/call/42/audio")
    )]
    #[case::unset(None, None)]
    fn a_link_is_absolute_or_absent(
        #[case] public_url: Option<&str>,
        #[case] expected: Option<&str>,
    ) {
        let ours = rendered(&body(
            Format::RadioScout,
            &a_call(),
            &Marks::on_call(true, false),
            public_url,
        ));
        let theirs = rendered(&body(
            Format::Discord,
            &a_call(),
            &Marks::on_call(true, false),
            public_url,
        ));

        assert_eq!(ours["call"]["audioUrl"].as_str(), expected);
        assert_eq!(theirs["embeds"][0]["url"].as_str(), expected);
    }

    /// An **encrypted** Call has no audio to link to at all, whatever the
    /// Instance's public URL is — the same fact `StoredCall` already says by
    /// omitting the field.
    #[test]
    fn a_call_with_no_audio_is_linked_to_nowhere() {
        let call = StoredCall {
            encrypted: true,
            audio_url: None,
            ..a_call()
        };

        let json = rendered(&body(
            Format::Discord,
            &call,
            &Marks::on_call(true, false),
            Some("https://scan.example"),
        ));

        assert!(json["embeds"][0]["url"].is_null());
    }

    /// The Discord message, pinned whole — this is a third party's schema, and
    /// a field they stop accepting is a `400` we would abandon Calls over.
    #[test]
    fn the_discord_message_is_pinned_field_by_field() {
        let body = body(
            Format::Discord,
            &a_call(),
            &Marks::on_call(true, false),
            Some("https://scan.example"),
        );

        assert_eq!(body.content_type, "application/json");
        insta::assert_json_snapshot!(rendered(&body));
    }

    /// Our own shape, pinned whole beside it — every field an automation reads.
    #[test]
    fn our_own_message_is_pinned_field_by_field() {
        let body = body(
            Format::RadioScout,
            &a_call(),
            &Marks::on_call(true, false),
            Some("https://scan.example"),
        );

        insta::assert_json_snapshot!(rendered(&body));
    }

    /// **A title is never empty and never over Discord's limit**, because either
    /// is a `400` and a `400` loses the Call. An uncurated channel falls back to
    /// its Ref, which is what every other surface does — and so does one whose
    /// stored label is blank, which a CSV or a configuration document can
    /// produce even though the admin form cannot.
    #[rstest]
    #[case::curated(Some("Fire Dispatch"), "Fire Dispatch")]
    #[case::uncurated(None, "Talkgroup 54241")]
    #[case::blank(Some("  "), "Talkgroup 54241")]
    fn a_channel_with_no_name_is_still_titled(#[case] label: Option<&str>, #[case] expected: &str) {
        let call = StoredCall {
            talkgroup_label: label.map(str::to_owned),
            ..a_call()
        };

        let json = rendered(&body(
            Format::Discord,
            &call,
            &Marks::on_call(true, false),
            Some("https://scan.example"),
        ));

        assert_eq!(json["embeds"][0]["title"], expected);
    }

    /// A label longer than Discord accepts costs the end of a title, never the
    /// Call — and is cut on a **character** boundary, because cutting UTF-8 by
    /// bytes panics and a label is exactly where a non-ASCII character lands.
    ///
    /// The shorter case is asserted beside it: a name at or under the limit must
    /// come through whole, which is what says the cut is a *ceiling* rather than
    /// a fixed width.
    #[rstest]
    #[case::over(400, TITLE_LIMIT)]
    #[case::exactly(TITLE_LIMIT, TITLE_LIMIT)]
    #[case::under(12, 12)]
    fn an_over_long_label_is_cut_rather_than_refused(
        #[case] label_chars: usize,
        #[case] expected: usize,
    ) {
        let call = StoredCall {
            talkgroup_label: Some("é".repeat(label_chars)),
            ..a_call()
        };

        let json = rendered(&body(
            Format::Discord,
            &call,
            &Marks::on_call(true, false),
            Some("https://scan.example"),
        ));

        let title = json["embeds"][0]["title"].as_str().expect("a title");
        assert_eq!(title.chars().count(), expected);
    }

    /// Only the facts a Call actually carries become fields: a recorder that
    /// sends no source list must not put an empty "Unit" row on every message —
    /// and an empty *value* is a `400` Discord would make us drop the Call over.
    #[test]
    fn a_call_that_says_less_renders_fewer_fields() {
        let call = StoredCall {
            unit_ref: None,
            unit_label: None,
            duration_ms: None,
            system_label: Some(String::new()),
            ..a_call()
        };

        let json = rendered(&body(
            Format::Discord,
            &call,
            &Marks::on_call(true, false),
            Some("https://scan.example"),
        ));

        let names: Vec<&str> = json["embeds"][0]["fields"]
            .as_array()
            .expect("fields")
            .iter()
            .map(|f| f["name"].as_str().expect("a name"))
            .collect();
        assert_eq!(names, vec!["System"]);
        // ...and the one field there is says something, rather than being the
        // empty string Discord refuses.
        assert_eq!(json["embeds"][0]["fields"][0]["value"], "System 11");
    }

    /// A radio with no curated name is still named, by its Ref — the same
    /// fallback `UnitLink` makes on the client, because the same number on three
    /// Calls says they are the same radio.
    #[rstest]
    #[case::curated(Some("Engine 1"), Some(1234), Some("Engine 1"))]
    #[case::bare_ref(None, Some(1234), Some("1234"))]
    #[case::silent(None, None, None)]
    // A stored blank is *absent*, not a name — Discord refuses an empty field
    // value with a `400`, and a `400` loses the Call.
    #[case::blank(Some("   "), Some(1234), Some("1234"))]
    #[case::blank_and_nameless(Some(""), None, None)]
    fn a_radio_is_named_the_way_every_other_surface_names_it(
        #[case] label: Option<&str>,
        #[case] unit_ref: Option<i64>,
        #[case] expected: Option<&str>,
    ) {
        let call = StoredCall {
            unit_label: label.map(str::to_owned),
            unit_ref,
            ..a_call()
        };

        assert_eq!(unit_name(&call).as_deref(), expected);
    }

    /// A scanner Call is four seconds long and a human wants "4s"; the machine
    /// has the milliseconds in the other format.
    #[rstest]
    #[case(0, "0s")]
    #[case(7_400, "7s")]
    #[case(59_999, "59s")]
    #[case(60_000, "1m 00s")]
    #[case(605_000, "10m 05s")]
    #[case(-1, "0s")]
    fn a_duration_reads_as_a_person_would_say_it(#[case] ms: i64, #[case] expected: &str) {
        assert_eq!(human_duration(ms), expected);
    }

    /// An instant no calendar can represent costs the field, not the delivery —
    /// a Recorder is entitled to send one.
    #[test]
    fn an_impossible_instant_costs_the_timestamp_and_nothing_else() {
        let call = StoredCall {
            timestamp: Some(i64::MAX),
            ..a_call()
        };

        let json = rendered(&body(
            Format::Discord,
            &call,
            &Marks::on_call(true, false),
            Some("https://scan.example"),
        ));

        assert!(json["embeds"][0]["timestamp"].is_null());
        assert_eq!(json["embeds"][0]["title"], "Fire Dispatch");
    }

    /// The renderer is **pure**: the Call it was shown is the Call the caller
    /// still holds, absolute URL or not.
    #[test]
    fn rendering_leaves_the_call_it_was_given_alone() {
        let call = a_call();

        body(
            Format::RadioScout,
            &call,
            &Marks::on_call(true, false),
            Some("https://scan.example"),
        );

        assert_eq!(call.audio_url.as_deref(), Some("/api/call/42/audio"));
    }

    /// A mark set nothing could produce still renders a message rather than a
    /// half-built one — the arm that exists so a future mark cannot make an
    /// embed Discord refuses.
    #[test]
    fn a_message_about_no_marks_omits_the_sentence_rather_than_emptying_it() {
        assert_eq!(marks_sentence(&Marks::default()), None);
        assert_eq!(
            marks_sentence(&Marks::on_call(true, false)).as_deref(),
            Some("Emergency")
        );
    }
}
