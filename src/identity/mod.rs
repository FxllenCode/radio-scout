//! The **Instance identity** (#77, spec US 4, 57, 58) — what an Operator
//! curates about the Instance itself rather than any entity in it: its name,
//! where a Listener goes for help, the **Reference page**, and the defaults
//! every Listener's app starts from.
//!
//! # Curation, and only curation
//!
//! The ticket asked for "admin and TOML", and the maintainer chose one home
//! (2026-10-10): this is edited in a browser, stored in the database, and never
//! in `radio-scout.toml`. Two homes for one value is a precedence rule an
//! Operator has to know, and the TOML is the *machine* (ADR-0012) — a name and a
//! help link are what Listeners see. `[live] max_listeners` is the exception
//! that proves it: a cap protects the machine, so it is configuration.
//!
//! # Improving on rdio-scanner
//!
//! rdio's equivalents are fields of its one options document (`options.go`),
//! pushed down the live socket in its `CFG` frame, so an Instance's name is
//! known only once a socket is up, and a dropped socket keeps showing whatever
//! it last said.
//!
//! | rdio | Radio-Scout |
//! |---|---|
//! | `branding` shown only on the main screen, the tab always "Rdio Scanner" | the name in the header of every screen, the tab, the installed app's label, the lock screen and every page a Listener shares |
//! | `email` behind a help button whose sheet just prints it | `helpUrl` — any `https://` page *or* an address, opened directly |
//! | `sortTalkgroups`: alphabetical, or the admin's order, for everyone | the order a **Profile** *starts* with; a Listener's own choice still wins (#57) |
//! | `time12hFormat` on the main display and search only | every time the app shows |
//! | `showListenersCount` pushed on every connect and disconnect, to every socket | the count rides the heartbeat a socket already has, and only while it is shown |
//! | `dimmerDelay` | the same, off unless asked for |
//! | no reference page | the **Reference page**, seeded but never published unasked |
//!
//! # Held in memory, re-read on the request that changes it
//!
//! Every app open, every socket and every shared page asks for some of this,
//! so it is read from the database once — at boot ([`Identity::load`]) — and
//! held. The admin form's write replaces what is held on the same request, the
//! `Access::is_gating` rule: a name changed in admin is the name the very next
//! page shows, not the one after a restart.

pub mod reference;

use std::sync::{Arc, RwLock};

use axum::extract::State;
use sea_orm::sea_query::OnConflict;
use sea_orm::{ConnectionTrait, DbErr, EntityTrait, Set};
use serde::{Deserialize, Serialize};
use tracing::error;

use crate::AppState;
use crate::db::entities::identity;
use crate::failure::{Failure, Reason};

/// What the product is called when the Operator has not named their Instance.
pub const PRODUCT_NAME: &str = "Radio-Scout";

/// How every time in the app is written — the Operator's call, for everyone
/// (2026-10-10). Twenty-four-hour by default, which is how the app has always
/// looked and how dispatch reads a clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum TimeFormat {
    #[default]
    #[serde(rename = "24h")]
    TwentyFour,
    #[serde(rename = "12h")]
    Twelve,
}

/// The order of the Talkgroups panel for a **Profile** that has not chosen
/// one — the Operator's default, never their override (#57's sort control
/// stays the Listener's).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PanelSort {
    /// By what a Listener reads — label, then Ref. What the panel has always
    /// done.
    #[default]
    Name,
    /// Busiest first.
    Active,
    /// By **Ref** — the TGID order a RadioReference listing is in, and what
    /// rdio's admin order usually was.
    Ref,
}

/// The column spelling of an enum is its wire spelling, so the database and
/// the document can never disagree about what `"12h"` means.
fn column<T: Serialize>(value: T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|spelled| spelled.as_str().map(str::to_owned))
        .unwrap_or_default()
}

/// ...and back. A spelling this release does not know — a newer release's,
/// read after a downgrade — is the default rather than a refused boot: these
/// are how the app looks, and nothing is worth less than a scanner that will
/// not start over one.
fn from_column<T: for<'de> Deserialize<'de> + Default>(spelled: &str) -> T {
    serde_json::from_value(serde_json::Value::from(spelled)).unwrap_or_default()
}

/// What an Operator decided about how the app looks and behaves for everyone.
///
/// One type for the database's row, the public document and the admin form's
/// answer, so "what the form saved" and "what every Listener is sent" cannot
/// come to describe two different Instances.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    /// The Instance's name. Absent is unnamed, and the product's name is shown.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// An `https://`, `http://` or `mailto:` address — never anything a browser
    /// would run.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub help_url: Option<String>,
    pub time_format: TimeFormat,
    pub panel_sort: PanelSort,
    /// When a **Run** walking forward in time reaches the newest Call there is,
    /// the live feed takes over rather than the audio stopping.
    pub playback_goes_live: bool,
    /// Seconds of quiet before the Live display dims. `0` never dims, which is
    /// what ships.
    pub dimmer_secs: u32,
    /// Whether Listeners are told how many people are listening. **Off** by
    /// default: the **Listener count** is the Operator's (CONTEXT.md), so
    /// publishing it is theirs to choose.
    pub show_listeners: bool,
}

/// A published **Reference page**.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Page {
    /// What the Operator wrote — what the editor opens on.
    pub markdown: String,
    /// What Listeners are shown: [`reference::render`]'s answer, made when the
    /// page was published or this Instance booted.
    pub html: String,
    pub published_at_ms: i64,
}

impl Page {
    /// A page as published at `published_at_ms` — rendered now, once.
    pub fn of(markdown: String, published_at_ms: i64) -> Self {
        Page {
            html: reference::render(&markdown),
            markdown,
            published_at_ms,
        }
    }
}

/// Everything held: the settings, and the page if there is one.
#[derive(Debug, Clone, Default)]
pub struct Held {
    pub settings: Settings,
    pub page: Option<Arc<Page>>,
}

impl Held {
    /// The row as the code reads it.
    fn of(row: identity::Model) -> Self {
        let page = row
            .reference
            .map(|markdown| Arc::new(Page::of(markdown, row.reference_at_ms.unwrap_or_default())));
        Held {
            settings: Settings {
                name: row.name,
                help_url: row.help_url,
                time_format: from_column(&row.time_format),
                panel_sort: from_column(&row.panel_sort),
                playback_goes_live: row.playback_goes_live,
                dimmer_secs: u32::try_from(row.dimmer_secs).unwrap_or_default(),
                show_listeners: row.show_listeners,
            },
            page,
        }
    }

    /// The row as the database stores it — the whole of it, every time.
    fn row(&self) -> identity::ActiveModel {
        let settings = &self.settings;
        identity::ActiveModel {
            id: Set(identity::ROW),
            name: Set(settings.name.clone()),
            help_url: Set(settings.help_url.clone()),
            time_format: Set(column(settings.time_format)),
            panel_sort: Set(column(settings.panel_sort)),
            playback_goes_live: Set(settings.playback_goes_live),
            dimmer_secs: Set(i32::try_from(settings.dimmer_secs).unwrap_or(i32::MAX)),
            show_listeners: Set(settings.show_listeners),
            reference: Set(self.page.as_ref().map(|page| page.markdown.clone())),
            reference_at_ms: Set(self.page.as_ref().map(|page| page.published_at_ms)),
        }
    }
}

/// The **Instance identity** as an [`AppState`] holds it: read once, and
/// replaced by the request that changes it.
#[derive(Clone, Default)]
pub struct Identity {
    held: Arc<RwLock<Held>>,
    /// One write at a time. Every write is the *whole* row, built from what is
    /// held — so two writes interleaved could each store the other's stale half
    /// (a name saved while a page is published would un-publish it). An Operator
    /// has one form, so this is never contended; it exists so the row's
    /// correctness does not depend on that.
    writing: Arc<tokio::sync::Mutex<()>>,
}

impl Identity {
    /// Read what is stored — at boot, before anything is served.
    pub async fn load<C: ConnectionTrait>(&self, db: &C) {
        self.take(identity::Entity::find_by_id(identity::ROW).one(db).await);
    }

    /// Hold what was read: the row, the defaults when there is none — which is
    /// every Instance that has never been given an identity — and the defaults,
    /// said out loud, when it could not be read at all.
    ///
    /// Taken as a value, `Access::arm`'s shape, so the arm that only a broken
    /// database reaches is a case a test hands it. An unnamed Instance is still
    /// a scanner, so a read that fails costs the name rather than the boot, and
    /// the admin form writes the whole row back the moment anybody saves.
    fn take(&self, read: Result<Option<identity::Model>, DbErr>) {
        let held = match read {
            Ok(row) => row.map(Held::of).unwrap_or_default(),
            Err(error) => {
                error!(%error, "could not read the instance identity; serving the defaults");
                Held::default()
            }
        };
        *self
            .held
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = held;
    }

    /// Everything held, as it stands.
    pub fn held(&self) -> Held {
        self.reading(Held::clone)
    }

    /// Read what is held without copying it — for the questions asked on a
    /// heartbeat or per page, which want one field and not the page with it.
    fn reading<T>(&self, read: impl FnOnce(&Held) -> T) -> T {
        read(
            &self
                .held
                .read()
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
        )
    }

    /// What this Instance is called — the Operator's name, or the product's.
    pub fn name(&self) -> String {
        self.reading(|held| held.settings.name.clone())
            .unwrap_or_else(|| PRODUCT_NAME.to_owned())
    }

    /// Whether Listeners are told how many people are listening.
    pub fn shows_listeners(&self) -> bool {
        self.reading(|held| held.settings.show_listeners)
    }

    /// Change what is held, store the result, and hold it — in that order, so
    /// what is held is never something the database refused.
    ///
    /// `change` is handed a copy to edit and may refuse; nothing is stored or
    /// held if it does.
    pub async fn write<C, E>(
        &self,
        db: &C,
        change: impl FnOnce(&mut Held) -> Result<(), E>,
    ) -> Result<Held, Failure>
    where
        C: ConnectionTrait,
        Failure: From<E>,
    {
        let _one_at_a_time = self.writing.lock().await;
        let mut next = self.held();
        change(&mut next)?;
        identity::Entity::insert(next.row())
            .on_conflict(
                OnConflict::column(identity::Column::Id)
                    .update_columns([
                        identity::Column::Name,
                        identity::Column::HelpUrl,
                        identity::Column::TimeFormat,
                        identity::Column::PanelSort,
                        identity::Column::PlaybackGoesLive,
                        identity::Column::DimmerSecs,
                        identity::Column::ShowListeners,
                        identity::Column::Reference,
                        identity::Column::ReferenceAtMs,
                    ])
                    .to_owned(),
            )
            .exec(db)
            .await
            .map_err(crate::failure::Stage::Curate.failed())?;
        *self
            .held
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = next.clone();
        Ok(next)
    }
}

/// `GET /api/instance` — the identity every Listener's app is drawn with.
///
/// Its own document rather than more fields on `GET /api/catalog`: the name
/// paints the header before a county-scale catalog has been read, an Embed
/// needs the name and nothing of the catalog, and this one is the same for
/// every Listener where the catalog is not.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InstanceDocument {
    #[serde(flatten)]
    pub settings: Settings,
    /// Whether there is a **Reference page** — so the app draws an entry for
    /// one only when there is something behind it.
    pub reference: bool,
}

crate::answers_json!(InstanceDocument);

impl From<Held> for InstanceDocument {
    fn from(held: Held) -> Self {
        InstanceDocument {
            reference: held.page.is_some(),
            settings: held.settings,
        }
    }
}

pub async fn document(State(state): State<AppState>) -> InstanceDocument {
    state.identity.held().into()
}

/// `GET /api/reference` — the published **Reference page**, as HTML.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PageDocument {
    pub html: String,
    pub published_at_ms: i64,
}

crate::answers_json!(PageDocument);

/// No page answers exactly like no route ([`Reason::NoReferencePage`]): the app
/// draws no entry for one, so whoever asks has typed the address.
pub async fn page(State(state): State<AppState>) -> Result<PageDocument, Failure> {
    let page = state.identity.held().page.ok_or(Reason::NoReferencePage)?;
    Ok(PageDocument {
        html: page.html.clone(),
        published_at_ms: page.published_at_ms,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    /// The column spelling is the wire spelling, both ways, for every variant.
    #[rstest]
    #[case(TimeFormat::TwentyFour, "24h")]
    #[case(TimeFormat::Twelve, "12h")]
    fn a_time_format_is_spelled_the_same_in_the_row(
        #[case] format: TimeFormat,
        #[case] spelled: &str,
    ) {
        assert_eq!(column(format), spelled);
        assert_eq!(from_column::<TimeFormat>(spelled), format);
    }

    #[rstest]
    #[case(PanelSort::Name, "name")]
    #[case(PanelSort::Active, "active")]
    #[case(PanelSort::Ref, "ref")]
    fn a_panel_sort_is_spelled_the_same_in_the_row(#[case] sort: PanelSort, #[case] spelled: &str) {
        assert_eq!(column(sort), spelled);
        assert_eq!(from_column::<PanelSort>(spelled), sort);
    }

    /// A spelling from a newer release is the default rather than a refused
    /// boot.
    #[test]
    fn an_unknown_spelling_reads_as_the_default() {
        assert_eq!(from_column::<TimeFormat>("36h"), TimeFormat::TwentyFour);
        assert_eq!(from_column::<PanelSort>("by-colour"), PanelSort::Name);
    }

    /// A row read and written back is the row it was — every column, so a field
    /// forgotten on either side fails here.
    #[test]
    fn a_row_survives_the_round_trip() {
        let row = identity::Model {
            id: identity::ROW,
            name: Some("Fulton Scanner".into()),
            help_url: Some("mailto:ops@example.com".into()),
            time_format: "12h".into(),
            panel_sort: "ref".into(),
            playback_goes_live: true,
            dimmer_secs: 30,
            show_listeners: true,
            reference: Some("# Codes".into()),
            reference_at_ms: Some(1_700_000_000_000),
        };

        let held = Held::of(row.clone());
        let back = held.row();

        assert_eq!(back.name.unwrap(), row.name);
        assert_eq!(back.help_url.unwrap(), row.help_url);
        assert_eq!(back.time_format.unwrap(), row.time_format);
        assert_eq!(back.panel_sort.unwrap(), row.panel_sort);
        assert_eq!(back.playback_goes_live.unwrap(), row.playback_goes_live);
        assert_eq!(back.dimmer_secs.unwrap(), row.dimmer_secs);
        assert_eq!(back.show_listeners.unwrap(), row.show_listeners);
        assert_eq!(back.reference.unwrap(), row.reference);
        assert_eq!(back.reference_at_ms.unwrap(), row.reference_at_ms);
        assert_eq!(held.page.expect("a page").html, "<h1>Codes</h1>\n");
    }

    /// A database that cannot be read costs the name, not the boot — and says
    /// so, at ERROR, because an Operator's curated identity silently going back
    /// to the defaults is a thing they would otherwise only notice by eye.
    #[test]
    fn an_unreadable_identity_is_the_defaults_said_out_loud() {
        let capture = crate::testing::LogCapture::start();
        let identity = Identity::default();
        identity.take(Ok(Some(identity::Model {
            id: identity::ROW,
            name: Some("Fulton".into()),
            help_url: None,
            time_format: "24h".into(),
            panel_sort: "name".into(),
            playback_goes_live: false,
            dimmer_secs: 0,
            show_listeners: false,
            reference: None,
            reference_at_ms: None,
        })));
        assert_eq!(identity.name(), "Fulton");

        identity.take(Err(DbErr::Custom("the disk went away".into())));

        assert_eq!(identity.name(), "Radio-Scout");
        let logged = capture.text();
        let said = logged
            .lines()
            .find(|line| line.contains("could not read the instance identity"))
            .unwrap_or_else(|| panic!("never said: {logged}"));
        assert!(said.contains("ERROR"), "{said}");
        assert!(said.contains("the disk went away"), "{said}");
    }

    /// Unnamed is the product's name, so every surface that prints one prints
    /// something.
    #[test]
    fn an_unnamed_instance_is_called_radio_scout() {
        assert_eq!(Identity::default().name(), "Radio-Scout");
    }
}
