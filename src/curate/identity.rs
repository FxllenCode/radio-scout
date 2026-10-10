//! Curating the **Instance identity** (#77) — the one form behind Settings →
//! Admin → Instance, and the **Reference page** editor beside it.
//!
//! Two resources rather than one, because they are edited differently: the
//! settings are a handful of fields patched as an Operator changes them, and
//! the page is a document published whole. Both write through
//! [`crate::identity::Identity::write`], so both land in the one row and in
//! what every Listener is served on the same request.

use std::sync::Arc;

use axum::extract::State;
use serde::{Deserialize, Serialize};

use super::{Rejected, Removed, nullable, optional_text};
use crate::AppState;
use crate::failure::Failure;
use crate::identity::{InstanceDocument, Page, PanelSort, Settings, TimeFormat, reference};

/// The longest name an Instance may have. It is a header on a phone, a tab and
/// an app's label under its icon; sixty-four characters is already more than
/// any of those shows.
pub const MAX_NAME_CHARS: usize = 64;

/// The longest the Live display may wait before dimming: an hour. Past that the
/// screen has been left alone, and a dimmer that never arrives is `0`.
pub const MAX_DIMMER_SECS: u32 = 60 * 60;

/// A change to the settings: an absent field is left alone, and `null` (or a
/// blank string) clears the two that can be cleared.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SettingsPatch {
    #[serde(default, deserialize_with = "nullable")]
    pub name: Option<Option<String>>,
    #[serde(default, deserialize_with = "nullable")]
    pub help_url: Option<Option<String>>,
    pub time_format: Option<TimeFormat>,
    pub panel_sort: Option<PanelSort>,
    pub playback_goes_live: Option<bool>,
    pub dimmer_secs: Option<u32>,
    pub show_listeners: Option<bool>,
}

/// `PATCH /api/admin/instance` — change some of the settings.
pub async fn update(
    State(state): State<AppState>,
    axum::Json(patch): axum::Json<SettingsPatch>,
) -> Result<InstanceDocument, Failure> {
    let held = state
        .identity
        .write(&state.db, |held| apply(&mut held.settings, patch))
        .await?;
    Ok(held.into())
}

/// The patch, applied — or the first field refused, leaving `settings` as it
/// was for the caller to throw away.
fn apply(settings: &mut Settings, patch: SettingsPatch) -> Result<(), Rejected> {
    if let Some(name) = patch.name {
        settings.name = instance_name(name)?;
    }
    if let Some(help_url) = patch.help_url {
        settings.help_url = help_link(help_url)?;
    }
    if let Some(dimmer_secs) = patch.dimmer_secs {
        if dimmer_secs > MAX_DIMMER_SECS {
            return Err(Rejected::OutOfRange {
                field: "dimmerSecs",
                least: 0,
                most: i64::from(MAX_DIMMER_SECS),
            });
        }
        settings.dimmer_secs = dimmer_secs;
    }
    settings.time_format = patch.time_format.unwrap_or(settings.time_format);
    settings.panel_sort = patch.panel_sort.unwrap_or(settings.panel_sort);
    settings.playback_goes_live = patch
        .playback_goes_live
        .unwrap_or(settings.playback_goes_live);
    settings.show_listeners = patch.show_listeners.unwrap_or(settings.show_listeners);
    Ok(())
}

/// A name, trimmed, blank meaning none — or the refusal for one too long to be
/// a header.
fn instance_name(raw: Option<String>) -> Result<Option<String>, Rejected> {
    let name = optional_text(raw);
    if let Some(name) = &name {
        at_most(name, "name", MAX_NAME_CHARS)?;
    }
    Ok(name)
}

/// Text no longer than `most` **characters** — not bytes, because a name or a
/// page in another script is not shorter for being multi-byte.
fn at_most(text: &str, field: &'static str, most: usize) -> Result<(), Rejected> {
    match text.chars().count() > most {
        true => Err(Rejected::TooLong { field, most }),
        false => Ok(()),
    }
}

/// Where a Listener goes for help: a web page, or an address to write to.
///
/// rdio's `email` could only ever be an address; most Operators already have a
/// page. A bare address is read as one and becomes a `mailto:`, because that is
/// what an Operator migrating from rdio will paste. Anything that is not
/// `https:`, `http:` or `mailto:` is refused, since this is put in front of
/// every Listener as a link and a `javascript:` one would run when tapped.
pub fn help_link(raw: Option<String>) -> Result<Option<String>, Rejected> {
    let Some(raw) = optional_text(raw) else {
        return Ok(None);
    };
    let spelled = match raw.contains(':') {
        true => raw,
        false if is_an_address(&raw) => format!("mailto:{raw}"),
        false => return Err(Rejected::UnusableHelpLink),
    };
    let link = url::Url::parse(&spelled).map_err(|_| Rejected::UnusableHelpLink)?;
    let usable = match link.scheme() {
        "https" | "http" => link.host_str().is_some(),
        "mailto" => is_an_address(link.path()),
        _ => false,
    };
    match usable {
        true => Ok(Some(link.to_string())),
        false => Err(Rejected::UnusableHelpLink),
    }
}

/// Something that reads as an email address: one `@`, with something either
/// side of it and no space. Not a validation of the address — the Operator's
/// mail server is the judge of that — only of its shape.
fn is_an_address(text: &str) -> bool {
    match text.split_once('@') {
        Some((who, at)) => {
            !who.is_empty()
                && !at.is_empty()
                && !at.contains('@')
                && !text.contains(char::is_whitespace)
        }
        None => false,
    }
}

/// What the **Reference page** editor opens on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReferenceEditor {
    /// What is published — absent when nothing is, and the editor starts from
    /// [`ReferenceEditor::seed`] instead.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub markdown: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub published_at_ms: Option<i64>,
    /// [`reference::SEED`]: always sent, so an Operator can start over from it.
    pub seed: &'static str,
}

crate::answers_json!(ReferenceEditor);

impl ReferenceEditor {
    fn of(page: Option<&Page>) -> Self {
        ReferenceEditor {
            markdown: page.map(|page| page.markdown.clone()),
            published_at_ms: page.map(|page| page.published_at_ms),
            seed: reference::SEED,
        }
    }
}

/// `GET /api/admin/reference` — what to edit.
pub async fn editor(State(state): State<AppState>) -> ReferenceEditor {
    ReferenceEditor::of(state.identity.held().page.as_deref())
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PageBody {
    pub markdown: String,
}

/// `PUT /api/admin/reference` — publish the page, whole. Listeners see it on
/// their next look; the app grows the entry for it on its next open.
pub async fn publish(
    State(state): State<AppState>,
    axum::Json(body): axum::Json<PageBody>,
) -> Result<ReferenceEditor, Failure> {
    let markdown = publishable(&body.markdown)?;
    let now_ms = state.clock.now_ms();
    let held = state
        .identity
        .write(&state.db, |held| {
            held.page = Some(Arc::new(Page::of(markdown, now_ms)));
            Ok::<(), Rejected>(())
        })
        .await?;
    Ok(ReferenceEditor::of(held.page.as_deref()))
}

/// A page worth publishing: something besides whitespace, and not more than
/// [`reference::MAX_CHARS`] of it. Kept as written rather than trimmed — a
/// trailing table or code block is whitespace-sensitive.
fn publishable(markdown: &str) -> Result<String, Rejected> {
    if markdown.trim().is_empty() {
        return Err(Rejected::Blank { field: "markdown" });
    }
    at_most(markdown, "markdown", reference::MAX_CHARS)?;
    Ok(markdown.to_owned())
}

/// `DELETE /api/admin/reference` — take the page down, and the app's entry for
/// it with it. Taking down a page that is not up is not an error: the state
/// asked for is the state there is.
pub async fn unpublish(State(state): State<AppState>) -> Result<Removed, Failure> {
    state
        .identity
        .write(&state.db, |held| {
            held.page = None;
            Ok::<(), Rejected>(())
        })
        .await?;
    Ok(Removed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    /// A name is trimmed, blank is none, and too long is refused naming the
    /// field.
    #[rstest]
    #[case(Some("  Fulton Scanner "), Ok(Some("Fulton Scanner")))]
    #[case(Some("   "), Ok(None))]
    #[case(None, Ok(None))]
    #[case(Some(&*"x".repeat(MAX_NAME_CHARS)), Ok(Some(&*"x".repeat(MAX_NAME_CHARS))))]
    #[case(Some(&*"x".repeat(MAX_NAME_CHARS + 1)), Err(Rejected::TooLong { field: "name", most: MAX_NAME_CHARS }))]
    // Characters, not bytes: a name in another script is not shorter for it.
    #[case(Some(&*"é".repeat(MAX_NAME_CHARS)), Ok(Some(&*"é".repeat(MAX_NAME_CHARS))))]
    fn an_instance_name(
        #[case] raw: Option<&str>,
        #[case] expected: Result<Option<&str>, Rejected>,
    ) {
        assert_eq!(
            instance_name(raw.map(str::to_owned)),
            expected.map(|name| name.map(str::to_owned))
        );
    }

    /// A help link is a page or an address, and nothing a browser would run.
    #[rstest]
    #[case("https://example.com/help", Some("https://example.com/help"))]
    #[case("http://scanner.lan/", Some("http://scanner.lan/"))]
    #[case("HTTPS://Example.com", Some("https://example.com/"))]
    #[case("mailto:ops@example.com", Some("mailto:ops@example.com"))]
    // What an Operator migrating from rdio's `email` will paste.
    #[case("ops@example.com", Some("mailto:ops@example.com"))]
    #[case("  ops@example.com  ", Some("mailto:ops@example.com"))]
    #[case("", None)]
    fn a_usable_help_link(#[case] raw: &str, #[case] link: Option<&str>) {
        assert_eq!(help_link(Some(raw.to_owned())), Ok(link.map(str::to_owned)));
    }

    #[rstest]
    #[case::script("javascript:alert(1)")]
    #[case::data("data:text/html,hi")]
    #[case::file("file:///etc/passwd")]
    #[case::no_host("https://")]
    #[case::not_an_address("mailto:nobody")]
    #[case::two_ats("a@b@c")]
    #[case::spaced("ops @example.com")]
    #[case::nothing_before("@example.com")]
    #[case::nothing_after("ops@")]
    #[case::a_word("help")]
    #[case::a_bare_host("example.com")]
    fn an_unusable_help_link(#[case] raw: &str) {
        assert_eq!(
            help_link(Some(raw.to_owned())),
            Err(Rejected::UnusableHelpLink)
        );
    }

    /// A patch changes what it names and nothing else.
    #[test]
    fn a_patch_changes_only_what_it_names() {
        let mut settings = Settings {
            name: Some("Fulton".into()),
            help_url: Some("https://example.com/".into()),
            dimmer_secs: 10,
            show_listeners: true,
            ..Settings::default()
        };

        apply(
            &mut settings,
            SettingsPatch {
                time_format: Some(TimeFormat::Twelve),
                ..SettingsPatch::default()
            },
        )
        .expect("a usable patch");

        assert_eq!(
            settings,
            Settings {
                name: Some("Fulton".into()),
                help_url: Some("https://example.com/".into()),
                time_format: TimeFormat::Twelve,
                dimmer_secs: 10,
                show_listeners: true,
                ..Settings::default()
            }
        );
    }

    /// ...and every field it can name, it changes.
    #[test]
    fn a_patch_can_change_every_field() {
        let mut settings = Settings::default();

        apply(
            &mut settings,
            SettingsPatch {
                name: Some(Some("Fulton".into())),
                help_url: Some(Some("ops@example.com".into())),
                time_format: Some(TimeFormat::Twelve),
                panel_sort: Some(PanelSort::Ref),
                playback_goes_live: Some(true),
                dimmer_secs: Some(MAX_DIMMER_SECS),
                show_listeners: Some(true),
            },
        )
        .expect("a usable patch");

        assert_eq!(
            settings,
            Settings {
                name: Some("Fulton".into()),
                help_url: Some("mailto:ops@example.com".into()),
                time_format: TimeFormat::Twelve,
                panel_sort: PanelSort::Ref,
                playback_goes_live: true,
                dimmer_secs: MAX_DIMMER_SECS,
                show_listeners: true,
            }
        );
    }

    /// `null` clears what can be cleared.
    #[test]
    fn null_clears_the_name_and_the_help_link() {
        let mut settings = Settings {
            name: Some("Fulton".into()),
            help_url: Some("https://example.com/".into()),
            ..Settings::default()
        };

        apply(
            &mut settings,
            SettingsPatch {
                name: Some(None),
                help_url: Some(None),
                ..SettingsPatch::default()
            },
        )
        .expect("a usable patch");

        assert_eq!(settings, Settings::default());
    }

    /// A dimmer longer than an hour is refused with its bounds.
    #[test]
    fn a_dimmer_past_an_hour_is_refused() {
        let refused = apply(
            &mut Settings::default(),
            SettingsPatch {
                dimmer_secs: Some(MAX_DIMMER_SECS + 1),
                ..SettingsPatch::default()
            },
        );

        assert_eq!(
            refused,
            Err(Rejected::OutOfRange {
                field: "dimmerSecs",
                least: 0,
                most: 3600
            })
        );
    }

    /// A page is something, and not too much of it.
    #[rstest]
    #[case("# Codes\n", Ok("# Codes\n".to_owned()))]
    #[case("  \n\t", Err(Rejected::Blank { field: "markdown" }))]
    #[case(&"x".repeat(reference::MAX_CHARS), Ok("x".repeat(reference::MAX_CHARS)))]
    #[case(&"x".repeat(reference::MAX_CHARS + 1), Err(Rejected::TooLong { field: "markdown", most: reference::MAX_CHARS }))]
    fn a_publishable_page(#[case] markdown: &str, #[case] expected: Result<String, Rejected>) {
        assert_eq!(publishable(markdown), expected);
    }
}
