//! **The embeddable player** (#75, spec US 59): the scanner on somebody else's
//! homepage.
//!
//! An **Embed** is a **Selection** an Operator publishes for another site to
//! frame — the fire department's homepage carrying fire dispatch, a newsroom
//! carrying the county. The Operator makes one in Settings → Admin → Embeds
//! (`crate::curate::embeds`) and is handed an `<iframe>` snippet; the host
//! pastes it; their readers see the Selection's recent Calls and can press
//! **Listen live**.
//!
//! Three routes: `GET /embed?t=…` is the page (a second, small entry of the
//! client build — `client/embed.html`, `client/src/embed/`), `GET
//! /api/embed?t=…` is what it reads (the name, the Selection, a page of recent
//! Calls), and listening is the ordinary live feed, `/api/live`, subscribed to
//! the embed's Selection.
//!
//! # Improving on rdio-scanner
//!
//! rdio-scanner has no embed. What its community does instead is iframe the
//! whole app — which works only because rdio sends no framing header at all, so
//! every rdio instance, its admin page included, can be framed by anybody. The
//! host gets a full scanner UI they cannot scope, the reader gets the
//! Operator's whole Instance, and nothing tells the Operator it is happening.
//! Here the Operator decides what is framed, can change it without the host
//! touching their HTML, and can take it down; and everything else on the
//! Instance refuses to be framed (`crate::framing`).
//!
//! # The decisions, and why (grilled before the first test)
//!
//! - **A row with a token, not a `?sel=` in the host's HTML.** The snippet
//!   names the row, so the Operator owns what it plays: re-scope it and the same
//!   `<iframe>` plays the new Selection; delete it and the frame says the feed
//!   is no longer available. A Selection spelled into a stranger's page could
//!   be neither.
//! - **Open listening, and never a restricted channel.** The token is printed
//!   into public page source, so it cannot be a credential; an embed hears what
//!   a **Listener** holding no code hears ([`crate::access::Viewer::resolve`]
//!   with `None`), intersected with its Selection. A grant appended to the
//!   frame's URL is not read either — this asks for `None` rather than for
//!   whatever the request carried. The admin listing counts the restricted
//!   channels an embed names, so an Operator learns that from the screen rather
//!   than from the host.
//! - **The socket opens when a reader presses Listen.** A newsroom's frontpage
//!   can put thousands of readers on a Pi, and most of them never press play.
//!   Loading the page costs one static document and one feed read; only a
//!   reader who listens holds a connection — and is counted as a **Listener**
//!   (#62), because they are one.
//! - **"Unreachable" degrades after load, and not before.** If the Instance is
//!   down when the host's page loads, the frame never receives this page, and
//!   nothing served from here can run in it — that case is the browser's own
//!   frame error, and is documented as such rather than promised away. Once the
//!   page is up it keeps the list it has whatever the network does: a reader
//!   listening is told it is reconnecting, retried with backoff, and caught up
//!   through the live feed's own **Backfill** when it is back; a reader who taps
//!   a Call that will not load is told that it would not play; a first load
//!   that fails is retried the same way; a deleted embed says it is no longer
//!   available.
//! - **HTTPS, in practice.** A site served over https — nearly all of them —
//!   is not allowed to frame a plain-http address, so an embed is only usable
//!   on an Instance served over https — today, behind a reverse proxy that
//!   terminates TLS (ADR-0008). The admin screen says so beside a plain-http
//!   snippet, which is where the address is chosen.
//!
//! # What the feed costs
//!
//! [`RECENT_CALLS`] of them, newest first, through [`crate::archive::window`] —
//! the Archive's own search with its total left off, because a paginator's
//! `COUNT` is the one statement whose cost grows with the Archive and nothing
//! on this page would read it. `tests/embed.rs` holds it to the same statement
//! count at two sizes.
//!
//! # What is deliberately not here
//!
//! - **The configuration document does not carry embeds** (#51), for the
//!   reason it carries no **Webhook**, **Event** or **Dirwatch**: it is the
//!   domain Calls are addressed to, not everything an Operator has published.
//! - **No per-embed list of allowed hosts.** Anybody may frame an embed; the
//!   audio is open listening, which anybody may already hear. An Operator who
//!   wants one host to stop deletes the embed and makes the remaining hosts a
//!   new one.
//! - **No theme parameter.** The page follows the reader's
//!   `prefers-color-scheme`.

use axum::Extension;
use axum::extract::{Query, State};
use axum::response::{IntoResponse, Response};
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use serde::Serialize;

use crate::AppState;
use crate::access::Viewer;
use crate::archive::{CallSearch, CallSort};
use crate::call::StoredCall;
use crate::db::entities::embed;
use crate::failure::{Failure, Reason, Stage};
use crate::framing::Framable;
use crate::selection::Selection;
use crate::share::{TOKEN_PARAM, TokenQuery};

/// Where an embed's page lives.
pub const EMBED_PATH: &str = "/embed";

/// ...and what the page reads.
pub const FEED_PATH: &str = "/api/embed";

/// How many recent Calls a feed carries.
///
/// A page's worth: enough that a reader arriving on a quiet afternoon sees
/// the channel is alive, few enough that a host's homepage is not where an
/// Archive is browsed — which is the full app's job, and the page links to it.
pub const RECENT_CALLS: u64 = 20;

/// An embed's address, as a path — the one spelling the listing, the snippet
/// and the tests share.
pub fn link_to(token: &str) -> String {
    format!("{EMBED_PATH}?{TOKEN_PARAM}={token}")
}

/// ...and the feed behind it.
pub fn feed_to(token: &str) -> String {
    format!("{FEED_PATH}?{TOKEN_PARAM}={token}")
}

/// What the page reads.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Feed {
    /// What the page says at its top.
    pub name: String,
    /// What it plays — and, literally, what it subscribes the live feed with.
    pub selection: Selection,
    /// The most recent Calls this Selection reaches under open listening,
    /// newest first.
    pub calls: Vec<StoredCall>,
}

crate::answers_json!(Feed);

/// `GET /embed?t=…` — the page, framable by anybody.
///
/// The same document whatever the token (`crate::web::embed_page`): whether the
/// embed exists is the feed's to answer, so a deleted one draws a sentence in
/// the host's frame rather than a browser's error page.
pub async fn page() -> Response {
    (Extension(Framable), crate::web::embed_page()).into_response()
}

/// `GET /api/embed?t=…` — the name, the Selection, and its recent Calls.
pub async fn feed(
    State(state): State<AppState>,
    Query(query): Query<TokenQuery>,
) -> Result<Feed, Failure> {
    let Some(token) = query.token() else {
        return Err(Reason::EmbedNotFound.into());
    };
    let embed = embed::Entity::find()
        .filter(embed::Column::Token.eq(token))
        .one(&state.db)
        .await
        .map_err(Stage::OpenEmbed.failed())?
        .ok_or(Reason::EmbedNotFound)?;

    let selection = crate::selection::stored(&embed.selection);
    // A Listener holding nothing — not whatever this request carried. See the
    // module note: the address is public, so it opens nothing.
    let viewer = Viewer::resolve(&state, None).await?;
    let search = CallSearch {
        selection: Some(selection.clone()),
        scope: viewer.scope,
        published_only: viewer.delaying,
        sort: CallSort::Newest,
        limit: RECENT_CALLS,
        ..CallSearch::default()
    };
    let calls = crate::archive::window(&state.db, &search)
        .await
        .map_err(Stage::OpenEmbed.failed())?;

    Ok(Feed {
        name: embed.name,
        selection,
        calls,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The address is the share link's spelling under another path — the token
    /// in `?t=`, which `http_log` never writes down.
    #[test]
    fn an_embed_lives_at_its_token() {
        assert_eq!(link_to("abc123"), "/embed?t=abc123");
        assert_eq!(feed_to("abc123"), "/api/embed?t=abc123");
    }
}
