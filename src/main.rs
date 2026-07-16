//! Radio-Scout binary entrypoint.
//!
//! Zero-config first run — create the base dir, wire up the filesystem blob
//! store and in-memory call repository, and serve. Ticket #17 adds the real
//! TOML/CLI config (including the S3/Garage storage backend) and #5 swaps in
//! the SeaORM-backed repository.

use std::path::PathBuf;
use std::sync::Arc;

use radio_scout::{AppState, BlobStore, InMemoryCallRepository, build_app};

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let base_dir: PathBuf = std::env::var_os("RADIO_SCOUT_BASE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("./radio-scout-data"));
    let port: u16 = std::env::var("RADIO_SCOUT_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(3000);

    let audio = Arc::new(
        BlobStore::filesystem(base_dir.join("audio"))
            .map_err(|e| std::io::Error::other(e.to_string()))?,
    );
    let calls = Arc::new(InMemoryCallRepository::new());
    let app = build_app(AppState::new(audio, calls));

    let listener = tokio::net::TcpListener::bind(("0.0.0.0", port)).await?;
    println!(
        "Radio-Scout listening on http://{} (base_dir: {})",
        listener.local_addr()?,
        base_dir.display()
    );
    axum::serve(listener, app).await
}
