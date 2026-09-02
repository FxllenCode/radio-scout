//! **Share links** — an expiring public URL for one **Call** (#64, spec US 32).
//!
//! A **Listener** who finds a moment worth sending mints a link; whoever they
//! send it to gets a page with that Call on it and nothing else. Sharing a
//! moment does not mean sharing the Instance: the link opens one Call, for a
//! bounded window, and reaches no search, no catalog and no other audio.
//!
//! rdio-scanner has nothing like this. Its only addressable state is `?id=` on a
//! Profile, so "listen to this" there means "go to my scanner, set these
//! filters, and find it".
//!
//! # Five things worth knowing
//!
//! **One live link per Call, which is the whole abuse bound.** Minting is
//! unauthenticated, because a Listener holds no credential and US 32 is a
//! Listener's story. A table keyed on anything but the Call would let anybody
//! fill a Pi's disk by POSTing in a loop; keyed on the Call it is bounded by the
//! Calls table, which **Retention** already bounds. So a second mint for a Call
//! already shared hands back *the link that is already out there* and pushes its
//! expiry out ([`Mint::Extend`]), and two Listeners sharing one Call share one
//! link. Revoking it therefore revokes it for both — the deliberate cost, and
//! the thing that makes an Operator's revoke mean *this Call is no longer
//! shared*.
//!
//! **The token rides the query string, and that is a rule-2 decision rather
//! than a URL-design one.** A share token is a bearer credential — the whole
//! link is the credential, the way a **Webhook**'s URL is — and
//! [`crate::http_log`] logs a request's **path** and deliberately never its
//! query, because ADR-0008 puts access codes in a query parameter. Putting the
//! token there means "the token is never logged" is true by construction rather
//! than by a redaction pass that can be wrong. `/s?t=…`, and `/s/audio?t=…` for
//! the bytes.
//!
//! **The audio is served through the token, never through `/api/call/{id}`.**
//! It is the same [`crate::serve`] decision — the filesystem store proxies with
//! range support, an S3-shaped store redirects to a presigned URL — reached
//! through a different door, so both storage backends work with no second
//! implementation. The door matters: listening is open today (ADR-0008), and
//! when #68 makes it scoped a share link must still open exactly one Call
//! rather than inherit whatever the Archive's own audio route then permits.
//!
//! **The page is server-rendered and self-contained.** A messaging app's link
//! preview is fetched by a crawler that does not run JavaScript, so an OG card
//! cannot come from the SPA; and "playable without the app" is a promise the
//! page keeps by being one file with its own `<audio>` element, working in a
//! checkout where `client/dist` was never built. That promise reaches one place
//! outside this module: `client/src/sw.ts` **denies `/s`**, or the service
//! worker would answer a share link with the app shell — breaking every link
//! opened in a browser that has this Instance installed, and only in that
//! browser.
//!
//! **An expiry is a promise about the link that was handed out.** So an expired
//! row is re-minted with a *new* token ([`Mint::Issue`]) rather than having its
//! old one brought back to life, and revoking deletes the row, which kills that
//! URL for good — a token is 128 random bits and is never reissued to the same
//! value.

pub mod page;

use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Query, State};
use axum::http::HeaderMap;
use serde::{Deserialize, Serialize};

use crate::AppState;
use crate::call::CallId;
use crate::db::repo;
use crate::failure::{Failure, Reason, Stage};

/// `[share]` — the whole of what an Operator configures about this (ADR-0012).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ShareConfig {
    /// Mint links at all. **On**, like `[quiet] enabled` and the **Mining**
    /// sweep and unlike `[enhancement] mode`: an Instance as it ships already
    /// serves its whole Archive to anyone who asks (ADR-0008), so a share link
    /// exposes nothing new, and a Listener should not have to find a setting
    /// before they can send somebody a moment.
    ///
    /// Off is for the Instance that has *closed* listening — behind a VPN,
    /// behind Cloudflare Access, or once #68's access codes scope it — where a
    /// link that bypasses the gate on purpose is a decision an Operator should
    /// be making rather than inheriting. Turning it off also stops the links
    /// already minted, which is what makes it usable as a lever.
    pub enabled: bool,
    /// How long a minted link stays usable.
    ///
    /// A week: long enough that a link sent on Friday still plays when somebody
    /// opens it on Monday, short enough that a URL pasted into a group chat is
    /// not a permanent public endpoint. The **Operator's** setting and not the
    /// Listener's, because there is one link per Call and a per-mint choice
    /// would leave "whose TTL wins when the second Listener shares it?" with no
    /// good answer.
    #[serde(rename = "link_ttl_secs", with = "crate::config::secs")]
    pub link_ttl: Duration,
}

impl Default for ShareConfig {
    fn default() -> Self {
        ShareConfig {
            enabled: true,
            link_ttl: Duration::from_secs(7 * 24 * 60 * 60),
        }
    }
}

/// The share surface as an [`crate::AppState`] holds it: the policy, plus where
/// this Instance can be reached from outside.
///
/// [`crate::webhook::Webhooks`]'s shape and for its reason — `[server]
/// public_url` is one setting two features read, and a handler that reached for
/// the whole `Config` would be able to read the database URL as well.
#[derive(Clone)]
pub struct Shares {
    config: ShareConfig,
    /// Where an absolute link points, or `None` — which is what ships, and is a
    /// real answer rather than a broken one (#54's argument, unchanged): an
    /// Instance genuinely may not know its own address, and a *guessed* URL in
    /// somebody's link preview is worse than a card with no image on it.
    public_url: Option<Arc<str>>,
}

impl Shares {
    pub fn new(config: ShareConfig, public_url: Option<String>) -> Self {
        Shares {
            config,
            public_url: public_url.map(Arc::from),
        }
    }

    /// Whether links may be minted — and, deliberately, whether the ones
    /// already minted still open: turning this off is an Operator closing a
    /// door, not hiding a button.
    pub fn enabled(&self) -> bool {
        self.config.enabled
    }

    pub fn link_ttl(&self) -> Duration {
        self.config.link_ttl
    }

    pub fn public_url(&self) -> Option<&str> {
        self.public_url.as_deref()
    }
}

impl Default for Shares {
    fn default() -> Self {
        Shares::new(ShareConfig::default(), None)
    }
}

/// Where a share link lives, as a path. One constant, because the mint's answer,
/// the page's own audio `src` and the canonical URL in its OG card must all
/// spell it the same way.
pub const SHARE_PATH: &str = "/s";

/// ...and the bytes behind it.
pub const SHARE_AUDIO_PATH: &str = "/s/audio";

/// The query parameter the token rides in — see the module note on why it is a
/// query parameter at all.
pub const TOKEN_PARAM: &str = "t";

/// Whether a link with this expiry is still usable at `now_ms`.
///
/// **Written once**, and read by the page, by the audio route and by minting —
/// `crate::serve`'s own discipline about its dedup window, for its reason: a mutation
/// that moved one edge and not another would hand somebody a link the page
/// plays and the audio refuses, which reads as a broken instance rather than as
/// an expired link.
///
/// Exclusive at the instant itself: a link whose expiry is *now* has run out.
pub fn live_at(expires_at_ms: i64, now_ms: i64) -> bool {
    now_ms < expires_at_ms
}

/// What minting has to do about the row already there.
///
/// Pure, and the only decision minting makes: everything else is a token to
/// generate and a row to write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mint {
    /// Nothing is stored for this Call, or what is stored has run out — write a
    /// new token down. An expired link is never resurrected (module note).
    Issue,
    /// A live link exists: hand back its token and push its expiry out.
    Extend,
}

/// Decide it, given the expiry of the link already minted for this Call (if
/// any).
pub fn mint(existing: Option<i64>, now_ms: i64) -> Mint {
    match existing {
        Some(expires_at_ms) if live_at(expires_at_ms, now_ms) => Mint::Extend,
        _ => Mint::Issue,
    }
}

/// When a link minted now runs out.
fn expires_at(now_ms: i64, ttl: Duration) -> i64 {
    now_ms.saturating_add(ttl.as_millis() as i64)
}

/// Why a share link is not going to open anything — the three ways, each with
/// its own [`Reason`].
///
/// A value rather than three branches, because **two surfaces answer it in two
/// shapes**: the page renders HTML a human reads, the audio route answers the
/// `<audio>` element in the one refusal vocabulary every other route uses. That
/// is #92's rule — a surface's refusal *shape* may differ, the policy underneath
/// may not — so the status and the logged slug come from the [`Reason`] in both
/// cases and cannot drift apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gone {
    /// No link by that token: never minted, already revoked, or superseded by a
    /// re-mint after it expired.
    Unknown,
    /// The link existed and its window has closed. **Told apart from
    /// `Unknown` on purpose** — "this expired" is the answer the ticket asks
    /// for, and it is the difference between "you were too slow" and "that was
    /// never a link", which the recipient reacts to differently.
    Expired,
    /// `[share] enabled` is off. Indistinguishable from `Unknown` to whoever is
    /// knocking — an Operator who closed the door owes them no map of it — and
    /// distinct in the **log**, so the Operator whose links stopped working can
    /// see why in the Logs view.
    Disabled,
}

impl Gone {
    /// The refusal this is, in the one vocabulary (ADR-0011 rules 3 and 6).
    pub fn reason(self) -> Reason {
        match self {
            Gone::Unknown => Reason::ShareNotFound,
            Gone::Expired => Reason::ShareExpired,
            Gone::Disabled => Reason::SharingDisabled,
        }
    }
}

/// What a Listener is handed back when they ask for a link.
///
/// A **path**, not an absolute URL, because every other URL this app answers
/// with is one ([`crate::call::StoredCall::audio_url`]) and the browser
/// minting it is the side that knows the origin it is on. An Instance may not
/// know its own address at all (`[server] public_url` ships unset), and a
/// guessed one in somebody's chat room is worse than none.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Minted {
    /// `/s?t=…` — path and query together, ready to be made absolute against
    /// whatever origin the Listener reached this Instance on.
    pub url: String,
    /// When it stops working, so the control can say so.
    pub expires_at_ms: i64,
}

crate::answers_json!(Minted);

/// The link a token names, as a URL.
///
/// `pub` rather than `pub(super)` because the integration harness mints links
/// too, and a test that spelled `/s?t=…` for itself would be a second
/// implementation of this — right until one of them moved.
pub fn link_to(token: &str) -> String {
    format!("{SHARE_PATH}?{TOKEN_PARAM}={token}")
}

/// ...and the audio behind it.
pub fn audio_link_to(token: &str) -> String {
    format!("{SHARE_AUDIO_PATH}?{TOKEN_PARAM}={token}")
}

/// A 128-bit random token, hex-encoded.
///
/// Half a session id's ([`crate::admin`]) width, deliberately: 128 bits is
/// unguessable at any rate an instance could be asked to answer, and this string
/// is one a human reads in a chat client, where twice the length buys nothing
/// and costs a line wrap.
fn new_token() -> String {
    use argon2::password_hash::rand_core::{OsRng, RngCore};
    let mut bytes = [0u8; 16];
    OsRng.fill_bytes(&mut bytes);
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// The token in `?t=…`.
#[derive(Debug, Deserialize)]
pub struct TokenQuery {
    /// Optional so a bare `/s` is an ordinary "no such link" rather than
    /// axum's bare 400, which would tell a recipient nothing and log nothing an
    /// Operator could grep.
    #[serde(default, rename = "t")]
    token: Option<String>,
}

/// `POST /api/call/{id}/share` — mint (or re-mint) this Call's public link.
pub async fn create(
    State(state): State<AppState>,
    axum::extract::Path(id): axum::extract::Path<CallId>,
) -> Result<Minted, Failure> {
    if !state.shares.enabled() {
        return Err(Gone::Disabled.reason().into());
    }
    // The Call has to exist before a link to it does — otherwise a typo'd id
    // mints a working URL onto a page that can never render.
    repo::find_call(&state.db, id)
        .await
        .map_err(Stage::MintShare.failed())?
        .ok_or(Reason::CallNotFound)?;

    let now_ms = state.clock.now_ms();
    let expires_at_ms = expires_at(now_ms, state.shares.link_ttl());
    let existing = repo::share_for_call(&state.db, id)
        .await
        .map_err(Stage::MintShare.failed())?;

    let token = match existing {
        Some(link) => match mint(Some(link.expires_at_ms), now_ms) {
            // The link already out there, with more time on it.
            Mint::Extend => {
                repo::renew_share(&state.db, link.id, None, expires_at_ms)
                    .await
                    .map_err(Stage::MintShare.failed())?;
                link.token
            }
            // The row survives — it is the abuse bound — and its secret does
            // not: the URL that ran out stays dead.
            Mint::Issue => {
                let token = new_token();
                repo::renew_share(&state.db, link.id, Some(&token), expires_at_ms)
                    .await
                    .map_err(Stage::MintShare.failed())?;
                token
            }
        },
        None => {
            let token = new_token();
            repo::insert_share(&state.db, id, &token, expires_at_ms, now_ms)
                .await
                .map_err(Stage::MintShare.failed())?;
            token
        }
    };

    // **DEBUG, not INFO.** The durable record that a Call is shared is the
    // *row*, which Settings → Admin → Share links shows; a line beside it tells
    // an Operator nothing they act on. And this is the one write surface in the
    // process that takes no credential, whose whole design note is about somebody
    // POSTing in a loop — at INFO that loop would also write a row per request
    // into the operator log, which is precisely rule 8.
    tracing::debug!(call_id = id, expires_at_ms, "share link minted");
    Ok(Minted {
        url: link_to(&token),
        expires_at_ms,
    })
}

/// A live link and the Call behind it, or the reason there is neither.
enum Resolved {
    Live(Box<Opened>),
    Gone(Gone),
}

/// A live link and the Call behind it.
struct Opened {
    token: String,
    expires_at_ms: i64,
    call: crate::db::entities::call::Model,
}

/// Turn a token into the Call it opens.
///
/// **One function for both routes**, so the page and its audio cannot disagree
/// about whether a link is live — which would show a Listener's recipient a Call
/// they then cannot play, and read as a broken instance rather than as an
/// expired link.
async fn resolve(state: &AppState, token: Option<&str>) -> Result<Resolved, Failure> {
    if !state.shares.enabled() {
        return Ok(Resolved::Gone(Gone::Disabled));
    }
    let Some(token) = token else {
        return Ok(Resolved::Gone(Gone::Unknown));
    };
    let Some((link, call)) = repo::share_by_token(&state.db, token)
        .await
        .map_err(Stage::OpenShare.failed())?
    else {
        return Ok(Resolved::Gone(Gone::Unknown));
    };
    if !live_at(link.expires_at_ms, state.clock.now_ms()) {
        return Ok(Resolved::Gone(Gone::Expired));
    }
    Ok(Resolved::Live(Box::new(Opened {
        token: link.token,
        expires_at_ms: link.expires_at_ms,
        call,
    })))
}

/// `GET /s?t=…` — the page a recipient opens.
pub async fn open(
    State(state): State<AppState>,
    Query(query): Query<TokenQuery>,
) -> Result<page::Rendered, Failure> {
    let opened = match resolve(&state, query.token.as_deref()).await? {
        Resolved::Live(opened) => opened,
        Resolved::Gone(gone) => return Ok(page::Rendered::gone(gone)),
    };

    let view = crate::archive::stored_calls(&state.db, std::slice::from_ref(&opened.call))
        .await
        .map_err(Stage::OpenShare.failed())?
        .pop()
        // One row in, one view out: `stored_calls` answers a slice with a Vec of
        // the same length, so this is unreachable rather than a case.
        .ok_or(Reason::CallNotFound)?;

    Ok(page::Rendered::call(page::Preview::of(
        &view,
        &opened.token,
        opened.expires_at_ms,
        state.shares.public_url(),
    )))
}

/// `GET /s/audio?t=…` — the bytes, through the token.
///
/// [`crate::serve`]'s decision reached through a different door, so a share link
/// works on both storage backends without a second implementation of ranges,
/// presigning, or the cache policy that tells a Call whose object may still be
/// swapped from one whose bytes are final.
pub async fn audio(
    State(state): State<AppState>,
    Query(query): Query<TokenQuery>,
    headers: HeaderMap,
) -> Result<crate::serve::Audio, Failure> {
    match resolve(&state, query.token.as_deref()).await? {
        Resolved::Live(opened) => crate::serve::serve_call(&state, opened.call.id, &headers).await,
        Resolved::Gone(gone) => Err(gone.reason().into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    /// A link is live right up to its expiry and not at it — the edge both
    /// routes and the mint read, so it is asserted at the instant rather than
    /// around it.
    #[rstest]
    #[case::well_inside(1_000, 500, true)]
    #[case::a_millisecond_left(1_000, 999, true)]
    #[case::at_the_instant(1_000, 1_000, false)]
    #[case::a_millisecond_past(1_000, 1_001, false)]
    #[case::long_gone(1_000, 90_000, false)]
    fn a_link_is_live_until_its_expiry(
        #[case] expires_at_ms: i64,
        #[case] now_ms: i64,
        #[case] live: bool,
    ) {
        assert_eq!(live_at(expires_at_ms, now_ms), live);
    }

    /// Minting extends what is live and issues afresh for everything else —
    /// including a row whose link ran out, because an expiry is a promise about
    /// the URL that was handed out and bringing it back would break it.
    #[rstest]
    #[case::never_shared(None, 1_000, Mint::Issue)]
    #[case::still_live(Some(2_000), 1_000, Mint::Extend)]
    #[case::a_millisecond_left(Some(1_001), 1_000, Mint::Extend)]
    #[case::expiring_this_instant(Some(1_000), 1_000, Mint::Issue)]
    #[case::long_expired(Some(10), 1_000, Mint::Issue)]
    fn minting_extends_a_live_link_and_replaces_a_dead_one(
        #[case] existing: Option<i64>,
        #[case] now_ms: i64,
        #[case] expected: Mint,
    ) {
        assert_eq!(mint(existing, now_ms), expected);
    }

    /// The window is the Operator's setting, measured from now.
    #[test]
    fn a_link_runs_out_one_ttl_from_now() {
        assert_eq!(expires_at(1_000, Duration::from_secs(60)), 61_000);
    }

    /// A clock far enough into the future that adding a week would overflow is
    /// not worth a panic — the link is simply as far out as an `i64` goes.
    #[test]
    fn an_expiry_past_the_end_of_time_saturates() {
        assert_eq!(expires_at(i64::MAX, Duration::from_secs(60)), i64::MAX);
    }

    /// Each way of not opening has its own slug, so an Operator reading the
    /// Logs view can tell "somebody clicked a dead link" from "I turned this
    /// off and forgot".
    #[rstest]
    #[case(Gone::Unknown, "share-not-found")]
    #[case(Gone::Expired, "share-expired")]
    #[case(Gone::Disabled, "sharing-disabled")]
    fn every_way_of_being_gone_is_its_own_reason(#[case] gone: Gone, #[case] slug: &str) {
        assert_eq!(gone.reason().slug(), slug);
    }

    /// Two tokens minted in a row differ, and are the width the module note
    /// claims — 128 bits, hex.
    #[test]
    fn a_token_is_128_random_bits() {
        let tokens: std::collections::HashSet<String> = (0..1_000).map(|_| new_token()).collect();

        assert_eq!(tokens.len(), 1_000, "tokens repeated");
        for token in &tokens {
            assert_eq!(token.len(), 32, "{token}");
            assert!(
                token.chars().all(|c| c.is_ascii_hexdigit()),
                "{token} is not hex"
            );
        }
    }

    /// The two paths and the parameter are spelled once, and this is what a
    /// client, a crawler and the page's own `<audio>` all read.
    #[test]
    fn a_link_is_the_token_in_a_query_parameter() {
        assert_eq!(link_to("abc"), "/s?t=abc");
        assert_eq!(audio_link_to("abc"), "/s/audio?t=abc");
    }
}
