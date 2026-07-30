//! Configuration (#17, spec US 35-36, ADR-0012): one [`Config`], resolved
//! once at boot from a TOML file, the environment and the command line.
//!
//! Two things have to be true at once. A scanner nobody has configured must
//! come up — a directory, a SQLite file, a folder of audio, all created on
//! first run (US 35) — and everything must be settable without a UI (US 36).
//! So [`resolve`] layers four sources, **loudest first: CLI flag →
//! environment variable → file → default**.
//!
//! rdio-scanner is the thing to beat here, and `server/config.go` gets four
//! things wrong that this module deliberately does not:
//!
//! - **The file silently overrides the flags.** `flag.Parse()` runs first and
//!   the INI is then loaded over the top (`config.go:96-137`), so `-listen`
//!   cannot override a configured `listen`. Ours is the other way round.
//! - **A typo does nothing.** An unknown key is ignored, a bad `db_port` falls
//!   back to a default, and a config file that fails to load is skipped without
//!   a word. Here every layer is validated and a configuration that cannot be
//!   run **refuses to boot**, naming the source, the value and what was
//!   expected — because a typo'd `retention.dayz` that keeps the default is how
//!   an operator loses a month of Calls.
//! - **Most settings aren't in the file at all** — retention, dedup and
//!   auto-populate live in rdio's *database*, behind its admin UI, so a
//!   headless install cannot be configured and nothing is version-controllable.
//! - **No environment support**, which is how a container is configured.
//!
//! Two rules shape the types rather than the code. Secrets never reach a log
//! line (ADR-0011 rule 2): [`S3`]'s `Debug` redacts the key, [`Loaded::log_summary`]
//! names the database *dialect* rather than its URL, and [`ConfigError::parse`]
//! drops the source snippet `toml` would otherwise quote. And an unusable value
//! is rejected by the type that owns it — [`ProxyNet`] parses in its
//! `Deserialize`, so a bad entry fails wherever it was written rather than
//! wherever someone remembered to check.

use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use clap::{Parser, Subcommand};
use ipnet::IpNet;
use serde::{Deserialize, Serialize};

use crate::admin::AdminConfig;
use crate::blob::{S3Config, StorageConfig};
use crate::enhance::{EnhancementConfig, Mode, Output};
use crate::ingest::IngestConfig;
use crate::logsink::LogSinkConfig;
use crate::observability;
use crate::push::PushConfig;
use crate::retention::RetentionConfig;

/// Radio-Scout's command line. Every flag here overrides the same setting from
/// the environment, the file, and the default, in that order.
///
/// Deliberately *not* exhaustive: the S3 credentials in `[storage.s3]` have no
/// flags, because a command line is world-readable in `ps` output and a secret
/// belongs in a `0600` file or the environment (ADR-0011 rule 2).
#[derive(Debug, Clone, Parser)]
#[command(name = "radio-scout", version, about = "Scanner audio from Trunk Recorder and SDRTrunk", long_about = None)]
pub struct Cli {
    /// What to do. Nothing means serve.
    #[command(subcommand)]
    pub command: Option<Command>,

    /// HTTP port for the API, the SPA and the live feed.
    #[arg(long, value_name = "PORT", global = true)]
    pub port: Option<u16>,

    /// Configuration file to read. Unset looks for `radio-scout.toml` in the
    /// working directory, and runs on the defaults if there isn't one.
    #[arg(long, value_name = "PATH", global = true)]
    pub config: Option<PathBuf>,

    /// Directory holding the database and, by default, the audio.
    #[arg(long, value_name = "DIR", global = true)]
    pub base_dir: Option<PathBuf>,

    /// Database connection URL. Unset means SQLite under the base directory.
    #[arg(long, value_name = "URL", global = true)]
    pub database_url: Option<String>,

    /// Where Call audio is stored.
    #[arg(long, value_name = "BACKEND", global = true)]
    pub storage_backend: Option<Backend>,

    /// Prune Calls older than this many days. 0 keeps them forever.
    #[arg(long, value_name = "DAYS", global = true)]
    pub retention_days: Option<u32>,

    /// Cap total stored audio at this many gigabytes.
    #[arg(long, value_name = "GB", global = true)]
    pub retention_max_size_gb: Option<f64>,

    /// Log filter directives, e.g. `debug` or `warn,radio_scout::ingest=trace`.
    #[arg(long, value_name = "DIRECTIVES", global = true)]
    pub log: Option<String>,

    /// An address or CIDR block whose `X-Forwarded-For` may be believed.
    /// Repeat, or separate with commas, for several.
    #[arg(
        long = "trusted-proxy",
        value_name = "ADDR",
        value_delimiter = ',',
        global = true
    )]
    pub trusted_proxies: Option<Vec<ProxyNet>>,

    /// Write a configuration file with every setting at its default, then exit.
    /// Defaults to `radio-scout.toml`; never overwrites an existing file.
    #[arg(long, value_name = "PATH", num_args = 0..=1, default_missing_value = CONFIG_FILE_NAME)]
    pub write_config: Option<PathBuf>,
}

/// The subcommands. Every setting flag above is `global`, so it reads the same
/// before or after one — `radio-scout service install --port 8080` is the
/// spelling an operator reaches for, and it is the one that gets baked in.
#[derive(Debug, Clone, Subcommand)]
pub enum Command {
    /// Run the scanner at boot: install, remove or control the OS service.
    Service(ServiceCommand),
}

/// `radio-scout service …` (#23, spec US 42).
#[derive(Debug, Clone, clap::Args)]
pub struct ServiceCommand {
    #[command(subcommand)]
    pub action: crate::service::Action,

    /// Show what would be written and run; change nothing.
    #[arg(long, global = true)]
    pub print: bool,

    /// The account the service runs as. Not supported on Windows, where a
    /// scheduled task runs as the system account.
    #[arg(long, value_name = "NAME", global = true)]
    pub user: Option<String>,
}

/// Everything `radio-scout service …` needs about this configuration (#23).
///
/// This is the same translation every other section gets — [`Config::ingest`],
/// [`Config::retention`], [`Config::push`] — for the service module, and it
/// lives here for the same reason: `main.rs` is excluded from coverage, so a
/// decision made there is a decision nothing tests. There are three, and each
/// one is a bug if it goes the other way. The base directory and the
/// configuration file are made **absolute**, because a service's working
/// directory is the service manager's rather than the operator's shell's — a
/// relative `radio-scout-data` in a unit file resolves against `/`, and a
/// `radio-scout.toml` discovered beside the operator would never be found
/// again. And the port comes from the *resolved* configuration rather than the
/// flag, because it decides whether the unit is granted the capability to bind
/// a privileged one.
pub fn service_params(
    cli: &Cli,
    loaded: &Loaded,
    cwd: &Path,
    exec: PathBuf,
    user: Option<String>,
) -> Result<crate::service::Params, ConfigError> {
    let base_dir = crate::service::absolute(&loaded.config.server.base_dir, cwd);
    let config_file = loaded
        .file
        .as_ref()
        .map(|path| crate::service::absolute(path, cwd));
    Ok(crate::service::Params {
        exec,
        args: service_args(cli, &base_dir, config_file.as_deref())?,
        base_dir,
        port: loaded.config.server.port,
        user,
    })
}

/// The command line a service should run: the flags this one was given, plus
/// the two things a service cannot inherit — an absolute base directory (its
/// working directory is not the operator's) and the configuration file that was
/// actually read (discovery looks in the *working* directory, which under
/// systemd or launchd is somewhere else entirely).
///
/// `--database-url` is refused rather than copied. It routinely carries a
/// password, and a unit file is world-readable — ADR-0011 rule 2 keeps a
/// credential out of a log line for the same reason it belongs out of this. The
/// operator has two places to put it that a service reads, and the message says
/// so.
pub fn service_args(
    cli: &Cli,
    base_dir: &Path,
    config_file: Option<&Path>,
) -> Result<Vec<String>, ConfigError> {
    if cli.database_url.is_some() {
        return Err(ConfigError::NotForService {
            flag: "--database-url",
            because: "it may carry a password and a service definition is world-readable",
            instead: "[database] url in the configuration file, or RADIO_SCOUT_DATABASE_URL",
        });
    }

    let mut args = Vec::new();
    let mut push = |flag: &str, value: String| {
        args.push(flag.to_string());
        args.push(value);
    };
    if let Some(path) = config_file {
        push("--config", path.display().to_string());
    }
    push("--base-dir", base_dir.display().to_string());
    if let Some(port) = cli.port {
        push("--port", port.to_string());
    }
    if let Some(backend) = cli.storage_backend {
        push("--storage-backend", backend.to_string());
    }
    if let Some(days) = cli.retention_days {
        push("--retention-days", days.to_string());
    }
    if let Some(gb) = cli.retention_max_size_gb {
        push("--retention-max-size-gb", gb.to_string());
    }
    if let Some(directives) = &cli.log {
        push("--log", directives.clone());
    }
    for proxy in cli.trusted_proxies.iter().flatten() {
        push("--trusted-proxy", proxy.to_string());
    }
    Ok(args)
}

/// Where a config file came from and what was in it, so an error can name the
/// file the operator has to edit.
#[derive(Debug, Clone)]
pub struct ConfigFile {
    pub path: PathBuf,
    pub text: String,
}

impl ConfigFile {
    pub fn new(path: impl Into<PathBuf>, text: impl Into<String>) -> Self {
        ConfigFile {
            path: path.into(),
            text: text.into(),
        }
    }
}

/// Why a boot could not be configured.
#[derive(Debug)]
pub enum ConfigError {
    /// A config file could not be read — or, for `--write-config`, written.
    File {
        path: PathBuf,
        source: std::io::Error,
    },
    /// The file did not parse, or carried a key we do not know.
    ///
    /// Deliberately *not* the `toml::de::Error`: its `Display` quotes the
    /// offending source line verbatim, so a typo'd key inside `[storage.s3]`
    /// would put the secret on an ERROR line the operator then pastes into an
    /// issue (ADR-0011 rule 2 — any level, any form). This carries the position
    /// and a message built by [`ConfigError::parse`], which never echoes the
    /// file.
    Parse {
        path: PathBuf,
        line: usize,
        column: usize,
        message: String,
    },
    /// A setting was present but unusable — `RADIO_SCOUT_PORT=banana`.
    ///
    /// `source` names where the operator wrote it (an environment variable, a
    /// TOML key), so the message points at the thing they have to edit.
    Invalid {
        source: String,
        value: String,
        expected: &'static str,
    },
    /// A setting another setting requires was left empty — S3 credentials for
    /// `storage.backend = "s3"`.
    Missing {
        key: &'static str,
        because: &'static str,
    },
    /// A flag that cannot be baked into a service definition (#23) was given to
    /// `service install`. Names the flag and where the setting belongs instead
    /// — never the value, which is why this exists.
    NotForService {
        flag: &'static str,
        because: &'static str,
        instead: &'static str,
    },
}

impl ConfigError {
    /// A TOML error, reduced to what is safe to say: where it was, and what
    /// serde called it.
    ///
    /// Two things are dropped. The **source snippet** always — it is a verbatim
    /// copy of the operator's line, and the line that fails to parse is exactly
    /// the line they mistyped, which for `[storage.s3]` is a credential. And
    /// serde's own message when the failing line assigns one of
    /// [`SECRET_KEYS`], because a type mismatch renders as
    /// ``invalid type: integer `12345` `` — the value itself.
    fn parse(path: &std::path::Path, text: &str, source: toml::de::Error) -> Self {
        let offset = source.span().map(|span| span.start).unwrap_or(0);
        let (line, column) = line_and_column(text, offset);
        let assigns_a_secret = text
            .lines()
            .nth(line.saturating_sub(1))
            .is_some_and(assigns_a_secret);
        let message = match assigns_a_secret {
            true => "invalid value for a credential (the value is not shown)".to_string(),
            false => source.message().to_string(),
        };
        ConfigError::Parse {
            path: path.to_path_buf(),
            line,
            column,
            message,
        }
    }

    /// An unusable value of an environment variable.
    fn invalid_env(var: &str, value: &str, expected: &'static str) -> Self {
        ConfigError::Invalid {
            source: var.to_string(),
            value: value.to_string(),
            expected,
        }
    }

    /// An unusable value of a configuration key — one that parsed as the right
    /// *type* but cannot be run (a zero batch size, a negative cap).
    fn invalid_key(key: &'static str, value: &str, expected: &'static str) -> Self {
        ConfigError::Invalid {
            source: key.to_string(),
            value: value.to_string(),
            expected,
        }
    }
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConfigError::File { path, source } => {
                write!(f, "{}: {source}", path.display())
            }
            ConfigError::Parse {
                path,
                line,
                column,
                message,
            } => {
                write!(f, "{}:{line}:{column}: {message}", path.display())
            }
            ConfigError::Invalid {
                source,
                value,
                expected,
            } => write!(f, "{source}: invalid value {value:?} (expected {expected})"),
            ConfigError::Missing { key, because } => {
                write!(f, "{key} must be set when {because}")
            }
            ConfigError::NotForService {
                flag,
                because,
                instead,
            } => write!(
                f,
                "{flag} cannot be baked into a service because {because}; set it with {instead} and install again"
            ),
        }
    }
}

impl std::error::Error for ConfigError {}

/// The whole of Radio-Scout's configuration.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub server: Server,
    pub database: Database,
    pub storage: Storage,
    pub retention: Retention,
    pub ingest: Ingest,
    pub admin: Admin,
    pub push: Push,
    pub enhancement: Enhancement,
    pub log: Log,
}

impl Config {
    /// The database to open. Zero-config is a SQLite file under `base_dir`,
    /// created on demand (ADR-0003); anything else is the operator's URL,
    /// passed to SeaORM as written.
    pub fn database_url(&self) -> String {
        match &self.database.url {
            Some(url) => url.clone(),
            None => format!(
                "sqlite://{}?mode=rwc",
                self.server.base_dir.join(DEFAULT_DB_FILE).display()
            ),
        }
    }

    /// The database's dialect — the part of [`Config::database_url`] that is
    /// safe to say out loud. A Postgres URL carries a password, so the URL
    /// itself is never logged (ADR-0011 rule 2).
    fn database_dialect(&self) -> String {
        let url = self.database_url();
        // Everything before the scheme separator — and before a `+ssl`-style
        // flavour, so the family is what gets said. A URL with neither is its
        // own dialect (`sqlite:file.db`).
        url.split_once("://")
            .map(|(scheme, _)| scheme)
            .unwrap_or(&url)
            .chars()
            .take_while(|character| !matches!(character, '+' | ':'))
            .collect()
    }

    /// The audio store to open (ADR-0002). Zero-config is a directory under
    /// `base_dir`; `storage.path` moves it without moving the database.
    pub fn storage(&self) -> StorageConfig {
        match self.storage.backend {
            Backend::Filesystem => StorageConfig::Filesystem {
                root: match &self.storage.path {
                    Some(path) => path.clone(),
                    None => self.server.base_dir.join(DEFAULT_AUDIO_DIR),
                },
            },
            Backend::S3 => StorageConfig::S3(S3Config {
                bucket: self.storage.s3.bucket.clone(),
                region: self.storage.s3.region.clone(),
                endpoint: self.storage.s3.endpoint.clone(),
                access_key_id: self.storage.s3.access_key_id.clone(),
                secret_access_key: self.storage.s3.secret_access_key.clone(),
                allow_http: self.storage.s3.allow_http,
            }),
        }
    }

    /// The retention policy the sweeper runs (#10, US 41), in the units the
    /// sweeper works in.
    pub fn retention(&self) -> RetentionConfig {
        let config = RetentionConfig {
            days: self.retention.days,
            max_size_bytes: None,
            log_days: self.retention.log_days,
            interval: Duration::from_secs(self.retention.interval_secs),
            batch_size: self.retention.batch_size,
            orphan_grace: Duration::from_secs(self.retention.orphan_grace_secs),
        };
        match self.retention.max_size_gb {
            Some(gb) => config.with_max_size_gb(gb),
            None => config,
        }
    }

    /// The operator log surface's sink (#30), or a sink that is off.
    ///
    /// The level has already been validated ([`Config::validate`]), so an
    /// unparseable one here can only mean a `Config` assembled in code rather
    /// than resolved — which reads as "off", the setting that stores nothing.
    pub fn log_sink(&self) -> LogSinkConfig {
        LogSinkConfig {
            level: LogSinkConfig::level_from_str(&self.log.database_level).unwrap_or(None),
            ..LogSinkConfig::default()
        }
    }

    /// The proxies the request log may believe (ADR-0011 rule 5, #28).
    pub fn trusted_proxies(&self) -> TrustedProxies {
        self.server.trusted_proxies.iter().copied().collect()
    }

    /// The ingest pipeline's tuning (#5, #8).
    pub fn ingest(&self) -> IngestConfig {
        IngestConfig {
            dedup_window_ms: self.ingest.dedup_window_ms,
            auto_populate: self.ingest.auto_populate,
        }
    }

    /// The admin surface's session and lockout policy (#19, ADR-0008).
    pub fn admin(&self) -> AdminConfig {
        AdminConfig {
            session_idle: Duration::from_secs(self.admin.session_idle_secs),
            session_max: Duration::from_secs(self.admin.session_max_secs),
            lockout_attempts: self.admin.lockout_attempts,
            lockout: Duration::from_secs(self.admin.lockout_secs),
        }
    }

    /// How Web Push behaves (#16, ADR-0005). The identity it signs with is not
    /// here: like the ingest key and the admin password, first run *writes* it
    /// (`RADIO_SCOUT_VAPID_PRIVATE_KEY`).
    pub fn push(&self) -> PushConfig {
        PushConfig {
            coalesce: Duration::from_secs(self.push.coalesce_secs),
            ttl: Duration::from_secs(self.push.ttl_secs),
            subject: self.push.subject.clone(),
        }
    }

    /// How audio enhancement behaves (#20, ADR-0006 as amended). *Scope* — the
    /// Systems and Talkgroups it applies to — is not here: it is a column on
    /// those rows, following the auto-populate precedent (#8).
    pub fn enhancement(&self) -> EnhancementConfig {
        EnhancementConfig {
            mode: self.enhancement.mode,
            output: self.enhancement.output,
            target_lufs: self.enhancement.target_lufs,
            queue_depth: self.enhancement.queue_depth,
        }
    }

    /// Refuse a configuration that parsed but cannot be run.
    ///
    /// Every check here is one an operator would otherwise meet as a runtime
    /// failure hours later — a 500 on the first upload rather than a message at
    /// the boot that misconfigured it.
    fn validate(&self) -> Result<(), ConfigError> {
        if self.storage.backend == Backend::S3 {
            for (key, value) in [
                ("storage.s3.bucket", &self.storage.s3.bucket),
                ("storage.s3.access_key_id", &self.storage.s3.access_key_id),
                (
                    "storage.s3.secret_access_key",
                    &self.storage.s3.secret_access_key,
                ),
            ] {
                if value.trim().is_empty() {
                    return Err(ConfigError::Missing {
                        key,
                        because: "storage.backend = \"s3\"",
                    });
                }
            }
        }
        // A cap must be a size. `days = 0` is rdio's "keep forever" and stays
        // legal, but a zero *cap* would mean "prune everything", which nobody
        // ever means — and `max_size_gb` absent already says "no cap".
        if let Some(gb) = self.retention.max_size_gb
            && !(gb.is_finite() && gb > 0.0)
        {
            return Err(ConfigError::invalid_key(
                "retention.max_size_gb",
                &gb.to_string(),
                "a positive number of gigabytes, or no key at all for no cap",
            ));
        }
        // Zero would page zero Calls at a time: a sweeper that runs forever and
        // deletes nothing, with a disk filling behind it.
        if self.retention.batch_size == 0 {
            return Err(ConfigError::invalid_key(
                "retention.batch_size",
                "0",
                "a positive number of Calls per batch",
            ));
        }
        // RFC 8292 §2.1: `sub` is a contact URI. A push service that refuses a
        // token over it fails *every* notification, silently, hours later.
        if !["mailto:", "https://"]
            .iter()
            .any(|scheme| self.push.subject.starts_with(scheme))
        {
            return Err(ConfigError::invalid_key(
                "push.subject",
                &self.push.subject,
                "a contact URI: \"mailto:you@example.com\" or \"https://example.com/contact\"",
            ));
        }
        // Zero admits nothing, so enhancement would be configured on and never
        // run — the `[admin]` zeros' shape. `mode = "off"` is how it is turned
        // off, so there is no second reading to guess at.
        if self.enhancement.queue_depth == 0 {
            return Err(ConfigError::invalid_key(
                "enhancement.queue_depth",
                "0",
                "a positive number of Calls — use mode = \"off\" to disable enhancement",
            ));
        }
        // LUFS is referenced to full scale, so a usable target is negative and
        // not arbitrarily so. The bounds are `loudnorm`'s own accepted range,
        // which makes an operator's ffmpeg knowledge transfer — and rules out
        // NaN, which would otherwise propagate silently through every gain.
        if !(LOUDNESS_RANGE_LUFS.contains(&self.enhancement.target_lufs)) {
            return Err(ConfigError::invalid_key(
                "enhancement.target_lufs",
                &self.enhancement.target_lufs.to_string(),
                "a loudness between -70 and -5 LUFS, e.g. -16",
            ));
        }
        // An output that parses but is not built. Accepting it and writing WAV
        // instead is rdio-scanner's failure mode — warn once, then silently do
        // something else forever (`server/ffmpeg.go:79-86`) — so this refuses,
        // and names the ticket that lands it rather than leaving an operator to
        // guess whether they mistyped something.
        if self.enhancement.output == Output::Opus {
            return Err(ConfigError::invalid_key(
                "enhancement.output",
                &self.enhancement.output.to_string(),
                "\"wav\" — \"opus\" needs libopus, which lands with #100",
            ));
        }
        validate_directives("log.directives", &self.log.directives)?;
        // Rule 5, enforced where an operator can be told about it: the sink has
        // no setting that reaches a level a listener's address may ride on.
        if LogSinkConfig::level_from_str(&self.log.database_level).is_none() {
            return Err(ConfigError::invalid_key(
                "log.database_level",
                &self.log.database_level,
                EXPECTED_DATABASE_LEVEL,
            ));
        }
        if self.ingest.dedup_window_ms < 0 {
            return Err(ConfigError::invalid_key(
                "ingest.dedup_window_ms",
                &self.ingest.dedup_window_ms.to_string(),
                "a duration in milliseconds, 0 to disable",
            ));
        }
        // Each of these bricks the admin surface at zero rather than merely
        // behaving oddly: a session that has already expired when it is issued,
        // or an address locked out before its first attempt. There is no
        // "0 disables it" reading to guess at — refusing to boot is the only
        // answer that doesn't lock an operator out of their own scanner.
        for (key, value, expected) in [
            (
                "admin.session_idle_secs",
                self.admin.session_idle_secs,
                "a positive number of seconds",
            ),
            (
                "admin.session_max_secs",
                self.admin.session_max_secs,
                "a positive number of seconds",
            ),
            (
                "admin.lockout_secs",
                self.admin.lockout_secs,
                "a positive number of seconds",
            ),
            (
                "admin.lockout_attempts",
                self.admin.lockout_attempts as u64,
                "a positive number of attempts",
            ),
        ] {
            if value == 0 {
                return Err(ConfigError::invalid_key(key, "0", expected));
            }
        }
        Ok(())
    }
}

/// The SQLite file zero-config creates inside `base_dir`.
const DEFAULT_DB_FILE: &str = "radio-scout.db";

/// The audio directory zero-config creates inside `base_dir`.
const DEFAULT_AUDIO_DIR: &str = "audio";

/// `[database]` — SQLite by default, Postgres by URL (US 40, ADR-0003).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Database {
    /// A full SeaORM connection URL. Unset means SQLite under `base_dir`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

/// `[storage]` — where Call audio lives (US 39, ADR-0002).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Storage {
    pub backend: Backend,
    /// Filesystem root for audio. Unset means `<base_dir>/audio`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
    pub s3: S3,
}

/// Which audio backend to open.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
#[clap(rename_all = "lowercase")]
pub enum Backend {
    /// The zero-config default: a directory of files.
    #[default]
    Filesystem,
    /// An S3-compatible object store — Garage, MinIO, AWS.
    S3,
}

impl Backend {
    /// The spelling used in the file, the flag and the environment.
    fn as_str(self) -> &'static str {
        match self {
            Backend::Filesystem => "filesystem",
            Backend::S3 => "s3",
        }
    }
}

impl std::fmt::Display for Backend {
    /// Unquoted in a log line, so `storage=s3` greps (ADR-0011 rule 6).
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Backend {
    type Err = ();

    /// One spelling per backend, shared by the file, the flag and the
    /// environment — so `RADIO_SCOUT_STORAGE_BACKEND=s3` and
    /// `backend = "s3"` cannot drift apart.
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        [Backend::Filesystem, Backend::S3]
            .into_iter()
            .find(|backend| backend.as_str() == text)
            .ok_or(())
    }
}

/// `[retention]` — how the archive is kept bounded (#10, US 41).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Retention {
    /// Prune Calls older than this many days. `0` keeps them forever.
    pub days: u32,
    /// Optional cap on total stored audio, in binary gigabytes. Absent means
    /// no cap.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_size_gb: Option<f64>,
    /// Prune stored log events (#30) older than this many days. `0` keeps them
    /// forever, the same reading `days` has — and its own setting, because a
    /// scanner keeping Calls forever must still bound its logs.
    pub log_days: u32,
    /// How often the sweeper runs.
    pub interval_secs: u64,
    /// Calls deleted per batch, bounding how long a write lock is held.
    pub batch_size: u64,
    /// How long an unreferenced audio object is left alone before orphan-GC
    /// reclaims it.
    pub orphan_grace_secs: u64,
}

impl Default for Retention {
    /// The shipped policy, taken from [`RetentionConfig`] rather than restated,
    /// so the file's defaults and the code's cannot drift apart.
    fn default() -> Self {
        let default = RetentionConfig::default();
        Retention {
            days: default.days,
            max_size_gb: None,
            log_days: default.log_days,
            interval_secs: default.interval.as_secs(),
            batch_size: default.batch_size,
            orphan_grace_secs: default.orphan_grace.as_secs(),
        }
    }
}

/// `[ingest]` — how uploads from recorders are handled (#5, #8).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Ingest {
    /// Duplicate-detection window in milliseconds. `0` disables it.
    pub dedup_window_ms: i64,
    /// Whether unknown Systems, Talkgroups and Units are created on arrival.
    pub auto_populate: bool,
}

impl Default for Ingest {
    /// As shipped — read off [`IngestConfig`] for the same reason.
    fn default() -> Self {
        let default = IngestConfig::default();
        Ingest {
            dedup_window_ms: default.dedup_window_ms,
            auto_populate: default.auto_populate,
        }
    }
}

/// `[admin]` — how the admin surface's sessions and lockout behave (#19,
/// ADR-0008).
///
/// The admin *password* is deliberately not here: like the ingest key, first
/// run **writes** it, so it lives in the environment and `.env`
/// (`RADIO_SCOUT_ADMIN_PASSWORD`) rather than in a file `--write-config`
/// generates. Everything about it that is a knob rather than a secret is here.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Admin {
    /// How long a session survives without being used, refreshed on each
    /// authenticated request.
    pub session_idle_secs: u64,
    /// How long a session may live at all, however active. Never refreshed.
    pub session_max_secs: u64,
    /// Failed logins one address may spend before it is locked out.
    pub lockout_attempts: u32,
    /// How long a spent address stays locked out, from its last attempt.
    pub lockout_secs: u64,
}

impl Default for Admin {
    /// As shipped — read off [`AdminConfig`] so the file's defaults and the
    /// code's cannot drift apart.
    fn default() -> Self {
        let default = AdminConfig::default();
        Admin {
            session_idle_secs: default.session_idle.as_secs(),
            session_max_secs: default.session_max.as_secs(),
            lockout_attempts: default.lockout_attempts,
            lockout_secs: default.lockout.as_secs(),
        }
    }
}

/// `[push]` — Web Push notifications (#16, spec US 32, ADR-0005).
///
/// The VAPID identity itself is deliberately not here, for the same reason the
/// admin password isn't: first run **writes** it, so it lives in the
/// environment and `.env` as `RADIO_SCOUT_VAPID_PRIVATE_KEY`. Everything here
/// is a knob rather than a secret.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Push {
    /// At most one notification per Talkgroup per device in this window. `0`
    /// notifies about every Call.
    pub coalesce_secs: u64,
    /// How long a push service holds a notification for a device that is
    /// offline.
    pub ttl_secs: u64,
    /// The VAPID `sub` claim: a `mailto:` or `https:` URI a push service's
    /// operator can reach this instance's operator through.
    pub subject: String,
}

impl Default for Push {
    /// As shipped — read off [`PushConfig`] so the file's defaults and the
    /// code's cannot drift apart.
    fn default() -> Self {
        let default = PushConfig::default();
        Push {
            coalesce_secs: default.coalesce.as_secs(),
            ttl_secs: default.ttl.as_secs(),
            subject: default.subject,
        }
    }
}

/// `[enhancement]` — audio enhancement (#20, spec US 33-34, ADR-0006 as
/// amended by #20).
///
/// Policy only. **Scope** — which Systems and Talkgroups are enhanced — is a
/// column on those rows rather than a list here, following the auto-populate
/// precedent (#8): a setting that names Refs in a file goes stale the moment a
/// recorder discovers a new one.
///
/// rdio-scanner puts the whole thing in its database behind its admin UI (a
/// four-value `audioConversion`, `server/options.go:56-59`), which is exactly
/// what this module's header refuses: a headless install cannot configure it
/// and nothing is version-controllable.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Enhancement {
    /// How far the chain runs. `off` — the default — is passthrough.
    pub mode: Mode,
    /// What enhanced audio is encoded as.
    pub output: Output,
    /// Integrated loudness every enhanced Call is normalized to, in LUFS.
    pub target_lufs: f64,
    /// How many Calls may be waiting to be enhanced.
    pub queue_depth: usize,
}

impl Default for Enhancement {
    /// As shipped — read off [`EnhancementConfig`] so the file's defaults and
    /// the code's cannot drift apart.
    fn default() -> Self {
        let default = EnhancementConfig::default();
        Enhancement {
            mode: default.mode,
            output: default.output,
            target_lufs: default.target_lufs,
            queue_depth: default.queue_depth,
        }
    }
}

/// What an unusable `[enhancement] mode` is told it should have been.
///
/// Spelled out rather than built from [`Mode::ALL`], because
/// [`ConfigError::Invalid`] carries a `&'static str` and joining at runtime
/// would not fit it. That makes drift possible, so a test holds this to
/// naming every mode the enum has.
const EXPECTED_MODE: &str = "\"off\", \"normalize\" or \"denoise\"";

/// ...and the same for `[enhancement] output`. Both spellings parse; whether
/// one is *built* is [`Config::validate`]'s business, so an operator who wrote
/// `opus` is told it is unbuilt rather than that it is unknown.
const EXPECTED_OUTPUT: &str = "\"wav\" or \"opus\"";

/// The loudness targets `[enhancement] target_lufs` will accept — `loudnorm`'s
/// own range, so what an operator knows from ffmpeg carries over. `contains`
/// on a float range is also how NaN is refused: it compares false against
/// everything, so a target that is not a number never reaches the gain.
const LOUDNESS_RANGE_LUFS: std::ops::RangeInclusive<f64> = -70.0..=-5.0;

/// `[log]` — how much the scanner says (ADR-0011).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Log {
    /// `tracing` filter directives: a bare level (`debug`) or a per-target list
    /// (`warn,radio_scout::ingest=trace`). `RUST_LOG` overrides this for one
    /// invocation; this is what survives a reboot.
    pub directives: String,
    /// What the operator log surface stores (#30): `off`, `error`, `warn` or
    /// `info`. Independent of `directives`, which is the console's — turning
    /// one up or down never moves the other.
    pub database_level: String,
}

impl Default for Log {
    fn default() -> Self {
        Log {
            directives: observability::DEFAULT_DIRECTIVES.to_string(),
            database_level: LogSinkConfig::level_name(LogSinkConfig::default().level).to_string(),
        }
    }
}

/// What an unusable `[log] database_level` is told it should have been.
///
/// Spelled out rather than built from [`LogSinkConfig::LEVELS`], because
/// [`ConfigError::Invalid`] carries a `&'static str`; a test holds it to naming
/// every level the sink accepts. The *reason* rides along because "why can't I
/// have debug?" is otherwise a mystery an operator would read as a typo.
const EXPECTED_DATABASE_LEVEL: &str = "\"off\", \"error\", \"warn\" or \"info\" — \
     DEBUG and below can carry a listener's address, which is never stored (ADR-0011 rule 5)";

/// `[storage.s3]` — credentials for the S3-compatible backend.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct S3 {
    pub bucket: String,
    pub region: String,
    /// Endpoint for a self-hosted store (Garage/MinIO); unset for AWS.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
    pub access_key_id: String,
    pub secret_access_key: String,
    /// Allow plain HTTP, for a self-hosted store on a LAN.
    pub allow_http: bool,
}

impl Default for S3 {
    fn default() -> Self {
        S3 {
            bucket: String::new(),
            // What Garage and MinIO answer to when they don't care, and AWS's
            // oldest region — a value that makes an unconfigured `region` a
            // non-event rather than a required field.
            region: "us-east-1".to_string(),
            endpoint: None,
            access_key_id: String::new(),
            secret_access_key: String::new(),
            allow_http: false,
        }
    }
}

/// Debug, minus the secret (ADR-0011 rule 2). `Config` derives `Debug`, so
/// anything that ever `?config`s a boot failure would otherwise put the S3
/// secret key in a log line. The access key *id* stays: it identifies which
/// credential is loaded and is not itself a secret.
impl std::fmt::Debug for S3 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("S3")
            .field("bucket", &self.bucket)
            .field("region", &self.region)
            .field("endpoint", &self.endpoint)
            .field("access_key_id", &self.access_key_id)
            .field("secret_access_key", &"<redacted>")
            .field("allow_http", &self.allow_http)
            .finish()
    }
}

/// `[server]` — the listening port and where data lives.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Server {
    pub port: u16,
    pub base_dir: PathBuf,
    /// Addresses and CIDR blocks whose `X-Forwarded-For` may be believed.
    /// Empty — the shipped posture — means the header is never believed.
    pub trusted_proxies: Vec<ProxyNet>,
}

impl Default for Server {
    fn default() -> Self {
        Server {
            port: 3000,
            base_dir: PathBuf::from("./radio-scout-data"),
            trusted_proxies: Vec::new(),
        }
    }
}

/// One entry of `[server] trusted_proxies`: a bare address (`10.0.0.1`) or a
/// CIDR block (`172.17.0.0/16`).
///
/// Parsing lives in the type so an unusable entry is a boot error wherever it
/// was written — the TOML, the environment or a flag — rather than a check
/// somebody has to remember to run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProxyNet(IpNet);

/// What an entry that is neither an address nor a block says for itself — in
/// the file (through serde), on the command line (through clap) and in the
/// environment, all from here, so the three cannot describe it differently.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NotAProxyNet;

/// What a proxy entry has to look like. One string, so the file, the flag and
/// the environment cannot describe it differently.
const EXPECTED_PROXY: &str = "an IP address or CIDR block, e.g. \"127.0.0.1\" or \"172.17.0.0/16\"";

impl std::fmt::Display for NotAProxyNet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "expected {EXPECTED_PROXY}")
    }
}

impl std::error::Error for NotAProxyNet {}

impl FromStr for ProxyNet {
    type Err = NotAProxyNet;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let text = text.trim();
        // A bare address is the single-host block containing it, so the trust
        // check has one shape rather than two.
        match text.parse::<IpNet>() {
            Ok(net) => Ok(ProxyNet(net)),
            Err(_) => text
                .parse::<IpAddr>()
                .map(|addr| ProxyNet(IpNet::from(addr)))
                .map_err(|_| NotAProxyNet),
        }
    }
}

impl std::fmt::Display for ProxyNet {
    /// A single host prints as the bare address it was almost certainly written
    /// as: `--write-config` and any future rewrite hand an operator back the
    /// spelling they used, not `127.0.0.1/32`.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.0.prefix_len() == self.0.max_prefix_len() {
            true => self.0.addr().fmt(f),
            false => self.0.fmt(f),
        }
    }
}

impl Serialize for ProxyNet {
    /// Through `Display`, so what is written back is spelled the way it was
    /// read.
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for ProxyNet {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        text.parse()
            .map_err(|err| serde::de::Error::custom(format!("{text:?}: {err}")))
    }
}

/// The proxies whose forwarding claims are believed, and the question they
/// exist to answer: whose address is this request really from?
///
/// Cheap to clone — the request log holds one for the life of the process.
#[derive(Debug, Clone, Default)]
pub struct TrustedProxies(Arc<[ProxyNet]>);

impl FromIterator<ProxyNet> for TrustedProxies {
    /// Build a trust list directly — what `[server] trusted_proxies` resolves
    /// to, without going through a whole `Config`.
    fn from_iter<I: IntoIterator<Item = ProxyNet>>(entries: I) -> Self {
        TrustedProxies(entries.into_iter().collect())
    }
}

impl TrustedProxies {
    /// Whether `addr` is one of the proxies the operator named.
    pub fn trusts(&self, addr: IpAddr) -> bool {
        self.0.iter().any(|net| net.0.contains(&addr))
    }

    /// The address to attribute a request to, given the TCP peer it arrived
    /// from and whatever `X-Forwarded-For` claimed.
    ///
    /// The header is believed only from a peer the operator trusted, and then
    /// only its **rightmost entry that is not itself a trusted proxy**: every
    /// hop appends the address it saw, so the right-hand end is the part our
    /// own infrastructure wrote and the left-hand end is whatever the client
    /// chose to send. rdio-scanner takes the header unconditionally
    /// (`server/main.go:265`), which lets anyone on a public instance forge a
    /// recorder's address into the operator's log.
    ///
    /// Anything unusable falls back to the peer — the one address the network
    /// stack, not a header, established.
    pub fn client_ip(&self, peer: IpAddr, forwarded_for: Option<&str>) -> IpAddr {
        if !self.trusts(peer) {
            return peer;
        }
        let Some(header) = forwarded_for else {
            return peer;
        };
        let hops: Vec<&str> = header
            .split(',')
            .map(str::trim)
            .filter(|hop| !hop.is_empty())
            .collect();

        let mut leftmost = peer;
        for hop in hops.iter().rev() {
            // A hop we can't parse makes the whole chain untrustworthy: an
            // attacker who can inject junk must not be able to shift which
            // entry we land on.
            let Ok(addr) = hop.parse::<IpAddr>() else {
                return peer;
            };
            leftmost = addr;
            if !self.trusts(addr) {
                return addr;
            }
        }
        // Every hop was a proxy we trust (or there were none): the leftmost is
        // as close to the client as this header can take us.
        leftmost
    }
}

/// The file a boot reads without being told to, and the one `--write-config`
/// creates.
pub const CONFIG_FILE_NAME: &str = "radio-scout.toml";

/// The file `--write-config` writes: every setting, at its default, with the
/// reason it exists beside it.
///
/// rdio-scanner's `-config_save` dumps the values it is running with and
/// nothing else (`server/config.go:118`), so an operator learns a setting
/// exists by reading Go source. This is the opposite: the commented lines *are*
/// the reference, and the tests below hold it to being both complete and
/// exactly the defaults — a template that lies is worse than none.
pub const TEMPLATE: &str = r##"# Radio-Scout configuration.
#
# Every setting below is shown at its default: uncomment to change one. A
# scanner with no config file at all runs on exactly these values.
#
# Precedence, loudest first: command-line flag, environment variable, this
# file, default. `radio-scout --help` lists the flags.

[server]
# HTTP port for the API, the SPA and the live-feed WebSocket — all one origin.
# port = 3000

# Everything the scanner owns lives here: the SQLite database, and by default
# the audio. Created on first run.
# base_dir = "./radio-scout-data"

# Addresses and CIDR blocks whose `X-Forwarded-For` header may be believed, for
# a deployment behind a reverse proxy or Docker's bridge. Empty (the default)
# means the header is never read and the log names the TCP peer — the header is
# attacker-controlled, so believing it from anyone would let a stranger forge a
# recorder's address into your log.
#   trusted_proxies = ["127.0.0.1", "172.17.0.0/16"]
# trusted_proxies = []

[database]
# A SeaORM connection URL. Unset means SQLite at <base_dir>/radio-scout.db,
# created on first run. Postgres looks like:
#   url = "postgres://user:password@host/radio_scout"

[storage]
# Where Call audio is stored: "filesystem" or "s3".
# backend = "filesystem"

# Filesystem root for audio. Unset means <base_dir>/audio — set it to keep the
# archive on a different disk from the database.
#   path = "/mnt/usb/radio-scout-audio"

[storage.s3]
# Used only when backend = "s3". Garage, MinIO and AWS all work; credentials
# may also come from RADIO_SCOUT_S3_ACCESS_KEY_ID / _SECRET_ACCESS_KEY, which
# is the better home for them in a container.
# bucket = ""
# region = "us-east-1"
#   endpoint = "http://garage.lan:3900"
# access_key_id = ""
# secret_access_key = ""
# allow_http = false

[retention]
# Prune Calls older than this many days. 0 keeps them forever.
# days = 7

# Optional cap on total stored audio, in gigabytes; the oldest Calls are pruned
# until the archive fits. No key at all means no cap.
#   max_size_gb = 10

# Prune stored log events (the Settings -> Logs view) older than this many days.
# 0 keeps them forever. Its own window, not the one above: logs are small, and
# the question they answer is often about a day whose audio has already gone.
# log_days = 30

# How often the sweeper runs, how many Calls it deletes per batch (a small
# batch keeps each write-lock short on a Pi), and how long an audio object with
# no Call row is left alone before it is reclaimed. The same sweep prunes logs.
# interval_secs = 3600
# batch_size = 500
# orphan_grace_secs = 3600

[ingest]
# Duplicate-detection window: a Call arriving within this many milliseconds of
# an identical one is dropped. 0 disables it.
# dedup_window_ms = 500

# Create Systems, Talkgroups and Units the first time a recorder mentions them.
# With this off, only Systems you have already defined are accepted.
# auto_populate = true

[admin]
# The admin password itself is NOT here: first run writes it, so it lives in
# the environment and `.env` as RADIO_SCOUT_ADMIN_PASSWORD. With none set, the
# first run generates one, writes it to that file 0600, and logs only the path
# — never the password. `cat .env` is how you read it back.
#
# If you terminate TLS at a reverse proxy, set [server] trusted_proxies to that
# proxy's address. The session cookie is marked Secure only when the proxy says
# the client's hop was HTTPS (X-Forwarded-Proto), and that header is believed
# only from a proxy you named — so with the list empty, an HTTPS deployment
# still gets a cookie a browser will replay over plain http:// to the same host.

# How long a session survives without being used (refreshed on every request),
# and how long it may live at all however active. The second is the bound on a
# cookie somebody walked off with, so use does not extend it.
# session_idle_secs = 28800
# session_max_secs = 604800

# Failed logins one address may spend before it is locked out, and for how
# long. The cooldown runs from the last attempt, so hammering keeps it locked
# and walking away clears it. The address is the TCP peer's unless you named
# that peer in [server] trusted_proxies.
# lockout_attempts = 5
# lockout_secs = 900

[push]
# Web Push notifications for a phone that has the app installed (#16). The
# identity notifications are signed with is NOT here: first run generates one
# and writes it to the env file as RADIO_SCOUT_VAPID_PRIVATE_KEY, exactly like
# the ingest key. Delete that line and the next boot makes a new one — which
# every browser that had already subscribed will no longer be notified by.
#
# Notifications only go to a listener who is *not* listening: a device with the
# live feed open already has the Call.

# At most one notification per Talkgroup per device in this window, carrying a
# count of the Calls it stands for. 0 notifies about every Call.
# coalesce_secs = 300

# How long a push service should hold a notification for a phone that is off or
# out of signal before giving up.
# ttl_secs = 3600

# The contact a push service's operator can reach you through (RFC 8292 asks
# for a mailto: or https: URI). Some services refuse notifications without a
# real one, so set it if you use a public instance.
# subject = "mailto:admin@localhost"

[enhancement]
# Audio enhancement (#20): reprocess each Call's audio after it is stored, to
# make it clearer and consistently loud between Talkgroups. Off by default, and
# never on the ingest path — a recorder's upload is answered before any of this
# starts, so enabling it cannot slow ingest down or lose a Call.
#
#   "off"        store exactly what the recorder sent (passthrough)
#   "normalize"  voice band-pass + EBU R128 loudness normalization
#   "denoise"    ...and RNNoise noise suppression on top
#
# "normalize" is the proven win — it fixes the level swings between Talkgroups
# that make scanning fatiguing. "denoise" is unproven on already-decoded
# digital (P25/DMR) audio; try it on your own systems before trusting it.
#
# This is the instance-wide setting. To enhance only some Systems or Talkgroups
# — which is how you keep one chatty System from eating a Pi — set it per row
# instead; a row that says nothing inherits this.
# mode = "off"

# What enhanced audio is written as. "wav" is 8 kHz 16-bit mono: it plays on
# every iOS version and in every browser, and is still smaller than what most
# recorders send. "opus" is roughly five times smaller again, but only plays in
# Safari from iOS 18.4 — and is not built yet (see the note it refuses with).
# output = "wav"

# The loudness every enhanced Call is normalized to, in LUFS. -16 is a speech
# target that sounds right on a phone speaker; EBU R128 broadcast is -23, which
# is noticeably quieter. This is the setting that fixes the level swings between
# Talkgroups. (The peak ceiling that keeps a normalized Call from clipping is
# fixed at -1.5 dBFS and is not configurable.)
# target_lufs = -16.0

# How many Calls may be waiting to be enhanced before an arriving one simply
# keeps the audio the recorder sent. Deep on purpose: the live feed is
# published at ingest, not after enhancement, so a backlog never delays a
# listener — it only decides how long a burst can outrun the worker.
# queue_depth = 512

[log]
# Filter directives: a bare level, or per-target. RUST_LOG overrides this for a
# single run.
#   directives = "warn,radio_scout::ingest=trace"
# directives = "info,sqlx::query=warn,sea_orm_migration=warn"

# What the Settings -> Logs view keeps, for an operator with no shell to read
# `journalctl` from: "off", "error", "warn" or "info". Independent of the
# directives above — turning the console up to chase a problem does not change
# what is stored, and turning it down does not empty the Logs view.
#
# There is deliberately no "debug": those are the lines that can carry a
# listener's IP address, and a public instance must never accumulate a database
# of who listened and when. The console still has them.
# database_level = "info"
"##;

/// Write [`TEMPLATE`] to `path`, refusing to overwrite anything already there.
///
/// A config file is hand-edited; clobbering one because a flag was mistyped
/// would lose work that only exists in that file.
pub fn write_template(path: &std::path::Path) -> Result<(), ConfigError> {
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .and_then(|mut file| std::io::Write::write_all(&mut file, TEMPLATE.as_bytes()))
        .map_err(|source| ConfigError::File {
            path: path.to_path_buf(),
            source,
        })
}

/// Load the configuration, finding the file on disk.
///
/// Discovery, in order: `--config`, `RADIO_SCOUT_CONFIG`, then
/// `radio-scout.toml` in `cwd`. A file that was *named* and cannot be read
/// stops the boot; the one merely looked for may be absent, because zero-config
/// (US 35) means a scanner with no file at all still runs.
///
/// `cwd` is a parameter rather than a call to `current_dir` so discovery can be
/// tested without a process-global chdir.
pub fn load(
    cli: &Cli,
    env: impl Fn(&str) -> Option<String>,
    cwd: &std::path::Path,
) -> Result<Loaded, ConfigError> {
    let named = cli
        .config
        .clone()
        .or_else(|| set_env(&env, "RADIO_SCOUT_CONFIG").map(PathBuf::from));

    let file = match named {
        Some(path) => Some(read_config(&path)?),
        None => {
            let path = cwd.join(CONFIG_FILE_NAME);
            match path.exists() {
                true => Some(read_config(&path)?),
                false => None,
            }
        }
    };

    Ok(Loaded {
        config: resolve(cli, env, file.as_ref())?,
        file: file.map(|file| file.path),
    })
}

/// A configuration and the file it came from, if any.
///
/// The provenance is kept because "which file did you read?" is the first
/// question when a setting doesn't seem to apply, and the answer has to be
/// available *after* logging is up — logging is configured by the very thing
/// being loaded.
#[derive(Debug)]
pub struct Loaded {
    pub config: Config,
    pub file: Option<PathBuf>,
}

impl Loaded {
    /// Say what this boot is configured to do, and where that came from.
    ///
    /// Never a credential (ADR-0011 rule 2): a Postgres URL carries a password
    /// and an S3 section carries a secret key, so the line names the database
    /// *dialect* and the storage *backend* — enough to tell a misconfiguration
    /// from a working one, and nothing an operator would have to redact before
    /// pasting it into an issue.
    pub fn log_summary(&self) {
        // Provenance and settings are separate events so each is written once,
        // rather than one message duplicated per branch.
        match &self.file {
            Some(path) => {
                tracing::info!(config_file = %path.display(), "configuration file loaded")
            }
            None => tracing::info!("no configuration file; running on defaults"),
        }
        let config = &self.config;
        let base_dir = config.server.base_dir.display();
        let database = config.database_dialect();
        let trusted_proxies = config.server.trusted_proxies.len();
        tracing::info!(
            port = config.server.port,
            %base_dir,
            %database,
            storage = %config.storage.backend,
            trusted_proxies,
            // The one setting that changes the bytes a listener receives, so
            // "why does this sound different" is answerable from the log.
            enhancement = %config.enhancement.mode,
            "configuration"
        );
    }
}

/// Read a config file, saying which file when it can't be read.
fn read_config(path: &std::path::Path) -> Result<ConfigFile, ConfigError> {
    std::fs::read_to_string(path)
        .map(|text| ConfigFile::new(path, text))
        .map_err(|source| ConfigError::File {
            path: path.to_path_buf(),
            source,
        })
}

/// Resolve the configuration a boot will run with: the file over the defaults,
/// the environment over that, the command line over everything.
///
/// `env` is the environment as a lookup (the binary passes `std::env::var`), so
/// resolution is testable without mutating a process-global.
pub fn resolve(
    cli: &Cli,
    env: impl Fn(&str) -> Option<String>,
    file: Option<&ConfigFile>,
) -> Result<Config, ConfigError> {
    let mut config = match file {
        Some(file) => toml::from_str(&file.text)
            .map_err(|source| ConfigError::parse(&file.path, &file.text, source))?,
        None => Config::default(),
    };

    // The environment, over the file.
    if let Some(port) = parsed_env(&env, "RADIO_SCOUT_PORT", "a port number 0-65535")? {
        config.server.port = port;
    }
    if let Some(base_dir) = set_env(&env, "RADIO_SCOUT_BASE_DIR") {
        config.server.base_dir = PathBuf::from(base_dir);
    }
    if let Some(list) = set_env(&env, "RADIO_SCOUT_TRUSTED_PROXIES") {
        config.server.trusted_proxies = list
            .split(',')
            .map(str::trim)
            .filter(|entry| !entry.is_empty())
            .map(|entry| {
                entry.parse().map_err(|_| {
                    ConfigError::invalid_env("RADIO_SCOUT_TRUSTED_PROXIES", entry, EXPECTED_PROXY)
                })
            })
            .collect::<Result<_, _>>()?;
    }
    if let Some(url) = set_env(&env, "RADIO_SCOUT_DATABASE_URL") {
        config.database.url = Some(url);
    }
    if let Some(backend) = set_env(&env, "RADIO_SCOUT_STORAGE_BACKEND") {
        config.storage.backend = backend.parse().map_err(|_| {
            ConfigError::invalid_env(
                "RADIO_SCOUT_STORAGE_BACKEND",
                &backend,
                "\"filesystem\" or \"s3\"",
            )
        })?;
    }
    if let Some(path) = set_env(&env, "RADIO_SCOUT_STORAGE_PATH") {
        config.storage.path = Some(PathBuf::from(path));
    }
    if let Some(bucket) = set_env(&env, "RADIO_SCOUT_S3_BUCKET") {
        config.storage.s3.bucket = bucket;
    }
    if let Some(region) = set_env(&env, "RADIO_SCOUT_S3_REGION") {
        config.storage.s3.region = region;
    }
    if let Some(endpoint) = set_env(&env, "RADIO_SCOUT_S3_ENDPOINT") {
        config.storage.s3.endpoint = Some(endpoint);
    }
    if let Some(id) = set_env(&env, "RADIO_SCOUT_S3_ACCESS_KEY_ID") {
        config.storage.s3.access_key_id = id;
    }
    if let Some(secret) = set_env(&env, "RADIO_SCOUT_S3_SECRET_ACCESS_KEY") {
        config.storage.s3.secret_access_key = secret;
    }
    if let Some(allow_http) = parsed_env(&env, "RADIO_SCOUT_S3_ALLOW_HTTP", "true or false")? {
        config.storage.s3.allow_http = allow_http;
    }
    if let Some(days) = parsed_env(&env, "RADIO_SCOUT_RETENTION_DAYS", "a number of days")? {
        config.retention.days = days;
    }
    if let Some(gb) = parsed_env(
        &env,
        "RADIO_SCOUT_RETENTION_MAX_SIZE_GB",
        "a number of gigabytes",
    )? {
        config.retention.max_size_gb = Some(gb);
    }
    if let Some(days) = parsed_env(&env, "RADIO_SCOUT_RETENTION_LOG_DAYS", "a number of days")? {
        config.retention.log_days = days;
    }
    if let Some(secs) = parsed_env(
        &env,
        "RADIO_SCOUT_RETENTION_INTERVAL_SECS",
        "a number of seconds",
    )? {
        config.retention.interval_secs = secs;
    }
    if let Some(batch) = parsed_env(
        &env,
        "RADIO_SCOUT_RETENTION_BATCH_SIZE",
        "a number of Calls per batch",
    )? {
        config.retention.batch_size = batch;
    }
    if let Some(secs) = parsed_env(
        &env,
        "RADIO_SCOUT_PUSH_COALESCE_SECS",
        "a number of seconds",
    )? {
        config.push.coalesce_secs = secs;
    }
    if let Some(secs) = parsed_env(&env, "RADIO_SCOUT_PUSH_TTL_SECS", "a number of seconds")? {
        config.push.ttl_secs = secs;
    }
    if let Some(subject) = set_env(&env, "RADIO_SCOUT_PUSH_SUBJECT") {
        config.push.subject = subject;
    }
    if let Some(secs) = parsed_env(
        &env,
        "RADIO_SCOUT_RETENTION_ORPHAN_GRACE_SECS",
        "a number of seconds",
    )? {
        config.retention.orphan_grace_secs = secs;
    }
    if let Some(ms) = parsed_env(
        &env,
        "RADIO_SCOUT_INGEST_DEDUP_WINDOW_MS",
        "a duration in milliseconds",
    )? {
        config.ingest.dedup_window_ms = ms;
    }
    if let Some(auto) = parsed_env(&env, "RADIO_SCOUT_INGEST_AUTO_POPULATE", "true or false")? {
        config.ingest.auto_populate = auto;
    }
    if let Some(secs) = parsed_env(
        &env,
        "RADIO_SCOUT_ADMIN_SESSION_IDLE_SECS",
        "a number of seconds",
    )? {
        config.admin.session_idle_secs = secs;
    }
    if let Some(secs) = parsed_env(
        &env,
        "RADIO_SCOUT_ADMIN_SESSION_MAX_SECS",
        "a number of seconds",
    )? {
        config.admin.session_max_secs = secs;
    }
    if let Some(attempts) = parsed_env(
        &env,
        "RADIO_SCOUT_ADMIN_LOCKOUT_ATTEMPTS",
        "a number of attempts",
    )? {
        config.admin.lockout_attempts = attempts;
    }
    if let Some(secs) = parsed_env(
        &env,
        "RADIO_SCOUT_ADMIN_LOCKOUT_SECS",
        "a number of seconds",
    )? {
        config.admin.lockout_secs = secs;
    }
    if let Some(mode) = set_env(&env, "RADIO_SCOUT_ENHANCEMENT_MODE") {
        config.enhancement.mode = mode.parse().map_err(|_| {
            ConfigError::invalid_env("RADIO_SCOUT_ENHANCEMENT_MODE", &mode, EXPECTED_MODE)
        })?;
    }
    if let Some(output) = set_env(&env, "RADIO_SCOUT_ENHANCEMENT_OUTPUT") {
        config.enhancement.output = output.parse().map_err(|_| {
            ConfigError::invalid_env("RADIO_SCOUT_ENHANCEMENT_OUTPUT", &output, EXPECTED_OUTPUT)
        })?;
    }
    if let Some(lufs) = parsed_env(
        &env,
        "RADIO_SCOUT_ENHANCEMENT_TARGET_LUFS",
        "a loudness in LUFS, e.g. -16",
    )? {
        config.enhancement.target_lufs = lufs;
    }
    if let Some(depth) = parsed_env(
        &env,
        "RADIO_SCOUT_ENHANCEMENT_QUEUE_DEPTH",
        "a number of Calls",
    )? {
        config.enhancement.queue_depth = depth;
    }
    // `RUST_LOG`, not a `RADIO_SCOUT_`-prefixed name: it is the variable every
    // Rust operator already reaches for, and ADR-0011 documents it as the
    // control surface. The `[log]` section is what survives a reboot.
    if let Some(directives) = set_env(&env, "RUST_LOG") {
        validate_directives("RUST_LOG", &directives)?;
        config.log.directives = directives;
    }
    // ...whereas the operator log surface's level is ours, so it takes the
    // prefix every other setting does (#30).
    if let Some(level) = set_env(&env, "RADIO_SCOUT_LOG_DATABASE_LEVEL") {
        if LogSinkConfig::level_from_str(&level).is_none() {
            return Err(ConfigError::invalid_env(
                "RADIO_SCOUT_LOG_DATABASE_LEVEL",
                &level,
                EXPECTED_DATABASE_LEVEL,
            ));
        }
        config.log.database_level = level;
    }

    // The command line, over everything.
    if let Some(port) = cli.port {
        config.server.port = port;
    }
    if let Some(base_dir) = &cli.base_dir {
        config.server.base_dir = base_dir.clone();
    }
    if let Some(url) = &cli.database_url {
        config.database.url = Some(url.clone());
    }
    if let Some(backend) = cli.storage_backend {
        config.storage.backend = backend;
    }
    if let Some(days) = cli.retention_days {
        config.retention.days = days;
    }
    if let Some(gb) = cli.retention_max_size_gb {
        config.retention.max_size_gb = Some(gb);
    }
    if let Some(proxies) = &cli.trusted_proxies {
        config.server.trusted_proxies.clone_from(proxies);
    }
    if let Some(directives) = &cli.log {
        validate_directives("--log", directives)?;
        config.log.directives.clone_from(directives);
    }

    config.validate()?;
    Ok(config)
}

/// Keys whose *value* is a credential (ADR-0011 rule 2). `access_key_id` is
/// deliberately absent: it identifies a credential without being one, and
/// naming it is how an operator tells which is loaded.
const SECRET_KEYS: &[&str] = &["secret_access_key", "url"];

/// Whether a line of TOML assigns one of [`SECRET_KEYS`].
fn assigns_a_secret(line: &str) -> bool {
    line.split_once('=')
        .is_some_and(|(key, _)| SECRET_KEYS.contains(&key.trim()))
}

/// The 1-based line and column of a byte offset into `text`.
fn line_and_column(text: &str, offset: usize) -> (usize, usize) {
    let before = &text[..offset.min(text.len())];
    let line = before.matches('\n').count() + 1;
    let column = before.rsplit('\n').next().unwrap_or(before).chars().count() + 1;
    (line, column)
}

/// Refuse log directives `tracing` cannot parse.
///
/// The subscriber falls back to the default rather than booting a silent
/// scanner (`observability::subscriber`), which is the right last resort but
/// the wrong *first* answer: an operator who asked for `radio_scout=trace` and
/// silently got INFO spends the outage looking at the wrong log.
fn validate_directives(source: &str, directives: &str) -> Result<(), ConfigError> {
    match observability::directives_are_valid(directives) {
        true => Ok(()),
        false => Err(ConfigError::Invalid {
            source: source.to_string(),
            value: directives.to_string(),
            expected: "tracing filter directives, e.g. \"debug\" or \"warn,radio_scout=trace\"",
        }),
    }
}

/// The value of `var`, or `None` when it is unset or blank.
///
/// Blank is "unset" rather than "the empty value": `RADIO_SCOUT_PORT=` in an
/// env file, or an unset variable interpolated by a shell wrapper, must not
/// take precedence over the file with nothing in it.
fn set_env(env: &impl Fn(&str) -> Option<String>, var: &str) -> Option<String> {
    env(var)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// [`set_env`], parsed — and a boot refused if it doesn't parse.
fn parsed_env<T: FromStr>(
    env: &impl Fn(&str) -> Option<String>,
    var: &str,
    expected: &'static str,
) -> Result<Option<T>, ConfigError> {
    set_env(env, var)
        .map(|value| {
            value
                .parse()
                .map_err(|_| ConfigError::invalid_env(var, &value, expected))
        })
        .transpose()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::LogCapture;
    use rstest::rstest;
    use std::time::Duration;
    use tracing::Level;

    /// A config file as an operator would have written it.
    fn file(text: &str) -> ConfigFile {
        ConfigFile::new("radio-scout.toml", text)
    }

    /// The command line, parsed exactly as `main` parses it.
    fn cli(args: &[&str]) -> Cli {
        Cli::try_parse_from(std::iter::once("radio-scout").chain(args.iter().copied()))
            .expect("parse flags")
    }

    /// An environment holding `pairs` and nothing else.
    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> + use<> {
        let pairs: Vec<(String, String)> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |name: &str| {
            pairs
                .iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.clone())
        }
    }

    /// The empty environment.
    fn no_env(_: &str) -> Option<String> {
        None
    }

    /// An address, as a test writes one.
    fn ip(text: &str) -> IpAddr {
        text.parse().expect("an IP address")
    }

    /// A trust list, as configuration would have produced one.
    fn proxies(entries: &[&str]) -> TrustedProxies {
        let config = resolve(
            &cli(&[]),
            env(&[("RADIO_SCOUT_TRUSTED_PROXIES", &entries.join(","))]),
            None,
        )
        .expect("resolve");
        config.trusted_proxies()
    }

    /// The audio store this config would open, as the two things a test wants
    /// to assert about: a filesystem root, or an S3 configuration.
    fn store_of(config: &Config) -> Result<PathBuf, Box<S3Config>> {
        match config.storage() {
            StorageConfig::Filesystem { root } => Ok(root),
            StorageConfig::S3(s3) => Err(Box::new(s3)),
        }
    }

    /// The filesystem root the audio store would open.
    fn fs_root(config: &Config) -> PathBuf {
        store_of(config).expect("a filesystem store")
    }

    /// The S3 configuration the audio store would open.
    fn s3_of(config: &Config) -> S3Config {
        *store_of(config).expect_err("an S3 store")
    }

    /// Zero-config (US 35): no file, no flags, and the server still knows where
    /// to listen and where to put its data.
    #[test]
    fn nothing_configured_means_the_defaults() {
        let config = resolve(&cli(&[]), no_env, None).expect("resolve");

        assert_eq!(config.server.port, 3000);
        assert_eq!(config.server.base_dir, PathBuf::from("./radio-scout-data"));
    }

    /// The file is the operator's declared intent (US 36).
    #[test]
    fn the_file_sets_the_port() {
        let config =
            resolve(&cli(&[]), no_env, Some(&file("[server]\nport = 8080\n"))).expect("resolve");

        assert_eq!(config.server.port, 8080);
    }

    /// Precedence, in one table: **CLI beats environment beats file beats
    /// default**. rdio-scanner gets this backwards — `flag.Parse()` runs first
    /// and the INI file then overwrites whatever the flags set
    /// (`server/config.go:96-137`), so a flag cannot override a configured
    /// value at all. Ours is the order every other server tool uses: the more
    /// specific to this invocation, the louder.
    #[rstest]
    #[case(&[], &[], None, 3000)]
    #[case(&[], &[], Some("[server]\nport = 8080\n"), 8080)]
    #[case(&[], &[("RADIO_SCOUT_PORT", "9000")], Some("[server]\nport = 8080\n"), 9000)]
    #[case(&["--port", "7000"], &[("RADIO_SCOUT_PORT", "9000")], Some("[server]\nport = 8080\n"), 7000)]
    #[case(&["--port", "7000"], &[], None, 7000)]
    #[case(&[], &[("RADIO_SCOUT_PORT", "9000")], None, 9000)]
    fn the_loudest_layer_wins(
        #[case] args: &[&str],
        #[case] vars: &[(&str, &str)],
        #[case] text: Option<&str>,
        #[case] expected: u16,
    ) {
        let file = text.map(file);
        let config = resolve(&cli(args), env(vars), file.as_ref()).expect("resolve");

        assert_eq!(config.server.port, expected);
    }

    /// Same order for a path, which takes a different route through parsing.
    #[rstest]
    #[case(&[], &[], Some("[server]\nbase_dir = \"/srv/from-file\"\n"), "/srv/from-file")]
    #[case(&[], &[("RADIO_SCOUT_BASE_DIR", "/srv/from-env")], Some("[server]\nbase_dir = \"/srv/from-file\"\n"), "/srv/from-env")]
    #[case(&["--base-dir", "/srv/from-flag"], &[("RADIO_SCOUT_BASE_DIR", "/srv/from-env")], None, "/srv/from-flag")]
    fn base_dir_follows_the_same_order(
        #[case] args: &[&str],
        #[case] vars: &[(&str, &str)],
        #[case] text: Option<&str>,
        #[case] expected: &str,
    ) {
        let file = text.map(file);
        let config = resolve(&cli(args), env(vars), file.as_ref()).expect("resolve");

        assert_eq!(config.server.base_dir, PathBuf::from(expected));
    }

    /// The zero-config database (US 35/40): SQLite, created on demand, beside
    /// everything else the scanner owns.
    #[test]
    fn the_database_defaults_to_sqlite_under_base_dir() {
        let config = resolve(&cli(&["--base-dir", "/srv/rs"]), no_env, None).expect("resolve");

        assert_eq!(
            config.database_url(),
            "sqlite:///srv/rs/radio-scout.db?mode=rwc"
        );
    }

    /// ...and Postgres is a URL away (US 40), from any of the three layers.
    #[rstest]
    #[case(&[], &[], Some("[database]\nurl = \"postgres://db/from-file\"\n"), "postgres://db/from-file")]
    #[case(&[], &[("RADIO_SCOUT_DATABASE_URL", "postgres://db/from-env")], Some("[database]\nurl = \"postgres://db/from-file\"\n"), "postgres://db/from-env")]
    #[case(&["--database-url", "postgres://db/from-flag"], &[("RADIO_SCOUT_DATABASE_URL", "postgres://db/from-env")], None, "postgres://db/from-flag")]
    fn a_configured_database_url_is_used_verbatim(
        #[case] args: &[&str],
        #[case] vars: &[(&str, &str)],
        #[case] text: Option<&str>,
        #[case] expected: &str,
    ) {
        let file = text.map(file);
        let config = resolve(&cli(args), env(vars), file.as_ref()).expect("resolve");

        assert_eq!(config.database_url(), expected);
    }

    /// The zero-config audio store (US 35/39): a directory under `base_dir`.
    #[test]
    fn audio_defaults_to_the_filesystem_under_base_dir() {
        let config = resolve(&cli(&["--base-dir", "/srv/rs"]), no_env, None).expect("resolve");

        assert_eq!(fs_root(&config), PathBuf::from("/srv/rs/audio"));
    }

    /// A separate disk for audio without moving the database: a Pi's SD card
    /// holds the metadata while the Calls go to a USB drive.
    #[test]
    fn the_audio_directory_can_point_elsewhere() {
        let config = resolve(
            &cli(&["--base-dir", "/srv/rs"]),
            no_env,
            Some(&file("[storage]\npath = \"/mnt/usb/audio\"\n")),
        )
        .expect("resolve");

        assert_eq!(fs_root(&config), PathBuf::from("/mnt/usb/audio"));
    }

    /// S3-compatible object storage (US 39, ADR-0002) — Garage first-class.
    #[test]
    fn the_s3_backend_is_built_from_its_section() {
        let config = resolve(
            &cli(&[]),
            no_env,
            Some(&file(
                r#"
                [storage]
                backend = "s3"

                [storage.s3]
                bucket = "calls"
                region = "garage"
                endpoint = "http://garage.lan:3900"
                access_key_id = "GK1234"
                secret_access_key = "s3cr3t"
                allow_http = true
                "#,
            )),
        )
        .expect("resolve");

        let s3 = s3_of(&config);
        assert_eq!(s3.bucket, "calls");
        assert_eq!(s3.region, "garage");
        assert_eq!(s3.endpoint.as_deref(), Some("http://garage.lan:3900"));
        assert_eq!(s3.access_key_id, "GK1234");
        assert_eq!(s3.secret_access_key, "s3cr3t");
        assert!(s3.allow_http);
    }

    /// Credentials belong in the environment as readily as in a file — a
    /// container has one and not the other — and `--storage-backend` selects
    /// the backend without touching the file at all.
    #[test]
    fn s3_credentials_can_come_from_the_environment() {
        let config = resolve(
            &cli(&["--storage-backend", "s3"]),
            env(&[
                ("RADIO_SCOUT_S3_BUCKET", "calls"),
                ("RADIO_SCOUT_S3_ACCESS_KEY_ID", "GK1234"),
                ("RADIO_SCOUT_S3_SECRET_ACCESS_KEY", "s3cr3t"),
            ]),
            None,
        )
        .expect("resolve");

        let s3 = s3_of(&config);
        assert_eq!(s3.bucket, "calls");
        assert_eq!(s3.access_key_id, "GK1234");
    }

    /// Selecting a backend whose credentials are missing must fail *here*,
    /// naming the key — not at the first upload, hours later, as a 500 with an
    /// object-store error in it.
    #[rstest]
    #[case("bucket", &[("RADIO_SCOUT_S3_ACCESS_KEY_ID", "k"), ("RADIO_SCOUT_S3_SECRET_ACCESS_KEY", "s")])]
    #[case("access_key_id", &[("RADIO_SCOUT_S3_BUCKET", "calls"), ("RADIO_SCOUT_S3_SECRET_ACCESS_KEY", "s")])]
    #[case("secret_access_key", &[("RADIO_SCOUT_S3_BUCKET", "calls"), ("RADIO_SCOUT_S3_ACCESS_KEY_ID", "k")])]
    fn the_s3_backend_without_credentials_refuses_to_boot(
        #[case] missing: &str,
        #[case] vars: &[(&str, &str)],
    ) {
        let error = resolve(&cli(&["--storage-backend", "s3"]), env(vars), None)
            .expect_err("an unusable storage backend must not boot");

        let message = error.to_string();
        assert!(message.contains(missing), "{message}");
        assert!(message.contains("storage.s3"), "{message}");
    }

    /// ADR-0011 rule 2 on the path that most wants to break it: a **failed
    /// parse**. `toml::de::Error`'s own `Display` quotes the offending source
    /// line verbatim, and the line an operator mistypes is the line they were
    /// editing — so a typo'd key inside `[storage.s3]` would put the secret on
    /// an ERROR line, which is exactly the startup output that gets pasted into
    /// an issue.
    #[rstest]
    // A typo'd key: the secret sits on the failing line itself.
    #[case(
        "[storage.s3]\nbucket = \"calls\"\nsecret_acess_key = \"SUPERSECRET\"\n",
        "SUPERSECRET"
    )]
    // A wrong type on the credential: serde's message quotes the value.
    #[case("[storage.s3]\nsecret_access_key = 12345\n", "12345")]
    // A database URL with a password, unquoted so it fails to parse.
    #[case("[database]\nurl = postgres://rs:hunter2@db.lan/x\n", "hunter2")]
    fn a_parse_error_never_carries_the_value_that_failed(#[case] text: &str, #[case] secret: &str) {
        let error = resolve(&cli(&[]), no_env, Some(&file(text))).expect_err("a parse error");

        let message = error.to_string();
        assert!(
            !message.contains(secret),
            "the failing value must not appear in:\n{message}"
        );
        // ...and it still says where to look, which is the whole job of the
        // message once the snippet is gone.
        assert!(message.contains("radio-scout.toml:"), "{message}");
    }

    /// The position is real, not a placeholder — an operator has to be able to
    /// jump to the line.
    #[rstest]
    #[case("[server]\nprot = 1\n", 2, 1)]
    #[case("# a comment\n\n[server]\nport = \"nope\"\n", 4, 8)]
    fn a_parse_error_points_at_the_line_and_column(
        #[case] text: &str,
        #[case] line: usize,
        #[case] column: usize,
    ) {
        let error = resolve(&cli(&[]), no_env, Some(&file(text))).expect_err("a parse error");

        assert!(
            error.to_string().contains(&format!(":{line}:{column}:")),
            "expected {line}:{column} in: {error}"
        );
    }

    /// ADR-0011 rule 2: a secret is never logged, at any level, in any form —
    /// which includes being carried into a line by a `Debug`-printed config.
    #[test]
    fn debug_output_never_carries_the_s3_secret() {
        let config = resolve(
            &cli(&["--storage-backend", "s3"]),
            env(&[
                ("RADIO_SCOUT_S3_BUCKET", "calls"),
                ("RADIO_SCOUT_S3_ACCESS_KEY_ID", "GK1234"),
                ("RADIO_SCOUT_S3_SECRET_ACCESS_KEY", "s3cr3t-do-not-print"),
            ]),
            None,
        )
        .expect("resolve");

        let debugged = format!("{config:?}");
        assert!(!debugged.contains("s3cr3t-do-not-print"), "{debugged}");
        // The access key id is an identifier, not a secret, and naming it is how
        // an operator tells which credential is loaded.
        assert!(debugged.contains("GK1234"), "{debugged}");
    }

    /// Retention (US 41, #10) reaches the sweeper through the same three
    /// layers, in operator units: days, and gigabytes rather than bytes.
    #[rstest]
    #[case(&[], &[], None, 7, None)]
    #[case(&[], &[], Some("[retention]\ndays = 30\nmax_size_gb = 2.5\n"), 30, Some(2_684_354_560))]
    #[case(&[], &[("RADIO_SCOUT_RETENTION_DAYS", "3")], Some("[retention]\ndays = 30\n"), 3, None)]
    #[case(&["--retention-days", "1"], &[("RADIO_SCOUT_RETENTION_DAYS", "3")], None, 1, None)]
    #[case(&["--retention-max-size-gb", "1"], &[("RADIO_SCOUT_RETENTION_MAX_SIZE_GB", "4")], None, 7, Some(1_073_741_824))]
    #[case(&[], &[("RADIO_SCOUT_RETENTION_MAX_SIZE_GB", "4")], None, 7, Some(4_294_967_296))]
    // rdio-scanner's `pruneDays = 0` semantics: keep forever, and a valid thing
    // to ask for.
    #[case(&[], &[], Some("[retention]\ndays = 0\n"), 0, None)]
    fn the_retention_policy_comes_from_all_three_layers(
        #[case] args: &[&str],
        #[case] vars: &[(&str, &str)],
        #[case] text: Option<&str>,
        #[case] expected_days: u32,
        #[case] expected_cap: Option<u64>,
    ) {
        let file = text.map(file);
        let config = resolve(&cli(args), env(vars), file.as_ref()).expect("resolve");

        let retention = config.retention();
        assert_eq!(retention.days, expected_days);
        assert_eq!(retention.max_size_bytes, expected_cap);
    }

    /// The knobs #10 has but the env-var stopgap could never reach.
    #[test]
    fn the_sweeper_s_cadence_and_batching_are_configurable() {
        let config = resolve(
            &cli(&[]),
            no_env,
            Some(&file(
                "[retention]\ninterval_secs = 60\nbatch_size = 25\norphan_grace_secs = 120\n",
            )),
        )
        .expect("resolve");

        let retention = config.retention();
        assert_eq!(retention.interval, Duration::from_secs(60));
        assert_eq!(retention.batch_size, 25);
        assert_eq!(retention.orphan_grace, Duration::from_secs(120));
    }

    /// A size cap that isn't a size, and a batch size that would make the
    /// sweeper delete nothing forever. Both are typos, and both used to be
    /// silently swallowed: `from_env_vars` parsed with `.ok()`.
    #[rstest]
    #[case("[retention]\nmax_size_gb = 0\n", "retention.max_size_gb")]
    #[case("[retention]\nmax_size_gb = -5\n", "retention.max_size_gb")]
    #[case("[retention]\nbatch_size = 0\n", "retention.batch_size")]
    fn an_impossible_retention_policy_refuses_to_boot(#[case] text: &str, #[case] key: &str) {
        let error =
            resolve(&cli(&[]), no_env, Some(&file(text))).expect_err("an impossible policy");

        assert!(error.to_string().contains(key), "{error}");
    }

    /// Ingest tuning (#5, #8) — rdio-scanner keeps both of these in its database
    /// behind the admin UI, so a headless install cannot set them at all.
    #[test]
    fn ingest_tuning_is_configurable() {
        let config = resolve(
            &cli(&[]),
            no_env,
            Some(&file(
                "[ingest]\ndedup_window_ms = 1500\nauto_populate = false\n",
            )),
        )
        .expect("resolve");

        let ingest = config.ingest();
        assert_eq!(ingest.dedup_window_ms, 1500);
        assert!(!ingest.auto_populate);
    }

    /// The admin surface's policy (#19), settable without a UI — which is the
    /// point, since the UI it gates is the thing you would need it for.
    #[test]
    fn admin_session_and_lockout_policy_is_configurable() {
        let config = resolve(
            &cli(&[]),
            no_env,
            Some(&file(
                "[admin]\nsession_idle_secs = 60\nsession_max_secs = 600\n\
                 lockout_attempts = 2\nlockout_secs = 30\n",
            )),
        )
        .expect("resolve");

        let admin = config.admin();
        assert_eq!(admin.session_idle, Duration::from_secs(60));
        assert_eq!(admin.session_max, Duration::from_secs(600));
        assert_eq!(admin.lockout_attempts, 2);
        assert_eq!(admin.lockout, Duration::from_secs(30));
    }

    /// Web Push (#16). The window is the anti-storm knob and the subject is
    /// what a push service's operator contacts ours through, so both have to be
    /// settable on a headless install.
    #[test]
    fn push_coalescing_and_contact_are_configurable() {
        let config = resolve(
            &cli(&[]),
            no_env,
            Some(&file(
                "[push]\ncoalesce_secs = 60\nttl_secs = 120\nsubject = \"mailto:ops@example.com\"\n",
            )),
        )
        .expect("resolve");

        let push = config.push();
        assert_eq!(push.coalesce, Duration::from_secs(60));
        assert_eq!(push.ttl, Duration::from_secs(120));
        assert_eq!(push.subject, "mailto:ops@example.com");
    }

    /// RFC 8292 requires `sub` to be a `mailto:` or `https:` URI, and some push
    /// services refuse a token without one — which would surface as every
    /// notification silently failing, long after the boot that misconfigured
    /// it.
    #[rstest]
    #[case("ops@example.com")] // an address, not a URI
    #[case("http://example.com/contact")] // not TLS
    #[case("")]
    fn a_contact_that_is_not_a_uri_refuses_to_boot(#[case] subject: &str) {
        let error = resolve(
            &cli(&[]),
            no_env,
            Some(&file(&format!("[push]\nsubject = \"{subject}\"\n"))),
        )
        .expect_err("an unusable contact");

        assert!(error.to_string().contains("push.subject"), "{error}");
        assert!(error.to_string().contains("mailto:"), "{error}");
    }

    /// Zero is a legitimate window — "tell me about everything" — and must not
    /// be mistaken for the impossible values `[admin]` refuses.
    #[test]
    fn a_zero_coalescing_window_is_allowed() {
        let config = resolve(
            &cli(&[]),
            no_env,
            Some(&file("[push]\ncoalesce_secs = 0\n")),
        )
        .expect("resolve");

        assert_eq!(config.push().coalesce, Duration::ZERO);
    }

    /// Every one of these bricks the admin surface at zero — a session already
    /// expired when it is issued, or an address locked out before its first
    /// attempt. There is no "0 disables it" reading to guess at, so the boot
    /// stops and names the key (ADR-0012). rdio would have taken the value and
    /// locked the operator out of their own scanner.
    #[rstest]
    #[case("[admin]\nsession_idle_secs = 0\n", "admin.session_idle_secs")]
    #[case("[admin]\nsession_max_secs = 0\n", "admin.session_max_secs")]
    #[case("[admin]\nlockout_attempts = 0\n", "admin.lockout_attempts")]
    #[case("[admin]\nlockout_secs = 0\n", "admin.lockout_secs")]
    fn an_impossible_admin_policy_refuses_to_boot(#[case] text: &str, #[case] key: &str) {
        let error =
            resolve(&cli(&[]), no_env, Some(&file(text))).expect_err("an impossible policy");

        assert!(error.to_string().contains(key), "{error}");
        assert!(error.to_string().contains("positive"), "{error}");
    }

    /// US 34, and the whole safety property of the feature: a Pi that was never
    /// asked to do DSP must not start doing it because it was upgraded. Off is
    /// the default at every layer — the section absent, the section present and
    /// empty, and the key absent from a section that sets something else.
    #[rstest]
    #[case::no_file(None)]
    #[case::empty_section(Some("[enhancement]\n"))]
    #[case::a_section_that_sets_something_else(Some("[enhancement]\ntarget_lufs = -18.0\n"))]
    fn enhancement_is_off_until_it_is_turned_on(#[case] text: Option<&str>) {
        let config = resolve(&cli(&[]), no_env, text.map(file).as_ref()).expect("resolve");

        assert_eq!(config.enhancement().mode, Mode::Off);
    }

    /// US 36 — settable without a UI, in either spelling, with the environment
    /// over the file. A container is configured by environment and nothing
    /// else, so a setting only the file can reach is a setting Docker cannot
    /// use.
    #[rstest]
    #[case::from_the_file("[enhancement]\nmode = \"normalize\"\n", &[], Mode::Normalize)]
    #[case::from_the_environment("", &[("RADIO_SCOUT_ENHANCEMENT_MODE", "denoise")], Mode::Denoise)]
    #[case::the_environment_wins(
        "[enhancement]\nmode = \"normalize\"\n",
        &[("RADIO_SCOUT_ENHANCEMENT_MODE", "off")],
        Mode::Off
    )]
    fn the_enhancement_mode_comes_from_either_layer(
        #[case] text: &str,
        #[case] vars: &[(&str, &str)],
        #[case] expected: Mode,
    ) {
        let config = resolve(&cli(&[]), env(vars), Some(&file(text))).expect("resolve");

        assert_eq!(config.enhancement().mode, expected);
    }

    /// WAV is the enhanced output because it is the only one that plays on
    /// *every* iOS version with no patent surface — and measured against real
    /// scanner audio (96 kHz AAC at 320 kbps) 8 kHz WAV is still 2.5x smaller
    /// than what recorders send, so universality costs nothing here.
    #[test]
    fn enhanced_audio_is_wav_unless_told_otherwise() {
        let config = resolve(&cli(&[]), no_env, None).expect("resolve");

        assert_eq!(config.enhancement().output, Output::Wav);
    }

    /// ADR-0012: boot says what it is configured to do. Enhancement is the one
    /// setting that changes the *bytes* a listener receives, so "why does my
    /// audio sound different than it used to" has to be answerable from the
    /// log rather than by diffing config files.
    #[rstest]
    #[case(Mode::Off, "enhancement=off")]
    #[case(Mode::Normalize, "enhancement=normalize")]
    fn boot_says_whether_audio_is_enhanced(#[case] mode: Mode, #[case] expected: &str) {
        let capture = LogCapture::start();

        Loaded {
            config: Config {
                enhancement: Enhancement {
                    mode,
                    ..Enhancement::default()
                },
                ..Config::default()
            },
            file: None,
        }
        .log_summary();

        let logged = capture.text();
        assert!(logged.contains(expected), "{logged}");
    }

    /// The two remaining knobs, from either layer: how loud a Call is
    /// normalized to, and how many Calls may be waiting to be enhanced.
    #[test]
    fn the_loudness_target_and_queue_depth_are_configurable() {
        let config = resolve(
            &cli(&[]),
            env(&[("RADIO_SCOUT_ENHANCEMENT_QUEUE_DEPTH", "64")]),
            Some(&file(
                "[enhancement]\ntarget_lufs = -23.0\nqueue_depth = 8\n",
            )),
        )
        .expect("resolve");

        let enhancement = config.enhancement();
        assert_eq!(enhancement.target_lufs, -23.0);
        assert_eq!(enhancement.queue_depth, 64, "the environment is louder");
    }

    /// Both of these brick enhancement rather than merely behaving oddly, so
    /// they stop the boot that wrote them (ADR-0012).
    ///
    /// A queue that admits nothing means enhancement silently never runs — the
    /// same shape as `[admin]`'s zeros, and with no "0 disables it" reading to
    /// guess at, since `mode = "off"` is how you disable it. A loudness target
    /// outside R128's usable span is not a preference but an impossibility:
    /// LUFS is referenced to full scale, so a positive target asks for audio
    /// louder than digital silence-to-clipping allows. The bounds are
    /// `loudnorm`'s own accepted range, so an operator's ffmpeg knowledge
    /// transfers.
    #[rstest]
    #[case::zero_queue("[enhancement]\nqueue_depth = 0\n", "enhancement.queue_depth")]
    #[case::positive_lufs("[enhancement]\ntarget_lufs = 3.0\n", "enhancement.target_lufs")]
    #[case::deafening("[enhancement]\ntarget_lufs = -1.0\n", "enhancement.target_lufs")]
    #[case::inaudible("[enhancement]\ntarget_lufs = -90.0\n", "enhancement.target_lufs")]
    #[case::not_a_number("[enhancement]\ntarget_lufs = nan\n", "enhancement.target_lufs")]
    fn an_impossible_enhancement_policy_refuses_to_boot(#[case] text: &str, #[case] key: &str) {
        let error =
            resolve(&cli(&[]), no_env, Some(&file(text))).expect_err("an impossible policy");

        assert!(error.to_string().contains(key), "{error}");
    }

    /// An output that parses but is not built must **refuse to boot**, from
    /// either layer, and say which ticket lands it.
    ///
    /// The alternative — accepting it and quietly writing WAV — is the failure
    /// mode this whole project exists to avoid: rdio-scanner, told to convert
    /// audio with no ffmpeg installed, warns *once* and then silently passes
    /// every Call through forever (`server/ffmpeg.go:79-86`), so an operator
    /// reading their config believes something is happening that is not.
    ///
    /// It refuses even with `mode = "off"`, when nothing would be encoded at
    /// all: an unusable setting should fail at the boot where it was written,
    /// not three boots later when enhancement is finally switched on.
    #[rstest]
    #[case::in_the_file("[enhancement]\noutput = \"opus\"\n", &[])]
    #[case::in_the_environment("", &[("RADIO_SCOUT_ENHANCEMENT_OUTPUT", "opus")])]
    #[case::even_with_enhancement_off("[enhancement]\nmode = \"off\"\noutput = \"opus\"\n", &[])]
    fn an_output_that_is_not_built_yet_refuses_to_boot(
        #[case] text: &str,
        #[case] vars: &[(&str, &str)],
    ) {
        let error = resolve(&cli(&[]), env(vars), Some(&file(text))).expect_err("opus is unbuilt");

        let message = error.to_string();
        assert!(message.contains("enhancement.output"), "{message}");
        assert!(message.contains("opus"), "{message}");
        assert!(
            message.contains("#100"),
            "an operator must be told where it lands: {message}"
        );
    }

    /// A format nobody has heard of is a different failure from one that exists
    /// but is not built yet, and gets a different message: "here is the set"
    /// rather than "here is the ticket".
    #[test]
    fn an_output_format_that_does_not_exist_refuses_to_boot() {
        let error = resolve(
            &cli(&[]),
            env(&[("RADIO_SCOUT_ENHANCEMENT_OUTPUT", "flac")]),
            None,
        )
        .expect_err("no such format");

        let message = error.to_string();
        assert!(
            message.contains("RADIO_SCOUT_ENHANCEMENT_OUTPUT"),
            "{message}"
        );
        for output in Output::ALL {
            assert!(
                message.contains(&output.to_string()),
                "the message must name every format; `{output}` is missing from {message}"
            );
        }
    }

    /// A mode nobody implements must stop the boot rather than quietly meaning
    /// `off` — an operator who asked for `normalise` and silently got
    /// passthrough would conclude the feature does not work. Both layers, and
    /// both must name the setting *and* every mode there is, so the message is
    /// enough to fix it without reading source.
    #[rstest]
    #[case::in_the_file("[enhancement]\nmode = \"normalise\"\n", &[], CONFIG_FILE_NAME)]
    #[case::in_the_environment(
        "",
        &[("RADIO_SCOUT_ENHANCEMENT_MODE", "loud")],
        "RADIO_SCOUT_ENHANCEMENT_MODE"
    )]
    fn an_unknown_enhancement_mode_refuses_to_boot(
        #[case] text: &str,
        #[case] vars: &[(&str, &str)],
        #[case] written_where: &str,
    ) {
        let error = resolve(&cli(&[]), env(vars), Some(&file(text))).expect_err("an unknown mode");

        // Where they wrote it, and what they could have written instead — the
        // two things that turn a refused boot into a fixed one. Deliberately
        // not the *punctuation*: the file layer reports through serde
        // (``expected one of `off`, …``) and the environment through our own
        // message (`expected "off", …`), and pinning either one's quoting
        // would test the formatter rather than the behaviour. The file's
        // locator is its path plus a line:column, because serde names the bad
        // *variant* rather than the key — the same shape `Backend` has.
        let message = error.to_string();
        assert!(message.contains(written_where), "{message}");
        for mode in Mode::ALL {
            assert!(
                message.contains(&mode.to_string()),
                "the message must name every mode; `{mode}` is missing from {message}"
            );
        }
    }

    /// The defaults are the shipped ones, not a second copy that can drift.
    #[test]
    fn unconfigured_ingest_and_retention_are_the_shipped_defaults() {
        let config = resolve(&cli(&[]), no_env, None).expect("resolve");

        let ingest = config.ingest();
        assert_eq!(
            ingest.dedup_window_ms,
            IngestConfig::default().dedup_window_ms
        );
        assert_eq!(ingest.auto_populate, IngestConfig::default().auto_populate);
        let retention = config.retention();
        let default = RetentionConfig::default();
        assert_eq!(retention.days, default.days);
        assert_eq!(retention.max_size_bytes, default.max_size_bytes);
        assert_eq!(retention.interval, default.interval);
        assert_eq!(retention.batch_size, default.batch_size);
        assert_eq!(retention.orphan_grace, default.orphan_grace);
    }

    /// A negative dedup window would make every Call a duplicate of nothing;
    /// it is a typo, not a policy.
    #[test]
    fn a_negative_dedup_window_refuses_to_boot() {
        let error = resolve(
            &cli(&[]),
            no_env,
            Some(&file("[ingest]\ndedup_window_ms = -1\n")),
        )
        .expect_err("a negative dedup window");

        assert!(
            error.to_string().contains("ingest.dedup_window_ms"),
            "{error}"
        );
    }

    /// Zero, though, is a policy — "no duplicate detection" — and the boundary
    /// the rejection above must not swallow.
    #[test]
    fn a_zero_dedup_window_is_allowed_and_means_off() {
        let config = resolve(
            &cli(&[]),
            no_env,
            Some(&file("[ingest]\ndedup_window_ms = 0\n")),
        )
        .expect("0 disables dedup; it is not an error");

        assert_eq!(config.ingest().dedup_window_ms, 0);
    }

    /// `RUST_LOG` was the only way to turn the logs up (ADR-0011); now the
    /// setting has a home that survives a reboot, and `RUST_LOG` is the
    /// per-invocation override of it.
    #[rstest]
    #[case(&[], &[], None, observability::DEFAULT_DIRECTIVES)]
    #[case(&[], &[], Some("[log]\ndirectives = \"warn\"\n"), "warn")]
    #[case(&[], &[("RUST_LOG", "debug")], Some("[log]\ndirectives = \"warn\"\n"), "debug")]
    #[case(&["--log", "trace"], &[("RUST_LOG", "debug")], None, "trace")]
    #[case(&[], &[("RUST_LOG", "warn,radio_scout::ingest=trace")], None, "warn,radio_scout::ingest=trace")]
    fn log_directives_come_from_all_three_layers(
        #[case] args: &[&str],
        #[case] vars: &[(&str, &str)],
        #[case] text: Option<&str>,
        #[case] expected: &str,
    ) {
        let file = text.map(file);
        let config = resolve(&cli(args), env(vars), file.as_ref()).expect("resolve");

        assert_eq!(config.log.directives, expected);
    }

    /// Directives that `tracing` cannot parse are held to the same standard as
    /// every other setting: an operator who asked for TRACE and silently got
    /// INFO is debugging the wrong thing at 2am.
    #[rstest]
    #[case(&[], Some("[log]\ndirectives = \"==\"\n"), "log.directives")]
    #[case(&[("RUST_LOG", "radio_scout=verbose")], None, "RUST_LOG")]
    fn unparseable_log_directives_refuse_to_boot(
        #[case] vars: &[(&str, &str)],
        #[case] text: Option<&str>,
        #[case] source: &str,
    ) {
        let file = text.map(file);
        let error =
            resolve(&cli(&[]), env(vars), file.as_ref()).expect_err("unparseable directives");

        assert!(error.to_string().contains(source), "{error}");
    }

    /// The operator log surface's level is its **own** setting (#30) — the one
    /// an operator turns down to keep a Pi's database small, independent of how
    /// chatty the console is.
    #[rstest]
    #[case(&[], None, Some(Level::INFO))]
    #[case(&[], Some("[log]\ndatabase_level = \"warn\"\n"), Some(Level::WARN))]
    #[case(&[], Some("[log]\ndatabase_level = \"error\"\n"), Some(Level::ERROR))]
    #[case(&[], Some("[log]\ndatabase_level = \"off\"\n"), None)]
    #[case(&[("RADIO_SCOUT_LOG_DATABASE_LEVEL", "off")], None, None)]
    // The environment is louder than the file, like every other setting.
    #[case(&[("RADIO_SCOUT_LOG_DATABASE_LEVEL", "error")], Some("[log]\ndatabase_level = \"info\"\n"), Some(Level::ERROR))]
    fn the_stored_log_level_comes_from_its_own_setting(
        #[case] vars: &[(&str, &str)],
        #[case] text: Option<&str>,
        #[case] expected: Option<Level>,
    ) {
        let file = text.map(file);
        let config = resolve(&cli(&[]), env(vars), file.as_ref()).expect("resolve");

        assert_eq!(config.log_sink().level, expected);
    }

    /// **ADR-0011 rule 5 as a boot error.** DEBUG and TRACE are the levels a
    /// listener's address may ride on, so the sink has no setting that reaches
    /// them — and an operator who asks is *told*, rather than silently getting
    /// something else. rdio-scanner would have stored them and logged every
    /// listener's IP besides.
    #[rstest]
    #[case(&[], Some("[log]\ndatabase_level = \"debug\"\n"), "log.database_level")]
    #[case(&[], Some("[log]\ndatabase_level = \"trace\"\n"), "log.database_level")]
    #[case(&[], Some("[log]\ndatabase_level = \"verbose\"\n"), "log.database_level")]
    #[case(&[("RADIO_SCOUT_LOG_DATABASE_LEVEL", "debug")], None, "RADIO_SCOUT_LOG_DATABASE_LEVEL")]
    fn a_stored_log_level_that_could_carry_a_listener_refuses_to_boot(
        #[case] vars: &[(&str, &str)],
        #[case] text: Option<&str>,
        #[case] source: &str,
    ) {
        let file = text.map(file);
        let error = resolve(&cli(&[]), env(vars), file.as_ref()).expect_err("a refused level");

        let error = error.to_string();
        assert!(error.contains(source), "{error}");
        // What it should have been, and enough of why to stop the operator
        // going looking for a typo.
        assert!(error.contains("\"warn\""), "{error}");
        assert!(error.contains("rule 5"), "{error}");
    }

    /// The refusal names every level that *is* accepted — spelled out as a
    /// `&'static str`, so nothing but a test keeps it honest as levels change.
    #[test]
    fn the_refused_level_names_every_level_that_works() {
        for (name, _) in LogSinkConfig::LEVELS {
            assert!(
                EXPECTED_DATABASE_LEVEL.contains(name),
                "{name} missing from {EXPECTED_DATABASE_LEVEL:?}"
            );
        }
    }

    /// Stored logs are bounded like the archive is (#30) — their own window,
    /// because `days = 0` (rdio's "keep Calls forever") must not also mean an
    /// unbounded logs table on a Pi.
    #[rstest]
    #[case(&[], None, 30)]
    #[case(&[], Some("[retention]\nlog_days = 3\n"), 3)]
    // 0 is "keep forever", the same reading `days` has.
    #[case(&[], Some("[retention]\nlog_days = 0\n"), 0)]
    #[case(&[("RADIO_SCOUT_RETENTION_LOG_DAYS", "5")], Some("[retention]\nlog_days = 3\n"), 5)]
    fn stored_logs_have_a_retention_window_of_their_own(
        #[case] vars: &[(&str, &str)],
        #[case] text: Option<&str>,
        #[case] expected: u32,
    ) {
        let file = text.map(file);
        let config = resolve(&cli(&[]), env(vars), file.as_ref()).expect("resolve");

        assert_eq!(config.retention().log_days, expected);
        // ...and it is genuinely separate from the archive's window.
        assert_eq!(config.retention().days, Retention::default().days);
    }

    /// Who may be believed when they forward (#28's deferred setting). Bare
    /// addresses and CIDR blocks both, because Docker's bridge is a subnet and
    /// naming its one gateway address is guesswork an operator shouldn't have
    /// to do.
    #[rstest]
    #[case(&[], &[], Some("[server]\ntrusted_proxies = [\"127.0.0.1\"]\n"), "127.0.0.1", true)]
    #[case(&[], &[], Some("[server]\ntrusted_proxies = [\"172.17.0.0/16\"]\n"), "172.17.0.9", true)]
    #[case(&[], &[], Some("[server]\ntrusted_proxies = [\"172.17.0.0/16\"]\n"), "172.18.0.9", false)]
    #[case(&[], &[], Some("[server]\ntrusted_proxies = [\"::1\"]\n"), "::1", true)]
    #[case(&[], &[], Some("[server]\ntrusted_proxies = [\"fd00::/8\"]\n"), "fd12::1", true)]
    // Nothing configured trusts nobody — the shipped posture (ADR-0011).
    #[case(&[], &[], None, "127.0.0.1", false)]
    // The environment takes the list comma-separated, as a container passes it.
    #[case(&[], &[("RADIO_SCOUT_TRUSTED_PROXIES", "10.0.0.1, 172.17.0.0/16")], None, "172.17.0.5", true)]
    #[case(&["--trusted-proxy", "10.0.0.1", "--trusted-proxy", "10.0.0.2"], &[], None, "10.0.0.2", true)]
    // ...and overrides the file rather than adding to it.
    #[case(&[], &[("RADIO_SCOUT_TRUSTED_PROXIES", "10.0.0.1")], Some("[server]\ntrusted_proxies = [\"127.0.0.1\"]\n"), "127.0.0.1", false)]
    fn trusted_proxies_come_from_all_three_layers(
        #[case] args: &[&str],
        #[case] vars: &[(&str, &str)],
        #[case] text: Option<&str>,
        #[case] peer: &str,
        #[case] trusted: bool,
    ) {
        let file = text.map(file);
        let config = resolve(&cli(args), env(vars), file.as_ref()).expect("resolve");

        assert_eq!(config.trusted_proxies().trusts(ip(peer)), trusted);
    }

    /// A typo'd proxy is a security setting that silently doesn't apply, so it
    /// stops the boot — and says which entry.
    #[rstest]
    #[case(&[], Some("[server]\ntrusted_proxies = [\"not-an-ip\"]\n"), "not-an-ip")]
    #[case(&[], Some("[server]\ntrusted_proxies = [\"10.0.0.0/64\"]\n"), "10.0.0.0/64")]
    #[case(&[("RADIO_SCOUT_TRUSTED_PROXIES", "10.0.0.1, nonsense")], None, "nonsense")]
    fn an_unparseable_proxy_refuses_to_boot(
        #[case] vars: &[(&str, &str)],
        #[case] text: Option<&str>,
        #[case] offender: &str,
    ) {
        let file = text.map(file);
        let error = resolve(&cli(&[]), env(vars), file.as_ref()).expect_err("an unparseable proxy");

        let message = error.to_string();
        // What they wrote...
        assert!(message.contains(offender), "{message}");
        // ...and what it should have been, which is the half that makes the
        // message actionable rather than merely accusatory.
        assert!(
            message.contains("CIDR"),
            "the message must say what was expected: {message}"
        );
    }

    /// Which address is the client's, given who the packet came from and what
    /// the header claims.
    ///
    /// rdio-scanner takes `X-Forwarded-For` unconditionally (`main.go:265`), so
    /// on a public instance anyone can forge a recorder's address into the
    /// operator's log. We believe the header only from a peer the operator
    /// named, and then take the **rightmost entry that isn't itself a trusted
    /// proxy** — the leftmost is the one an attacker controls, since anything a
    /// client sends is prepended to by every hop after it.
    #[rstest]
    // Nothing is trusted: the header is ignored however plausible it looks.
    #[case(&[], "203.0.113.9", Some("198.51.100.7"), "203.0.113.9")]
    // The proxy is trusted, so its claim about who it spoke for is believed.
    #[case(&["10.0.0.1"], "10.0.0.1", Some("198.51.100.7"), "198.51.100.7")]
    // Trusted peer, no header: it really was talking to us for itself.
    #[case(&["10.0.0.1"], "10.0.0.1", None, "10.0.0.1")]
    #[case(&["10.0.0.1"], "10.0.0.1", Some(""), "10.0.0.1")]
    // A chain of trusted hops: skip our own infrastructure, keep the first
    // address outside it.
    #[case(&["10.0.0.0/8"], "10.0.0.1", Some("198.51.100.7, 10.0.0.9, 10.0.0.2"), "198.51.100.7")]
    // A forged prefix cannot promote itself: the rightmost untrusted entry is
    // the address the trusted hop actually saw.
    #[case(&["10.0.0.1"], "10.0.0.1", Some("1.2.3.4, 198.51.100.7"), "198.51.100.7")]
    // Junk in the chain makes the walk stop where it meets it — fall back to
    // the peer rather than to whichever entry the junk shifts into place.
    #[case(&["10.0.0.1"], "10.0.0.1", Some("198.51.100.7, oops"), "10.0.0.1")]
    // Junk to the *left* of the answer is never reached: the walk goes
    // right-to-left and stops at the first untrusted hop, so a client that
    // prepends nonsense cannot invalidate what a trusted proxy appended.
    #[case(&["10.0.0.1"], "10.0.0.1", Some("oops, 198.51.100.7"), "198.51.100.7")]
    // Every hop trusted: the leftmost is as close to the client as we can get.
    #[case(&["10.0.0.0/8"], "10.0.0.1", Some("10.0.0.5, 10.0.0.9"), "10.0.0.5")]
    // Whitespace is the header's normal shape.
    #[case(&["10.0.0.1"], "10.0.0.1", Some("  198.51.100.7  "), "198.51.100.7")]
    // IPv6 travels the same road.
    #[case(&["::1"], "::1", Some("2001:db8::5"), "2001:db8::5")]
    fn the_client_address_behind_a_proxy(
        #[case] trusted: &[&str],
        #[case] peer: &str,
        #[case] forwarded_for: Option<&str>,
        #[case] expected: &str,
    ) {
        let proxies = proxies(trusted);

        assert_eq!(proxies.client_ip(ip(peer), forwarded_for), ip(expected));
    }

    proptest::proptest! {
        /// However mangled the header, the address we log is either the peer we
        /// actually got the packet from or something the header really said —
        /// never an invention, and never a panic.
        #[test]
        fn a_resolved_client_address_is_never_invented(
            header in "[ ,.:0-9a-fx]{0,40}",
        ) {
            let proxies = proxies(&["10.0.0.0/8"]);
            let peer = ip("10.0.0.1");
            let resolved = proxies.client_ip(peer, Some(&header));
            proptest::prop_assert!(
                resolved == peer || header.contains(&resolved.to_string()),
                "{resolved} came from neither the peer nor {header:?}"
            );
        }
    }

    /// Zero-config means zero config *file* (US 35): a directory with nothing
    /// in it boots on the defaults.
    #[test]
    fn no_file_anywhere_is_not_an_error() {
        let dir = tempfile::tempdir().expect("tempdir");

        let loaded = load(&cli(&[]), no_env, dir.path()).expect("load");

        assert_eq!(loaded.config.server.port, 3000);
        assert!(loaded.file.is_none());
    }

    /// `radio-scout.toml` beside the working directory is picked up without
    /// being asked for — the file an operator creates with `--write-config`.
    #[test]
    fn a_config_file_in_the_working_directory_is_found() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join(CONFIG_FILE_NAME), "[server]\nport = 8080\n")
            .expect("write config");

        let loaded = load(&cli(&[]), no_env, dir.path()).expect("load");

        assert_eq!(loaded.config.server.port, 8080);
        assert_eq!(
            loaded.file.as_deref(),
            Some(dir.path().join(CONFIG_FILE_NAME).as_path())
        );
    }

    /// A named file wins over the one in the working directory — how a service
    /// unit points at `/etc/radio-scout.toml`.
    #[rstest]
    #[case::by_flag(true)]
    #[case::by_environment(false)]
    fn a_named_config_file_wins_over_the_local_one(#[case] by_flag: bool) {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join(CONFIG_FILE_NAME), "[server]\nport = 8080\n")
            .expect("write local config");
        let named = dir.path().join("elsewhere.toml");
        std::fs::write(&named, "[server]\nport = 9999\n").expect("write named config");
        let named = named.display().to_string();

        let loaded = match by_flag {
            true => load(&cli(&["--config", &named]), no_env, dir.path()),
            false => load(
                &cli(&[]),
                env(&[("RADIO_SCOUT_CONFIG", &named)]),
                dir.path(),
            ),
        }
        .expect("load");

        assert_eq!(loaded.config.server.port, 9999);
    }

    /// A file that was *asked for* and isn't there is a mistake worth stopping
    /// for: silently booting on the defaults is how an operator discovers their
    /// retention window never applied.
    #[test]
    fn a_named_config_file_that_is_missing_refuses_to_boot() {
        let dir = tempfile::tempdir().expect("tempdir");
        let missing = dir.path().join("nope.toml");

        let error = load(
            &cli(&["--config", &missing.display().to_string()]),
            no_env,
            dir.path(),
        )
        .expect_err("a config file that isn't there");

        let message = error.to_string();
        assert!(message.contains("nope.toml"), "{message}");
    }

    /// The template is the documentation an operator actually reads, so it has
    /// to *be* the defaults — not a second copy of them that drifts. Parsing it
    /// back must yield exactly what a scanner with no file at all runs on.
    #[test]
    fn the_template_parses_back_to_the_defaults() {
        let config = resolve(&cli(&[]), no_env, Some(&file(TEMPLATE))).expect("template resolves");

        assert_eq!(config, Config::default());
    }

    /// ...and it has to show *every* key at *its* default, or it is
    /// documentation that lies. Compared line for line against the serialized
    /// defaults, so a setting added to `Config` — or a default changed in the
    /// code — fails here until the template says the same thing.
    ///
    /// Settings with no default (`database.url`, `storage.path`,
    /// `retention.max_size_gb`, `storage.s3.endpoint`) serialize to nothing and
    /// are exempt; the template shows those as examples instead.
    #[test]
    fn the_template_shows_every_key_at_its_default() {
        let serialized = toml::to_string(&Config::default()).expect("serialize defaults");

        for line in serialized.lines().filter(|line| line.contains(" = ")) {
            assert!(
                TEMPLATE.contains(&format!("# {line}")),
                "the template never shows `{line}`:\n{TEMPLATE}"
            );
        }
    }

    /// Every section header too — a `[storage.s3]` nobody knows exists is a
    /// backend nobody can turn on.
    #[test]
    fn the_template_documents_every_section() {
        let serialized = toml::to_string(&Config::default()).expect("serialize defaults");

        for line in serialized.lines().filter(|line| line.starts_with('[')) {
            assert!(
                TEMPLATE.contains(line),
                "the template never mentions {line}:\n{TEMPLATE}"
            );
        }
    }

    /// Writing it is a one-shot: an operator who has already configured
    /// something must not lose it to a mistyped flag.
    #[test]
    fn the_template_is_written_once_and_never_over_an_existing_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(CONFIG_FILE_NAME);

        write_template(&path).expect("write the template");
        assert_eq!(std::fs::read_to_string(&path).expect("read back"), TEMPLATE);

        let error = write_template(&path).expect_err("a second write must refuse");
        assert!(error.to_string().contains(CONFIG_FILE_NAME), "{error}");
        assert_eq!(
            std::fs::read_to_string(&path).expect("read back"),
            TEMPLATE,
            "the existing file must be untouched"
        );
    }

    proptest::proptest! {
        /// Whatever an operator sets, the file they wrote is the file we read:
        /// serializing a configuration and parsing it back is the identity.
        /// (`--write-config` writes the *defaults*; this is the guarantee that
        /// makes the format round-trippable at all.)
        #[test]
        fn a_serialized_configuration_parses_back_unchanged(
            port in 0u16..=u16::MAX,
            days in 0u32..4000,
            gb in proptest::option::of(0.1f64..1024.0),
            dedup in 0i64..10_000,
            auto_populate in proptest::bool::ANY,
            directives in "(info|debug|warn|trace)",
            log_days in 0u32..4000,
            database_level in "(off|error|warn|info)",
        ) {
            let config = Config {
                server: Server { port, ..Default::default() },
                retention: Retention { days, max_size_gb: gb, log_days, ..Default::default() },
                ingest: Ingest { dedup_window_ms: dedup, auto_populate },
                log: Log { directives, database_level },
                ..Default::default()
            };

            let text = toml::to_string(&config).expect("serialize");
            let parsed = resolve(&cli(&[]), no_env, Some(&file(&text)))
                .expect("a configuration we wrote must parse");

            proptest::prop_assert_eq!(parsed, config);
        }
    }

    /// "Why isn't my setting applying?" is unanswerable if boot won't say which
    /// file it read. rdio-scanner ignores a config file it fails to load
    /// without a word (`server/config.go:135`); ours names the file, or says
    /// there wasn't one.
    #[test]
    fn boot_says_which_file_it_read() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join(CONFIG_FILE_NAME), "[server]\nport = 8080\n")
            .expect("write config");
        let capture = LogCapture::start();

        load(&cli(&[]), no_env, dir.path())
            .expect("load")
            .log_summary();

        let logged = capture.text();
        assert!(logged.contains(CONFIG_FILE_NAME), "{logged}");
        assert!(logged.contains("port=8080"), "{logged}");
    }

    /// ...and says so when there is no file, so "running on the defaults" is a
    /// fact an operator can see rather than infer.
    #[test]
    fn boot_says_when_there_was_no_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let capture = LogCapture::start();

        load(&cli(&[]), no_env, dir.path())
            .expect("load")
            .log_summary();

        let logged = capture.text();
        assert!(logged.contains("no configuration file"), "{logged}");
        assert!(logged.contains("port=3000"), "{logged}");
    }

    /// ADR-0011 rule 2, where it bites hardest: a Postgres URL carries a
    /// password, so the summary names the *dialect* and never the URL.
    #[test]
    fn the_summary_never_logs_a_database_password() {
        let dir = tempfile::tempdir().expect("tempdir");
        let capture = LogCapture::start();

        load(
            &cli(&["--database-url", "postgres://rs:hunter2@db.lan/radio_scout"]),
            no_env,
            dir.path(),
        )
        .expect("load")
        .log_summary();

        capture.assert_never_logged("hunter2");
        capture.assert_never_logged("db.lan");
        let logged = capture.text();
        assert!(logged.contains("database=postgres"), "{logged}");
    }

    /// Same rule for the storage credential.
    #[test]
    fn the_summary_never_logs_the_s3_secret() {
        let dir = tempfile::tempdir().expect("tempdir");
        let capture = LogCapture::start();

        load(
            &cli(&["--storage-backend", "s3"]),
            env(&[
                ("RADIO_SCOUT_S3_BUCKET", "calls"),
                ("RADIO_SCOUT_S3_ACCESS_KEY_ID", "GK1234"),
                ("RADIO_SCOUT_S3_SECRET_ACCESS_KEY", "s3cr3t-do-not-log"),
            ]),
            dir.path(),
        )
        .expect("load")
        .log_summary();

        capture.assert_never_logged("s3cr3t-do-not-log");
        let logged = capture.text();
        assert!(logged.contains("storage=s3"), "{logged}");
    }

    /// Every setting reachable from the environment, in one table — because a
    /// container is configured by environment and nothing else, and a variable
    /// that silently isn't read is a setting an operator cannot use.
    #[rstest]
    #[case::backend(&[("RADIO_SCOUT_STORAGE_BACKEND", "filesystem")], |c: &Config| assert_eq!(c.storage.backend, Backend::Filesystem))]
    #[case::path(&[("RADIO_SCOUT_STORAGE_PATH", "/mnt/audio")], |c: &Config| assert_eq!(fs_root(c), PathBuf::from("/mnt/audio")))]
    #[case::region(&[("RADIO_SCOUT_S3_REGION", "eu-west-1")], |c: &Config| assert_eq!(c.storage.s3.region, "eu-west-1"))]
    #[case::endpoint(&[("RADIO_SCOUT_S3_ENDPOINT", "http://garage.lan:3900")], |c: &Config| assert_eq!(c.storage.s3.endpoint.as_deref(), Some("http://garage.lan:3900")))]
    #[case::allow_http(&[("RADIO_SCOUT_S3_ALLOW_HTTP", "true")], |c: &Config| assert!(c.storage.s3.allow_http))]
    #[case::max_size(&[("RADIO_SCOUT_RETENTION_MAX_SIZE_GB", "0.5")], |c: &Config| assert_eq!(c.retention().max_size_bytes, Some(536_870_912)))]
    #[case::days(&[("RADIO_SCOUT_RETENTION_DAYS", "3")], |c: &Config| assert_eq!(c.retention().days, 3))]
    #[case::interval(&[("RADIO_SCOUT_RETENTION_INTERVAL_SECS", "60")], |c: &Config| assert_eq!(c.retention().interval, Duration::from_secs(60)))]
    #[case::batch_size(&[("RADIO_SCOUT_RETENTION_BATCH_SIZE", "25")], |c: &Config| assert_eq!(c.retention().batch_size, 25))]
    #[case::orphan_grace(&[("RADIO_SCOUT_RETENTION_ORPHAN_GRACE_SECS", "120")], |c: &Config| assert_eq!(c.retention().orphan_grace, Duration::from_secs(120)))]
    #[case::dedup(&[("RADIO_SCOUT_INGEST_DEDUP_WINDOW_MS", "1500")], |c: &Config| assert_eq!(c.ingest().dedup_window_ms, 1500))]
    #[case::auto_populate(&[("RADIO_SCOUT_INGEST_AUTO_POPULATE", "false")], |c: &Config| assert!(!c.ingest().auto_populate))]
    #[case::port(&[("RADIO_SCOUT_PORT", "9000")], |c: &Config| assert_eq!(c.server.port, 9000))]
    #[case::base_dir(&[("RADIO_SCOUT_BASE_DIR", "/srv/rs")], |c: &Config| assert_eq!(c.server.base_dir, PathBuf::from("/srv/rs")))]
    #[case::database(&[("RADIO_SCOUT_DATABASE_URL", "postgres://db/rs")], |c: &Config| assert_eq!(c.database_url(), "postgres://db/rs"))]
    #[case::log(&[("RUST_LOG", "trace")], |c: &Config| assert_eq!(c.log.directives, "trace"))]
    #[case::proxies(&[("RADIO_SCOUT_TRUSTED_PROXIES", "10.0.0.1")], |c: &Config| assert!(c.trusted_proxies().trusts(ip("10.0.0.1"))))]
    #[case::session_idle(&[("RADIO_SCOUT_ADMIN_SESSION_IDLE_SECS", "60")], |c: &Config| assert_eq!(c.admin().session_idle, Duration::from_secs(60)))]
    #[case::session_max(&[("RADIO_SCOUT_ADMIN_SESSION_MAX_SECS", "600")], |c: &Config| assert_eq!(c.admin().session_max, Duration::from_secs(600)))]
    #[case::lockout_attempts(&[("RADIO_SCOUT_ADMIN_LOCKOUT_ATTEMPTS", "2")], |c: &Config| assert_eq!(c.admin().lockout_attempts, 2))]
    #[case::lockout_secs(&[("RADIO_SCOUT_ADMIN_LOCKOUT_SECS", "30")], |c: &Config| assert_eq!(c.admin().lockout, Duration::from_secs(30)))]
    #[case::push_coalesce(&[("RADIO_SCOUT_PUSH_COALESCE_SECS", "60")], |c: &Config| assert_eq!(c.push().coalesce, Duration::from_secs(60)))]
    #[case::push_ttl(&[("RADIO_SCOUT_PUSH_TTL_SECS", "120")], |c: &Config| assert_eq!(c.push().ttl, Duration::from_secs(120)))]
    #[case::push_subject(&[("RADIO_SCOUT_PUSH_SUBJECT", "mailto:ops@example.com")], |c: &Config| assert_eq!(c.push().subject, "mailto:ops@example.com"))]
    #[case::enhancement_mode(&[("RADIO_SCOUT_ENHANCEMENT_MODE", "normalize")], |c: &Config| assert_eq!(c.enhancement().mode, Mode::Normalize))]
    #[case::enhancement_lufs(&[("RADIO_SCOUT_ENHANCEMENT_TARGET_LUFS", "-20.5")], |c: &Config| assert_eq!(c.enhancement().target_lufs, -20.5))]
    #[case::enhancement_queue(&[("RADIO_SCOUT_ENHANCEMENT_QUEUE_DEPTH", "16")], |c: &Config| assert_eq!(c.enhancement().queue_depth, 16))]
    // `RADIO_SCOUT_ENHANCEMENT_OUTPUT` is absent on purpose: its only non-default
    // value refuses to boot, so there is nothing here it could resolve *to*.
    // That it is read is proved by `an_output_that_is_not_built_yet_refuses_to_boot`.
    fn every_setting_can_come_from_the_environment(
        #[case] vars: &[(&str, &str)],
        #[case] expected: fn(&Config),
    ) {
        let config = resolve(&cli(&[]), env(vars), None).expect("resolve");

        expected(&config);
    }

    /// Configuration's job ends at a `StorageConfig`; this is the seam where
    /// that meets the store it describes. Both backends have to *open* from
    /// what resolution produced — a config that only type-checks is a boot that
    /// fails at the first Call.
    #[test]
    fn a_resolved_storage_config_opens() {
        let dir = tempfile::tempdir().expect("tempdir");
        let filesystem = resolve(
            &cli(&["--base-dir", &dir.path().display().to_string()]),
            no_env,
            None,
        )
        .expect("resolve");
        let s3 = resolve(
            &cli(&["--storage-backend", "s3"]),
            env(&[
                ("RADIO_SCOUT_S3_BUCKET", "calls"),
                ("RADIO_SCOUT_S3_ACCESS_KEY_ID", "GK1234"),
                ("RADIO_SCOUT_S3_SECRET_ACCESS_KEY", "s3cr3t"),
            ]),
            None,
        )
        .expect("resolve");

        assert!(crate::BlobStore::open(&filesystem.storage()).is_ok());
        // Zero-config creates the audio directory by opening the store.
        assert!(dir.path().join("audio").is_dir());
        let s3 = crate::BlobStore::open(&s3.storage()).expect("open s3");
        // The S3 backend serves by presigned redirect rather than proxying
        // (ADR-0002) — which is how a caller can tell the two apart.
        assert!(s3.is_presigning());
    }

    /// A backend name we don't have is a typo, not a request for a plugin.
    #[test]
    fn an_unknown_storage_backend_refuses_to_boot() {
        let error = resolve(
            &cli(&[]),
            env(&[("RADIO_SCOUT_STORAGE_BACKEND", "gdrive")]),
            None,
        )
        .expect_err("an unknown backend");

        let message = error.to_string();
        assert!(message.contains("gdrive"), "{message}");
        assert!(message.contains("filesystem"), "{message}");
    }

    /// The trust list survives being written back out — `--write-config` and a
    /// hand-edited file have to agree on how an entry is spelled.
    #[test]
    fn trusted_proxies_round_trip_through_the_file() {
        let config = resolve(
            &cli(&[]),
            no_env,
            Some(&file(
                "[server]\ntrusted_proxies = [\"127.0.0.1\", \"172.17.0.0/16\", \"fd00::/8\"]\n",
            )),
        )
        .expect("resolve");

        let text = toml::to_string(&config).expect("serialize");
        // Spelled the way it was written: a bare address stays bare, a block
        // keeps its prefix.
        assert!(text.contains("\"127.0.0.1\""), "{text}");
        assert!(text.contains("\"172.17.0.0/16\""), "{text}");
        let reparsed = resolve(&cli(&[]), no_env, Some(&file(&text))).expect("re-resolve");
        assert_eq!(reparsed, config);
    }

    /// `--log` is validated like every other layer.
    #[test]
    fn unparseable_log_directives_on_the_command_line_refuse_to_boot() {
        let error = resolve(&cli(&["--log", "=="]), no_env, None).expect_err("junk directives");

        assert!(error.to_string().contains("--log"), "{error}");
    }

    /// A `radio-scout.toml` that is there but unreadable stops the boot rather
    /// than being silently skipped — the failure mode rdio-scanner ships
    /// (`server/config.go:135` ignores the load error entirely).
    #[test]
    fn a_local_config_file_that_cannot_be_read_refuses_to_boot() {
        let dir = tempfile::tempdir().expect("tempdir");
        // A directory by that name is present, and is not readable as a file.
        std::fs::create_dir(dir.path().join(CONFIG_FILE_NAME)).expect("mkdir");

        let error = load(&cli(&[]), no_env, dir.path()).expect_err("an unreadable config file");

        assert!(error.to_string().contains(CONFIG_FILE_NAME), "{error}");
    }

    /// An environment variable is configuration too, so it is held to the same
    /// standard as the file: a value that isn't a port refuses to boot and says
    /// which variable held it. The env-var stopgap this replaces parsed with
    /// `.ok()` and silently kept the default.
    #[test]
    fn an_unparseable_env_value_refuses_to_boot() {
        let error = resolve(&cli(&[]), env(&[("RADIO_SCOUT_PORT", "banana")]), None)
            .expect_err("a port that isn't a number must not boot");

        let message = error.to_string();
        assert!(message.contains("RADIO_SCOUT_PORT"), "{message}");
        assert!(message.contains("banana"), "{message}");
    }

    /// A key the file mentions that we do not know is a typo, and a typo'd
    /// setting that silently keeps its default is how an operator loses a month
    /// of Calls. rdio-scanner ignores both the unknown key and the file that
    /// fails to load (`server/config.go:135`); we refuse to boot and say which
    /// key, in which file.
    #[test]
    fn an_unknown_key_refuses_to_boot() {
        let error = resolve(&cli(&[]), no_env, Some(&file("[server]\nprot = 8080\n")))
            .expect_err("an unknown key must not boot");

        let message = error.to_string();
        assert!(message.contains("prot"), "{message}");
        assert!(message.contains("radio-scout.toml"), "{message}");
    }

    /// A service runs from a working directory that is not the operator's and
    /// finds no `radio-scout.toml` beside it, so the two things discovery
    /// resolved have to be written down. Everything else is what was typed.
    #[test]
    fn a_service_bakes_in_the_settings_it_was_installed_with() {
        let cli = Cli::parse_from([
            "radio-scout",
            "service",
            "install",
            "--port",
            "8080",
            "--log",
            "debug",
            "--trusted-proxy",
            "172.17.0.0/16",
        ]);

        let args = service_args(
            &cli,
            Path::new("/var/lib/radio-scout"),
            Some(Path::new("/etc/radio-scout.toml")),
        )
        .expect("nothing secret was given");

        assert_eq!(
            args,
            [
                "--config",
                "/etc/radio-scout.toml",
                "--base-dir",
                "/var/lib/radio-scout",
                "--port",
                "8080",
                "--log",
                "debug",
                "--trusted-proxy",
                "172.17.0.0/16",
            ]
        );
    }

    /// The three things `main.rs` used to decide, where they can be seen: both
    /// paths absolute against the working directory, and the port from the
    /// resolved configuration rather than the flag — it decides whether the
    /// unit is granted the capability to bind a privileged one, so reading it
    /// from the flag would ignore a `port = 80` in the file.
    #[test]
    fn a_service_gets_absolute_paths_and_the_port_that_was_resolved() {
        let cli = Cli::parse_from(["radio-scout", "service", "install"]);
        let loaded = Loaded {
            config: Config {
                server: Server {
                    port: 80,
                    base_dir: PathBuf::from("radio-scout-data"),
                    ..Server::default()
                },
                ..Config::default()
            },
            file: Some(PathBuf::from("radio-scout.toml")),
        };

        let params = service_params(
            &cli,
            &loaded,
            Path::new("/home/pi"),
            PathBuf::from("/usr/local/bin/radio-scout"),
            Some("radio-scout".into()),
        )
        .expect("nothing secret");

        assert_eq!(params.base_dir, PathBuf::from("/home/pi/radio-scout-data"));
        assert_eq!(params.port, 80);
        assert_eq!(params.user.as_deref(), Some("radio-scout"));
        assert_eq!(
            params.args,
            [
                "--config",
                "/home/pi/radio-scout.toml",
                "--base-dir",
                "/home/pi/radio-scout-data",
            ]
        );
    }

    /// The refusal has to survive the extra layer, or the flag it refuses gets
    /// baked in after all.
    #[test]
    fn a_database_url_is_still_refused_through_the_parameters() {
        let cli = Cli::parse_from([
            "radio-scout",
            "service",
            "install",
            "--database-url",
            "postgres://scanner:hunter2@db.example/radio",
        ]);
        let loaded = Loaded {
            config: Config::default(),
            file: None,
        };

        let error = service_params(
            &cli,
            &loaded,
            Path::new("/home/pi"),
            PathBuf::from("/usr/local/bin/radio-scout"),
            None,
        )
        .expect_err("a URL that may carry a password");

        assert!(!error.to_string().contains("hunter2"), "{error}");
    }

    /// Every flag, not just the ones an example happens to use: a setting that
    /// silently fails to carry over is a service running on a different policy
    /// than the command that installed it — and retention is the one where that
    /// costs an archive.
    #[test]
    fn every_setting_flag_carries_over_to_the_service() {
        let cli = Cli::parse_from([
            "radio-scout",
            "service",
            "install",
            "--storage-backend",
            "s3",
            "--retention-days",
            "30",
            "--retention-max-size-gb",
            "12.5",
        ]);

        let args = service_args(&cli, Path::new("/srv/scanner"), None).expect("nothing secret");

        assert_eq!(
            args,
            [
                "--base-dir",
                "/srv/scanner",
                "--storage-backend",
                "s3",
                "--retention-days",
                "30",
                "--retention-max-size-gb",
                "12.5",
            ]
        );
    }

    /// Zero-config is the common case, and there is nothing to point at.
    #[test]
    fn with_no_configuration_file_a_service_still_gets_its_base_directory() {
        let cli = Cli::parse_from(["radio-scout", "service", "install"]);

        let args = service_args(&cli, Path::new("/srv/scanner"), None).expect("nothing secret");

        assert_eq!(args, ["--base-dir", "/srv/scanner"]);
    }

    /// A unit file is world-readable and a database URL routinely carries a
    /// password, so this is ADR-0011 rule 2 one step further out: the credential
    /// never reaches the file, and the refusal never reaches the message.
    #[test]
    fn a_database_url_is_refused_rather_than_written_into_a_service_definition() {
        let cli = Cli::parse_from([
            "radio-scout",
            "service",
            "install",
            "--database-url",
            "postgres://scanner:hunter2@db.example/radio",
        ]);

        let error = service_args(&cli, Path::new("/srv/scanner"), None)
            .expect_err("a URL that may carry a password");

        let message = error.to_string();
        assert!(message.contains("--database-url"), "{message}");
        assert!(message.contains("RADIO_SCOUT_DATABASE_URL"), "{message}");
        assert!(!message.contains("hunter2"), "the secret leaked: {message}");
    }

    /// Every setting flag is `global`, which is the whole reason
    /// `service install --port 8080` reads the way an operator expects.
    #[test]
    fn a_setting_flag_reads_the_same_before_and_after_the_subcommand() {
        let before = Cli::parse_from(["radio-scout", "--port", "8080", "service", "install"]);
        let after = Cli::parse_from(["radio-scout", "service", "install", "--port", "8080"]);

        assert_eq!(before.port, Some(8080));
        assert_eq!(after.port, before.port);
    }

    /// Nothing named means serve, which is what running the binary has always
    /// done and what every existing invocation depends on.
    #[test]
    fn no_subcommand_still_means_serve() {
        let cli = Cli::parse_from(["radio-scout", "--port", "3000"]);

        assert!(cli.command.is_none());
    }
}
