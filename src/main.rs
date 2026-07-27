//! Radio-Scout binary entrypoint.
//!
//! Resolve the configuration (#17: TOML + flags + environment), then serve —
//! create the base dir, open the database and the audio store, make sure there
//! is an ingest API key, and run the retention sweeper (#10) in the background.
//! Zero-config is the path where none of that is configured: a directory, a
//! SQLite file and a folder of audio, all created on first run (US 35).
//!
//! Everything here is bootstrap glue: the decisions worth testing live in the
//! library (`config`, `startup`, `observability`, `retention`), and this file
//! wires them together in the order a boot needs them. What it *does* own is
//! that order — configuration before logging (logging is configured), and both
//! before anything that can fail with something worth logging.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use clap::Parser;
use radio_scout::admin::AdminAuth;
use radio_scout::config::{self, Cli, Config};
use radio_scout::db;
use radio_scout::push::Push;
use radio_scout::retention;
use radio_scout::startup::{self, INGEST_KEY_VAR};
use radio_scout::{AppState, BlobStore, build_app, now_ms, observability};
use tracing::{debug, error, info};

/// The configuration step failed: an unusable setting, or a config file that
/// could not be read — or, for `--write-config`, written. Distinct from a
/// failed *start*, so an init system can tell "fix your configuration" from
/// "the port was busy".
const EXIT_MISCONFIGURED: u8 = 2;

#[tokio::main]
async fn main() -> ExitCode {
    // Flags first: `--help` and `--version` must answer before anything can go
    // wrong, and `--write-config` needs no configuration of its own.
    let cli = Cli::parse();

    // `.env` alongside the working directory, if there is one. A real
    // environment variable always wins over the file. It is where the ingest
    // key lives (ADR-0008) and it may carry any `RADIO_SCOUT_*` setting or
    // `RUST_LOG`; loaded before configuration is resolved so both see the same
    // environment.
    let env_file = dotenvy::dotenv();

    if let Some(path) = &cli.write_config {
        observability::init(observability::DEFAULT_DIRECTIVES);
        return match config::write_template(path) {
            Ok(()) => {
                info!(path = %path.display(), "wrote a configuration file with every setting at its default");
                ExitCode::SUCCESS
            }
            Err(error) => {
                error!(%error, "could not write the configuration file");
                ExitCode::from(EXIT_MISCONFIGURED)
            }
        };
    }

    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let loaded = match config::load(&cli, |name| std::env::var(name).ok(), &cwd) {
        Ok(loaded) => loaded,
        Err(error) => {
            // Nothing is configured yet, including logging — so bring it up on
            // the defaults rather than saying why we are stopping through a
            // channel ADR-0011 forbids.
            observability::init(observability::DEFAULT_DIRECTIVES);
            error!(%error, "invalid configuration");
            return ExitCode::from(EXIT_MISCONFIGURED);
        }
    };

    observability::init(&loaded.config.log.directives);
    match &env_file {
        Ok(path) => debug!(env_file = %path.display(), "loaded env file"),
        Err(error) => debug!(%error, "no env file loaded"),
    }
    loaded.log_summary();

    match serve(loaded.config, env_file.ok()).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            error!(%error, "radio-scout stopped");
            ExitCode::FAILURE
        }
    }
}

/// Bring the scanner up and serve until the process ends.
async fn serve(
    config: Config,
    env_file: Option<PathBuf>,
) -> Result<(), Box<dyn std::error::Error>> {
    let base_dir = config.server.base_dir.clone();
    std::fs::create_dir_all(&base_dir)?;

    let db = db::connect(&config.database_url()).await?;

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
    let env_file = env_file.unwrap_or_else(|| base_dir.join(".env"));
    let configured = std::env::var(INGEST_KEY_VAR).ok();
    startup::log_ingest_key(
        &startup::provision_ingest_key(&db, configured.as_deref(), &env_file, now_ms()).await?,
    );

    // The admin password (#19, ADR-0008), from the same file for the same
    // reason: first run *writes* it. With none configured a random one is
    // generated — Radio-Scout never ships a guessable default the way rdio does
    // — and if it cannot be saved, no password is set and the admin surface
    // stays shut rather than opening on a credential only the server ever saw.
    let admin = startup::provision_admin_password(
        std::env::var(startup::ADMIN_PASSWORD_VAR).ok().as_deref(),
        &env_file,
    );
    startup::log_admin_password(&admin);

    // The Web Push identity (#16, ADR-0005), from the same file for the same
    // reason again: first run writes it. Unlike the other two it must be the
    // *same* key next boot — a browser pins the public half at subscribe time —
    // so a key that cannot be saved leaves notifications off rather than
    // running on one that will not survive a restart.
    let vapid = startup::provision_vapid_key(
        std::env::var(startup::VAPID_KEY_VAR).ok().as_deref(),
        &env_file,
    );
    startup::log_vapid_key(&vapid);
    let push = match vapid.key() {
        Some(key) => Push::new(key, config.push()),
        None => Push::disabled(),
    };

    let audio = Arc::new(BlobStore::open(&config.storage())?);

    // Retention (#10): bound the archive so the disk can't fill. Sweeps once
    // now and then on its interval, for the life of the process.
    let retention = config.retention();
    retention.log();
    retention::spawn(db.clone(), audio.clone(), retention);

    let mut state = AppState::new(audio, db, config.ingest());
    state.trusted_proxies = config.trusted_proxies();
    state.admin = AdminAuth::provisioned(&admin, config.admin());
    state.push = push;
    // Notifications ride the live-feed fanout (#16), so an ingest never waits
    // on a push service. A server with no identity spawns nothing.
    radio_scout::push::spawn(state.clone());
    let app = build_app(state);

    let port = config.server.port;
    // A port already in use is the most common way a boot fails; `?` alone would
    // answer with a Debug-printed `Os { code: 48, .. }` and no mention of the
    // port (ADR-0011 rule 4: an operator must be told what to act on).
    let listener = tokio::net::TcpListener::bind(("0.0.0.0", port))
        .await
        .inspect_err(|error| error!(port, %error, "could not bind the listening port"))?;
    let addr = listener.local_addr()?;
    // `port` from the socket, not the setting: `--port 0` asks the OS to pick
    // one, and the number a client has to connect to is the only useful thing
    // to say about it.
    info!(
        %addr,
        port = addr.port(),
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
