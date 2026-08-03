//! How a configuration becomes a running Instance (#90).
//!
//! Everything else in the suite starts from `common::TestApp` and asks what a
//! running Radio-Scout *does*. This binary asks the question one level down:
//! given a resolved [`Config`], does `instance::start` actually assemble the
//! Instance the binary boots — the credentials provisioned, the sweeper
//! sweeping, the sink draining, in an order an operator could observe?
//!
//! It is deliberately written against `instance::start` rather than against the
//! harness, because the harness is now one of its two callers and cannot be the
//! thing that proves it.

mod common;

use clap::Parser;
use common::CallUpload;
use radio_scout::blob::AudioStore;
use radio_scout::config::{self, Cli, Config};
use radio_scout::db::entities::call;
use radio_scout::db::repo::{self, NewCall};
use radio_scout::instance::{self, Credentials, Wiring};
use radio_scout::logsink::{self, StoredLevel};
use radio_scout::retention;
use radio_scout::startup;
use radio_scout::webpush::VapidKey;
use radio_scout::{BlobStore, Clock};
use sea_orm::{EntityTrait, PaginatorTrait};

/// A configuration that runs entirely inside `dir`: an ephemeral port, a
/// database and an audio store under it, and — since credential sources are
/// inputs rather than process-environment reads — an env file of its own.
///
/// Postgres when the run was given one (#22), so this binary is dual-dialect
/// like the rest of the suite.
async fn config_in(dir: &tempfile::TempDir) -> Config {
    let mut config = Config::default();
    config.server.port = 0;
    config.server.base_dir = dir.path().to_path_buf();
    if let Some(server) = common::postgres_server() {
        config.database.url = Some(common::create_test_database(&server).await);
    }
    config
}

/// What this Instance's own env file pins `var` to — the operator's only copy
/// of a generated credential, and so the only way a test can present one. It is
/// `base_dir/.env`, exactly where the binary puts it when there was no env file
/// to have read one from.
fn saved(dir: &tempfile::TempDir, var: &str) -> String {
    common::env_value(&dir.path().join(common::ENV_FILE), var)
}

/// An absolute URL for `path` on this Instance.
fn url(instance: &instance::Instance, path: &str) -> String {
    format!("http://{}{path}", instance.addr)
}

/// A client logged in to this Instance's admin surface with the password its
/// own boot generated — the only credential anybody has, and the one an
/// operator would read out of the env file.
async fn admin_client(instance: &instance::Instance, dir: &tempfile::TempDir) -> reqwest::Client {
    let client = reqwest::Client::builder()
        .cookie_store(true)
        .build()
        .expect("client");
    let response = client
        .post(url(instance, "/api/admin/login"))
        .json(&serde_json::json!({ "password": saved(dir, startup::ADMIN_PASSWORD_VAR) }))
        .send()
        .await
        .expect("login");
    assert_eq!(
        response.status(),
        200,
        "the admin surface refused the password this boot generated"
    );
    client
}

/// POST a synthetic Call from `talkgroup`, timestamped now — a recorder's own
/// shape, and recent enough that the Retention sweeper this Instance runs has
/// no opinion about it.
async fn upload(
    client: &reqwest::Client,
    instance: &instance::Instance,
    key: &str,
    talkgroup: i64,
) {
    let response = client
        .post(url(instance, "/api/call-upload"))
        .multipart(
            CallUpload::new()
                .key(key)
                .talkgroup(talkgroup)
                .at(radio_scout::now_ms())
                .into_form(),
        )
        .send()
        .await
        .expect("upload");
    assert_eq!(response.status(), 200);
    assert!(
        response
            .text()
            .await
            .expect("body")
            .contains("successfully")
    );
}

/// Assert the Logs view (#30) carries an event whose message is `needle`.
///
/// The sink is asynchronous by design — a log call never waits on a database —
/// so this waits, on the Workers' own idle signal (#93) rather than by polling
/// the endpoint. An event is owed from the moment it was offered to the sink,
/// so a settled Instance has written everything anything had said by then.
async fn assert_logged(instance: &instance::Instance, client: &reqwest::Client, needle: &str) {
    instance.workers().idle().await;
    let page: serde_json::Value = client
        .get(url(instance, "/api/admin/logs?limit=500"))
        .send()
        .await
        .expect("logs")
        .json()
        .await
        .expect("a logs page");
    assert!(
        page["results"]
            .as_array()
            .expect("a results array")
            .iter()
            .any(|event| event["message"] == needle),
        "no stored log event ever said {needle:?}; the page holds {page:#}"
    );
}

/// The whole of the ticket in one assertion: hand the module a configuration
/// and get back something serving on a port.
///
/// Before this, the sequence existed twice — thirteen steps in `main.rs` and
/// twelve re-implemented differently in the harness — so nothing in-process
/// covered how a configuration becomes a running Instance at all.
#[tokio::test]
async fn a_resolved_configuration_becomes_something_serving() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let instance = instance::start(config_in(&tmp).await, Wiring::default())
        .await
        .expect("start");

    let response = reqwest::get(format!("http://{}/healthz", instance.addr))
        .await
        .expect("GET /healthz");

    assert_eq!(response.status(), 200);
    assert_eq!(response.text().await.expect("body"), "ok");
    // `--port 0` asks the OS for a port; the handle carries the one it got,
    // because that is the only number a client can connect to.
    assert_ne!(instance.addr.port(), 0);
}

/// **Provisioning happens before serving**, and this is how that is observable
/// rather than asserted by a comment: the *first* request the bound port ever
/// accepts already authenticates with the credentials this boot generated.
///
/// If any of the three were provisioned after `axum::serve` started, there
/// would be a window — however short — in which the port answers and the
/// credential does not exist. A test that logged in "eventually" would pass
/// straight through it.
#[tokio::test]
async fn the_first_request_the_port_accepts_is_already_provisioned() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let instance = instance::start(config_in(&tmp).await, Wiring::default())
        .await
        .expect("start");

    // First run wrote all three, and the env file is the operator's only copy.
    let key = saved(&tmp, startup::INGEST_KEY_VAR);
    let password = saved(&tmp, startup::ADMIN_PASSWORD_VAR);
    let client = reqwest::Client::new();

    let ingest = client
        .post(url(&instance, "/api/call-upload"))
        .multipart(CallUpload::new().key(&key).into_form())
        .send()
        .await
        .expect("upload");
    assert_eq!(ingest.status(), 200);
    assert!(
        ingest.text().await.expect("body").contains("successfully"),
        "the generated ingest key did not authorize"
    );

    let login = client
        .post(url(&instance, "/api/admin/login"))
        .json(&serde_json::json!({ "password": password }))
        .send()
        .await
        .expect("login");
    assert_eq!(
        login.status(),
        200,
        "the generated admin password was not set"
    );

    // ...and the Web Push identity, whose public half is what a browser pins.
    let served = client
        .get(url(&instance, "/api/push/key"))
        .send()
        .await
        .expect("push key")
        .json::<serde_json::Value>()
        .await
        .expect("json");
    let generated = VapidKey::parse(&saved(&tmp, startup::VAPID_KEY_VAR)).expect("a saved key");
    assert_eq!(served["key"], generated.public_base64url());
}

/// **The sink is installed before the subscriber, and drained after the
/// database opens** — observable because a line written before there was
/// anywhere to put it still reaches the operator's Logs view.
///
/// `db::connect` logs its migrations, and it runs before any writer could have
/// a database to write to. Those lines wait in the queue. If the Instance did
/// not drain the queue it was handed — or if the sink were installed after the
/// subscriber, the way a harness that builds its own logging would do it — the
/// only place they would ever appear is the console, which an operator with no
/// shell cannot read.
#[tokio::test]
async fn boot_lines_written_before_the_database_existed_reach_the_operator_log() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (sink, writer) = logsink::channel(StoredLevel::default()).expect("an enabled sink");
    // Exactly the binary's order: the channel, then the subscriber, then the
    // Instance that opens a database and drains what has piled up.
    let capture = common::logs::LogCapture::with_sink(sink);

    let instance = instance::start(config_in(&tmp).await, Wiring::default().log_writer(writer))
        .await
        .expect("start");

    let logs = admin_client(&instance, &tmp).await;
    assert_logged(&instance, &logs, "applying schema migrations").await;

    // ...and the line every boot ends with says where to find the thing that
    // just started: the port it actually got, and the directory it is keeping
    // the archive in. Both are what an operator asks first.
    let listening = capture.only_line_containing("radio-scout listening");
    assert!(
        listening.contains(&format!("port={}", instance.addr.port())),
        "{listening}"
    );
    assert!(
        listening.contains(&tmp.path().display().to_string()),
        "{listening}"
    );
}

/// **The Retention sweeper runs**, like the real one — the first of the two
/// steps the old harness omitted entirely.
///
/// A Call from 1970 is already in the archive when the Instance opens it, and
/// the sweep runs *immediately* at boot rather than one interval in (rdio's
/// hourly ticker first fires at +1h, so a box that restarts often never prunes
/// at all). Nothing here waits on an interval: the assertion is about the boot
/// sweep.
#[tokio::test]
async fn the_retention_sweeper_prunes_an_aged_out_call_at_boot() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let config = config_in(&tmp).await;
    // An archive with one Call in it, put there before anything is serving.
    let db = radio_scout::db::connect(&config.database_url())
        .await
        .expect("db");
    let store = BlobStore::open(&config.storage()).expect("store");
    store
        .put("old/call.wav", bytes::Bytes::from_static(b"RIFF"))
        .await
        .expect("put");
    repo::insert_call(
        &db,
        // 1970, and `[retention] days` defaults to 7.
        &NewCall::new(1, 2, 0),
        common::audio_at("old/call.wav"),
        &repo::Resolved::default(),
        true,
        0,
    )
    .await
    .expect("seed an aged-out Call");

    let instance = instance::start(config, Wiring::default())
        .await
        .expect("start");

    instance.workers().idle().await;

    assert_eq!(
        call::Entity::find()
            .count(&instance.db)
            .await
            .expect("count calls"),
        0,
        "the boot sweep never ran: a Call from 1970 is still in the archive"
    );
    assert!(
        instance
            .store
            .size("old/call.wav")
            .await
            .expect("stat")
            .is_none(),
        "the row was pruned but its audio was left on the disk"
    );
}

/// **...and it keeps sweeping**, which is a different claim and until #93 an
/// untestable one.
///
/// The boot sweep above is the ticker's *first* tick, which completes
/// immediately. Everything after it — that there is a scheduler at all, that it
/// fires again, that an Instance left running for a week goes on bounding its
/// archive — was unreachable from an integration test, because "it swept again"
/// is not something an archive can be asked: a second sweep over an archive
/// already inside its policy looks exactly like no second sweep.
///
/// A Worker can be asked. `settled_at_least` counts sweeps, so the Call is
/// seeded *after* the Instance is up and the wait is for two more sweeps to
/// have finished — two, so that at least one of them began after the insert.
#[tokio::test]
async fn the_sweeper_goes_on_sweeping_after_the_boot_sweep() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let mut config = config_in(&tmp).await;
    // Short enough that the test is not waiting, long enough that a dual-dialect
    // run is not sweeping Postgres in a tight loop.
    config.retention.interval = std::time::Duration::from_millis(20);

    let instance = instance::start(config, Wiring::default())
        .await
        .expect("start");
    let sweeper = instance
        .workers()
        .meter(retention::WORKER)
        .expect("a running Instance has a retention worker");
    sweeper.settled_at_least(1).await;

    // An aged-out Call the boot sweep cannot have seen, because it did not
    // exist yet.
    repo::insert_call(
        &instance.db,
        // 1970, and `[retention] days` defaults to 7.
        &NewCall::new(1, 2, 0),
        None,
        &repo::Resolved::default(),
        true,
        0,
    )
    .await
    .expect("seed an aged-out Call");

    // Counted *after* the insert, never before: read it first and both of the
    // sweeps waited for could be ones that had already looked. Two more, so the
    // second of them provably began after the row was there — it cannot start
    // until the first finishes, and the first finishes after this reading.
    let seeded_at = sweeper.load().done;
    sweeper.settled_at_least(seeded_at + 2).await;

    assert_eq!(
        call::Entity::find()
            .count(&instance.db)
            .await
            .expect("count calls"),
        0,
        "a Call that aged out after boot survived two sweeps: the ticker fired once and stopped"
    );
}

/// **Stopping stops the Workers too** (#93), and does not come back until they
/// have actually ended.
///
/// Before this they were `abort()`ed and never joined, so `stop` returned while
/// a sweeper could still be mid-transaction on the database the caller was
/// about to reopen. There is no way to observe a task from outside except by
/// what it still owes and whether it is still going — so that is the assertion:
/// every Worker has settled and none is running.
#[tokio::test]
async fn stopping_an_instance_stops_and_joins_every_worker() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let mut config = config_in(&tmp).await;
    // Something to stop *while it is working*: a sweeper on a tight interval,
    // and an enhancement worker, which the shipped default does not run.
    config.retention.interval = std::time::Duration::from_millis(20);
    config.enhancement.mode = radio_scout::enhance::Mode::Normalize;
    // ...and the log writer, which is the fourth and the odd one out: it
    // belongs to the process rather than to a run, so only a full `stop` — not
    // the `restart` below this test — is allowed to end it.
    let (sink, writer) = logsink::channel(StoredLevel::default()).expect("an enabled sink");
    let _capture = common::logs::LogCapture::with_sink(sink);
    let mut instance = instance::start(config, Wiring::default().log_writer(writer))
        .await
        .expect("start");
    let names: Vec<&str> = instance
        .workers()
        .loads()
        .iter()
        .map(|reading| reading.name)
        .collect();
    assert_eq!(names.len(), 4, "all four Workers are running: {names:?}");

    instance.stop().await;

    assert!(
        instance
            .workers()
            .loads()
            .iter()
            .all(|reading| reading.load.is_idle()),
        "a stopped Instance still owes work: {:?}",
        instance.workers().loads()
    );
    // ...and the port really is free, which is the thing a caller of `stop`
    // actually depends on.
    assert!(
        tokio::net::TcpListener::bind(instance.addr).await.is_ok(),
        "stop returned before the port was released"
    );
}

/// **A restart is stop-then-start on one handle**, and it is a real restart:
/// the old port stops answering, the archive is still there, and the ingest key
/// the first boot generated still authorizes — because it went to the env file,
/// which is what makes a recorder survive a restart at all (ADR-0008).
///
/// Before this, "restart" in the suite meant standing up a second app on a
/// hand-shared database URL and a hand-shared store directory, four lines of
/// preamble at a time, with nothing stopping the first one.
#[tokio::test]
async fn a_restart_keeps_the_archive_and_the_credentials_and_frees_the_old_port() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let mut instance = instance::start(config_in(&tmp).await, Wiring::default())
        .await
        .expect("start");
    let key = saved(&tmp, startup::INGEST_KEY_VAR);
    let client = reqwest::Client::new();
    upload(&client, &instance, &key, 100).await;
    let before = instance.addr;

    instance.restart().await.expect("restart");

    assert_ne!(
        instance.addr, before,
        "a stopped Instance released its port"
    );
    assert!(
        reqwest::get(format!("http://{before}/healthz"))
            .await
            .is_err(),
        "the old port is still answering, so the first Instance never stopped"
    );
    // The same key, on the new port, against the archive the first one built.
    upload(&client, &instance, &key, 200).await;
    assert_eq!(
        call::Entity::find()
            .count(&instance.db)
            .await
            .expect("count calls"),
        2
    );
}

/// **A setting written in a configuration file changes what the running
/// Instance does** — the whole path, in-process: discovery finds the file,
/// `resolve` turns it into a [`Config`], and `start` hands that to the
/// subsystem whose own type the section is (#87).
///
/// `[admin] lockout_attempts` is the setting under test because its effect is
/// unambiguous at the HTTP boundary and it is nobody's default: one wrong
/// password, and the next attempt is refused rather than merely wrong.
/// Everything else about this Instance — the port, the base directory, the
/// database — is written in the same file, so nothing is reaching around it.
#[tokio::test]
async fn a_setting_written_in_a_configuration_file_changes_what_the_instance_does() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let database = match common::postgres_server() {
        Some(server) => format!("url = {:?}\n", common::create_test_database(&server).await),
        None => String::new(),
    };
    std::fs::write(
        tmp.path().join("radio-scout.toml"),
        format!(
            "[server]\nport = 0\nbase_dir = {:?}\n\n[database]\n{database}\n[admin]\nlockout_attempts = 1\n",
            tmp.path().to_str().expect("a utf-8 temp path"),
        ),
    )
    .expect("write a configuration file");

    // Discovery, exactly as a boot does it: no `--config`, nothing in the
    // environment, a `radio-scout.toml` in the working directory.
    let loaded = config::load(&Cli::parse_from(["radio-scout"]), |_| None, tmp.path())
        .expect("a configuration");
    assert_eq!(
        loaded.file.as_deref(),
        Some(tmp.path().join("radio-scout.toml").as_path()),
        "the file under test is not the one that was read"
    );
    let instance = instance::start(loaded.config, Wiring::default())
        .await
        .expect("start");

    let client = reqwest::Client::new();
    let wrong = |password: &'static str| {
        client
            .post(url(&instance, "/api/admin/login"))
            .json(&serde_json::json!({ "password": password }))
            .send()
    };
    assert_eq!(wrong("nope").await.expect("login").status(), 401);
    assert_eq!(
        wrong("nope-again").await.expect("login").status(),
        429,
        "`lockout_attempts = 1` never reached the admin surface"
    );
}

/// **A shut admin surface is a provisioning outcome, not a knob.** An env file
/// that cannot be written means the generated password exists only inside this
/// process, and a credential only the server ever saw would lock the operator
/// out of their own configuration while leaving them convinced they had one —
/// so nothing is set and nothing authenticates.
///
/// The harness used to reach this state by handing the app an `AdminAuth`
/// nobody can log into, which is a different thing that happens to look the
/// same: it proved the *state* was refusable without ever proving a boot can
/// arrive at it.
#[tokio::test]
async fn an_env_file_that_cannot_be_written_leaves_the_admin_surface_shut() {
    let tmp = tempfile::tempdir().expect("tempdir");
    // A directory is not a writable env file.
    let unwritable = tmp.path().join("env-is-a-directory");
    std::fs::create_dir(&unwritable).expect("mkdir");

    let instance = instance::start(
        config_in(&tmp).await,
        Wiring::default().credentials(Credentials {
            env_file: Some(unwritable),
            ..Credentials::default()
        }),
    )
    .await
    .expect("start");

    let refused = reqwest::Client::new()
        .post(url(&instance, "/api/admin/login"))
        .json(&serde_json::json!({ "password": "" }))
        .send()
        .await
        .expect("login");
    assert_eq!(
        refused.status(),
        401,
        "an unprovisioned admin surface let somebody in"
    );
    // ...and the same boot leaves Web Push off, for the same reason: an
    // identity that cannot be saved would be a different one next restart, and
    // every subscription a browser had pinned would go quietly dead.
    assert_eq!(
        reqwest::get(url(&instance, "/api/push/key"))
            .await
            .expect("push key")
            .status(),
        404
    );
}

/// **A disabled Push is a provisioning outcome too** — and this is its other
/// route: a configured value that is not a P-256 key. A typo is a configuration
/// mistake, but not one worth refusing to serve audio over, so notifications go
/// off and the scanner keeps scanning.
#[tokio::test]
async fn a_vapid_value_that_is_not_a_key_leaves_push_off_and_the_scanner_up() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let instance = instance::start(
        config_in(&tmp).await,
        Wiring::default().credentials(Credentials {
            vapid_key: Some("not-a-key".into()),
            ..Credentials::default()
        }),
    )
    .await
    .expect("start");

    assert_eq!(
        reqwest::get(url(&instance, "/api/push/key"))
            .await
            .expect("push key")
            .status(),
        404
    );
    assert_eq!(
        reqwest::get(url(&instance, "/healthz"))
            .await
            .expect("healthz")
            .status(),
        200,
        "a typo'd push key took the whole scanner down"
    );
}

/// One hour after the epoch — a moment nothing in the suite could arrive at by
/// accident, so a row stamped with it was stamped by the clock this Instance
/// was wired with and by nothing else.
const AN_HOUR_IN: i64 = 3_600_000;

/// **The clock is an input.** A Call ingested by an Instance wired with a
/// frozen clock is stamped with that clock's reading, not the wall clock's.
///
/// `created_at_ms` is when *this instance* took the Call in, as against
/// `call_at_ms`, which is when the recorder says the radio traffic happened —
/// so it is exactly the field a clock decides.
#[tokio::test]
async fn an_ingested_call_is_stamped_by_the_clock_the_instance_was_given() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let instance = instance::start(
        config_in(&tmp).await,
        Wiring::default().clock(Clock::frozen(AN_HOUR_IN)),
    )
    .await
    .expect("start");

    upload(
        &reqwest::Client::new(),
        &instance,
        &saved(&tmp, startup::INGEST_KEY_VAR),
        100,
    )
    .await;

    let stored = call::Entity::find()
        .one(&instance.db)
        .await
        .expect("read calls")
        .expect("the Call");
    assert_eq!(stored.created_at_ms, AN_HOUR_IN);
}

/// ...and the sweeper reads the same clock, which is what makes Retention
/// testable at all: this archive holds a Call that is 56 years old by the wall
/// clock and one hour old by the Instance's, and `[retention] days = 7` keeps
/// it.
///
/// The wait is for the sweep to have *happened* — since #93 its Worker's own
/// idle signal rather than the DEBUG line it happens to write — so the
/// assertion that nothing was pruned is a fact rather than a race.
#[tokio::test]
async fn the_sweeper_prunes_by_the_clock_the_instance_was_given() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let config = config_in(&tmp).await;
    let db = radio_scout::db::connect(&config.database_url())
        .await
        .expect("db");
    repo::insert_call(
        &db,
        &NewCall::new(1, 2, 0),
        common::audio_at("an-hour-old.wav"),
        &repo::Resolved::default(),
        true,
        0,
    )
    .await
    .expect("seed");
    let instance = instance::start(config, Wiring::default().clock(Clock::frozen(AN_HOUR_IN)))
        .await
        .expect("start");

    instance.workers().idle().await;
    assert_eq!(
        call::Entity::find()
            .count(&instance.db)
            .await
            .expect("count calls"),
        1,
        "the sweeper read the wall clock rather than the one it was given"
    );
}

/// A configured credential is used as given and nothing is generated over it —
/// the everyday case, because a recorder's key has to keep working across a
/// restart and across a wiped database (ADR-0008).
#[tokio::test]
async fn configured_credentials_are_used_rather_than_generated() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let instance = instance::start(
        config_in(&tmp).await,
        Wiring::default().credentials(Credentials {
            ingest_key: Some("a-recorders-key".into()),
            admin_password: Some("an-operators-password".into()),
            ..Credentials::default()
        }),
    )
    .await
    .expect("start");

    let client = reqwest::Client::new();
    let ingest = client
        .post(url(&instance, "/api/call-upload"))
        .multipart(CallUpload::new().key("a-recorders-key").into_form())
        .send()
        .await
        .expect("upload");
    assert_eq!(ingest.status(), 200);

    let login = client
        .post(url(&instance, "/api/admin/login"))
        .json(&serde_json::json!({ "password": "an-operators-password" }))
        .send()
        .await
        .expect("login");
    assert_eq!(login.status(), 200);
}

// ---------------------------------------------------------------------------
// The ways a boot fails. Each of these is a thing an operator meets at the
// moment nothing else is working, so each has to name what they must act on —
// a bare `Os { code: 48 }` with no mention of the port is the failure mode
// ADR-0011 rule 4 exists for.
// ---------------------------------------------------------------------------

/// A `base_dir` that cannot be created names the path.
#[tokio::test]
async fn a_base_dir_that_cannot_be_created_names_the_path() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let in_the_way = tmp.path().join("a-file");
    std::fs::write(&in_the_way, "not a directory").expect("write");
    let mut config = config_in(&tmp).await;
    config.server.base_dir = in_the_way.join("radio-scout-data");

    let Err(error) = instance::start(config, Wiring::default()).await else {
        panic!("a directory cannot live inside a file");
    };

    assert!(
        matches!(error, instance::StartError::BaseDir { .. }),
        "{error:?}"
    );
    assert!(error.to_string().contains("radio-scout-data"), "{error}");
}

/// A database that cannot be opened says so, rather than failing later as a
/// 500 on the first upload.
#[tokio::test]
async fn a_database_that_cannot_be_opened_says_so() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let mut config = config_in(&tmp).await;
    config.database.url = Some(format!(
        "sqlite://{}?mode=rwc",
        tmp.path().join("no/such/directory/t.db").display()
    ));

    let Err(error) = instance::start(config, Wiring::default()).await else {
        panic!("that database is not there to open");
    };

    assert!(
        matches!(error, instance::StartError::Database(_)),
        "{error:?}"
    );
    assert!(error.to_string().contains("database"), "{error}");
}

/// ...and so does an audio store, which is the other half of the archive and
/// fails for its own reasons — a path that is not a directory, a bucket that
/// will not build.
#[tokio::test]
async fn a_store_that_cannot_be_opened_says_so() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let in_the_way = tmp.path().join("audio-is-a-file");
    std::fs::write(&in_the_way, "not a directory").expect("write");
    let mut config = config_in(&tmp).await;
    config.storage.path = Some(in_the_way.join("calls"));

    let Err(error) = instance::start(config, Wiring::default()).await else {
        panic!("an audio directory cannot live inside a file");
    };

    assert!(matches!(error, instance::StartError::Store(_)), "{error:?}");
    assert!(error.to_string().contains("audio store"), "{error}");
}

/// A port already in use is the most common way a boot fails, so the message
/// says which port — the one thing the operator has to change.
#[tokio::test]
async fn a_port_already_in_use_names_the_port() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let held = std::net::TcpListener::bind("127.0.0.1:0").expect("hold a port");
    let taken = held.local_addr().expect("addr");

    let Err(error) = instance::start(config_in(&tmp).await, Wiring::default().bind(taken)).await
    else {
        panic!("that port is taken");
    };

    assert!(
        matches!(error, instance::StartError::Bind { port, .. } if port == taken.port()),
        "{error:?}"
    );
    assert!(
        error.to_string().contains(&taken.port().to_string()),
        "{error}"
    );
}

/// `serve_forever` is what the binary does with the handle: it comes back when
/// the Instance is stopped, and reports nothing wrong about being stopped.
#[tokio::test]
async fn serve_forever_returns_once_the_instance_is_stopped() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let mut instance = instance::start(config_in(&tmp).await, Wiring::default())
        .await
        .expect("start");

    instance.stop().await;

    instance.serve_forever().await.expect("a clean stop");
}
