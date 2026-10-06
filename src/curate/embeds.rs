//! **Embeds**, curated from the browser (#75, spec US 59).
//!
//! One row per embed: what it is called and which **Selection** it plays. The
//! listing hands out where each lives — a path always, and an absolute URL when
//! `[server] public_url` says what this Instance is called from outside —
//! because the `<iframe>` snippet is the thing an Operator came here for.
//!
//! **The address is on the listing, and that is not the share-link rule
//! broken.** A Share link's token and a Webhook's URL are credentials, so their
//! rows carry neither. An embed's token is printed into a stranger's public page
//! source by design and opens nothing a Listener holding no code could not hear
//! (`crate::embed`), so it is an address rather than a secret.
//!
//! **Restricted channels are counted, not refused.** An embed plays open
//! listening only, so a Selection naming a restricted channel is not an error —
//! the channel simply never plays in it. Refusing would make "everything" an
//! unsaveable Selection on any Instance that gates one channel; saying nothing
//! would let an Operator learn it from the host. So each row says how many of
//! the channels it names will not play ([`EmbedRow::restricted`]), and the
//! screen says it in a sentence.

use axum::extract::{Path, State};
use sea_orm::{ActiveModelTrait, EntityTrait, IntoActiveModel, QueryOrder, Set};
use serde::{Deserialize, Serialize};

use super::{Created, Listing, Rejected, Removed, What, required_name};
use crate::AppState;
use crate::db::entities::embed;
use crate::db::repo;
use crate::failure::{Failure, Stage};
use crate::selection::Selection;

/// One Embed, as the curation screen lists it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EmbedRow {
    pub id: i64,
    pub name: String,
    /// `/embed?t=…` — what the snippet frames, against whatever origin the
    /// Operator's readers reach this Instance on.
    pub path: String,
    /// The same, absolute, when `[server] public_url` is set — and `None` when
    /// it is not, rather than a guess: an Operator's browser may be on a LAN
    /// address no reader of the host's page can reach, so the screen builds one
    /// from where it is and says that is what it did.
    pub url: Option<String>,
    pub selection: Selection,
    /// How many restricted channels this Selection names — channels that will
    /// never play here, because an embed hears what a Listener holding no code
    /// hears (module note).
    pub restricted: usize,
    pub created_at_ms: i64,
}

crate::answers_json!(EmbedRow);

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NewEmbed {
    pub name: String,
    /// Absent selects nothing — [`Selection::default`] — which is the safe
    /// reading of a form that forgot to send one, and an embed an Operator can
    /// see is empty rather than one that plays their whole Instance.
    #[serde(default)]
    pub selection: Selection,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EmbedPatch {
    pub name: Option<String>,
    pub selection: Option<Selection>,
}

/// `GET /api/admin/embeds` — every embed, oldest first.
pub async fn list(State(state): State<AppState>) -> Result<Listing<EmbedRow>, Failure> {
    let rows = embed::Entity::find()
        .order_by_asc(embed::Column::Id)
        .all(&state.db)
        .await
        .map_err(Stage::Curate.failed())?;
    let restricted = restricted_channels(&state).await?;

    Ok(Listing::new(
        rows.into_iter()
            .map(|row| row_of(row, &restricted, state.shares.public_url()))
            .collect(),
    ))
}

/// `POST /api/admin/embeds` — publish one.
pub async fn create(
    State(state): State<AppState>,
    axum::Json(body): axum::Json<NewEmbed>,
) -> Result<Created<EmbedRow>, Failure> {
    let name = required_name(&body.name, "name")?;

    let row = embed::ActiveModel {
        name: Set(name),
        // The share link's generator: the same 128 bits, minted the same way.
        token: Set(crate::share::new_token()),
        selection: Set(super::downstreams::scope_json(&body.selection)),
        created_at_ms: Set(state.clock.now_ms()),
        ..Default::default()
    }
    .insert(&state.db)
    .await
    .map_err(Stage::Curate.failed())?;

    let restricted = restricted_channels(&state).await?;
    Ok(Created(row_of(row, &restricted, state.shares.public_url())))
}

/// `PATCH /api/admin/embeds/{id}` — rename, or re-scope. The address stays,
/// which is the point: the host's snippet plays whatever this says next.
pub async fn update(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    axum::Json(body): axum::Json<EmbedPatch>,
) -> Result<EmbedRow, Failure> {
    let existing = embed::Entity::find_by_id(id)
        .one(&state.db)
        .await
        .map_err(Stage::Curate.failed())?
        .ok_or(Rejected::NotFound(What::Embed))?;

    let mut row = existing.into_active_model();
    if let Some(name) = &body.name {
        row.name = Set(required_name(name, "name")?);
    }
    if let Some(selection) = &body.selection {
        row.selection = Set(super::downstreams::scope_json(selection));
    }
    let row = row
        .update(&state.db)
        .await
        .map_err(Stage::Curate.failed())?;

    let restricted = restricted_channels(&state).await?;
    Ok(row_of(row, &restricted, state.shares.public_url()))
}

/// `DELETE /api/admin/embeds/{id}` — take one down. Every reader who loads the
/// host's page, or presses Listen, after this is told the feed is no longer
/// available. A reader *already* listening keeps the open live feed until they
/// stop or reload — open listening they could reach without the embed, so it
/// leaks nothing, and cutting them off sooner would cost a poll per listener
/// (`client/src/embed/mount.ts`).
pub async fn remove(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Removed, Failure> {
    let deleted = embed::Entity::delete_by_id(id)
        .exec(&state.db)
        .await
        .map_err(Stage::Curate.failed())?;
    if deleted.rows_affected == 0 {
        return Err(Rejected::NotFound(What::Embed).into());
    }
    Ok(Removed)
}

/// Every channel this Instance restricts — or none, for no statement, when it
/// restricts nothing ([`crate::access::Access::is_gating`], which is re-read on
/// the request that changes it).
async fn restricted_channels(state: &AppState) -> Result<Vec<(i64, i64)>, Failure> {
    if !state.access.is_gating() {
        return Ok(Vec::new());
    }
    repo::restricted_channels(&state.db)
        .await
        .map_err(Stage::Curate.failed())
}

/// The stored row as the screen reads it.
fn row_of(row: embed::Model, restricted: &[(i64, i64)], public_url: Option<&str>) -> EmbedRow {
    let selection = crate::selection::stored(&row.selection);
    let path = crate::embed::link_to(&row.token);
    EmbedRow {
        id: row.id,
        name: row.name,
        url: crate::config::absolute_url(&path, public_url),
        path,
        restricted: restricted
            .iter()
            .filter(|(system_ref, talkgroup_ref)| selection.selects(*system_ref, *talkgroup_ref))
            .count(),
        selection,
        created_at_ms: row.created_at_ms,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model() -> embed::Model {
        embed::Model {
            id: 3,
            name: String::from("Fire dispatch"),
            token: String::from("0123abcd"),
            selection: serde_json::json!({ "sel": { "11": { "*": true } } }).to_string(),
            created_at_ms: 1_700_000_000_000,
        }
    }

    /// Without a public URL there is a path and nothing absolute — the screen
    /// builds the rest from where it is, and says so.
    #[test]
    fn a_row_without_a_public_url_hands_out_a_path() {
        let row = row_of(model(), &[], None);

        assert_eq!(row.path, "/embed?t=0123abcd");
        assert_eq!(row.url, None);
    }

    /// With one, the snippet's address is absolute — and a trailing slash on the
    /// setting is not a doubled one in a stranger's HTML.
    #[rstest::rstest]
    #[case::bare("https://scanner.example.org")]
    #[case::trailing_slash("https://scanner.example.org/")]
    fn a_public_url_makes_the_address_absolute(#[case] origin: &str) {
        let row = row_of(model(), &[], Some(origin));

        assert_eq!(
            row.url.as_deref(),
            Some("https://scanner.example.org/embed?t=0123abcd")
        );
    }

    /// The count is of restricted channels **this Selection names** — the
    /// System's wildcard reaches 11's, and nothing reaches 22's.
    #[test]
    fn the_restricted_count_is_what_the_selection_reaches() {
        let row = row_of(model(), &[(11, 100), (11, 200), (22, 100)], None);

        assert_eq!(row.restricted, 2);
    }

    /// A stored Selection that will not parse plays nothing, and the screen
    /// shows the same nothing the feed applies (`crate::selection::stored`).
    #[test]
    fn an_unreadable_selection_reads_as_nothing_on_the_screen_too() {
        let row = row_of(
            embed::Model {
                selection: String::from("<not json>"),
                ..model()
            },
            &[(11, 100)],
            None,
        );

        assert!(row.selection.is_all_off());
        assert_eq!(row.restricted, 0);
    }
}
