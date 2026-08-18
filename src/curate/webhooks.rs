//! **Webhooks**, curated from the browser (#54, spec US 45).
//!
//! One row per webhook: where it posts, in which shape, which **marks** it asked
//! for, which Calls can reach it, and whether to post at all. The listing
//! carries its **health** beside it — queue depth, last success, consecutive
//! failures, last reason — the [`super::downstreams`] arrangement and for its
//! reason: #70's status page does not exist yet, and an Operator whose channel
//! has gone quiet has to be able to find out why somewhere.
//!
//! # The URL
//!
//! A Downstream's URL is public and only its key is a secret. **A Webhook's URL
//! *is* the secret** — a Discord one ends in a token, and anyone holding it can
//! post into that channel forever. So it is treated as a Downstream's key is,
//! one notch stricter, and both rules are structural rather than remembered:
//!
//! - [`WebhookRow`] **has no field for it**, so no future edit to the listing
//!   can start returning one. What it carries instead is [`crate::webhook::host_of`]'s
//!   answer — `discord.com` — because a screen of rows an Operator cannot tell
//!   apart is a screen they will misconfigure.
//! - A `PATCH` that does not mention `url` leaves the stored one alone, which is
//!   what makes editing a webhook's scope possible without re-pasting a
//!   credential the screen can never show again.

use axum::extract::{Path, State};
use sea_orm::{ActiveModelTrait, EntityTrait, IntoActiveModel, QueryOrder, Set};
use serde::{Deserialize, Serialize};

use super::{Created, Listing, Rejected, Removed, What, nullable, optional_text, required_name};
use crate::AppState;
use crate::db::entities::webhook;
use crate::db::repo;
use crate::failure::{Failure, Stage};
use crate::selection::Selection;
use crate::webhook::{Format, Marks};

/// One Webhook, as the curation screen lists it — **never the URL**.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WebhookRow {
    pub id: i64,
    pub label: Option<String>,
    /// The URL's **host**, which is all of it that may ever be shown. `None` for
    /// a stored value that is not an absolute URL at all — which is a webhook
    /// that will never deliver, and saying so beats rendering a blank.
    pub host: Option<String>,
    /// Which shape the body takes, as [`Format`]'s slug.
    pub format: &'static str,
    /// Which marks this webhook asked for. An empty list fires for nothing,
    /// which the screen shows rather than hides.
    pub marks: Marks,
    /// Which Calls can reach it, as the live feed's own **Selection** JSON.
    pub scope: Selection,
    pub disabled: bool,
    /// **The durable queue depth** — Calls written down and not yet taken. The
    /// number that survives a restart, as distinct from the sender Worker's own
    /// depth, which counts attempts in flight.
    pub queued: i64,
    pub last_success_ms: Option<i64>,
    pub last_failure_ms: Option<i64>,
    /// The last failure as a slug plus its status — `sink-refused (404)`.
    pub last_failure: Option<String>,
    pub consecutive_failures: i32,
    pub created_at_ms: i64,
}

crate::answers_json!(WebhookRow);

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NewWebhook {
    pub label: Option<String>,
    pub url: String,
    /// Absent means [`Format::RadioScout`] — our own shape, which carries every
    /// fact. A form that forgot to send one should get the general answer
    /// rather than a third party's schema.
    pub format: Option<String>,
    #[serde(default)]
    pub marks: Vec<String>,
    #[serde(default)]
    pub scope: Selection,
    #[serde(default)]
    pub disabled: bool,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WebhookPatch {
    #[serde(default, deserialize_with = "nullable")]
    pub label: Option<Option<String>>,
    /// Absent leaves the stored URL alone — which is what lets an Operator edit
    /// a webhook's scope without re-pasting a credential the screen can never
    /// show them again.
    pub url: Option<String>,
    pub format: Option<String>,
    pub marks: Option<Vec<String>>,
    pub scope: Option<Selection>,
    pub disabled: Option<bool>,
}

/// `GET /api/admin/webhooks` — every webhook, with its health.
pub async fn list(State(state): State<AppState>) -> Result<Listing<WebhookRow>, Failure> {
    let rows = webhook::Entity::find()
        .order_by_asc(webhook::Column::Id)
        .all(&state.db)
        .await
        .map_err(Stage::Curate.failed())?;
    // One `GROUP BY` for the whole roster rather than a `COUNT` per row (#86's
    // argument, which applies to a listing whatever its length).
    let depths = repo::webhook_depths(&state.db)
        .await
        .map_err(Stage::Curate.failed())?;

    Ok(Listing::new(
        rows.into_iter()
            .map(|row| {
                let queued = depths.get(&row.id).copied().unwrap_or_default();
                row_of(row, queued)
            })
            .collect(),
    ))
}

/// `POST /api/admin/webhooks` — add one.
pub async fn create(
    State(state): State<AppState>,
    axum::Json(body): axum::Json<NewWebhook>,
) -> Result<Created<WebhookRow>, Failure> {
    let url = checked_url(&body.url)?;
    let format = checked_format(body.format.as_deref())?;
    let marks = checked_marks(&body.marks)?;

    let row = webhook::ActiveModel {
        label: Set(optional_text(body.label)),
        url: Set(url),
        format: Set(format.slug().to_owned()),
        marks: Set(crate::webhook::marks_json(&marks)),
        scope: Set(super::downstreams::scope_json(&body.scope)),
        disabled: Set(body.disabled),
        consecutive_failures: Set(0),
        created_at_ms: Set(state.clock.now_ms()),
        ..Default::default()
    }
    .insert(&state.db)
    .await
    .map_err(Stage::Curate.failed())?;

    Ok(Created(row_of(row, 0)))
}

/// `PATCH /api/admin/webhooks/{id}` — relabel, re-point, reshape, re-scope,
/// change which marks, or switch off.
///
/// **Switching one off empties its queue.** A webhook disabled for a week and
/// switched back on would otherwise post a week of Emergencies into somebody's
/// chat room at once — which is not what "disabled" reads as, and is a worse
/// outcome here than the same mistake on a Downstream, where the peer's own
/// dedup absorbs it.
pub async fn update(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    axum::Json(body): axum::Json<WebhookPatch>,
) -> Result<WebhookRow, Failure> {
    let existing = webhook::Entity::find_by_id(id)
        .one(&state.db)
        .await
        .map_err(Stage::Curate.failed())?
        .ok_or(Rejected::NotFound(What::Webhook))?;

    let mut row = existing.into_active_model();
    if let Some(label) = body.label {
        row.label = Set(super::cleared(label));
    }
    if let Some(url) = &body.url {
        row.url = Set(checked_url(url)?);
    }
    if let Some(format) = &body.format {
        row.format = Set(checked_format(Some(format))?.slug().to_owned());
    }
    if let Some(marks) = &body.marks {
        row.marks = Set(crate::webhook::marks_json(&checked_marks(marks)?));
    }
    if let Some(scope) = &body.scope {
        row.scope = Set(super::downstreams::scope_json(scope));
    }
    let switching_off = body.disabled.unwrap_or(false);
    if let Some(disabled) = body.disabled {
        row.disabled = Set(disabled);
    }

    let row = row
        .update(&state.db)
        .await
        .map_err(Stage::Curate.failed())?;
    if switching_off {
        repo::clear_webhook_deliveries(&state.db, id)
            .await
            .map_err(Stage::Curate.failed())?;
    }

    let queued = match switching_off {
        true => 0,
        false => repo::webhook_depths(&state.db)
            .await
            .map_err(Stage::Curate.failed())?
            .get(&id)
            .copied()
            .unwrap_or_default(),
    };
    Ok(row_of(row, queued))
}

/// `DELETE /api/admin/webhooks/{id}` — forget one, and everything owed it.
///
/// No [`Rejected::HasCalls`] guard, for [`super::downstreams::remove`]'s reason:
/// what goes here is a queue of *pending posts*, not Calls. The **Archive** is
/// untouched.
pub async fn remove(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Removed, Failure> {
    webhook::Entity::find_by_id(id)
        .one(&state.db)
        .await
        .map_err(Stage::Curate.failed())?
        .ok_or(Rejected::NotFound(What::Webhook))?;

    // The queue first: a delivery row outliving its webhook would be read on
    // every wake-up by a `next_webhook_delivery` nobody asks for.
    repo::clear_webhook_deliveries(&state.db, id)
        .await
        .map_err(Stage::Curate.failed())?;
    webhook::Entity::delete_by_id(id)
        .exec(&state.db)
        .await
        .map_err(Stage::Curate.failed())?;

    Ok(Removed)
}

/// A URL that could actually be posted to, or the refusal naming it.
///
/// **Absolute, and `http`/`https` only.** A relative URL cannot be POSTed at
/// all, and a `file://` or `gopher://` one is either a typo or an attempt to
/// make this Instance fetch something local — neither of which should be stored
/// and discovered later as a delivery that fails forever. This is the one
/// validation an Operator gets *before* the credential disappears from the
/// screen, which is why it is worth being strict about.
fn checked_url(raw: &str) -> Result<String, Rejected> {
    let url = required_name(raw, "url")?;
    // The same predicate `[server] public_url` is held to
    // ([`crate::webhook::is_postable_url`]) — two surfaces asking the same
    // question, so they cannot answer it differently.
    match crate::webhook::is_postable_url(&url) {
        true => Ok(url),
        // **The URL is not echoed back.** Every other refusal in this module
        // quotes what it was given; this one cannot, because a refusal is
        // rendered into a form and logged as a 400, and the thing being refused
        // may be a live credential with a typo in the scheme.
        false => Err(Rejected::UnusableWebhookUrl),
    }
}

/// A body shape this Instance can render, or the refusal naming the choices.
fn checked_format(raw: Option<&str>) -> Result<Format, Rejected> {
    let Some(format) = optional_text(raw.map(str::to_owned)) else {
        return Ok(Format::default());
    };
    Format::from_slug(&format).ok_or(Rejected::UnknownFormat { format })
}

/// A mark set this Instance knows, or the refusal naming the vocabulary.
///
/// **Refused rather than dropped**, which is the opposite of what
/// [`crate::webhook::marks_of`] does to a *stored* one — and deliberately: an
/// unknown mark in the database is a row written by a newer release and must
/// cost only itself, where an unknown mark in a form is a typo an Operator can
/// fix right now, and silently discarding it would leave them with a webhook
/// that never fires and no idea why (the [`Rejected::UnknownLed`] precedent).
fn checked_marks(raw: &[String]) -> Result<Marks, Rejected> {
    raw.iter()
        .map(|mark| {
            crate::webhook::Mark::from_slug(mark.trim()).ok_or(Rejected::UnknownMark {
                mark: mark.trim().to_owned(),
            })
        })
        .collect()
}

/// The stored row as the screen reads it — **the URL is dropped here**, which is
/// the one line in this module that matters most.
fn row_of(row: webhook::Model, queued: i64) -> WebhookRow {
    WebhookRow {
        id: row.id,
        label: row.label,
        host: crate::webhook::host_of(&row.url),
        // The same readings the sender applies, so the screen cannot show a
        // policy delivery is not using.
        format: crate::webhook::format_of(&row.format).slug(),
        marks: crate::webhook::marks_of(&row.marks),
        scope: crate::downstream::scope_of(&row.scope),
        disabled: row.disabled,
        queued,
        last_success_ms: row.last_success_ms,
        last_failure_ms: row.last_failure_ms,
        last_failure: row.last_failure,
        consecutive_failures: row.consecutive_failures,
        created_at_ms: row.created_at_ms,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    fn model() -> webhook::Model {
        webhook::Model {
            id: 7,
            label: Some(String::from("dispatch channel")),
            url: String::from("https://discord.com/api/webhooks/1/s3cret-t0ken"),
            format: String::from("discord"),
            marks: serde_json::json!(["emergency"]).to_string(),
            scope: serde_json::json!({ "sel": { "11": { "*": true } } }).to_string(),
            disabled: false,
            last_success_ms: Some(1_700_000_000_000),
            last_failure_ms: None,
            last_failure: None,
            consecutive_failures: 0,
            created_at_ms: 1_600_000_000_000,
        }
    }

    /// **The credential never leaves, under any name.** Asserted over the
    /// serialized document rather than by reading the struct, because what leaks
    /// a secret is a *field somebody adds* — and this fails when one is.
    #[test]
    fn a_listed_webhook_carries_no_url() {
        let json = serde_json::to_string(&row_of(model(), 3)).expect("json");

        assert!(!json.contains("s3cret-t0ken"), "{json}");
        assert!(!json.contains("api/webhooks"), "{json}");
        assert!(!json.contains(r#""url""#), "{json}");
        // ...and the host does, because a screen of rows an Operator cannot tell
        // apart is a screen they will misconfigure.
        assert!(json.contains(r#""host":"discord.com""#), "{json}");
        assert!(json.contains(r#""queued":3"#), "{json}");
    }

    /// The policy on the screen is the policy the sender applies — read through
    /// the same three functions, so a row cannot render a scope, a format or a
    /// mark set that delivery is not using.
    #[test]
    fn the_screen_shows_the_policy_delivery_is_applying() {
        let row = row_of(model(), 0);

        assert_eq!(row.format, "discord");
        assert_eq!(row.marks.slugs(), vec!["emergency"]);
        assert!(row.scope.selects(11, 54241), "the System's wildcard");
        assert!(!row.scope.selects(22, 54241), "another System");
    }

    /// A stored configuration that will not parse reads as **nothing selected**
    /// on the screen too, so an Operator sees the same empty policy routing is
    /// really applying rather than a shape the listing invented.
    #[test]
    fn an_unreadable_stored_configuration_reads_as_empty_on_the_screen_too() {
        let row = row_of(
            webhook::Model {
                scope: String::from("<not json>"),
                marks: String::from("<not json>"),
                format: String::from("smoke signals"),
                ..model()
            },
            0,
        );

        assert!(row.scope.is_all_off());
        assert!(row.marks.slugs().is_empty());
        // ...except the format, which has no safe empty value — see
        // `crate::webhook::format_of`.
        assert_eq!(row.format, "radio-scout");
    }

    /// A URL that could never be posted to is refused **while the Operator can
    /// still see what they typed**, which is the last moment it will ever be on
    /// their screen.
    #[rstest]
    #[case::https("https://discord.com/api/webhooks/1/t", true)]
    #[case::http("http://hooks.internal:9000/x", true)]
    #[case::padded("  https://hooks.internal/x  ", true)]
    #[case::blank("", false)]
    #[case::relative("/api/webhooks/1/t", false)]
    #[case::no_scheme("discord.com/api/webhooks", false)]
    #[case::wrong_scheme("file:///etc/passwd", false)]
    #[case::no_host("https:///nohost", false)]
    fn a_url_that_could_never_be_posted_to_is_refused(#[case] raw: &str, #[case] accepted: bool) {
        assert_eq!(checked_url(raw).is_ok(), accepted, "{raw:?}");
    }

    /// **A refused URL is never echoed back**, unlike every other refusal here:
    /// it is rendered into a form and logged as a 400, and what is being refused
    /// may be a live credential with a typo in front of it.
    #[test]
    fn a_refused_url_is_not_repeated_in_the_refusal() {
        let refused = checked_url("ftp://discord.com/api/webhooks/1/t0ken")
            .expect_err("refused")
            .to_string();

        assert!(!refused.contains("t0ken"), "{refused}");
        assert!(refused.contains("https://"), "{refused}");
    }

    /// A mark a form sends that this release does not know is a **typo an
    /// Operator can fix**, so it is refused by name — where the same slug in the
    /// *database* is dropped, because there it is a newer release's row.
    #[rstest]
    #[case(vec![], Ok(vec![]))]
    #[case(vec!["emergency"], Ok(vec!["emergency"]))]
    #[case(vec![" emergency "], Ok(vec!["emergency"]))]
    #[case(vec!["emergency", "emergency"], Ok(vec!["emergency"]))]
    #[case(vec!["tone"], Err("tone"))]
    #[case(vec!["emergency", "EMERGENCY"], Err("EMERGENCY"))]
    fn a_mark_this_release_does_not_know_is_refused_by_name(
        #[case] raw: Vec<&str>,
        #[case] expected: Result<Vec<&str>, &str>,
    ) {
        let checked = checked_marks(&raw.iter().map(|m| (*m).to_owned()).collect::<Vec<_>>());

        match expected {
            Ok(slugs) => assert_eq!(checked.expect("accepted").slugs(), slugs),
            Err(mark) => assert_eq!(
                checked.expect_err("refused"),
                Rejected::UnknownMark {
                    mark: mark.to_owned()
                }
            ),
        }
    }

    /// An absent format is our own shape, and an unknown one is refused naming
    /// the choices — the [`Rejected::UnknownLed`] shape, for its reason.
    #[rstest]
    #[case(None, Ok(Format::RadioScout))]
    #[case(Some(""), Ok(Format::RadioScout))]
    #[case(Some("discord"), Ok(Format::Discord))]
    #[case(Some("radio-scout"), Ok(Format::RadioScout))]
    #[case(Some("slack"), Err("slack"))]
    fn a_format_is_checked_against_the_shapes_that_exist(
        #[case] raw: Option<&str>,
        #[case] expected: Result<Format, &str>,
    ) {
        match expected {
            Ok(format) => assert_eq!(checked_format(raw).expect("accepted"), format),
            Err(bad) => assert_eq!(
                checked_format(raw).expect_err("refused"),
                Rejected::UnknownFormat {
                    format: bad.to_owned()
                }
            ),
        }
    }

    /// The unknown-format sentence lists what *is* allowed, because "slack is
    /// not a format" without the alternatives sends an Operator to the source.
    #[test]
    fn an_unknown_format_names_the_shapes_that_exist() {
        let told = Rejected::UnknownFormat {
            format: String::from("slack"),
        }
        .to_string();

        for format in crate::webhook::FORMATS {
            assert!(told.contains(format.slug()), "{told}");
        }
    }

    /// ...and the unknown-mark sentence lists the marks, for the same reason —
    /// which is also what will tell an Operator that `tone` has arrived once #55
    /// lands.
    #[test]
    fn an_unknown_mark_names_the_marks_that_exist() {
        let told = Rejected::UnknownMark {
            mark: String::from("tone"),
        }
        .to_string();

        for mark in crate::webhook::MARKS {
            assert!(told.contains(mark.slug()), "{told}");
        }
    }

    /// A stored URL that is not one at all renders as no host rather than as a
    /// blank that looks like a host nobody bothered to name.
    #[test]
    fn a_webhook_whose_url_is_not_a_url_says_so() {
        let row = row_of(
            webhook::Model {
                url: String::new(),
                ..model()
            },
            0,
        );

        assert_eq!(row.host, None);
    }
}
