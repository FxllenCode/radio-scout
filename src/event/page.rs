//! The page behind an **Event**'s share link (#67, spec US 38) — rendered here,
//! whole, as a string.
//!
//! [`crate::share::page`] is one Call; this is the incident. Everything the two
//! have in common is *shared* rather than written twice — the shell, the style,
//! the escaping, the UTC-then-localise trick, the `noindex`/`no-store` response
//! and the **Mark** badges — because a page from this Instance should look like
//! a page from this Instance, and two implementations of that drift.
//!
//! **What differs is that a recipient is handed a list.** Which means three
//! things this page decides and the Call's does not:
//!
//! - **Every member gets its own `<audio>`.** ADR-0005's constraint is about the
//!   *app*, which owns one element because it is a scanner; a static page is a
//!   document, and a reader here is choosing what to play rather than being
//!   played to. Nothing preloads, so opening an incident of four hundred Calls
//!   costs four hundred `HEAD`s of nothing at all until somebody presses one.
//! - **It pages, and says so.** An Event is curated by hand, but nothing stops
//!   an Operator putting a county's night in one, and a page that rendered every
//!   member would be the one surface here able to hand a stranger ten megabytes
//!   of markup. [`MEMBERS_SHOWN`] is what a crawler and a reader both get; the
//!   rest is reachable by downloading it, which is the control this page already
//!   offers.
//! - **The card describes the incident, not a transmission.** A link preview for
//!   an Event says what it is called and how much of it there is, because that
//!   is what somebody sending one is sending.
//!
//! There is no expiry on this page and no sentence about one: an Event's link is
//! a toggle an Operator holds, not a window (`crate::event`).

use std::fmt::Write;

use crate::call::StoredCall;
use crate::share::page::{
    SITE_NAME, category, document, duration, escape, iso, mark_badges, meta, utc,
};

/// How many members a share page renders.
///
/// Generous, because an Event is a curated incident and the common one is tens
/// rather than hundreds — and bounded, because nothing *stops* an Operator
/// putting a county's night in one, and this is the only surface here that would
/// hand a stranger the whole of it as markup.
pub const MEMBERS_SHOWN: usize = 200;

/// Everything the page says about an Event.
///
/// Built from the rows rather than *being* them, for [`crate::share::page::Preview`]'s
/// reason: what a page needs is words. A Talkgroup nobody labelled reads as
/// "Talkgroup 54241" here, where the wire carries a `talkgroupRef` and lets the
/// client decide.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Preview {
    pub name: String,
    pub notes: Option<String>,
    /// Every member this page draws, in the order the incident happened.
    pub members: Vec<Member>,
    /// How many there are altogether — which is `members.len()` until an Event
    /// is bigger than [`MEMBERS_SHOWN`], and is what the card counts either way.
    pub total: usize,
    /// How long the whole incident runs, where every member was measured.
    pub duration_ms: i64,
    /// Where the two downloads are, as paths.
    pub zip_url: Option<String>,
    pub stitched_url: Option<String>,
    /// This page's own absolute URL and the card's icon — `None` until an
    /// Operator sets `[server] public_url`. The card renders either way; these
    /// are the two that cannot be relative.
    pub canonical: Option<String>,
    pub icon: Option<String>,
}

/// One member, as a row on the page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Member {
    pub talkgroup: String,
    /// The System, the category, the length and who keyed it — whichever of
    /// those anybody recorded, already joined.
    pub facts: String,
    pub at_ms: Option<i64>,
    pub emergency: bool,
    pub encrypted: bool,
    pub tone: bool,
    /// Where the bytes are, as a path — absent for an **Encrypted Call**, which
    /// froze as a row with no object at all. Absent means facts and no player,
    /// rather than a player that 404s.
    pub audio_url: Option<String>,
}

impl Member {
    /// Read one off a frozen Call and the link to its audio.
    pub fn of(call: &StoredCall, audio_url: Option<String>) -> Self {
        let mut facts = vec![
            call.system_label
                .clone()
                .unwrap_or_else(|| format!("System {}", call.system_ref)),
        ];
        if let Some(category) = category(call) {
            facts.push(category);
        }
        if let Some(duration_ms) = call.duration_ms {
            facts.push(duration(duration_ms));
        }
        if let Some(unit) = call.unit_label.clone().or_else(|| {
            // A bare Ref is the honest fallback and the client's own
            // (`lib/call.ts`): the same number on three Calls says they were
            // the same radio.
            call.unit_ref.map(|unit| format!("Unit {unit}"))
        }) {
            facts.push(unit);
        }
        Member {
            talkgroup: call
                .talkgroup_label
                .clone()
                .unwrap_or_else(|| format!("Talkgroup {}", call.talkgroup_ref)),
            facts: facts.join(" · "),
            at_ms: call.timestamp,
            emergency: call.emergency,
            encrypted: call.encrypted,
            tone: call.tone,
            audio_url,
        }
    }
}

impl Preview {
    /// The line under the title: how many transmissions, and how long they run.
    ///
    /// **Counts rather than an instant**, which is the whole difference from a
    /// Call's card. An Event spans hours, so "when" is a range and a range in a
    /// preview is noise; what somebody wants to know before opening a link is
    /// how much of it there is.
    fn summary(&self) -> String {
        let calls = match self.total {
            1 => String::from("1 call"),
            total => format!("{total} calls"),
        };
        match self.duration_ms {
            0 => calls,
            ms => format!("{calls} · {}", duration(ms)),
        }
    }
}

/// The whole page for a shared Event.
pub fn render(preview: &Preview) -> String {
    let summary = preview.summary();
    let mut head = String::new();
    head.push_str(&meta("og:type", Some("website")));
    head.push_str(&meta("og:title", Some(&preview.name)));
    head.push_str(&meta(
        "og:description",
        Some(&match &preview.notes {
            // The Operator's own sentence beats a generated one — they wrote it
            // to say what this was.
            Some(notes) if !notes.trim().is_empty() => format!("{summary} · {notes}"),
            _ => summary.clone(),
        }),
    ));
    head.push_str(&meta("og:site_name", Some(SITE_NAME)));
    head.push_str(&meta("og:url", preview.canonical.as_deref()));
    // The icon is the one tag that cannot be relative, so it is the one an
    // Instance which has not been told its address goes without (#54's rule).
    // There is deliberately **no `og:audio`**: an Event is many Calls, and a
    // card that played one of them would be picking for the reader.
    head.push_str(&meta("og:image", preview.icon.as_deref()));

    let mut body = String::new();
    let _ = writeln!(
        body,
        "<h1>{}</h1>\n<p class=\"sub\">{}</p>",
        escape(&preview.name),
        escape(&summary)
    );
    if let Some(notes) = preview
        .notes
        .as_deref()
        .filter(|notes| !notes.trim().is_empty())
    {
        let _ = writeln!(body, "<p class=\"notes\">{}</p>", escape(notes));
    }
    body.push_str(&downloads(preview));

    body.push_str("<ol class=\"calls\">\n");
    for member in &preview.members {
        body.push_str(&row(member));
    }
    body.push_str("</ol>\n");
    if preview.total > preview.members.len() {
        // Said out loud rather than silently truncated: a reader who was sent an
        // incident has to be able to tell "that is all of it" from "that is the
        // start of it" (ADR-0011 rule 8's argument, one surface along).
        // **And it only points at the download where there is one.** An Instance
        // with `[export] enabled = false` offers none, and an Event larger than
        // `[export] max_calls` has one that refuses — so a sentence that always
        // said "download the event for all of them" would send a recipient after
        // something they cannot have, which is the lie a control that is offered
        // and then refused tells (#64's rule).
        let rest = match preview.zip_url.is_some() {
            true => " — download the event for all of them",
            false => "",
        };
        let _ = writeln!(
            body,
            "<p class=\"more\">Showing the first {} of {} calls{rest}.</p>",
            preview.members.len(),
            preview.total
        );
    }
    let _ = writeln!(body, "<p class=\"foot\">Shared from {SITE_NAME}.</p>");

    document(&preview.name, &head, &body)
}

/// The two downloads, where this Instance offers them at all.
///
/// Absent when `[export] enabled` is off, rather than present and refused: a
/// control that lies is #64's rule, and it reaches this page the same way it
/// reaches the app's own.
fn downloads(preview: &Preview) -> String {
    let mut out = String::new();
    for (url, label) in [
        (preview.zip_url.as_deref(), "Download all (zip)"),
        (
            preview.stitched_url.as_deref(),
            "Download as one audio file",
        ),
    ] {
        if let Some(url) = url {
            let _ = writeln!(out, "<a class=\"get\" href=\"{}\">{label}</a>", escape(url));
        }
    }
    out
}

/// One member's row.
fn row(member: &Member) -> String {
    let mut out = String::from("<li class=\"call\">\n");
    let _ = writeln!(
        out,
        "<p class=\"tg\">{}</p>\n<p class=\"sub\">{}</p>",
        escape(&member.talkgroup),
        escape(&member.facts)
    );
    if let Some(at_ms) = member.at_ms {
        let _ = writeln!(
            out,
            "<p class=\"when\"><time datetime=\"{}\">{}</time></p>",
            escape(&iso(at_ms)),
            escape(&utc(at_ms))
        );
    }
    let marks = mark_badges(member.emergency, member.encrypted, member.tone);
    if !marks.is_empty() {
        let _ = writeln!(out, "<p class=\"marks\">{marks}</p>");
    }
    match &member.audio_url {
        // **`preload="none"`**, where a Call's page says `metadata`: there is one
        // player there and up to two hundred here, and a page that asked the
        // store for every header on open would be an incident costing a Pi four
        // hundred reads because somebody followed a link.
        Some(url) => {
            let _ = writeln!(
                out,
                "<audio controls preload=\"none\" src=\"{}\"></audio>",
                escape(url)
            );
        }
        None => out.push_str("<p class=\"noaudio\">This call has no audio.</p>\n"),
    }
    out.push_str("</li>\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a_member(talkgroup: &str) -> Member {
        Member {
            talkgroup: String::from(talkgroup),
            facts: String::from("Fulton County · Fire · 0:12"),
            at_ms: Some(1_700_000_000_000),
            emergency: false,
            encrypted: false,
            tone: false,
            audio_url: Some(String::from("/e/audio?t=tok&i=1")),
        }
    }

    fn a_preview() -> Preview {
        Preview {
            name: String::from("Mill Street fire"),
            notes: Some(String::from("Second alarm, 03:40.")),
            members: vec![a_member("Fire Dispatch"), a_member("Fireground 2")],
            total: 2,
            duration_ms: 74_000,
            zip_url: Some(String::from("/e/export?t=tok&format=zip")),
            stitched_url: Some(String::from("/e/export?t=tok&format=wav")),
            canonical: None,
            icon: None,
        }
    }

    /// The card a messaging app renders: what the incident is called, how much
    /// of it there is, and what the Operator wrote about it.
    #[test]
    fn the_card_says_what_the_incident_is() {
        let html = render(&a_preview());

        assert!(
            html.contains(r#"<meta property="og:title" content="Mill Street fire">"#),
            "{html}"
        );
        assert!(
            html.contains(
                r#"<meta property="og:description" content="2 calls · 1:14 · Second alarm, 03:40.">"#
            ),
            "{html}"
        );
        // An Event is many Calls, so there is deliberately no `og:audio`: a card
        // that played one of them would be picking for the reader.
        assert!(!html.contains("og:audio"), "{html}");
    }

    /// Without a note there is still a sentence — the counts, which is what
    /// somebody wants before they open a link.
    #[test]
    fn a_card_with_no_note_still_counts_the_incident() {
        let html = render(&Preview {
            notes: None,
            ..a_preview()
        });

        assert!(
            html.contains(r#"<meta property="og:description" content="2 calls · 1:14">"#),
            "{html}"
        );
    }

    /// ...and a blank one is treated as absent, rather than rendering a card
    /// that trails off into a separator with nothing after it.
    #[test]
    fn a_blank_note_is_no_note() {
        let html = render(&Preview {
            notes: Some(String::from("   ")),
            ..a_preview()
        });

        assert!(html.contains(r#"content="2 calls · 1:14">"#), "{html}");
        assert!(!html.contains("class=\"notes\""), "{html}");
    }

    /// One member is "1 call", not "1 calls".
    #[test]
    fn one_call_reads_as_one_call() {
        let html = render(&Preview {
            members: vec![a_member("Fire Dispatch")],
            total: 1,
            duration_ms: 0,
            ..a_preview()
        });

        assert!(html.contains(r#"content="1 call · Second alarm"#), "{html}");
    }

    /// **A player per member**, and none of them preloading: an incident of two
    /// hundred Calls must not cost a Pi two hundred reads because somebody
    /// followed a link.
    #[test]
    fn every_member_gets_its_own_player() {
        let html = render(&a_preview());

        assert_eq!(html.matches("<audio controls").count(), 2, "{html}");
        assert_eq!(html.matches(r#"preload="none""#).count(), 2, "{html}");
        assert!(html.contains(r#"src="/e/audio?t=tok&amp;i=1""#), "{html}");
    }

    /// An **Encrypted Call** froze as a row with no object. It is on the page
    /// because the activity is the fact, and it gets a sentence rather than a
    /// player that 404s.
    #[test]
    fn a_member_with_no_audio_gets_a_sentence() {
        let html = render(&Preview {
            members: vec![Member {
                encrypted: true,
                audio_url: None,
                ..a_member("Tac 4")
            }],
            total: 1,
            ..a_preview()
        });

        assert!(html.contains("This call has no audio."), "{html}");
        assert!(
            html.contains(r#"<span class="mark">Encrypted</span>"#),
            "{html}"
        );
        assert!(!html.contains("<audio"), "{html}");
    }

    /// **A truncated list says so.** A reader who was sent an incident has to be
    /// able to tell "that is all of it" from "that is the start of it".
    #[test]
    fn a_page_that_shows_part_of_an_event_says_so() {
        let html = render(&Preview {
            members: vec![a_member("Fire Dispatch")],
            total: 412,
            ..a_preview()
        });

        assert!(
            html.contains("Showing the first 1 of 412 calls — download the event for all of them."),
            "{html}"
        );
    }

    /// ...and where there is no download it says the first half and stops,
    /// rather than sending a recipient after something they cannot have.
    #[test]
    fn a_truncated_page_with_no_download_points_at_nothing() {
        let html = render(&Preview {
            members: vec![a_member("Fire Dispatch")],
            total: 412,
            zip_url: None,
            stitched_url: None,
            ..a_preview()
        });

        assert!(html.contains("Showing the first 1 of 412 calls."), "{html}");
        assert!(!html.contains("download the event"), "{html}");
    }

    /// ...and one that shows all of it does not.
    #[test]
    fn a_whole_event_says_nothing_about_being_partial() {
        assert!(!render(&a_preview()).contains("Showing the first"));
    }

    /// The downloads are absent rather than present-and-refused when
    /// `[export] enabled` is off — a control that lies is worse than no control
    /// (#64's rule).
    #[test]
    fn an_instance_that_does_not_export_offers_no_download() {
        let html = render(&Preview {
            zip_url: None,
            stitched_url: None,
            ..a_preview()
        });

        assert!(!html.contains("class=\"get\""), "{html}");
        assert!(!html.contains("/e/export"), "{html}");
    }

    /// **The one surface in the project that puts a Recorder's string into
    /// markup**, so every one of them is escaped — the Event's own name
    /// included, which is an Operator's.
    #[test]
    fn a_name_that_looks_like_markup_renders_as_text() {
        let html = render(&Preview {
            name: String::from("<script>alert(1)</script>"),
            members: vec![Member {
                talkgroup: String::from("<img src=x onerror=1>"),
                ..a_member("x")
            }],
            total: 1,
            ..a_preview()
        });

        assert!(!html.contains("<script>alert"), "{html}");
        assert!(!html.contains("<img src=x"), "{html}");
        assert!(
            html.contains("&lt;script&gt;alert(1)&lt;/script&gt;"),
            "{html}"
        );
        assert!(html.contains("&lt;img src=x onerror=1&gt;"), "{html}");
    }

    /// With an address, the card's two absolute tags point at this Instance.
    #[test]
    fn a_public_url_completes_the_card() {
        let html = render(&Preview {
            canonical: Some(String::from("https://scan.example/e?t=tok")),
            icon: Some(String::from("https://scan.example/icon-512.png")),
            ..a_preview()
        });

        assert!(
            html.contains(r#"<meta property="og:url" content="https://scan.example/e?t=tok">"#),
            "{html}"
        );
        assert!(
            html.contains(
                r#"<meta property="og:image" content="https://scan.example/icon-512.png">"#
            ),
            "{html}"
        );
    }

    /// A member's facts come off the frozen Call, and a Talkgroup nobody
    /// labelled still reads as something.
    #[test]
    fn a_member_reads_off_the_frozen_call() {
        let call = StoredCall {
            system_ref: 11,
            talkgroup_ref: 54241,
            talkgroup_tag: Some(String::from("Dispatch")),
            duration_ms: Some(12_000),
            unit_ref: Some(1234),
            ..StoredCall::default()
        };

        let member = Member::of(&call, Some(String::from("/e/audio?t=tok&i=3")));

        assert_eq!(member.talkgroup, "Talkgroup 54241");
        assert_eq!(member.facts, "System 11 · Dispatch · 0:12 · Unit 1234");
    }
}
