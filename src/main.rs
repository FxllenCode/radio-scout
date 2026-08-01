//! Radio-Scout binary entrypoint.
//!
//! Resolve the configuration (#17: TOML + flags + environment), bring logging
//! up on it, and hand it to `radio_scout::instance` (#90), which owns every
//! step of assembling a running Instance. Zero-config is the path where none of
//! it is configured: a directory, a SQLite file and a folder of audio, all
//! created on first run (US 35).
//!
//! Everything here is bootstrap glue and is excluded from coverage, so what is
//! left is deliberately only what the library cannot answer: this process's
//! flags, its environment, its working directory, and where its own executable
//! is. What it *does* own is the order — configuration before logging (logging
//! is configured), and both before anything that can fail with something worth
//! logging.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use radio_scout::config::{self, Cli, Config};
use radio_scout::instance::{self, Credentials, Wiring};
use radio_scout::logsink;
use radio_scout::observability;
use radio_scout::service;
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
        observability::init(observability::DEFAULT_DIRECTIVES, None);
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
            observability::init(observability::DEFAULT_DIRECTIVES, None);
            error!(%error, "invalid configuration");
            return ExitCode::from(EXIT_MISCONFIGURED);
        }
    };

    // The operator log surface (#30). The sink is built *before* the subscriber
    // and drained *after* the database opens, because logging starts first: the
    // migration lines an operator most wants to read back are written before
    // anything could have stored them, and they wait in the queue until `serve`
    // hands the writer a database.
    let (sink, log_writer) = match logsink::channel(loaded.config.log.database_level) {
        Some((sink, writer)) => (Some(sink), Some(writer)),
        None => (None, None),
    };
    observability::init(&loaded.config.log.directives, sink);

    // `service …` (#23) is configured exactly like a boot — it bakes the
    // settings just resolved into the definition it writes — but it never
    // serves, so it answers before anything is opened.
    if let Some(config::Command::Service(command)) = cli.command.clone() {
        return service_command(&cli, command, &loaded, &cwd);
    }

    match &env_file {
        Ok(path) => debug!(env_file = %path.display(), "loaded env file"),
        Err(error) => debug!(%error, "no env file loaded"),
    }
    loaded.log_summary();

    match serve(loaded.config, env_file.ok(), log_writer).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            error!(%error, "radio-scout stopped");
            ExitCode::FAILURE
        }
    }
}

/// `radio-scout service …`: hand the resolved configuration to the plan that
/// will run it at boot (#23).
///
/// The one thing the library cannot answer is where this executable is; every
/// decision — which paths become absolute, which flags may be baked in, and
/// what the plan then does — is `config::service_params` and
/// `service::dispatch`, both of which the suite can see.
fn service_command(
    cli: &Cli,
    command: config::ServiceCommand,
    loaded: &config::Loaded,
    cwd: &std::path::Path,
) -> ExitCode {
    let exec = match std::env::current_exe() {
        Ok(exec) => exec,
        Err(error) => {
            error!(%error, "could not find this executable's own path");
            return ExitCode::FAILURE;
        }
    };
    let params = match config::service_params(cli, loaded, cwd, exec, command.user) {
        Ok(params) => params,
        Err(error) => {
            error!(%error, "these settings cannot be baked into a service");
            return ExitCode::from(EXIT_MISCONFIGURED);
        }
    };
    match service::dispatch(command.action, command.print, &params) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            error!(%error, "the service command did not finish");
            ExitCode::FAILURE
        }
    }
}

/// Bring the scanner up and serve until the process ends.
///
/// Everything about *how* an Instance is assembled lives in
/// `radio_scout::instance` (#90), so this is the one thing the library cannot
/// answer: what this process's environment says the credentials are, and where
/// its env file is. A subsystem wired here instead of there would be a
/// subsystem no test can reach — this file is excluded from coverage.
async fn serve(
    config: Config,
    env_file: Option<PathBuf>,
    log_writer: Option<logsink::LogWriter>,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut wiring = Wiring::default().credentials(Credentials::from_env(env_file, |name| {
        std::env::var(name).ok()
    }));
    if let Some(writer) = log_writer {
        wiring = wiring.log_writer(writer);
    }
    instance::start(config, wiring)
        .await?
        .serve_forever()
        .await?;
    Ok(())
}
