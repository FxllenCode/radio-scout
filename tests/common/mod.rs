//! Shared integration-test harness. Included via `mod common;` from the
//! individual `tests/*.rs` binaries (each is its own crate, so the module is
//! recompiled per binary — `dead_code` is allowed for helpers a given binary
//! doesn't use).
#![allow(dead_code)]

pub mod logs;

use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use futures_util::StreamExt;
use radio_scout::db::{self};
use radio_scout::live::LiveFeed;
use radio_scout::{AppState, BlobStore, IngestConfig, build_app};
use sea_orm::DatabaseConnection;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

/// Bring up a DB-backed app on an ephemeral port over a fresh SQLite DB +
/// filesystem blob store in a TempDir. Returns its `host:port`, a DB handle (to
/// seed keys / assert rows), and the TempDir (drop it last — it owns the DB +
/// audio files).
pub async fn spawn() -> (String, DatabaseConnection, tempfile::TempDir) {
    spawn_with_ingest(IngestConfig::default()).await
}

/// Like [`spawn`] but with a custom [`IngestConfig`] — e.g. `auto_populate: false`
/// to exercise the auto-populate toggle (#8).
pub async fn spawn_with_ingest(
    ingest: IngestConfig,
) -> (String, DatabaseConnection, tempfile::TempDir) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let audio = BlobStore::filesystem(tmp.path().join("audio")).expect("blob");
    let (addr, dbc) = spawn_with_config(audio, tmp.path(), ingest).await;
    (addr, dbc, tmp)
}

/// Like [`spawn`] but with a short live-feed heartbeat period, so heartbeat /
/// dead-connection-reaping behavior (#9) is observable without waiting the
/// production 30 s.
pub async fn spawn_with_heartbeat(
    heartbeat: Duration,
) -> (String, DatabaseConnection, tempfile::TempDir) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let audio = Arc::new(BlobStore::filesystem(tmp.path().join("audio")).expect("blob"));
    let dbc = connect_db(tmp.path()).await;
    let mut state = AppState::new(audio, dbc.clone(), IngestConfig::default());
    state.live = LiveFeed::with_heartbeat(heartbeat);
    let addr = serve(build_app(state)).await;
    (addr, dbc, tmp)
}

/// Like [`spawn`] but also hands back the blob store the app writes audio to, so
/// a test can drive retention (#10) over the same store ingest used and assert
/// that objects — not just rows — went away.
pub async fn spawn_with_store() -> (
    String,
    DatabaseConnection,
    Arc<BlobStore>,
    tempfile::TempDir,
) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let audio = Arc::new(BlobStore::filesystem(tmp.path().join("audio")).expect("blob"));
    let dbc = connect_db(tmp.path()).await;
    let app = build_app(AppState::new(
        audio.clone(),
        dbc.clone(),
        IngestConfig::default(),
    ));
    (serve(app).await, dbc, audio, tmp)
}

/// Bring up the app with a specific blob backend (e.g. an S3-backed store to
/// exercise the presigned-redirect serve path); the SQLite DB lives under `dir`.
/// The caller owns `dir` and must keep it alive for the app's lifetime.
pub async fn spawn_with_blob(audio: BlobStore, dir: &Path) -> (String, DatabaseConnection) {
    spawn_with_config(audio, dir, IngestConfig::default()).await
}

/// Shared bring-up: a fresh SQLite DB under `dir` + the given blob store and
/// ingest config, served on an ephemeral port.
pub async fn spawn_with_config(
    audio: BlobStore,
    dir: &Path,
    ingest: IngestConfig,
) -> (String, DatabaseConnection) {
    let audio = Arc::new(audio);
    let dbc = connect_db(dir).await;
    let app = build_app(AppState::new(audio, dbc.clone(), ingest));
    (serve(app).await, dbc)
}

/// Open a fresh SQLite DB (create-if-missing) under `dir`.
async fn connect_db(dir: &Path) -> DatabaseConnection {
    let url = format!("sqlite://{}?mode=rwc", dir.join("t.db").display());
    db::connect(&url).await.expect("db")
}

/// Serve `app` on an ephemeral loopback port, returning its `host:port`.
///
/// With connect info, exactly as the binary serves it: the request log (#28)
/// reads the peer address from there, so a harness without it would make the
/// address field untestable.
async fn serve(app: Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .expect("serve");
    });
    format!("127.0.0.1:{}", addr.port())
}

/// `GET http://<addr><path>`, following redirects. A test that needs to observe
/// a redirect itself (the S3 presign path) builds its own client.
pub async fn get(addr: &str, path: &str) -> reqwest::Response {
    reqwest::get(format!("http://{addr}{path}"))
        .await
        .expect("request")
}

/// A connected live-feed WebSocket client.
pub type Ws = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

/// Open a live-feed WebSocket to `/api/live` without reading anything — the raw
/// socket, `hello` greeting still queued. Use [`connect`] unless the test asserts
/// on the greeting itself.
pub async fn connect_raw(addr: &str) -> Ws {
    let (ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/api/live"))
        .await
        .expect("ws connect");
    ws
}

/// Open a live-feed WebSocket and return it together with the parsed `hello`
/// greeting the server sends on connect (#9).
pub async fn connect_and_hello(addr: &str) -> (Ws, serde_json::Value) {
    let mut ws = connect_raw(addr).await;
    let hello = serde_json::from_str(&next_text(&mut ws).await).expect("hello json");
    (ws, hello)
}

/// Open a live-feed WebSocket, consuming (and discarding) the `hello` greeting —
/// the common case, leaving the socket ready for subscribe/call frames.
pub async fn connect(addr: &str) -> Ws {
    connect_and_hello(addr).await.0
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
