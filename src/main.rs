//! Radio-Scout binary entrypoint.
//!
//! Zero-config first run — create the base dir, open the SQLite DB (WAL) and the
//! filesystem blob store, generate a default ingest API key if none exists, and
//! serve, with the retention sweeper (#10) running in the background. Ticket
//! #17 adds the real TOML/CLI config (including the S3/Garage backend, Postgres,
//! and the `[retention]` section); #19 adds admin key management.

use std::path::PathBuf;
use std::sync::Arc;

use radio_scout::db::{self, repo};
use radio_scout::retention::{self, RetentionConfig};
use radio_scout::{AppState, BlobStore, IngestConfig, build_app, now_ms};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let base_dir: PathBuf = std::env::var_os("RADIO_SCOUT_BASE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("./radio-scout-data"));
    let port: u16 = std::env::var("RADIO_SCOUT_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(3000);

    std::fs::create_dir_all(&base_dir)?;

    let db_url = format!(
        "sqlite://{}?mode=rwc",
        base_dir.join("radio-scout.db").display()
    );
    let db = db::connect(&db_url).await?;

    // First run: no keys yet -> generate a default one scoped to all Systems and
    // print it so the operator can configure their recorder (ADR-0008).
    if repo::count_api_keys(&db).await? == 0 {
        let raw_key = uuid::Uuid::new_v4().simple().to_string();
        let now = now_ms();
        repo::create_api_key(&db, &raw_key, None, Some("default (first run)".into()), now).await?;
        println!("Generated default ingest API key: {raw_key}");
        println!("  Point your Trunk Recorder / SDRTrunk uploader at this server with that key.");
    }

    let audio = Arc::new(BlobStore::filesystem(base_dir.join("audio"))?);

    // Retention (#10): bound the archive so the disk can't fill. Sweeps once
    // now and then on its interval, for the life of the process.
    // Env vars are the pre-#17 stopgap, same pattern as base_dir/port above.
    let retention = RetentionConfig::from_env_vars(|key| std::env::var(key).ok());
    println!("Retention: {}", retention.describe());
    retention::spawn(db.clone(), audio.clone(), retention);

    let app = build_app(AppState::new(audio, db, IngestConfig::default()));

    let listener = tokio::net::TcpListener::bind(("0.0.0.0", port)).await?;
    println!(
        "Radio-Scout listening on http://{} (base_dir: {})",
        listener.local_addr()?,
        base_dir.display()
    );
    axum::serve(listener, app).await?;
    Ok(())
}
