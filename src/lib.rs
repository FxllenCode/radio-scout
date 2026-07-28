//! Radio-Scout library crate.
//!
//! A real Call flows ingest -> blob store (ADR-0002) -> live-feed WebSocket ->
//! audio served back over HTTP with range support. `build_app` returns the Axum
//! router the binary serves and the integration harness drives in-process over
//! its real HTTP + WS boundary (ADR-0009).

pub mod admin;
pub mod archive;
pub mod blob;
pub mod call;
pub mod catalog;
pub mod config;
pub mod db;
pub mod enhance;
pub mod failure;
pub mod http_log;
pub mod import;
pub mod ingest;
pub mod live;
pub mod observability;
pub mod push;
pub mod retention;
pub mod selection;
pub mod service;
pub mod startup;
pub mod web;
pub mod webpush;

#[cfg(test)]
mod testing;

use std::sync::Arc;

use axum::Router;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{any, get, post};
use sea_orm::DatabaseConnection;

use crate::admin::AdminAuth;
use crate::call::CallId;
use crate::config::TrustedProxies;
use crate::db::repo;
use crate::enhance::Enhancer;
use crate::failure::ServerError;
use crate::live::LiveFeed;
use crate::push::Push;

// Re-exported so the binary and the integration harness can wire the app up
// without reaching into module paths.
pub use crate::blob::{BlobStore, S3Config, StorageConfig};
pub use crate::ingest::IngestConfig;

/// Shared application state, cloned into every handler. All fields are cheap to
/// clone (Arc / channel / DB pool handle).
#[derive(Clone)]
pub struct AppState {
    pub audio: Arc<BlobStore>,
    pub db: DatabaseConnection,
    pub live: LiveFeed,
    pub ingest: IngestConfig,
    /// Whose `X-Forwarded-For` the request log may believe (#17). Empty — the
    /// shipped default — means nobody's.
    pub trusted_proxies: TrustedProxies,
    /// The admin surface's credential and its live sessions (#19).
    pub admin: AdminAuth,
    /// The Web Push surface: the server's VAPID identity, or nothing at all
    /// when push is unconfigured (#16).
    pub push: Push,
    /// The enhancement queue, or nothing at all when `[enhancement] mode` is
    /// `off` — which is what ships (#20).
    pub enhancer: Enhancer,
}

impl AppState {
    /// Assemble state from a blob store, a database connection, and ingest
    /// config, with a fresh live-feed hub, no trusted proxies, and an admin
    /// surface nothing can authenticate to until a password is provisioned.
    pub fn new(audio: Arc<BlobStore>, db: DatabaseConnection, ingest: IngestConfig) -> Self {
        AppState {
            audio,
            db,
            live: LiveFeed::new(),
            ingest,
            trusted_proxies: TrustedProxies::default(),
            admin: AdminAuth::locked(),
            push: Push::disabled(),
            enhancer: Enhancer::disabled(),
        }
    }
}

/// Build the Axum application: the ingest endpoint, the live-feed WebSocket, and
/// audio serving. This is the single seam the binary and tests share.
pub fn build_app(state: AppState) -> Router {
    Router::new()
        .route("/api/call-upload", post(ingest::call_upload))
        .route(
            "/api/trunk-recorder-call-upload",
            post(ingest::trunk_recorder_call_upload),
        )
        .route("/api/live", any(live::ws_handler))
        // Archive read surface (#13): search, its cascading filter options, and
        // per-Call download.
        .route("/api/calls", get(archive::search))
        .route("/api/calls/filters", get(archive::filters))
        // What a listener can select from (#12) — Systems + Talkgroups, whether
        // or not any of their Calls are still in the archive.
        .route("/api/catalog", get(catalog::catalog))
        .route("/api/call/{id}/audio", get(serve_audio))
        .route("/api/call/{id}/download", get(archive::download))
        // Web Push (#16): the listener-facing half. Unauthenticated like the
        // rest of listening (ADR-0008) — what it grants is notifications to a
        // device that already holds the endpoint.
        .route("/api/push/key", get(push::key))
        .route("/api/push/subscribe", post(push::subscribe))
        .route("/api/push/unsubscribe", post(push::unsubscribe))
        // The way in to the admin surface, and the only route under
        // `/api/admin/` outside the session guard — there is no session yet.
        .route("/api/admin/login", post(admin::login))
        .merge(admin_routes(state.admin.clone()))
        .route("/healthz", get(healthz))
        // Everything else is the frontend: embedded SPA assets + client-side
        // routing (ADR-0007). The API/WS/health routes above take precedence.
        .fallback(web::spa_handler)
        // One line per request (#28), outermost so it sees every outcome —
        // including the 404s and 405s the router answers on its own. It carries
        // its own slice of state (the trust list, #17) rather than the whole of
        // it, because the layer is added before `with_state` and needs nothing
        // else.
        .layer(axum::middleware::from_fn_with_state(
            state.trusted_proxies.clone(),
            http_log::log_requests,
        ))
        .with_state(state)
}

/// Everything under `/api/admin/` that mutates or reveals configuration.
///
/// One router, so the session guard #19 puts over it is a **prefix layer** and
/// not a decoration each handler has to remember: a route added here is gated
/// by default, and a route that must not be — `/api/admin/login` — has to be
/// written outside on purpose.
fn admin_routes(admin: AdminAuth) -> Router<AppState> {
    Router::new()
        .route("/api/admin/session", get(admin::session))
        .route("/api/admin/logout", post(admin::logout))
        .route(
            "/api/admin/talkgroups/import",
            post(import::import_talkgroups),
        )
        // `route_layer`, not `layer`: it runs only for paths this router
        // matched, so an unrouted URL still 404s rather than being told to log
        // in first — which would turn the guard into a map of what exists.
        .route_layer(axum::middleware::from_fn_with_state(
            admin,
            admin::require_session,
        ))
}

/// How long a client may keep a Call's audio. The bytes behind a settled Call
/// never change, so this is `immutable`; `private` keeps it out of shared
/// proxies, since listening will become access-scoped (ADR-0008). A week is long
/// enough for the client's next-Call prefetch (#14) and for re-listening within
/// a session, without pinning audio that retention has since pruned.
const AUDIO_CACHE_CONTROL: &str = "private, max-age=604800, immutable";

/// ...and how long it may keep audio that is **queued for enhancement** (#20).
///
/// `immutable` is a promise the bytes behind this URL will never change, and for
/// a pending Call that promise is exactly false: the worker is about to point
/// the row at a different object. A client that cached it in the window would
/// keep the un-levelled version for a week and never learn otherwise, so the
/// promise is withheld until there is nothing left to replace. Short rather than
/// `no-store`, because the Call still has to play now — and a range request
/// mid-playback should not re-fetch the whole object.
const PENDING_AUDIO_CACHE_CONTROL: &str = "private, max-age=30";

/// The `Cache-Control` a Call's audio is served with, given its enhancement
/// state.
fn audio_cache_control(enhancement: &str) -> &'static str {
    match enhancement == db::entities::call::EnhancementState::PENDING {
        true => PENDING_AUDIO_CACHE_CONTROL,
        false => AUDIO_CACHE_CONTROL,
    }
}

/// `GET /api/call/{id}/audio` — serve a stored call's audio (ADR-0002).
///
/// The filesystem backend proxies with HTTP range support (iOS `<audio>` needs
/// it). The S3 backend instead redirects to a short-lived presigned URL after an
/// access-scope check (listening is open in v1, so the check is a no-op).
async fn serve_audio(
    State(state): State<AppState>,
    Path(id): Path<CallId>,
    headers: HeaderMap,
) -> Response {
    let audio = match repo::get_call_audio(&state.db, id).await {
        Ok(Some(audio)) => audio,
        Ok(None) => return (StatusCode::NOT_FOUND, "call not found\n").into_response(),
        Err(err) => return ServerError::new("look-up-call", err).into_response(),
    };

    let cache_control = audio_cache_control(&audio.enhancement);
    let object_key = audio.object_key;
    if state.audio.is_presigning() {
        match state.audio.presigned_get_url(&object_key).await {
            Some(Ok(url)) => return Redirect::temporary(&url).into_response(),
            Some(Err(err)) => return ServerError::new("sign-audio-url", err).into_response(),
            None => {}
        }
    }

    let size = match state.audio.size(&object_key).await {
        Ok(Some(size)) => size,
        Ok(None) => return (StatusCode::NOT_FOUND, "audio not found\n").into_response(),
        Err(err) => return ServerError::new("stat-audio", err).into_response(),
    };
    let mime = audio
        .mime
        .unwrap_or_else(|| "application/octet-stream".to_string());

    match parse_range_header(headers.get(header::RANGE), size) {
        RangeOutcome::None => match state.audio.get(&object_key).await {
            Ok(Some(bytes)) => (
                StatusCode::OK,
                [
                    (header::CONTENT_TYPE, mime),
                    (header::ACCEPT_RANGES, "bytes".to_string()),
                    (header::CACHE_CONTROL, cache_control.to_string()),
                ],
                bytes,
            )
                .into_response(),
            Ok(None) => (StatusCode::NOT_FOUND, "audio not found\n").into_response(),
            Err(err) => ServerError::new("read-audio", err).into_response(),
        },
        RangeOutcome::Range { start, end } => {
            match state.audio.get_range(&object_key, start, end + 1).await {
                Ok(bytes) => (
                    StatusCode::PARTIAL_CONTENT,
                    [
                        (header::CONTENT_TYPE, mime),
                        (header::ACCEPT_RANGES, "bytes".to_string()),
                        (header::CONTENT_RANGE, format!("bytes {start}-{end}/{size}")),
                        (header::CACHE_CONTROL, cache_control.to_string()),
                    ],
                    bytes,
                )
                    .into_response(),
                Err(err) => ServerError::new("read-audio-range", err).into_response(),
            }
        }
        RangeOutcome::Unsatisfiable => (
            StatusCode::RANGE_NOT_SATISFIABLE,
            [(header::CONTENT_RANGE, format!("bytes */{size}"))],
            "range not satisfiable\n",
        )
            .into_response(),
    }
}

/// The parsed outcome of a `Range` request header.
#[derive(Debug, PartialEq, Eq)]
enum RangeOutcome {
    /// No (usable) range header — serve the whole object.
    None,
    /// A satisfiable single byte range, inclusive `[start, end]`.
    Range { start: u64, end: u64 },
    /// A malformed or unsatisfiable range.
    Unsatisfiable,
}

/// Parse a single-range `Range: bytes=...` header against an object of `size`
/// bytes. Multi-range requests are treated as unsatisfiable (we don't emit
/// multipart/byteranges).
fn parse_range_header(value: Option<&HeaderValue>, size: u64) -> RangeOutcome {
    let Some(value) = value else {
        return RangeOutcome::None;
    };
    let Ok(text) = value.to_str() else {
        return RangeOutcome::Unsatisfiable;
    };
    let Some(spec) = text.trim().strip_prefix("bytes=") else {
        return RangeOutcome::Unsatisfiable;
    };
    let spec = spec.trim();
    // Multi-range (`a-b,c-d`) and empty specs need no explicit guard: a comma
    // makes one side fail to parse below, and an empty spec has no `-` to split.
    // (Keeping the guard would be an equivalent mutant — dead by construction.)
    let Some((raw_start, raw_end)) = spec.split_once('-') else {
        return RangeOutcome::Unsatisfiable;
    };
    if size == 0 {
        return RangeOutcome::Unsatisfiable;
    }

    let (start, end) = match (raw_start.trim(), raw_end.trim()) {
        ("", "") => return RangeOutcome::Unsatisfiable,
        // Suffix range: the last N bytes.
        ("", suffix) => {
            let Ok(n) = suffix.parse::<u64>() else {
                return RangeOutcome::Unsatisfiable;
            };
            if n == 0 {
                return RangeOutcome::Unsatisfiable;
            }
            let n = n.min(size);
            (size - n, size - 1)
        }
        // Open-ended: from `start` to the end.
        (start, "") => {
            let Ok(start) = start.parse::<u64>() else {
                return RangeOutcome::Unsatisfiable;
            };
            (start, size - 1)
        }
        // Closed range.
        (start, end) => {
            let (Ok(start), Ok(end)) = (start.parse::<u64>(), end.parse::<u64>()) else {
                return RangeOutcome::Unsatisfiable;
            };
            (start, end.min(size - 1))
        }
    };

    if start > end || start >= size {
        return RangeOutcome::Unsatisfiable;
    }
    RangeOutcome::Range { start, end }
}

/// `GET /healthz` — liveness probe.
async fn healthz() -> &'static str {
    "ok"
}

/// Wall-clock time in unix milliseconds — the crate's one clock reading.
///
/// Milliseconds since the epoch is how every timestamp is stored and compared
/// (dialect-agnostic, see `db::entities::call`). A clock before 1970 reads as 0
/// rather than panicking; nothing here is worth killing a scanner over.
pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since_epoch| since_epoch.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use rstest::rstest;

    fn parse(header: &str, size: u64) -> RangeOutcome {
        parse_range_header(Some(&HeaderValue::from_str(header).unwrap()), size)
    }

    #[rstest]
    // Satisfiable ranges (size = 10 unless noted).
    #[case("bytes=0-9", 10, RangeOutcome::Range { start: 0, end: 9 })]
    #[case("bytes=4-9", 10, RangeOutcome::Range { start: 4, end: 9 })]
    #[case("bytes=0-0", 10, RangeOutcome::Range { start: 0, end: 0 })]
    // Closed end past EOF clamps to size-1.
    #[case("bytes=4-100", 10, RangeOutcome::Range { start: 4, end: 9 })]
    // Open-ended runs to EOF.
    #[case("bytes=5-", 10, RangeOutcome::Range { start: 5, end: 9 })]
    // Suffix = last N bytes; over-long suffix clamps to the whole object.
    #[case("bytes=-4", 10, RangeOutcome::Range { start: 6, end: 9 })]
    #[case("bytes=-100", 10, RangeOutcome::Range { start: 0, end: 9 })]
    // Whitespace around the numbers is tolerated.
    #[case("bytes= 4 - 9 ", 10, RangeOutcome::Range { start: 4, end: 9 })]
    // Unsatisfiable / malformed.
    #[case("bytes=9-4", 10, RangeOutcome::Unsatisfiable)] // start > end
    #[case("bytes=10-20", 10, RangeOutcome::Unsatisfiable)] // start >= size
    #[case("bytes=a-9", 10, RangeOutcome::Unsatisfiable)] // non-numeric start
    #[case("bytes=4-b", 10, RangeOutcome::Unsatisfiable)] // non-numeric end
    #[case("bytes=x-", 10, RangeOutcome::Unsatisfiable)] // non-numeric open-ended
    #[case("bytes=-x", 10, RangeOutcome::Unsatisfiable)] // non-numeric suffix
    #[case("bytes=5", 10, RangeOutcome::Unsatisfiable)] // missing '-'
    #[case("bytes=", 10, RangeOutcome::Unsatisfiable)] // empty spec
    #[case("bytes=-", 10, RangeOutcome::Unsatisfiable)] // both empty
    #[case("bytes=-0", 10, RangeOutcome::Unsatisfiable)] // zero-length suffix
    #[case("bytes=0-1,2-3", 10, RangeOutcome::Unsatisfiable)] // multi-range not emitted
    #[case("items=0-9", 10, RangeOutcome::Unsatisfiable)] // wrong unit prefix
    #[case("bytes=0-9", 0, RangeOutcome::Unsatisfiable)] // zero-length object
    fn range_header_cases(#[case] header: &str, #[case] size: u64, #[case] expected: RangeOutcome) {
        assert_eq!(
            parse(header, size),
            expected,
            "header={header:?} size={size}"
        );
    }

    #[test]
    fn no_range_header_serves_the_whole_object() {
        assert_eq!(parse_range_header(None, 10), RangeOutcome::None);
    }

    #[test]
    fn non_ascii_range_header_is_unsatisfiable() {
        // A header value that isn't valid UTF-8 -> `to_str()` fails.
        let value = HeaderValue::from_bytes(&[0xFF, 0xFE]).unwrap();
        assert_eq!(
            parse_range_header(Some(&value), 10),
            RangeOutcome::Unsatisfiable
        );
    }

    proptest! {
        /// A well-formed closed range fully inside the object round-trips exactly.
        #[test]
        fn closed_range_within_bounds_round_trips(
            size in 1u64..10_000,
            start in 0u64..10_000,
            len in 0u64..10_000,
        ) {
            prop_assume!(start < size);
            let end = (start + len).min(size - 1);
            let header = format!("bytes={start}-{end}");
            prop_assert_eq!(
                parse(&header, size),
                RangeOutcome::Range { start, end }
            );
        }

        /// Whatever the input, a `Range` outcome is always a valid slice: no
        /// panic, `start <= end`, and `end` inside the object.
        #[test]
        fn parsed_range_is_always_a_valid_slice(
            size in 1u64..10_000,
            body in "[ -~]{0,24}",
        ) {
            let header = format!("bytes={body}");
            if let Ok(value) = HeaderValue::from_str(&header)
                && let RangeOutcome::Range { start, end } = parse_range_header(Some(&value), size)
            {
                prop_assert!(start <= end, "start {start} > end {end}");
                prop_assert!(end < size, "end {end} >= size {size}");
            }
        }
    }
}
