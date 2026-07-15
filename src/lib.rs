//! Radio-Scout library crate.
//!
//! Ticket #1 (the walking skeleton): a real Call flows ingest -> object store ->
//! live-feed WebSocket -> audio served back over HTTP. `build_app` returns the
//! Axum router the binary serves and the integration harness drives in-process
//! over its real HTTP + WS boundary (ADR-0009).

pub mod call;
pub mod ingest;
pub mod live;
pub mod store;
pub mod web;

use std::sync::Arc;

use axum::Router;
use axum::extract::{Path, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get, post};

use crate::call::CallId;
use crate::live::LiveFeed;
use crate::store::{AudioStore, CallRepository};

// Re-exported so the binary and the integration harness can wire the app up
// without reaching into module paths.
pub use crate::store::{FilesystemAudioStore, InMemoryCallRepository};

/// Shared application state, cloned into every handler. All fields are cheap to
/// clone (Arc / channel handle).
#[derive(Clone)]
pub struct AppState {
    pub audio: Arc<dyn AudioStore>,
    pub calls: Arc<dyn CallRepository>,
    pub live: LiveFeed,
}

impl AppState {
    /// Assemble state from an audio store and a call repository, with a fresh
    /// live-feed hub.
    pub fn new(audio: Arc<dyn AudioStore>, calls: Arc<dyn CallRepository>) -> Self {
        AppState {
            audio,
            calls,
            live: LiveFeed::new(),
        }
    }
}

/// Build the Axum application: the ingest endpoint, the live-feed WebSocket, and
/// audio serving. This is the single seam the binary and tests share.
pub fn build_app(state: AppState) -> Router {
    Router::new()
        .route("/", get(web::index))
        .route("/api/call-upload", post(ingest::call_upload))
        .route("/api/live", any(live::ws_handler))
        .route("/api/call/{id}/audio", get(serve_audio))
        .route("/healthz", get(healthz))
        .with_state(state)
}

/// `GET /api/call/{id}/audio` — stream a stored call's audio.
///
/// Skeleton: whole-body response. Ticket #4 adds HTTP range support and, for the
/// S3 backend, short-lived presigned redirects after an access-scope check.
async fn serve_audio(State(state): State<AppState>, Path(id): Path<CallId>) -> Response {
    let Some(call) = state.calls.get(id) else {
        return (StatusCode::NOT_FOUND, "call not found\n").into_response();
    };

    match state.audio.get(&call.object_key) {
        Ok(Some(bytes)) => {
            let mime = call
                .audio_mime
                .unwrap_or_else(|| "application/octet-stream".to_string());
            (
                StatusCode::OK,
                [
                    (header::CONTENT_TYPE, mime),
                    (header::ACCEPT_RANGES, "bytes".to_string()),
                ],
                bytes,
            )
                .into_response()
        }
        Ok(None) => (StatusCode::NOT_FOUND, "audio not found\n").into_response(),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("could not read audio: {err}\n"),
        )
            .into_response(),
    }
}

/// `GET /healthz` — liveness probe.
async fn healthz() -> &'static str {
    "ok"
}
