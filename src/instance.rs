//! One module owns "a configured **Instance** is running" (#90).
//!
//! [`start`] takes a resolved [`Config`] and hands back a running Instance: the
//! base directory created, the database open, the store open, the router built,
//! a port bound and served. The handle carries what an outside observer needs —
//! the address it bound, the database and the store — and can be stopped.
//!
//! ## Why this exists
//!
//! The sequence used to exist twice. `main.rs` ran thirteen steps; the
//! integration harness re-implemented twelve of them differently and omitted
//! two entirely — no Retention sweeper, no operator log sink — so the suite was
//! green about an Instance that is not the one that boots, and nothing
//! in-process covered how a configuration becomes a running Instance at all.
//!
//! ## The rule for adding a subsystem
//!
//! A new subsystem is **a configuration section** (ADR-0012 / #87: the section
//! *is* the subsystem's own type), plus an entry in [`Wiring`] **only if
//! something genuinely varies** — and it is wired *here*, never in the binary's
//! own startup. `main.rs` is bootstrap glue that is excluded from coverage; a
//! subsystem wired there is a subsystem no test can reach.
//!
//! [`Wiring`] is deliberately small, and five of its seven entries earn their
//! place by varying between two *real* runs: the object store (a filesystem
//! directory, an S3 bucket, a substitute that can be made to fail), what is
//! composed around the database handle (the same move, one level up — #97),
//! the clock, the credential
//! **sources**, and the draining half of the log sink, which is there because
//! of an ordering rather than a value. The other two are on borrowed time and
//! say so where they are declared: `bind`, because the suite listens on
//! loopback where a scanner listens on every interface, and `heartbeat`, which
//! #94 removes along with the need for it.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use crate::db::Db;
use tokio::task::JoinHandle;
use tracing::{error, info};

use crate::admin::AdminAuth;
use crate::blob::AudioStore;
use crate::config::Config;
use crate::enhance::Enhancer;
use crate::logsink;
use crate::push::Push;
use crate::retention;
use crate::startup;
use crate::worker::{Worker, Workers};
use crate::{AppState, BlobStore, Clock, build_app};

/// Something composed around the database handle before anything is given it —
/// see [`Wiring::decorate_db`].
///
/// `Arc` rather than `Box` because [`Parts`] is cloned on every restart, and
/// `Send + Sync` because an Instance is assembled inside a spawned task.
pub type Decorate = Arc<dyn Fn(Db) -> Db + Send + Sync>;

/// The env file a generated credential is written to when the boot had none to
/// have read one from — beside the database rather than in the working
/// directory, which under systemd or Docker is routinely read-only.
const ENV_FILE_NAME: &str = ".env";

/// What varies between two runs of the same configuration.
///
/// Everything else is a configuration value. Keeping this small is the point:
/// a knob here is a decision that cannot be written down in `radio-scout.toml`,
/// and an operator can only reach what is written down.
/// Fields are private and filled through the setters below, so there is one way
/// to describe a run rather than two that can disagree.
#[derive(Default)]
pub struct Wiring {
    /// The object store to use, when it is not the one `[storage]` describes —
    /// an S3 store built elsewhere, or a decorator that can be made to fail.
    store: Option<Arc<dyn AudioStore>>,
    /// Something to compose around the database handle before the application
    /// is given it.
    ///
    /// A **decorator, not a handle** (#97): opening the database stays where a
    /// boot does it — `db::connect`, migrations and all, inside [`assemble`] —
    /// and this only wraps the result. Handing in a finished handle instead
    /// would let every spawned Instance skip the one step a boot cannot skip,
    /// which is the shape #90 removed from the harness in the first place.
    /// Kept across a restart, because a restart reconnects and the composition
    /// has to survive it. Nothing an operator can write in `radio-scout.toml`
    /// produces one, which is what this list is for.
    db: Option<Decorate>,
    /// Where the three credentials are read from and written to.
    credentials: Credentials,
    /// What time it is (#90). The wall clock unless a caller says otherwise.
    clock: Clock,
    /// Where to listen, when it is not `0.0.0.0:{[server] port}`.
    ///
    /// A socket address rather than a listener, so a **restart** can bind again
    /// — an ephemeral port asked for twice is two different ports, and a handle
    /// that could only be started once would not be a handle you can restart.
    /// The suite asks for loopback; an operator-facing bind-address setting is
    /// a separate ask, and would be `[server]`'s rather than this.
    bind: Option<SocketAddr>,
    /// How often the live feed pings an idle connection (#9).
    ///
    /// The one knob here that varies between *tests* rather than between real
    /// runs, and it is on borrowed time: #94 makes reaping and heartbeat
    /// testable as a table of state and event, and takes this with it.
    heartbeat: Option<Duration>,
    /// The draining half of the operator log sink (#30), when one is on.
    ///
    /// It arrives from outside because of the order it has to be built in: the
    /// sink is made *before* the subscriber — logging starts first, and the
    /// migration lines an operator most wants to read back are written before
    /// there is anywhere to store them — and the queue is drained here, once
    /// the database is open.
    ///
    /// It becomes a [`crate::worker::Worker`] like the other three (#93), but
    /// deliberately **not** one of [`Running`]'s: the subscriber feeding it
    /// belongs to the process rather than to a run, so [`Instance::restart`]
    /// leaves it draining into the same database instead of ending the logging
    /// of everything that outlives the restart. Only [`Instance::stop`] stops
    /// it, and then it drains what it already holds first.
    log_writer: Option<logsink::LogWriter>,
}

impl Wiring {
    /// Use this store rather than opening the one `[storage]` names.
    pub fn store(mut self, store: Arc<dyn AudioStore>) -> Self {
        self.store = Some(store);
        self
    }

    /// Compose `decorate` around the database handle this Instance opens.
    pub fn decorate_db(mut self, decorate: Decorate) -> Self {
        self.db = Some(decorate);
        self
    }

    /// Where this Instance's credentials come from and where a generated one is
    /// written.
    pub fn credentials(mut self, credentials: Credentials) -> Self {
        self.credentials = credentials;
        self
    }

    /// Read the time from here rather than from the machine's clock.
    pub fn clock(mut self, clock: Clock) -> Self {
        self.clock = clock;
        self
    }

    /// Listen here rather than on every interface.
    pub fn bind(mut self, addr: SocketAddr) -> Self {
        self.bind = Some(addr);
        self
    }

    /// Ping idle live-feed connections this often, rather than on the shipped
    /// period.
    pub fn heartbeat(mut self, heartbeat: Duration) -> Self {
        self.heartbeat = Some(heartbeat);
        self
    }

    /// Drain the operator log sink this writer belongs to, once the database is
    /// open.
    pub fn log_writer(self, writer: logsink::LogWriter) -> Self {
        self.maybe_log_writer(Some(writer))
    }

    /// The same, for a caller who may not have one — `[log] database_level =
    /// "off"` makes no sink and therefore no writer, and that is a setting, not
    /// a failure.
    pub fn maybe_log_writer(mut self, writer: Option<logsink::LogWriter>) -> Self {
        self.log_writer = writer;
        self
    }
}

/// Where the three credentials that stay out of `radio-scout.toml` come from,
/// and where a generated one is written (ADR-0008, ADR-0012).
///
/// A **source**, not a value: each field is the raw configured text, exactly as
/// `RADIO_SCOUT_API_KEY` and friends carry it, and `None` means "nothing
/// configured — generate one". Blank counts as unset, which is what
/// `RADIO_SCOUT_API_KEY=` in an env file produces.
///
/// This is an input rather than a `std::env::var` inside [`start`] for two
/// reasons. A test gets a genuinely *provisioned* Instance — a real generated
/// Web Push identity, a real env file in a directory of its own — instead of
/// subsystems assembled by hand around the provisioning step. And process
/// environment is a global: reading it in here would make two Instances in one
/// process impossible to configure differently, and would make every test that
/// touched it racy with every other.
#[derive(Default, Clone)]
pub struct Credentials {
    /// The env file a generated credential is written to. `None` puts it beside
    /// the database, in `base_dir` — under systemd or Docker (#23) the working
    /// directory is routinely `/` or read-only, and a write that failed there
    /// would leave a scanner with no usable ingest key at all.
    pub env_file: Option<PathBuf>,
    /// `RADIO_SCOUT_API_KEY` — registered on every boot, so a recorder's key
    /// survives a restart *and* a wiped database.
    pub ingest_key: Option<String>,
    /// `RADIO_SCOUT_ADMIN_PASSWORD` (#19) — with none configured and none
    /// generatable, the admin surface stays shut.
    pub admin_password: Option<String>,
    /// `RADIO_SCOUT_VAPID_PRIVATE_KEY` (#16) — the one that must be the *same*
    /// next boot, because a browser pins its public half at subscribe time.
    pub vapid_key: Option<String>,
}

/// Written by hand rather than derived, for the reason `blob::S3Config`'s is:
/// three plaintext secrets live in here, and this is exactly the kind of type
/// that ends up in a `{:?}` on an error line an operator then pastes into an
/// issue. ADR-0011 rule 2 has no exception for `Debug`.
///
/// Which ones are *set* is not a secret and is the whole diagnostic — "the
/// admin surface is shut" and "you configured a password I could not read" look
/// identical without it.
impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let set = |value: &Option<String>| match value {
            Some(_) => "<redacted>",
            None => "unset",
        };
        f.debug_struct("Credentials")
            .field("env_file", &self.env_file)
            .field("ingest_key", &set(&self.ingest_key))
            .field("admin_password", &set(&self.admin_password))
            .field("vapid_key", &set(&self.vapid_key))
            .finish()
    }
}

impl Credentials {
    /// The three credentials as this process's environment has them, written
    /// back to `env_file` when one has to be generated.
    ///
    /// `env` is a lookup rather than a call to `std::env::var` for the reason
    /// [`crate::config::resolve`] takes one: it keeps the process-global out of
    /// everything that can be tested.
    pub fn from_env(env_file: Option<PathBuf>, env: impl Fn(&str) -> Option<String>) -> Self {
        Credentials {
            env_file,
            ingest_key: env(startup::INGEST_KEY_VAR),
            admin_password: env(startup::ADMIN_PASSWORD_VAR),
            vapid_key: env(startup::VAPID_KEY_VAR),
        }
    }
}

/// Why an Instance could not be started.
///
/// One variant per step that can fail, because an operator reads this at the
/// moment nothing else works: "could not bind port 3000" and "could not open
/// the database" call for different actions, and a `Box<dyn Error>` would make
/// them the same sentence.
#[derive(Debug)]
pub enum StartError {
    /// `base_dir` could not be created.
    BaseDir {
        path: PathBuf,
        source: std::io::Error,
    },
    /// The database could not be opened or migrated.
    Database(sea_orm::DbErr),
    /// The audio store could not be opened.
    Store(object_store::Error),
    /// The listening port could not be bound — the most common way a boot
    /// fails, so it names the port rather than answering with a bare errno.
    Bind { port: u16, source: std::io::Error },
    /// The server stopped with an error after it had started serving.
    Serve(std::io::Error),
}

impl std::fmt::Display for StartError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StartError::BaseDir { path, source } => {
                write!(f, "could not create {}: {source}", path.display())
            }
            StartError::Database(source) => write!(f, "could not open the database: {source}"),
            StartError::Store(source) => write!(f, "could not open the audio store: {source}"),
            StartError::Bind { port, source } => {
                write!(f, "could not bind port {port}: {source}")
            }
            StartError::Serve(source) => write!(f, "the server stopped: {source}"),
        }
    }
}

impl std::error::Error for StartError {}

/// A running Radio-Scout.
pub struct Instance {
    /// The address it is actually listening on — `[server] port = 0` asks the
    /// OS to choose, and the number it chose is the only one a client can use.
    pub addr: SocketAddr,
    /// The database every handler and worker shares.
    pub db: Db,
    /// The audio store every handler and worker shares.
    pub store: Arc<dyn AudioStore>,
    running: Running,
    /// Every Worker's reading, shared with the `AppState` handlers are given
    /// (#93) — the half of a Worker that has to reach a status surface.
    workers: Workers,
    /// The operator log writer (#30), which belongs to the **process** rather
    /// than to a run: the subscriber feeding it outlives a restart, so a
    /// restart that stopped it would end the logging of everything that
    /// outlives the restart. Owned here all the same, so a full
    /// [`Instance::stop`] drains it rather than leaving it to be dropped.
    log: Option<Worker>,
    /// What it would take to bring this Instance up again — so a **restart** is
    /// stop-then-start on one handle rather than a second Instance assembled
    /// beside the first.
    config: Config,
    parts: Parts,
}

/// Everything a run needs that is *not* its configuration, and that a restart
/// therefore has to remember.
///
/// These travel together everywhere — into [`assemble`], back out on the
/// handle, and into the next `assemble` — so they are one type rather than four
/// parameters that could be passed in the wrong order.
#[derive(Clone, Default)]
struct Parts {
    credentials: Credentials,
    clock: Clock,
    /// Where to listen and how often to ping, neither of which is a setting.
    bind: Option<SocketAddr>,
    heartbeat: Option<Duration>,
    /// What to compose around the database handle, if a caller asked for
    /// anything — kept because a restart re-opens the database and whatever was
    /// composed around it has to survive that.
    db: Option<Decorate>,
}

/// The tasks one run of an Instance owns.
struct Running {
    /// The serving task, until it has been awaited — a `JoinHandle` panics if
    /// it is polled a second time, and both [`Instance::stop`] and
    /// [`Instance::serve_forever`] await this one. `None` means "already
    /// finished", which for both of them is the outcome they wanted.
    server: Option<JoinHandle<Result<(), std::io::Error>>>,
    /// Tells `axum::serve` to stop accepting and let its connections finish.
    /// `None` once it has been used, because a shutdown is sent once.
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
    /// The background workers this run spawned, in the order they were started
    /// — so stopping them is that order reversed (#93).
    workers: Vec<Worker>,
}

impl Instance {
    /// Serve until the process ends — what the binary does with the handle.
    ///
    /// A server task that was cancelled or panicked is not an error to report:
    /// the runtime has already said so, and there is nothing left to serve.
    pub async fn serve_forever(mut self) -> Result<(), StartError> {
        self.finish_serving().await
    }

    /// Wait for the serving task to end, whoever is waiting.
    ///
    /// One place, because a `JoinHandle` panics if it is polled a second time
    /// and there are two callers: [`Instance::stop`], which wants the socket
    /// closed before it returns, and [`Instance::serve_forever`], which is what
    /// the binary blocks on. `None` — already awaited — is the outcome both of
    /// them were asking for.
    async fn finish_serving(&mut self) -> Result<(), StartError> {
        match self.running.server.take() {
            Some(server) => server.await.unwrap_or(Ok(())).map_err(StartError::Serve),
            None => Ok(()),
        }
    }

    /// Stop serving and stop the workers, and don't come back until the port is
    /// free.
    ///
    /// Graceful rather than an abort of the listener: a request already in
    /// flight finishes, and — the part a caller depends on — the socket is
    /// closed by the time this returns, so "the old Instance stopped" is
    /// observable rather than a race.
    pub async fn stop(&mut self) {
        self.stop_run().await;
        // The log writer outlives a *restart* but not a stop: this is the end
        // of the process's logging, so it drains what it already holds rather
        // than dropping the last thing an operator would want to read.
        if let Some(log) = self.log.take() {
            log.stop().await;
        }
    }

    /// Stop everything belonging to **this run** — the server and its workers —
    /// leaving anything that spans runs alone.
    ///
    /// What [`Instance::restart`] stops. The distinction is the log writer's:
    /// the subscriber feeding it belongs to the process, not to a run, so a
    /// restart that stopped it would silently end the logging of everything
    /// that outlives the restart.
    async fn stop_run(&mut self) {
        if let Some(shutdown) = self.running.shutdown.take() {
            // The receiver is gone only if the server task has already ended,
            // which is the outcome being asked for.
            let _ = shutdown.send(());
        }
        // Whatever the server has to say about stopping, it was asked to.
        let _ = self.finish_serving().await;
        // The server first, so nothing new arrives while the workers wind down,
        // then the workers last-started-first — see [`worker::stop_all`].
        crate::worker::stop_all(self.running.workers.drain(..).collect()).await;
    }

    /// Stop, then start again on the same configuration, the same database and
    /// the same store.
    ///
    /// The address changes when `[server] port` is `0` — the OS picks a fresh
    /// one — so the handle's `addr` is re-read rather than assumed. Everything
    /// that survives a real restart survives this one: the archive, the audio,
    /// and the credentials, because they are in the database and the env file
    /// rather than in this process.
    pub async fn restart(&mut self) -> Result<(), StartError> {
        self.restart_with(self.config.clone(), None).await
    }

    /// [`Instance::restart`], with the configuration — and optionally the store
    /// — the *next* boot will have.
    ///
    /// This is a restart after the operator edited something: turning
    /// enhancement on, changing where audio goes. `None` keeps the store this
    /// Instance is already using, which is what an unchanged `[storage]` means.
    pub async fn restart_with(
        &mut self,
        config: Config,
        store: Option<Arc<dyn AudioStore>>,
    ) -> Result<(), StartError> {
        self.stop_run().await;
        let store = store.unwrap_or_else(|| self.store.clone());
        // A fresh registry: the run's three Workers are new, with new meters,
        // and carrying the old readings over would be a status page reporting a
        // queue that no longer exists. The log writer is the exception — it is
        // still the same running Worker — so it is re-registered first, which
        // is also where it belongs, being the oldest.
        let workers = Workers::default();
        if let Some(log) = &self.log {
            workers.register(log.name(), log.meter());
        }
        let run = assemble(
            config.clone(),
            self.parts.clone(),
            None,
            Some(store),
            workers,
        )
        .await?;
        self.addr = run.addr;
        self.db = run.db;
        self.store = run.store;
        self.running = run.running;
        self.workers = run.workers;
        self.config = config;
        Ok(())
    }

    /// The configuration this Instance is running on.
    pub fn config(&self) -> &Config {
        &self.config
    }

    /// Every Worker's reading, by name (#93) — what an Operator-facing status
    /// surface shows, and what a test awaits instead of sleeping.
    ///
    /// The readings, not the handles: stopping belongs to this Instance, which
    /// is the only thing that knows a run is over, while the readings are
    /// shared with `AppState` so a handler can serve them.
    pub fn workers(&self) -> &Workers {
        &self.workers
    }
}

/// Bring a configured Instance up, and hand back a handle onto it.
pub async fn start(config: Config, wiring: Wiring) -> Result<Instance, StartError> {
    let parts = Parts {
        credentials: wiring.credentials,
        clock: wiring.clock,
        bind: wiring.bind,
        heartbeat: wiring.heartbeat,
        db: wiring.db,
    };
    assemble(
        config,
        parts,
        wiring.log_writer,
        wiring.store,
        Workers::default(),
    )
    .await
}

/// One run: everything from the base directory to a bound port.
///
/// `workers` arrives from outside rather than being made here because a
/// **restart** carries one Worker across — the log writer — and it has to be
/// registered before the run's own, so a status surface still lists them oldest
/// first.
async fn assemble(
    config: Config,
    parts: Parts,
    log_writer: Option<logsink::LogWriter>,
    store: Option<Arc<dyn AudioStore>>,
    workers: Workers,
) -> Result<Instance, StartError> {
    let base_dir = config.server.base_dir.clone();
    std::fs::create_dir_all(&base_dir).map_err(|source| StartError::BaseDir {
        path: base_dir.clone(),
        source,
    })?;

    let db = crate::db::connect(&config.database_url())
        .await
        .map_err(StartError::Database)?;
    let db = match &parts.db {
        Some(decorate) => decorate(db),
        None => db,
    };

    // Now there is somewhere to put them, everything logged since boot — the
    // migrations included — is written out, and the sink keeps up from here.
    let log = log_writer.map(|writer| workers.adopt(writer.start(db.clone())));

    // The three credentials that stay out of the TOML because first run
    // *writes* them (ADR-0008). All of it happens before a port is bound, so
    // there is no window in which the server answers unprovisioned.
    let env_file = parts
        .credentials
        .env_file
        .clone()
        .unwrap_or_else(|| base_dir.join(ENV_FILE_NAME));
    let ingest_key = startup::provision_ingest_key(
        &db,
        parts.credentials.ingest_key.as_deref(),
        &env_file,
        parts.clock.now_ms(),
    )
    .await
    .map_err(StartError::Database)?;
    startup::log_ingest_key(&ingest_key);

    let admin =
        startup::provision_admin_password(parts.credentials.admin_password.as_deref(), &env_file);
    startup::log_admin_password(&admin);

    // Unlike the other two, an identity that cannot be read back leaves push
    // *off*: a new key every boot would silently orphan every subscription a
    // browser had already pinned to the old one.
    let vapid = startup::provision_vapid_key(parts.credentials.vapid_key.as_deref(), &env_file);
    startup::log_vapid_key(&vapid);
    let push = match vapid.key() {
        Some(key) => Push::new(key, config.push.clone()),
        None => Push::disabled(),
    };

    let audio = match store {
        Some(store) => store,
        None => Arc::new(BlobStore::open(&config.storage()).map_err(StartError::Store)?),
    };

    // Retention (#10): bound the archive so the disk can't fill. Sweeps once
    // now and then on its interval, for the life of the Instance.
    let retention = config.retention.clone();
    retention.log();
    let mut running =
        vec![workers.adopt(
            retention::Sweeper::new(db.clone(), audio.clone(), retention, parts.clock).start(),
        )];

    let mut state = AppState::new(audio.clone(), db.clone(), config.ingest.clone());
    state.workers = workers.clone();
    if let Some(heartbeat) = parts.heartbeat {
        state.live = crate::live::LiveFeed::with_heartbeat(heartbeat);
    }
    state.trusted_proxies = config.trusted_proxies();
    state.admin = AdminAuth::provisioned(&admin, config.admin.clone());
    state.push = push;
    state.enhancer = Enhancer::from_config(config.enhancement.clone());
    state.clock = parts.clock;
    // Notifications ride the live-feed fanout (#16), so an ingest never waits
    // on a push service. An Instance with no identity spawns nothing.
    running.extend(crate::push::spawn(state.clone()).map(|worker| workers.adopt(worker)));
    // Enhancement (#20) runs off its own queue, behind ingest rather than in
    // it. With `[enhancement] mode = "off"` — what ships — this spawns nothing,
    // and the first thing it does when it is on is pick up whatever a previous
    // process was part-way through.
    running.extend(crate::enhance::spawn(state.clone()).map(|worker| workers.adopt(worker)));
    let app = build_app(state);

    let bind = parts
        .bind
        .unwrap_or_else(|| SocketAddr::from(([0, 0, 0, 0], config.server.port)));
    let port = bind.port();
    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .map_err(|source| StartError::Bind { port, source })?;
    let addr = listener
        .local_addr()
        .map_err(|source| StartError::Bind { port, source })?;
    let base_dir = base_dir.display();
    // On one line deliberately: `tracing`'s expansion leaves a zero-count
    // region on each field's own line, so a wrapped call reads as untested
    // however hard the line is asserted on — and this one is, in
    // `tests/instance.rs`.
    info!(%addr, port = addr.port(), %base_dir, "radio-scout listening");

    // With connect info: the request log (#28) names the host an ingest came
    // from, which is the diagnostic that matters when a recorder says it is
    // uploading and the archive disagrees.
    let (tell_to_stop, stop) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .with_graceful_shutdown(async move {
            // A dropped sender means the handle is gone, which is also a
            // reason to stop.
            let _ = stop.await;
        })
        .await
        .inspect_err(|error| error!(%error, "the server stopped"))
    });

    Ok(Instance {
        addr,
        db,
        store: audio,
        running: Running {
            server: Some(server),
            shutdown: Some(tell_to_stop),
            workers: running,
        },
        workers,
        log,
        config,
        parts,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::ErrorKind;

    /// The binary reads its credentials out of the process environment; the
    /// *library* reads them out of a lookup, so the one global that would make
    /// two Instances in one process impossible to configure differently stays
    /// at the edge (ADR-0012's #90 amendment).
    #[test]
    fn credentials_come_from_the_lookup_they_are_given() {
        let credentials =
            Credentials::from_env(Some(PathBuf::from("/srv/.env")), |name| match name {
                startup::INGEST_KEY_VAR => Some("a-key".to_string()),
                startup::ADMIN_PASSWORD_VAR => Some("a-password".to_string()),
                _ => None,
            });

        assert_eq!(
            credentials.env_file.as_deref(),
            Some(std::path::Path::new("/srv/.env"))
        );
        assert_eq!(credentials.ingest_key.as_deref(), Some("a-key"));
        assert_eq!(credentials.admin_password.as_deref(), Some("a-password"));
        assert_eq!(
            credentials.vapid_key, None,
            "a variable the environment does not set is unset, not blank"
        );
    }

    /// `Debug` is written by hand precisely so a `{:?}` — in a `?` chain, an
    /// `assert!` message, a future panic handler — cannot be the thing that
    /// leaks a credential (ADR-0011 rule 2), the same bargain
    /// `startup::AdminPassword` and `blob::S3Config` make. All three secrets,
    /// because a redaction that covers two of three is not one.
    #[test]
    fn debugging_the_credential_sources_never_shows_one() {
        let credentials = Credentials::from_env(Some(PathBuf::from("/srv/.env")), |_| {
            Some("s3cr3t-do-not-print".to_string())
        });

        let rendered = format!("{credentials:?}");

        assert!(!rendered.contains("s3cr3t-do-not-print"), "{rendered}");
        // ...and *which* are set still comes through, because "the admin
        // surface is shut" and "I could not read the password you configured"
        // are different problems and this is how they are told apart.
        assert_eq!(rendered.matches("<redacted>").count(), 3, "{rendered}");
        assert!(rendered.contains("/srv/.env"), "{rendered}");
        assert!(
            format!("{:?}", Credentials::default()).contains("unset"),
            "an unconfigured credential reads as unset, not as a redacted one"
        );
    }

    /// Every way a boot can fail says what the operator has to act on. The
    /// integration tests prove each *arises*; this proves each one reads as
    /// something to do (ADR-0011 rule 4), including the one that can only
    /// happen after the port was successfully bound.
    #[test]
    fn every_start_failure_names_what_to_act_on() {
        let denied = || std::io::Error::new(ErrorKind::PermissionDenied, "denied");
        let cases = [
            (
                StartError::BaseDir {
                    path: PathBuf::from("/srv/radio-scout"),
                    source: denied(),
                },
                "/srv/radio-scout",
            ),
            (
                StartError::Database(sea_orm::DbErr::Custom("no such file".into())),
                "no such file",
            ),
            (
                StartError::Store(object_store::Error::NotImplemented {
                    operation: "put".to_string(),
                    implementer: "LocalFileSystem".to_string(),
                }),
                "audio store",
            ),
            (
                StartError::Bind {
                    port: 3000,
                    source: denied(),
                },
                "3000",
            ),
            (StartError::Serve(denied()), "the server stopped"),
        ];

        for (error, expected) in cases {
            let message = error.to_string();
            assert!(message.contains(expected), "{message:?} omits {expected:?}");
        }
    }
}
