//! Shared integration-test harness. Included via `mod common;` from the
//! individual `tests/*.rs` binaries (each is its own crate, so the module is
//! recompiled per binary — `dead_code` is allowed for helpers a given binary
//! doesn't use).
#![allow(dead_code)]

use std::path::Path;
use std::sync::Arc;

use futures_util::StreamExt;
use radio_scout::db::{self};
use radio_scout::{AppState, BlobStore, IngestConfig, build_app};
use sea_orm::DatabaseConnection;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

/// Bring up a DB-backed app on an ephemeral port over a fresh SQLite DB +
/// filesystem blob store in a TempDir. Returns its `host:port`, a DB handle (to
/// seed keys / assert rows), and the TempDir (drop it last — it owns the DB +
/// audio files).
pub async fn spawn() -> (String, DatabaseConnection, tempfile::TempDir) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let audio = BlobStore::filesystem(tmp.path().join("audio")).expect("blob");
    let (addr, dbc) = spawn_with_blob(audio, tmp.path()).await;
    (addr, dbc, tmp)
}

/// Bring up the app with a specific blob backend (e.g. an S3-backed store to
/// exercise the presigned-redirect serve path); the SQLite DB lives under `dir`.
/// The caller owns `dir` and must keep it alive for the app's lifetime.
pub async fn spawn_with_blob(audio: BlobStore, dir: &Path) -> (String, DatabaseConnection) {
    let audio = Arc::new(audio);
    let url = format!("sqlite://{}?mode=rwc", dir.join("t.db").display());
    let dbc = db::connect(&url).await.expect("db");
    let app = build_app(AppState::new(audio, dbc.clone(), IngestConfig::default()));

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serve");
    });
    (format!("127.0.0.1:{}", addr.port()), dbc)
}

/// A connected live-feed WebSocket client.
pub type Ws = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

/// Open a live-feed WebSocket to `/api/live`.
pub async fn connect(addr: &str) -> Ws {
    let (ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/api/live"))
        .await
        .expect("ws connect");
    ws
}

/// Read the next text frame, skipping control frames; panics if the socket
/// closes first.
pub async fn next_text(ws: &mut Ws) -> String {
    loop {
        match ws.next().await {
            Some(Ok(WsMessage::Text(t))) => return t.as_str().to_owned(),
            Some(Ok(WsMessage::Ping(_) | WsMessage::Pong(_))) => continue,
            Some(Ok(WsMessage::Close(_))) | None => panic!("ws closed before a text frame"),
            Some(Ok(_)) => continue,
            Some(Err(e)) => panic!("ws error: {e}"),
        }
    }
}
