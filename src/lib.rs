//! Radio-Scout library crate.
//!
//! A real Call flows ingest -> blob store (ADR-0002) -> live-feed WebSocket ->
//! audio served back over HTTP with range support. `build_app` returns the Axum
//! router the binary serves and the integration harness drives in-process over
//! its real HTTP + WS boundary (ADR-0009).

pub mod blob;
pub mod call;
pub mod db;
pub mod ingest;
pub mod live;
pub mod web;

use std::sync::Arc;

use axum::Router;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{any, get, post};
use sea_orm::DatabaseConnection;

use crate::call::CallId;
use crate::db::repo;
use crate::live::LiveFeed;

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
}

impl AppState {
    /// Assemble state from a blob store, a database connection, and ingest
    /// config, with a fresh live-feed hub.
    pub fn new(audio: Arc<BlobStore>, db: DatabaseConnection, ingest: IngestConfig) -> Self {
        AppState {
            audio,
            db,
            live: LiveFeed::new(),
            ingest,
        }
    }
}

/// Build the Axum application: the ingest endpoint, the live-feed WebSocket, and
/// audio serving. This is the single seam the binary and tests share.
pub fn build_app(state: AppState) -> Router {
    Router::new()
        .route("/api/call-upload", post(ingest::call_upload))
        .route("/api/live", any(live::ws_handler))
        .route("/api/call/{id}/audio", get(serve_audio))
        .route("/healthz", get(healthz))
        // Everything else is the frontend: embedded SPA assets + client-side
        // routing (ADR-0007). The API/WS/health routes above take precedence.
        .fallback(web::spa_handler)
        .with_state(state)
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
    let (object_key, audio_mime) = match repo::get_call_audio(&state.db, id).await {
        Ok(Some(audio)) => audio,
        Ok(None) => return (StatusCode::NOT_FOUND, "call not found\n").into_response(),
        Err(err) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("could not look up call: {err}\n"),
            )
                .into_response();
        }
    };

    if state.audio.is_presigning() {
        match state.audio.presigned_get_url(&object_key).await {
            Some(Ok(url)) => return Redirect::temporary(&url).into_response(),
            Some(Err(err)) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("could not sign audio url: {err}\n"),
                )
                    .into_response();
            }
            None => {}
        }
    }

    let size = match state.audio.size(&object_key).await {
        Ok(Some(size)) => size,
        Ok(None) => return (StatusCode::NOT_FOUND, "audio not found\n").into_response(),
        Err(err) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("could not stat audio: {err}\n"),
            )
                .into_response();
        }
    };
    let mime = audio_mime.unwrap_or_else(|| "application/octet-stream".to_string());

    match parse_range_header(headers.get(header::RANGE), size) {
        RangeOutcome::None => match state.audio.get(&object_key).await {
            Ok(Some(bytes)) => (
                StatusCode::OK,
                [
                    (header::CONTENT_TYPE, mime),
                    (header::ACCEPT_RANGES, "bytes".to_string()),
                ],
                bytes,
            )
                .into_response(),
            Ok(None) => (StatusCode::NOT_FOUND, "audio not found\n").into_response(),
            Err(err) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("could not read audio: {err}\n"),
            )
                .into_response(),
        },
        RangeOutcome::Range { start, end } => {
            match state.audio.get_range(&object_key, start, end + 1).await {
                Ok(bytes) => (
                    StatusCode::PARTIAL_CONTENT,
                    [
                        (header::CONTENT_TYPE, mime),
                        (header::ACCEPT_RANGES, "bytes".to_string()),
                        (header::CONTENT_RANGE, format!("bytes {start}-{end}/{size}")),
                    ],
                    bytes,
                )
                    .into_response(),
                Err(err) => (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("could not read audio: {err}\n"),
                )
                    .into_response(),
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
    if spec.is_empty() || spec.contains(',') {
        return RangeOutcome::Unsatisfiable;
    }
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
