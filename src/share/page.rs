//! The page behind a share link (#64, spec US 32) — rendered here, whole, as a
//! string.
//!
//! **Server-rendered, and it has to be.** A messaging app's link preview is
//! fetched by a crawler that does not run JavaScript, so an OG card cannot come
//! from the SPA; and "playable without the app" is a promise this keeps by being
//! one self-contained file with its own `<audio>` element — it works in a
//! checkout where `client/dist` was never built, which is the same reason
//! [`crate::web`]'s fallback page exists.
//!
//! **Rendering is pure**, so every rule about it is a value a test states: what
//! the card says, which meta tags an Instance that has not been told its own
//! address omits, that an **Encrypted Call** gets facts and no player, and that
//! a Talkgroup somebody named `<script>` renders as text. That last one is not
//! hypothetical — a label arrives from a recorder as often as from an Operator,
//! and this is the only surface in the project that puts one into HTML rather
//! than into JSON a browser escapes for us.
//!
//! **The instant is rendered in UTC and localised by the browser.** The card has
//! to carry a readable time and the crawler fetching it is not in the
//! recipient's timezone — nor is the server. So the markup carries the
//! machine-readable instant, the description carries UTC spelled out, and three
//! lines of script rewrite what a human sees to their own locale. A visitor with
//! JavaScript off reads UTC, which is correct rather than wrong.

use std::fmt::Write;

use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};

use super::{Gone, audio_link_to, link_to};
use crate::call::StoredCall;

/// A page and the status it answers with.
pub struct Rendered {
    html: String,
    /// `None` for a Call, otherwise why there is nothing to show — which
    /// decides both the status and the line written down.
    gone: Option<Gone>,
}

impl Rendered {
    pub fn call(preview: Preview) -> Self {
        Rendered::ok(render(&preview))
    }

    /// A page somebody else built — an **Event**'s (#67), which is a list where
    /// this is one Call.
    ///
    /// Shared rather than re-implemented because what [`Rendered`] carries is
    /// not the markup: it is the `noindex`, the `no-store`, and the rule that a
    /// refusal is *recorded here and rendered there*. A second `IntoResponse`
    /// would be a second chance to forget one of the three.
    pub fn ok(html: String) -> Self {
        Rendered { html, gone: None }
    }

    pub fn gone(gone: Gone) -> Self {
        Rendered {
            html: render_gone(gone),
            gone: Some(gone),
        }
    }
}

impl IntoResponse for Rendered {
    fn into_response(self) -> Response {
        let status = match self.gone {
            // **Recorded here, rendered here.** `Reason::into_response` would
            // answer a human looking at a web page with a recorder's
            // `text/plain`; what is shared with every other refusal is the slug,
            // the level and the status, which is #92's rule exactly — the shape
            // may differ, the policy may not. `record` is ingest's own split.
            Some(gone) => {
                let reason = gone.reason();
                reason.record();
                reason.status()
            }
            None => StatusCode::OK,
        };
        (
            status,
            [
                (header::CONTENT_TYPE, "text/html; charset=utf-8"),
                // A link somebody was *given* must not become a link anybody
                // can find: no store, and no index.
                (header::CACHE_CONTROL, "private, no-store"),
                (
                    header::HeaderName::from_static("x-robots-tag"),
                    "noindex, nofollow",
                ),
            ],
            self.html,
        )
            .into_response()
    }
}

/// Everything the page says about one Call.
///
/// Built from a [`StoredCall`] rather than *being* one, because what a page
/// needs is words: a Talkgroup that nobody labelled reads as "Talkgroup 54241"
/// here, where the wire carries a `talkgroupRef` and lets the client decide.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Preview {
    pub talkgroup: String,
    pub system: String,
    /// The Tag, the Group, or both — whatever this Call is filed under.
    pub category: Option<String>,
    /// Who keyed it, where anybody knows (#47).
    pub unit: Option<String>,
    pub at_ms: Option<i64>,
    pub duration_ms: Option<i64>,
    pub emergency: bool,
    pub encrypted: bool,
    pub tone: bool,
    /// Where the bytes are, as a path — absent for an **Encrypted Call**, which
    /// is a row with no object behind it at all (spec US 9). Absent means the
    /// page renders facts and no player, rather than a player that 404s.
    pub audio_url: Option<String>,
    pub audio_mime: Option<String>,
    pub expires_at_ms: i64,
    /// This page's own absolute URL, the audio's, and the card's icon — each
    /// `None` until an Operator sets `[server] public_url`. The card renders
    /// either way; these are the three things that cannot be relative.
    pub canonical: Option<String>,
    pub absolute_audio: Option<String>,
    pub icon: Option<String>,
}

impl Preview {
    /// Read one off the Archive's own view of a Call.
    pub fn of(
        call: &StoredCall,
        token: &str,
        expires_at_ms: i64,
        public_url: Option<&str>,
    ) -> Self {
        // Through the **token**, never through `/api/call/{id}/audio`: a share
        // link opens one Call and reaches nothing else, and that has to stay
        // true when #68 makes the Archive's own audio route access-scoped.
        let audio_url = call.audio_url.is_some().then(|| audio_link_to(token));
        Preview {
            talkgroup: call
                .talkgroup_label
                .clone()
                .unwrap_or_else(|| format!("Talkgroup {}", call.talkgroup_ref)),
            system: call
                .system_label
                .clone()
                .unwrap_or_else(|| format!("System {}", call.system_ref)),
            category: category(call),
            unit: call.unit_label.clone().or_else(|| {
                // A bare Ref is the honest fallback and the client's own
                // (`lib/call.ts`): the same number on three Calls says they
                // were the same radio, which is the whole thing rdio throws
                // away by never showing units at all.
                call.unit_ref.map(|unit| format!("Unit {unit}"))
            }),
            at_ms: call.timestamp,
            duration_ms: call.duration_ms,
            emergency: call.emergency,
            encrypted: call.encrypted,
            tone: call.tone,
            absolute_audio: audio_url
                .as_deref()
                .and_then(|path| crate::config::absolute_url(path, public_url)),
            audio_url,
            audio_mime: call.audio_mime.clone(),
            expires_at_ms,
            canonical: crate::config::absolute_url(&link_to(token), public_url),
            icon: crate::config::absolute_url(ICON_PATH, public_url),
        }
    }

    /// The facts under the title: where, what it is filed under, when, how long,
    /// and who keyed it — whichever of those anybody recorded.
    ///
    /// **The instant is in or out**, and that is the only difference between the
    /// card's line and the page's. A crawler reads the card and will not run the
    /// script that localises a timestamp, so the card spells the instant out;
    /// the page puts it on a line of its own where the browser can rewrite it
    /// into the reader's own timezone.
    fn facts(&self, with_instant: bool) -> String {
        let mut parts = vec![self.system.clone()];
        if let Some(category) = &self.category {
            parts.push(category.clone());
        }
        if let Some(at_ms) = self.at_ms.filter(|_| with_instant) {
            parts.push(utc(at_ms));
        }
        if let Some(duration_ms) = self.duration_ms {
            parts.push(duration(duration_ms));
        }
        if let Some(unit) = &self.unit {
            parts.push(unit.clone());
        }
        parts.join(" · ")
    }
}

/// What a Call is filed under: its Tag, its Group, or both.
pub(crate) fn category(call: &StoredCall) -> Option<String> {
    match (
        call.talkgroup_group.as_deref(),
        call.talkgroup_tag.as_deref(),
    ) {
        (Some(group), Some(tag)) => Some(format!("{group} · {tag}")),
        (Some(one), None) | (None, Some(one)) => Some(one.to_owned()),
        (None, None) => None,
    }
}

/// A duration as `m:ss`, the client's own spelling.
pub(crate) fn duration(ms: i64) -> String {
    let total = ms.max(0) / 1_000;
    format!("{}:{:02}", total / 60, total % 60)
}

/// An instant in UTC, spelled out — the one timezone a server and a crawler can
/// agree on. See the module note for what the browser then does with it.
pub(crate) fn utc(at_ms: i64) -> String {
    let format =
        time::macros::format_description!("[year]-[month]-[day] [hour]:[minute]:[second] UTC");
    time::OffsetDateTime::from_unix_timestamp_nanos(at_ms as i128 * 1_000_000)
        .ok()
        .and_then(|at| at.format(&format).ok())
        // A stamp no calendar can hold is not worth failing a page over: the
        // Call is still playable, and every other fact about it is still true.
        .unwrap_or_default()
}

/// ...and as the machine-readable attribute beside it.
pub(crate) fn iso(at_ms: i64) -> String {
    time::OffsetDateTime::from_unix_timestamp_nanos(at_ms as i128 * 1_000_000)
        .ok()
        .and_then(|at| {
            at.format(&time::format_description::well_known::Rfc3339)
                .ok()
        })
        .unwrap_or_default()
}

/// HTML-escape a string that came from outside.
///
/// Every label on this page arrives from a **Recorder** or an **Operator**, and
/// this is the only place in the project that puts one into markup rather than
/// into JSON. `'` and `"` are escaped as well as the three that must be, because
/// the same function renders attribute values — a `content="…"` in the OG card —
/// and one escape used two ways cannot be the wrong one in either.
pub(crate) fn escape(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for ch in raw.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(ch),
        }
    }
    out
}

/// One `<meta property=…>`, or nothing when there is nothing to say.
pub(crate) fn meta(property: &str, content: Option<&str>) -> String {
    match content {
        Some(content) => format!(
            "<meta property=\"{property}\" content=\"{}\">\n",
            escape(content)
        ),
        None => String::new(),
    }
}

/// The whole page for a live link.
pub fn render(preview: &Preview) -> String {
    let title = format!("{} — {}", preview.talkgroup, preview.system);
    let summary = preview.facts(true);
    let mut head = String::new();
    head.push_str(&meta("og:type", Some("website")));
    head.push_str(&meta("og:title", Some(&title)));
    head.push_str(&meta("og:description", Some(&summary)));
    head.push_str(&meta("og:site_name", Some(SITE_NAME)));
    head.push_str(&meta("og:url", preview.canonical.as_deref()));
    // The icon and the audio are the two tags that *cannot* be relative, so they
    // are the two an Instance which has not been told its address goes without.
    // A card with a title and a description still renders everywhere; one with a
    // guessed image renders somebody else's picture.
    head.push_str(&meta("og:image", preview.icon.as_deref()));
    head.push_str(&meta("og:audio", preview.absolute_audio.as_deref()));
    if preview.absolute_audio.is_some() {
        head.push_str(&meta("og:audio:type", preview.audio_mime.as_deref()));
    }

    let mut body = String::new();
    let _ = writeln!(
        body,
        "<h1>{}</h1>\n<p class=\"sub\">{}</p>",
        escape(&preview.talkgroup),
        escape(&preview.facts(false))
    );
    if let Some(at_ms) = preview.at_ms {
        let _ = writeln!(
            body,
            "<p class=\"when\"><time datetime=\"{}\">{}</time></p>",
            escape(&iso(at_ms)),
            escape(&utc(at_ms))
        );
    }
    let marks = marks(preview);
    if !marks.is_empty() {
        let _ = writeln!(body, "<p class=\"marks\">{marks}</p>");
    }
    match &preview.audio_url {
        Some(url) => {
            let _ = writeln!(
                body,
                "<audio controls preload=\"metadata\" src=\"{}\"></audio>",
                escape(url)
            );
        }
        // An **Encrypted Call** is a row with no audio object at all. It is
        // still worth sending somebody — that the channel was busy is the fact —
        // so it gets the page and a sentence instead of a player that 404s.
        None => body.push_str("<p class=\"noaudio\">This call has no audio.</p>\n"),
    }
    let _ = writeln!(
        body,
        "<p class=\"foot\">Shared from {SITE_NAME} · this link stops working <time datetime=\"{}\">{}</time>.</p>",
        escape(&iso(preview.expires_at_ms)),
        escape(&utc(preview.expires_at_ms))
    );

    document(&title, &head, &body)
}

/// The **Marks** a Call carries, as badges — the same closed vocabulary the app
/// shows (CONTEXT.md), never a free-form tag.
fn marks(preview: &Preview) -> String {
    mark_badges(preview.emergency, preview.encrypted, preview.tone)
}

/// ...over the three bits themselves, so an **Event**'s page renders a member's
/// marks identically without building a [`Preview`] it has no expiry for (#67).
///
/// #92's rule one page along: two surfaces, one vocabulary. A badge that said
/// "Tone-out" on one page and "Tone" on the other would be this Instance
/// disagreeing with itself about a **Mark**.
pub(crate) fn mark_badges(emergency: bool, encrypted: bool, tone: bool) -> String {
    let mut out = String::new();
    for (on, label) in [
        (emergency, "Emergency"),
        (encrypted, "Encrypted"),
        (tone, "Tone-out"),
    ] {
        if on {
            let _ = write!(out, "<span class=\"mark\">{label}</span>");
        }
    }
    out
}

/// The page for a link that opens nothing.
///
/// Three states, two sentences: an expired link says so, and the other two are
/// deliberately the same sentence — an Operator who closed the door owes whoever
/// is knocking no map of it, and the difference is in the log line instead.
pub fn render_gone(gone: Gone) -> String {
    let (title, sentence) = match gone {
        Gone::Expired => (
            "This link has expired",
            "The person who shared it can send you a new one.",
        ),
        Gone::Unknown | Gone::Disabled => (
            "This link is not valid",
            "It may have been revoked, or the address may be mistyped.",
        ),
    };
    document(
        title,
        // No card at all: there is nothing to preview, and a crawler that
        // rendered "This link has expired" as a rich embed would be worse than
        // one that rendered nothing.
        "",
        &format!("<h1>{title}</h1>\n<p class=\"sub\">{sentence}</p>\n"),
    )
}

/// What the card calls this Instance.
///
/// Deliberately the product's name rather than the Operator's: **branding is
/// #71**, and inventing a second place for an instance title here would be a
/// setting to migrate the moment that lands.
pub(crate) const SITE_NAME: &str = "Radio-Scout";

/// The icon a card shows — the PWA's own, which the binary already embeds, so
/// there is no image to generate and none to store (`client/public/`, rasterized
/// by `client/scripts/build-icons.sh`).
pub(crate) const ICON_PATH: &str = "/icon-512.png";

/// The shell every page here shares.
///
/// # `noindex`, and what it costs
///
/// A link somebody was *given* must not become a link anybody can *find*, so
/// the page carries a robots directive — as a meta tag and as a header, because
/// the audio route answers with the header alone.
///
/// That is a real trade-off and it is taken deliberately. The unfurlers this
/// ticket is about — Discord, Slack, iMessage, Signal, WhatsApp, Telegram — are
/// not search crawlers and do not read robots directives, so the card renders
/// there. **Twitterbot does read them**, and a search engine certainly does. So
/// there is no `twitter:card` tag here: emitting one while telling Twitterbot
/// not to render would be incoherent, and the line this draws is the ticket's
/// own — a share link is something you *send to a person*, not something you
/// publish. An Operator who wants a Call on a public timeline has the Call's
/// own page in the app for that.
pub(crate) fn document(title: &str, head: &str, body: &str) -> String {
    format!(
        r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1, viewport-fit=cover">
<meta name="robots" content="noindex, nofollow">
<title>{title}</title>
{head}<style>{STYLE}</style>
</head>
<body>
<main class="card">
{body}</main>
{SCRIPT}</body>
</html>
"#,
        title = escape(title),
    )
}

/// Inline, because the page is one file by design and a stylesheet request
/// would be a second thing that can fail on somebody else's network.
const STYLE: &str = ":root{color-scheme:dark light}\
*{box-sizing:border-box}\
body{margin:0;min-height:100vh;display:flex;align-items:center;justify-content:center;\
font:15px/1.55 ui-sans-serif,system-ui,sans-serif;background:#09090b;color:#fafafa;padding:1.5rem}\
@media (prefers-color-scheme:light){body{background:#fafafa;color:#09090b}}\
.card{width:100%;max-width:32rem;border:1px solid #27272a80;border-radius:14px;padding:1.4rem}\
h1{font-size:1.35rem;margin:0 0 .3rem;line-height:1.2}\
.sub,.when,.foot{margin:.15rem 0;color:#a1a1aa;font-size:.85rem}\
.foot{margin-top:1rem;font-size:.75rem}\
.marks{margin:.6rem 0 0}\
.mark{display:inline-block;margin-right:.4rem;padding:.1rem .5rem;border-radius:999px;\
border:1px solid #a1a1aa80;font-size:.72rem;text-transform:uppercase;letter-spacing:.04em}\
.noaudio{margin-top:1rem;font-size:.85rem;color:#a1a1aa}\
audio{width:100%;margin-top:1rem}";

/// Localise **every** instant the markup carries — the Call's, and the moment
/// the link stops working.
///
/// All of them rather than the one, because two times on one card in two
/// timezones is worse than either alone: a recipient reading "7:38 PM" above
/// "23:38 UTC" has to work out whether those are the same evening.
///
/// Four lines, and the page is correct without them: a visitor with JavaScript
/// off reads UTC, which is exactly what the `datetime` attribute beside it says.
const SCRIPT: &str = "<script>\
document.querySelectorAll('time[datetime]').forEach(function(t){\
try{t.textContent=new Date(t.dateTime).toLocaleString()}catch(e){}\
});\
</script>\n";

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    fn a_call() -> StoredCall {
        StoredCall {
            restricted: false,
            id: 42,
            system_ref: 11,
            system_label: Some(String::from("Fulton County")),
            talkgroup_ref: 54241,
            talkgroup_label: Some(String::from("Fire Dispatch")),
            talkgroup_group: Some(String::from("Fire")),
            talkgroup_tag: Some(String::from("Dispatch")),
            led: Some(String::from("red")),
            patches: vec![],
            frequency: Some(853_500_000),
            unit_ref: Some(1234),
            unit_label: Some(String::from("Engine 1")),
            timestamp: Some(1_700_000_000_000),
            audio_mime: Some(String::from("audio/wav")),
            duration_ms: Some(74_000),
            emergency: false,
            encrypted: false,
            tone: false,
            tones: Vec::new(),
            quiet: Vec::new(),
            starred: false,
            site_ref: Some(3),
            site_label: Some(String::from("North Tower")),
            object_key: String::from("calls/42.wav"),
            audio_url: Some(String::from("/api/call/42/audio")),
        }
    }

    fn preview_of(call: &StoredCall, public_url: Option<&str>) -> Preview {
        Preview::of(call, "tok", 1_700_600_000_000, public_url)
    }

    /// The card a messaging app renders: what it is, where it was, and when.
    /// Title and description are the two tags every crawler reads, so they are
    /// present whether or not this Instance knows its own address.
    #[test]
    fn the_card_says_what_the_call_is() {
        let html = render(&preview_of(&a_call(), None));

        assert!(
            html.contains(r#"<meta property="og:title" content="Fire Dispatch — Fulton County">"#),
            "{html}"
        );
        assert!(
            html.contains(
                r#"<meta property="og:description" content="Fulton County · Fire · Dispatch · 2023-11-14 22:13:20 UTC · 1:14 · Engine 1">"#
            ),
            "{html}"
        );
        assert!(
            html.contains(r#"<meta property="og:site_name" content="Radio-Scout">"#),
            "{html}"
        );
        // ...and deliberately **no** `twitter:card`: the page carries a robots
        // directive Twitterbot honours, so a card tag aimed at the one crawler
        // that has been told not to render would be incoherent. See
        // [`document`].
        assert!(!html.contains("twitter:"), "{html}");
    }

    /// **The three tags that cannot be relative are the three an Instance which
    /// has not been told its address goes without** (#54's rule, unchanged): a
    /// card with a title and a description renders everywhere, where a *guessed*
    /// origin renders somebody else's picture.
    #[test]
    fn without_a_public_url_the_card_carries_no_absolute_tag() {
        let html = render(&preview_of(&a_call(), None));

        for absolute in ["og:url", "og:image", "og:audio"] {
            assert!(!html.contains(absolute), "{absolute} in {html}");
        }
    }

    /// ...and with one, all three point at this Instance — the page, the icon
    /// the binary already embeds, and the audio behind the same token.
    #[test]
    fn a_public_url_completes_the_card() {
        let html = render(&preview_of(&a_call(), Some("https://scan.example/")));

        assert!(
            html.contains(r#"<meta property="og:url" content="https://scan.example/s?t=tok">"#),
            "{html}"
        );
        assert!(
            html.contains(
                r#"<meta property="og:image" content="https://scan.example/icon-512.png">"#
            ),
            "{html}"
        );
        assert!(
            html.contains(
                r#"<meta property="og:audio" content="https://scan.example/s/audio?t=tok">"#
            ),
            "{html}"
        );
        assert!(
            html.contains(r#"<meta property="og:audio:type" content="audio/wav">"#),
            "{html}"
        );
    }

    /// The player points at the **token**, never at the Archive's own audio
    /// route: a share link opens one Call and reaches nothing else.
    #[test]
    fn the_player_plays_through_the_token() {
        let html = render(&preview_of(&a_call(), None));

        assert!(
            html.contains(r#"<audio controls preload="metadata" src="/s/audio?t=tok"></audio>"#),
            "{html}"
        );
        assert!(!html.contains("/api/call/"), "{html}");
    }

    /// An **Encrypted Call** is a row with no audio object at all (spec US 9).
    /// It is still worth sending somebody — that the channel was busy is the
    /// fact — so it gets the page and a sentence rather than a player that 404s.
    #[test]
    fn an_encrypted_call_is_shareable_and_has_no_player() {
        let call = StoredCall {
            encrypted: true,
            audio_url: None,
            object_key: String::new(),
            audio_mime: None,
            ..a_call()
        };

        let html = render(&preview_of(&call, Some("https://scan.example")));

        assert!(!html.contains("<audio"), "{html}");
        assert!(html.contains("This call has no audio."), "{html}");
        assert!(html.contains("Encrypted"), "{html}");
        // Nothing to play means nothing to point a card's audio tag at.
        assert!(!html.contains("og:audio"), "{html}");
    }

    /// The **Marks** a Call carries are shown, and only those it carries — the
    /// closed vocabulary (CONTEXT.md), never a free-form tag.
    #[rstest]
    #[case::none(false, false, false, vec![])]
    #[case::emergency(true, false, false, vec!["Emergency"])]
    #[case::tone(false, false, true, vec!["Tone-out"])]
    #[case::all_three(true, true, true, vec!["Emergency", "Encrypted", "Tone-out"])]
    fn the_marks_a_call_carries_are_the_ones_shown(
        #[case] emergency: bool,
        #[case] encrypted: bool,
        #[case] tone: bool,
        #[case] shown: Vec<&str>,
    ) {
        let call = StoredCall {
            emergency,
            encrypted,
            tone,
            ..a_call()
        };

        let html = render(&preview_of(&call, None));

        for mark in ["Emergency", "Encrypted", "Tone-out"] {
            assert_eq!(
                html.contains(&format!("<span class=\"mark\">{mark}</span>")),
                shown.contains(&mark),
                "{mark} in {html}"
            );
        }
    }

    /// **A label is somebody else's text.** It arrives from a Recorder as often
    /// as from an Operator, and this is the only surface in the project that
    /// puts one into markup rather than into JSON a browser escapes for us — so
    /// it is escaped in the body *and* inside the card's attribute values, where
    /// an unescaped quote would end the attribute and start an element.
    #[test]
    fn a_label_never_becomes_markup() {
        let call = StoredCall {
            talkgroup_label: Some(String::from(r#"<script>alert("x")</script>"#)),
            ..a_call()
        };

        let html = render(&preview_of(&call, None));

        assert!(!html.contains("<script>alert"), "{html}");
        assert!(
            html.contains("&lt;script&gt;alert(&quot;x&quot;)&lt;/script&gt;"),
            "{html}"
        );
    }

    /// A Talkgroup, System or radio nobody has named reads as its **Ref**: the
    /// same number on two Calls says they were the same channel, which is what
    /// a blank would throw away (`lib/call.ts`'s rule, server-side).
    #[test]
    fn an_unnamed_channel_reads_as_its_ref() {
        let call = StoredCall {
            system_label: None,
            talkgroup_label: None,
            talkgroup_group: None,
            talkgroup_tag: None,
            unit_label: None,
            ..a_call()
        };

        let preview = preview_of(&call, None);

        assert_eq!(preview.talkgroup, "Talkgroup 54241");
        assert_eq!(preview.system, "System 11");
        assert_eq!(preview.unit.as_deref(), Some("Unit 1234"));
        assert_eq!(preview.category, None);
    }

    /// A radio nobody heard is not rendered at all — the [`crate::call`] rule a
    /// list already follows: a stat line needs a placeholder and a summary does
    /// not.
    #[test]
    fn a_call_that_heard_no_radio_names_none() {
        let call = StoredCall {
            unit_ref: None,
            unit_label: None,
            ..a_call()
        };

        assert_eq!(preview_of(&call, None).unit, None);
    }

    /// What a Call is filed under, with either half missing.
    #[rstest]
    #[case::both(Some("Fire"), Some("Dispatch"), Some("Fire · Dispatch"))]
    #[case::group_only(Some("Fire"), None, Some("Fire"))]
    #[case::tag_only(None, Some("Dispatch"), Some("Dispatch"))]
    #[case::neither(None, None, None)]
    fn a_category_is_whichever_halves_there_are(
        #[case] group: Option<&str>,
        #[case] tag: Option<&str>,
        #[case] expected: Option<&str>,
    ) {
        let call = StoredCall {
            talkgroup_group: group.map(String::from),
            talkgroup_tag: tag.map(String::from),
            ..a_call()
        };

        assert_eq!(category(&call).as_deref(), expected);
    }

    /// A Call whose instant or length nobody recorded still renders — those
    /// facts are simply absent, which every Call stored before #42 looks like.
    #[test]
    fn a_call_with_no_instant_and_no_length_still_renders() {
        let call = StoredCall {
            timestamp: None,
            duration_ms: None,
            ..a_call()
        };

        let html = render(&preview_of(&call, None));

        assert!(html.contains("Fire Dispatch"), "{html}");
        assert!(!html.contains(r#"class="when""#), "{html}");
        assert!(
            html.contains(r#"<meta property="og:description" content="Fulton County · Fire · Dispatch · Engine 1">"#),
            "{html}"
        );
    }

    /// The instant is carried machine-readably beside the words, which is what
    /// lets the browser localise what a human sees while the crawler reads UTC.
    #[test]
    fn the_instant_is_utc_in_words_and_rfc3339_in_the_attribute() {
        let html = render(&preview_of(&a_call(), None));

        assert!(
            html.contains(
                r#"<time datetime="2023-11-14T22:13:20Z">2023-11-14 22:13:20 UTC</time>"#
            ),
            "{html}"
        );
    }

    /// A length reads the way the app spells one.
    #[rstest]
    #[case(0, "0:00")]
    #[case(999, "0:00")]
    #[case(1_000, "0:01")]
    #[case(74_000, "1:14")]
    #[case(3_600_000, "60:00")]
    // A negative length is not a thing a recorder sends, and it is not worth a
    // panic or a `-1:-1` if one ever does.
    #[case(-5_000, "0:00")]
    fn a_length_reads_as_minutes_and_seconds(#[case] ms: i64, #[case] expected: &str) {
        assert_eq!(duration(ms), expected);
    }

    /// A stamp no calendar can hold costs the page nothing: the Call is still
    /// playable and every other fact about it is still true.
    #[test]
    fn an_impossible_instant_renders_as_nothing_rather_than_failing() {
        assert_eq!(utc(i64::MAX), "");
        assert_eq!(iso(i64::MAX), "");
    }

    /// Every character that could end an attribute or open an element.
    #[rstest]
    #[case("plain", "plain")]
    #[case("a & b", "a &amp; b")]
    #[case("<b>", "&lt;b&gt;")]
    #[case("\"quoted\"", "&quot;quoted&quot;")]
    #[case("it's", "it&#39;s")]
    fn escaping_covers_markup_and_attributes(#[case] raw: &str, #[case] expected: &str) {
        assert_eq!(escape(raw), expected);
    }

    /// An expired link says so; the other two say the same thing, because an
    /// Operator who closed the door owes whoever is knocking no map of it. None
    /// of the three carries a card — a rich embed reading "this link has
    /// expired" is worse than no embed at all.
    #[rstest]
    #[case::expired(Gone::Expired, "This link has expired")]
    #[case::unknown(Gone::Unknown, "This link is not valid")]
    #[case::disabled(Gone::Disabled, "This link is not valid")]
    fn a_dead_link_says_which_kind_of_dead(#[case] gone: Gone, #[case] sentence: &str) {
        let html = render_gone(gone);

        assert!(html.contains(sentence), "{html}");
        assert!(!html.contains("og:"), "{html}");
        assert!(!html.contains("<audio"), "{html}");
    }

    /// Every page says when it stops working, so a recipient who bookmarks one
    /// knows it is not a bookmark.
    #[test]
    fn the_page_says_when_the_link_runs_out() {
        let html = render(&preview_of(&a_call(), None));

        assert!(
            html.contains(r#"this link stops working <time datetime="2023-11-21T20:53:20Z">2023-11-21 20:53:20 UTC</time>"#),
            "{html}"
        );
    }

    /// Nothing on this page reaches anything else on the Instance: no link out,
    /// no script but the one that localises a timestamp, and a crawler is told
    /// not to index it.
    #[test]
    fn nothing_else_is_reachable_from_a_share_page() {
        let html = render(&preview_of(&a_call(), Some("https://scan.example")));

        assert!(!html.contains("<a "), "{html}");
        assert_eq!(html.matches("<script").count(), 1, "{html}");
        assert!(
            html.contains(r#"<meta name="robots" content="noindex, nofollow">"#),
            "{html}"
        );
    }
}
