//! Radio-Scout binary entrypoint.
//!
//! Skeleton: zero-config first run — create the base dir, wire up the
//! filesystem audio store + in-memory call repository, and serve. Ticket #17
//! adds the real TOML + CLI config and #3 swaps in the SeaORM-backed repository.

use std::path::PathBuf;
use std::sync::Arc;

use radio_scout::{AppState, FilesystemAudioStore, InMemoryCallRepository, build_app};

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let base_dir: PathBuf = std::env::var_os("RADIO_SCOUT_BASE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("./radio-scout-data"));
    let port: u16 = std::env::var("RADIO_SCOUT_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(3000);

    std::fs::create_dir_all(base_dir.join("audio"))?;

    let audio = Arc::new(FilesystemAudioStore::new(base_dir.join("audio")));
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
