//! Radio-Scout binary entrypoint.
//!
//! Zero-config first run — create the base dir, open the SQLite DB (WAL) and the
//! filesystem blob store, make sure there is an ingest API key, and serve, with
//! the retention sweeper (#10) running in the background. Ticket #17 adds the
//! real TOML/CLI config (including the S3/Garage backend, Postgres, the
//! `[retention]` section and a `[log]` section); #19 adds admin key management.
//!
//! Everything here is bootstrap glue: the decisions worth testing live in the
//! library (`startup`, `observability`, `retention`), and this file wires them
//! together in the order a boot needs them.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use radio_scout::db;
use radio_scout::retention::{self, RetentionConfig};
use radio_scout::startup::{self, INGEST_KEY_VAR};
use radio_scout::{AppState, BlobStore, IngestConfig, build_app, now_ms, observability};
use tracing::{debug, error, info};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // `.env` alongside the binary's working directory, if there is one. A real
    // environment variable always wins over the file. This is a development
    // convenience (see `.env.example`) and the same pre-#17 stopgap as the env
    // vars below — #17 replaces the lot with TOML + CLI flags.
    //
    // Loaded before logging is initialised so `RUST_LOG` can live there too.
    let env_file = dotenvy::dotenv();

    observability::init();
    match &env_file {
        Ok(path) => debug!(env_file = %path.display(), "loaded env file"),
        Err(error) => debug!(%error, "no env file loaded"),
    }

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
    // configured, first run generates one and writes it to the env file it would
    // have been read from; it is never logged (ADR-0011 rule 2).
    //
    // With no env file to have read it from, it goes beside the database rather
    // than into the working directory: under systemd or Docker (#23) the cwd is
    // routinely `/` or read-only, and a write that fails there would leave a
    // scanner with no usable ingest key at all. `base_dir` was just created, so
    // it is known to be writable.
    let env_file = env_file.unwrap_or_else(|_| base_dir.join(".env"));
    let configured = std::env::var(INGEST_KEY_VAR).ok();
    startup::log_ingest_key(
        &startup::provision_ingest_key(&db, configured.as_deref(), &env_file, now_ms()).await?,
    );

    let audio = Arc::new(BlobStore::filesystem(base_dir.join("audio"))?);

    // Retention (#10): bound the archive so the disk can't fill. Sweeps once
    // now and then on its interval, for the life of the process.
    // Env vars are the pre-#17 stopgap, same pattern as base_dir/port above.
    let retention = RetentionConfig::from_env_vars(|key| std::env::var(key).ok());
    retention.log();
    retention::spawn(db.clone(), audio.clone(), retention);

    let app = build_app(AppState::new(audio, db, IngestConfig::default()));

    // A port already in use is the most common way a boot fails; `?` alone would
    // answer with a Debug-printed `Os { code: 48, .. }` and no mention of the
    // port (ADR-0011 rule 4: an operator must be told what to act on).
    let listener = tokio::net::TcpListener::bind(("0.0.0.0", port))
        .await
        .inspect_err(|error| error!(port, %error, "could not bind the listening port"))?;
    let addr = listener.local_addr()?;
    info!(
        %addr,
        port,
        base_dir = %base_dir.display(),
        "radio-scout listening"
    );
    // With connect info: the request log (#28) names the host an ingest came
    // from, which is the diagnostic that matters when a recorder says it is
    // uploading and the archive disagrees.
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await?;
    Ok(())
}
