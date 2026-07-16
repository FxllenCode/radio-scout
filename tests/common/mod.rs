//! Shared integration-test harness. Included via `mod common;` from the
//! individual `tests/*.rs` binaries (each is its own crate, so the module is
//! recompiled per binary — `dead_code` is allowed for helpers a given binary
//! doesn't use).
#![allow(dead_code)]

use std::sync::Arc;

use radio_scout::db::{self};
use radio_scout::{AppState, BlobStore, IngestConfig, build_app};
use sea_orm::DatabaseConnection;

/// Bring up a DB-backed app on an ephemeral port over a fresh SQLite DB + blob
/// store in a TempDir. Returns its `host:port`, a DB handle (to seed keys /
/// assert rows), and the TempDir (drop it last — it owns the DB + audio files).
pub async fn spawn() -> (String, DatabaseConnection, tempfile::TempDir) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let audio = Arc::new(BlobStore::filesystem(tmp.path().join("audio")).expect("blob"));
    let url = format!("sqlite://{}?mode=rwc", tmp.path().join("t.db").display());
    let dbc = db::connect(&url).await.expect("db");
    let app = build_app(AppState::new(audio, dbc.clone(), IngestConfig::default()));

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serve");
    });
    (format!("127.0.0.1:{}", addr.port()), dbc, tmp)
}
