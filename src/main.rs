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
    // `.env` alongside the binary's working directory, if there is one. A real
    // environment variable always wins over the file. This is a development
    // convenience (see `.env.example`) and the same pre-#17 stopgap as the env
    // vars below — #17 replaces the lot with TOML + CLI flags.
    let _ = dotenvy::dotenv();

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

    // The ingest key (ADR-0008). A configured one — `RADIO_SCOUT_API_KEY`, from
    // the environment or `.env` — is registered on every boot, so a recorder's
    // key keeps working across restarts and across a wiped database. With none
    // configured, first run generates one and prints it.
    match std::env::var("RADIO_SCOUT_API_KEY") {
        Ok(configured) if !configured.trim().is_empty() => {
            let added = repo::ensure_api_key(&db, configured.trim(), None, now_ms()).await?;
            println!(
                "Ingest API key from RADIO_SCOUT_API_KEY: {}",
                if added { "registered" } else { "already known" }
            );
        }
        _ if repo::count_api_keys(&db).await? == 0 => {
            let raw_key = uuid::Uuid::new_v4().simple().to_string();
            let now = now_ms();
            repo::create_api_key(&db, &raw_key, None, Some("default (first run)".into()), now)
                .await?;
            println!("Generated default ingest API key: {raw_key}");
            println!(
                "  Point your Trunk Recorder / SDRTrunk uploader at this server with that key."
            );
            println!("  Set RADIO_SCOUT_API_KEY in .env to pin a key of your own instead.");
        }
        _ => {}
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
