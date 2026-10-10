//! The **Reference page** (#77, spec US 58): an Operator's own markdown, shown
//! to every Listener — the county's codes, not a wrong national list.
//!
//! # Rendered here, once
//!
//! The markdown is turned into HTML on the server, when it is saved and when
//! the Instance boots, and the HTML is held in memory beside the rest of the
//! **Instance identity**. A Listener's phone downloads a few kilobytes of HTML
//! rather than a markdown library, and a Pi renders a page once rather than
//! once per reader. Only the markdown is stored: a renderer fixed in a later
//! release applies to every page from that boot on, where stored HTML would
//! keep whatever the old one let through until somebody re-saved it.
//!
//! # Markdown and nothing more
//!
//! The Operator is trusted with the words and not with the page. Raw HTML in
//! the markdown — a block or an inline tag — is **shown as text**, and a link
//! or an image whose address is not `http`, `https`, `mailto`, `tel` or
//! relative keeps its words and loses its address. So a pasted `<script>`, a
//! `javascript:` link or a `data:` image cannot run in a Listener's browser,
//! whoever got hold of the admin password. That is the whole of the
//! sanitising, and it is deliberately an allow-list over the parser's events
//! rather than a scrub of its output: there is no HTML to scrub, because none
//! is ever let through to be written.

use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};

/// What a fresh page starts from in the editor: only what is genuinely the
/// same everywhere, and a table for the Operator to fill with their own codes.
///
/// **Offered, never published.** An Instance with no page shows no entry for
/// one, so nothing here reaches a Listener until an Operator has read it and
/// pressed publish — the opposite of shipping a list that is wrong for most of
/// the counties it would reach.
pub const SEED: &str = include_str!("seed.md");

/// The longest page an Operator may publish, in characters of markdown.
///
/// Sixty-four thousand is a long codes page several times over, and it bounds
/// what every Listener downloads and what the Instance holds in memory.
pub const MAX_CHARS: usize = 64 * 1024;

/// Markdown in, HTML out — with raw HTML shown as text and every address
/// checked (see the module note).
pub fn render(markdown: &str) -> String {
    let options = Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH;
    // Whether each open link or image was let through, so its end tag goes the
    // same way its start did. A stack because an image may sit inside a link.
    let mut kept: Vec<bool> = Vec::new();
    let events = Parser::new_ext(markdown, options).filter_map(|event| match event {
        // A block of raw HTML becomes a paragraph of its text, so it still
        // reads as the separate block it was written as.
        Event::Start(Tag::HtmlBlock) => Some(Event::Start(Tag::Paragraph)),
        Event::End(TagEnd::HtmlBlock) => Some(Event::End(TagEnd::Paragraph)),
        Event::Html(raw) | Event::InlineHtml(raw) => Some(Event::Text(raw)),
        Event::Start(Tag::Link { ref dest_url, .. } | Tag::Image { ref dest_url, .. }) => {
            let keep = safe(dest_url);
            kept.push(keep);
            keep.then_some(event)
        }
        Event::End(TagEnd::Link | TagEnd::Image) => kept.pop().unwrap_or(true).then_some(event),
        other => Some(other),
    });
    let mut html = String::with_capacity(markdown.len() + markdown.len() / 2);
    pulldown_cmark::html::push_html(&mut html, events);
    html
}

/// Whether an address may be followed from a Listener's browser.
///
/// The scheme is whatever precedes the first `:`, if a `:` comes before any
/// `/`, `?` or `#` — the way a browser reads one. No scheme at all is a
/// relative address and stays on this Instance. Anything else must be one of
/// four, **compared exactly**: a browser strips tabs and newlines from inside a
/// scheme before reading it, so `java\tscript:` is `javascript:` to a browser
/// and nothing at all to this check — which refuses it, because it is not
/// spelled like any of the four.
fn safe(address: &str) -> bool {
    let address = address.trim();
    let end = address.find([':', '/', '?', '#']);
    match end.map(|at| (at, &address[at..])) {
        Some((at, rest)) if rest.starts_with(':') => {
            let scheme = address[..at].to_ascii_lowercase();
            matches!(scheme.as_str(), "http" | "https" | "mailto" | "tel")
        }
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    /// It is markdown: headings, emphasis, lists and the tables a codes page is
    /// made of.
    #[test]
    fn markdown_renders() {
        let html = render("# Codes\n\n| Code | Meaning |\n| --- | --- |\n| 10-4 | OK |\n");

        assert!(html.contains("<h1>Codes</h1>"), "{html}");
        assert!(html.contains("<table>"), "{html}");
        assert!(html.contains("<td>10-4</td>"), "{html}");
    }

    /// Raw HTML is shown, never run — a block and an inline tag alike.
    #[rstest]
    #[case::block("<script>alert(1)</script>\n", "&lt;script&gt;alert(1)&lt;/script&gt;")]
    #[case::inline(
        "a <img src=x onerror=alert(1)> b",
        "&lt;img src=x onerror=alert(1)&gt;"
    )]
    #[case::comment("<!-- hidden -->\n", "&lt;!-- hidden --&gt;")]
    fn raw_html_is_text(#[case] markdown: &str, #[case] shown: &str) {
        let html = render(markdown);

        assert!(html.contains(shown), "{html}");
        assert!(!html.contains("<script"), "{html}");
        assert!(!html.contains("<img"), "{html}");
    }

    /// A block of raw HTML still reads as a block of its own.
    #[test]
    fn a_raw_html_block_is_a_paragraph_of_text() {
        assert_eq!(
            render("<div>hi</div>\n"),
            "<p>&lt;div&gt;hi&lt;/div&gt;\n</p>\n"
        );
    }

    /// An address a browser would run, or load from nowhere, keeps its words
    /// and loses the address.
    #[rstest]
    #[case::javascript("[call](javascript:alert(1))", "<p>call</p>\n")]
    #[case::shouted("[call](JavaScript:alert(1))", "<p>call</p>\n")]
    #[case::data("[call](data:text/html,hi)", "<p>call</p>\n")]
    #[case::vbscript("[call](vbscript:msgbox)", "<p>call</p>\n")]
    #[case::tab_inside("[call](java&#9;script:alert(1))", "<p>call</p>\n")]
    #[case::autolink("<javascript:alert(1)>", "<p>javascript:alert(1)</p>\n")]
    #[case::image("![map](data:image/png;base64,AAAA)", "<p>map</p>\n")]
    fn an_unsafe_address_is_dropped(#[case] markdown: &str, #[case] rendered: &str) {
        assert_eq!(render(markdown), rendered);
    }

    /// The addresses a reference page actually needs are followed.
    #[rstest]
    #[case::https(
        "[RR](https://www.radioreference.com/)",
        r#"href="https://www.radioreference.com/""#
    )]
    #[case::http("[old](http://example.com)", r#"href="http://example.com""#)]
    #[case::mailto("[us](mailto:ops@example.com)", r#"href="mailto:ops@example.com""#)]
    #[case::tel("[call](tel:+15551234)", r#"href="tel:+15551234""#)]
    #[case::relative("[search](/search?tg=1)", r#"href="/search?tg=1""#)]
    #[case::fragment("[up](#codes)", r##"href="#codes""##)]
    #[case::protocol_relative("[x](//example.com/a)", r#"href="//example.com/a""#)]
    #[case::colon_after_a_slash("[x](/a:b)", r#"href="/a:b""#)]
    #[case::image(
        "![map](https://example.com/map.png)",
        r#"src="https://example.com/map.png""#
    )]
    fn a_safe_address_is_kept(#[case] markdown: &str, #[case] attribute: &str) {
        let html = render(markdown);

        assert!(html.contains(attribute), "{html}");
    }

    /// An image inside a link: dropping the image keeps the link, and dropping
    /// the link keeps the image — the stack, not a flag.
    #[rstest]
    #[case::bad_image_good_link(
        "[![x](javascript:a)](https://example.com)",
        "<p><a href=\"https://example.com\">x</a></p>\n"
    )]
    #[case::good_image_bad_link(
        "[![x](https://example.com/i.png)](javascript:a)",
        "<p><img src=\"https://example.com/i.png\" alt=\"x\" /></p>\n"
    )]
    fn nested_addresses_are_judged_one_at_a_time(#[case] markdown: &str, #[case] rendered: &str) {
        assert_eq!(render(markdown), rendered);
    }

    /// The seed renders, and its phonetic table is whole — twenty-six rows.
    #[test]
    fn the_seed_renders_whole() {
        let html = render(SEED);

        assert!(html.contains("<h1>Reference</h1>"), "{html}");
        assert_eq!(html.matches("<td>Zulu</td>").count(), 1);
        assert_eq!(html.matches("<tr>").count(), 1 + 1 + 1 + 26, "{html}");
        assert!(SEED.chars().count() < MAX_CHARS);
    }
}
