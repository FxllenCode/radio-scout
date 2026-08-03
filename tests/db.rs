//! Data-layer integration tests (ticket #3): migrations, resolve-or-create, the
//! Call insert with child rows, and the archive-search aggregation query.
//!
//! SQLite runs everywhere (a fresh temp-file DB per test). The aggregation query
//! also runs against Postgres when `TEST_POSTGRES_URL` is set — #22's
//! dual-dialect job stands one server up and sets it (ADR-0003: every migration
//! and query is exercised on both dialects). Without it, that test skips.

mod common;

use radio_scout::archive;
use radio_scout::blob::StoredAudio;
use radio_scout::db::entities::{
    api_key, call_frequency, call_patch, call_unit, group, system, tag, talkgroup, talkgroup_group,
    unit,
};
use radio_scout::db::repo::{Disposition, DropReason, NewCall, Resolved};
use radio_scout::db::{self, Db, repo};
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, PaginatorTrait, QueryFilter, Set};

const NOW: i64 = 1_700_000_000_000;

/// A fresh SQLite database (temp file so a connection pool shares one DB) with
/// migrations applied. The TempDir must outlive the connection.
async fn sqlite() -> (Db, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let url = format!("sqlite://{}?mode=rwc", dir.path().join("t.db").display());
    let db = db::connect(&url).await.expect("connect + migrate sqlite");
    (db, dir)
}

/// `db::connect` leaves a SQLite database in the mode the app is designed
/// around (#83).
///
/// **WAL is the assertion that bites.** Nothing else in the suite notices the
/// `get_database_backend() == Sqlite` arm not firing, and the only symptom
/// would be a Pi whose archive queries start blocking behind every upload —
/// slowness on hardware, and nothing at all in a test run.
///
/// `foreign_keys` is checked beside it as a statement of the contract, not as a
/// guard on that arm: sqlx turns foreign keys on for every SQLite connection it
/// opens, so this passes with the explicit `PRAGMA` removed. The pragma is kept
/// because the guarantee should not rest on a driver default — but a test
/// cannot prove that, and pretending otherwise is the kind of assertion this
/// ticket is about.
#[tokio::test]
async fn connect_puts_sqlite_in_wal_with_foreign_keys_on() {
    use sea_orm::{ConnectionTrait, DbBackend, Statement};

    let (db, _dir) = sqlite().await;

    let mode = db
        .query_one(Statement::from_string(
            DbBackend::Sqlite,
            "PRAGMA journal_mode;",
        ))
        .await
        .expect("journal_mode")
        .expect("a row");
    assert_eq!(
        mode.try_get::<String>("", "journal_mode").unwrap(),
        "wal",
        "a reader must not block the ingest writer"
    );

    let foreign_keys = db
        .query_one(Statement::from_string(
            DbBackend::Sqlite,
            "PRAGMA foreign_keys;",
        ))
        .await
        .expect("foreign_keys")
        .expect("a row");
    assert_eq!(
        foreign_keys.try_get::<i32>("", "foreign_keys").unwrap(),
        1,
        "a child row must not outlive its parent"
    );
}

#[tokio::test]
async fn migrations_apply_and_tables_are_queryable() {
    let (db, _dir) = sqlite().await;
    // Every table exists and is empty on a fresh DB.
    assert_eq!(system::Entity::find().count(&db).await.unwrap(), 0);
    assert_eq!(talkgroup::Entity::find().count(&db).await.unwrap(), 0);
    assert_eq!(call_patch::Entity::find().count(&db).await.unwrap(), 0);
}

/// `authorize_ingest` at the repo boundary (unit half; the HTTP integration test
/// lives in `tests/ingest.rs`). Covers the missing / global / scoped / **disabled**
/// branches (ADR-0008) — the disabled case is the load-bearing security check.
#[tokio::test]
async fn authorize_ingest_enforces_validity_scope_and_disabled() {
    let (db, _dir) = sqlite().await;

    // Unknown key -> denied.
    assert!(!repo::authorize_ingest(&db, "nope", 11).await.unwrap());

    // Global (unscoped) key -> allowed for any system.
    repo::create_api_key(&db, "global", None, None, NOW)
        .await
        .unwrap();
    assert!(repo::authorize_ingest(&db, "global", 11).await.unwrap());
    assert!(repo::authorize_ingest(&db, "global", 22).await.unwrap());

    // System-scoped key -> only its own system.
    repo::create_api_key(&db, "sys11", Some(11), None, NOW)
        .await
        .unwrap();
    assert!(repo::authorize_ingest(&db, "sys11", 11).await.unwrap());
    assert!(!repo::authorize_ingest(&db, "sys11", 22).await.unwrap());

    // Disabled key -> denied even though the hash matches.
    api_key::ActiveModel {
        key_hash: Set(repo::hash_key("revoked")),
        disabled: Set(true),
        created_at_ms: Set(NOW),
        ..Default::default()
    }
    .insert(&db)
    .await
    .unwrap();
    assert!(!repo::authorize_ingest(&db, "revoked", 11).await.unwrap());
}

/// A key configured out-of-band (`RADIO_SCOUT_API_KEY`, typically from `.env`
/// while there is no admin surface to manage keys — #19) has to survive restarts *and* a database
/// that already has keys — otherwise every boot either duplicates it or, worse,
/// leaves the recorder's configured key unusable.
#[tokio::test]
async fn ensure_api_key_registers_once_and_stays_authorized() {
    let (db, _dir) = sqlite().await;

    assert!(
        repo::ensure_api_key(&db, "from-dotenv", None, NOW)
            .await
            .unwrap(),
        "first boot registers it"
    );
    assert!(
        repo::authorize_ingest(&db, "from-dotenv", 11)
            .await
            .unwrap()
    );

    // The row says where the key came from. A key's secret is never logged and
    // never re-readable (ADR-0011 rule 2), so the label is the only handle an
    // operator has on which row is which — the id alone doesn't say whether a
    // key came from `.env` or was generated (#83).
    let registered = api_key::Entity::find().one(&db).await.unwrap().unwrap();
    assert_eq!(
        registered.label.as_deref(),
        Some("configured (RADIO_SCOUT_API_KEY)")
    );

    // Restarting must not add a second row for the same secret.
    assert!(
        !repo::ensure_api_key(&db, "from-dotenv", None, NOW + 1)
            .await
            .unwrap(),
        "a later boot finds it already there"
    );
    assert_eq!(repo::count_api_keys(&db).await.unwrap(), 1);

    // A different secret is a different key, not a replacement.
    assert!(
        repo::ensure_api_key(&db, "another", None, NOW)
            .await
            .unwrap()
    );
    assert_eq!(repo::count_api_keys(&db).await.unwrap(), 2);
    assert!(
        repo::authorize_ingest(&db, "from-dotenv", 11)
            .await
            .unwrap()
    );
}

/// Re-registering a key an operator deliberately **disabled** must not quietly
/// bring it back to life (ADR-0008: revocation is the load-bearing control).
#[tokio::test]
async fn ensure_api_key_does_not_revive_a_revoked_key() {
    let (db, _dir) = sqlite().await;
    api_key::ActiveModel {
        key_hash: Set(repo::hash_key("revoked")),
        disabled: Set(true),
        created_at_ms: Set(NOW),
        ..Default::default()
    }
    .insert(&db)
    .await
    .unwrap();

    assert!(
        !repo::ensure_api_key(&db, "revoked", None, NOW + 1)
            .await
            .unwrap(),
        "the row exists; leave it disabled"
    );

    assert_eq!(repo::count_api_keys(&db).await.unwrap(), 1);
    assert!(!repo::authorize_ingest(&db, "revoked", 11).await.unwrap());
}

#[tokio::test]
async fn resolve_or_create_is_idempotent_and_scoped() {
    let (db, _dir) = sqlite().await;

    let a = repo::resolve_or_create_system(&db, 11, Some("Alpha".into()), NOW)
        .await
        .unwrap();
    let b = repo::resolve_or_create_system(&db, 11, Some("Alpha".into()), NOW)
        .await
        .unwrap();
    assert_eq!(a.id, b.id, "same Ref -> same System");
    assert_eq!(system::Entity::find().count(&db).await.unwrap(), 1);

    // A Talkgroup Ref is unique only within its System: the same Ref under two
    // Systems is two distinct Talkgroups. (Talkgroup find-or-create idempotency is
    // covered end-to-end by the auto-populate tests below.)
    let other = repo::resolve_or_create_system(&db, 22, None, NOW)
        .await
        .unwrap();
    let tg_a = seed_talkgroup(&db, a.id, 5).await;
    let tg_b = seed_talkgroup(&db, other.id, 5).await;
    assert_ne!(
        tg_a.id, tg_b.id,
        "same Ref in different Systems -> distinct"
    );
}

#[tokio::test]
async fn insert_call_persists_call_with_children() {
    let (db, _dir) = sqlite().await;

    // A patch ref is stored only when its System has a Talkgroup for it (#81),
    // so the patched Talkgroups have to be ones this System already knows.
    let sys = repo::resolve_or_create_system(&db, 11, Some("Alpha".into()), NOW)
        .await
        .unwrap();
    seed_talkgroup(&db, sys.id, 200).await;
    seed_talkgroup(&db, sys.id, 300).await;

    let new = repo::NewCall {
        system_label: Some("Alpha".into()),
        talkgroup_label: Some("Dispatch".into()),
        talkgroup_tag: Some("Fire".into()),
        talkgroup_groups: vec!["Emergency".into(), "Public".into()],
        frequency: Some(774_031_250),
        source_ref: Some(4_424_000),
        audio_mime: Some("audio/x-wav".into()),
        audio_name: Some("audio.wav".into()),
        duration_ms: Some(4_250),
        patches: vec![200, 300],
        units: vec![repo::NewCallUnit {
            unit_ref: 4_424_000,
            label: Some("Engine 1".into()),
            offset_ms: Some(1_500),
            ..Default::default()
        }],
        frequencies: vec![repo::NewCallFrequency {
            freq: 774_031_250,
            pos_ms: Some(250),
            len_ms: Some(1_500),
            dbm: Some(-50.5),
            error_count: Some(2),
            spike_count: Some(1),
            ..Default::default()
        }],
        ..NewCall::new(11, 100, NOW)
    };

    let stored = repo::insert_call(
        &db,
        &new,
        common::audio_at("ab/abcd.wav"),
        &Resolved::unresolved(),
        true,
        NOW,
    )
    .await
    .unwrap();

    // System, Talkgroup, Tag resolved-and-created.
    assert_eq!(system::Entity::find().count(&db).await.unwrap(), 1);
    let tg = talkgroup::Entity::find_by_id(stored.talkgroup_id)
        .one(&db)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(tg.r#ref, 100);
    assert!(tg.tag_id.is_some(), "tag linked");

    // Groups (assembled in Rust, sorted).
    let groups = repo::groups_for_talkgroup(&db, tg.id).await.unwrap();
    assert_eq!(groups, vec!["Emergency".to_string(), "Public".to_string()]);

    // Child rows — asserted by column, not by count (#83). A count proves a row
    // was written; it says nothing about a column that stopped being written,
    // and every one of these is a column some screen reads: the Call's duration
    // drives the archive's timeline, a Unit's offset is where in the recording
    // that radio keyed up, and a frequency's position and signal level are the
    // whole content of a Call that spans a control-channel handoff.
    assert_eq!(stored.duration_ms, Some(4_250), "the Call's own duration");

    assert_eq!(call_patch::Entity::find().count(&db).await.unwrap(), 2);

    let units = call_unit::Entity::find().all(&db).await.unwrap();
    assert_eq!(units.len(), 1);
    assert_eq!(units[0].unit_ref, 4_424_000);
    assert_eq!(units[0].label.as_deref(), Some("Engine 1"));
    assert_eq!(units[0].offset_ms, Some(1_500));

    let frequencies = call_frequency::Entity::find().all(&db).await.unwrap();
    assert_eq!(frequencies.len(), 1);
    assert_eq!(frequencies[0].freq, 774_031_250);
    assert_eq!(frequencies[0].pos_ms, Some(250));
    assert_eq!(frequencies[0].len_ms, Some(1_500));
    assert_eq!(frequencies[0].dbm, Some(-50.5));
    assert_eq!(frequencies[0].error_count, Some(2));
    assert_eq!(frequencies[0].spike_count, Some(1));

    // Patch archive helper.
    let patched = repo::calls_patched_to(&db, 200).await.unwrap();
    assert_eq!(patched.len(), 1);
    assert_eq!(patched[0].id, stored.id);
}

#[tokio::test]
async fn dialect_sensitive_queries_on_sqlite() {
    let (db, _dir) = sqlite().await;
    run_search_suite(&db).await;
    run_catalog_suite(&db).await;
    run_retention_suite(&db).await;
}

/// The archive-search + Group aggregation, the selection catalog, and
/// retention's `SUM(audio_size)` on Postgres — the highest dialect-divergence
/// risks (ADR-0003). They run in one test because each builds on the dataset the
/// last one seeded. Runs only when the run was given a Postgres.
///
/// Against a **database of its own** (#22), like every other test: connecting to
/// the server's own database instead would make the suite pass once and then
/// fail on the rows it left behind.
#[tokio::test]
async fn dialect_sensitive_queries_on_postgres_when_available() {
    let Some(server) = common::postgres_server() else {
        // Test-runner output, not application output: a skipped test has to say
        // so to whoever is reading the run, and no subscriber is installed here.
        #[allow(clippy::print_stderr)]
        {
            eprintln!(
                "skipping Postgres dual-dialect test: TEST_POSTGRES_URL unset (needs Docker/CI)"
            );
        }
        return;
    };
    let url = common::create_test_database(&server).await;
    let db = db::connect(&url).await.expect("connect + migrate postgres");
    run_search_suite(&db).await;
    run_catalog_suite(&db).await;
    run_retention_suite(&db).await;
}

/// The selection catalog (#12), run identically on both dialects. Two divergence
/// risks meet here: the Talkgroup→Group link join that assembles a list without
/// DB-side aggregation, and the **ordering**, which the query deliberately does
/// in Rust because SQLite and Postgres collate text differently — so a panel
/// that reads one way on a Pi's SQLite must read the same way on a hosted
/// Postgres.
///
/// Reads the dataset [`run_search_suite`] seeded, before retention adds its own.
async fn run_catalog_suite(db: &Db) {
    let catalog = radio_scout::catalog::read(db).await.unwrap();

    let systems: Vec<_> = catalog
        .systems
        .iter()
        .map(|s| (s.r#ref, s.label.as_deref()))
        .collect();
    assert_eq!(
        systems,
        vec![(100, Some("Alpha")), (200, Some("Beta"))],
        "systems ordered by label on both dialects"
    );

    let alpha: Vec<_> = catalog.systems[0]
        .talkgroups
        .iter()
        .map(|t| (t.r#ref, t.tag.as_deref(), t.groups.clone()))
        .collect();
    assert_eq!(
        alpha,
        vec![
            (1, Some("Fire"), vec!["Emergency".to_string()]),
            (
                2,
                Some("Law"),
                vec!["Emergency".to_string(), "Public".to_string()]
            ),
        ],
        "each Talkgroup's Tag and its sorted Groups, assembled in Rust"
    );
    assert_eq!(
        catalog.systems[1]
            .talkgroups
            .iter()
            .map(|t| (t.r#ref, t.groups.clone()))
            .collect::<Vec<_>>(),
        vec![(1, vec!["Public".to_string()])],
        "a Talkgroup Ref repeated in another System stays under its own"
    );
}

/// Retention's queries (#10), run identically on both dialects. The size total
/// is the divergence that matters: Postgres widens `SUM(bigint)` to `numeric`
/// while SQLite keeps it an integer, so the query casts.
///
/// Runs *after* [`run_search_suite`] and builds on the calls it seeded — those
/// have no recorded `audio_size`, which is exactly the NULL-tolerance case.
async fn run_retention_suite(db: &Db) {
    let seeded_without_sizes = repo::referenced_object_keys(db).await.unwrap().len();
    assert_eq!(
        repo::total_audio_bytes(db).await.unwrap(),
        0,
        "rows with no recorded size count as zero, not as an error"
    );

    let big = seed_sized_call(db, 900, 1, 1_000, 5_000, "big").await;
    let small = seed_sized_call(db, 900, 2, 24, 6_000, "small").await;
    assert_eq!(repo::total_audio_bytes(db).await.unwrap(), 1024);
    assert_eq!(
        repo::referenced_object_keys(db).await.unwrap().len(),
        seeded_without_sizes + 2
    );

    // Oldest-first paging, both by age and unconditionally.
    let aged = repo::calls_older_than(db, 5_500, 100).await.unwrap();
    assert!(aged.iter().any(|c| c.id == big), "the 5000ms call aged out");
    assert!(
        !aged.iter().any(|c| c.id == small),
        "the 6000ms call did not"
    );
    assert_eq!(
        repo::oldest_calls(db, 1).await.unwrap().len(),
        1,
        "paging honours the limit"
    );

    // Deleting drops the rows and the keys they referenced, and the total with them.
    assert_eq!(repo::delete_calls(db, &[big, small]).await.unwrap(), 2);
    assert_eq!(repo::total_audio_bytes(db).await.unwrap(), 0);
    assert_eq!(
        repo::referenced_object_keys(db).await.unwrap().len(),
        seeded_without_sizes
    );
}

/// A call with a recorded audio size, for the retention suite.
async fn seed_sized_call(
    db: &Db,
    system_ref: i64,
    talkgroup_ref: i64,
    audio_size: i64,
    at_ms: i64,
    key: &str,
) -> i64 {
    repo::insert_call(
        db,
        &repo::NewCall::new(system_ref, talkgroup_ref, at_ms),
        Some(StoredAudio::written(
            format!("{key}.wav"),
            audio_size as usize,
        )),
        &Resolved::unresolved(),
        true,
        NOW,
    )
    .await
    .unwrap()
    .id
}

/// Seed a self-contained dataset and assert the whole archive-search surface —
/// cascading filters, ordering, paging, the total count, the batched
/// Call view, and the cascading filter options. Run identically on both
/// dialects: every query here is one ADR-0003 flags as divergence-prone.
async fn run_search_suite(db: &Db) {
    let (a, b, c, d) = seed_search_dataset(db).await;
    assert_search_filters(db, a, b, c, d).await;
    assert_sort_and_count(db, a, b, c, d).await;
    assert_batched_call_view(db, a, b, c, d).await;
    assert_cascading_filter_options(db).await;
}

/// The dataset every search assertion below reads:
/// - system 100 "Alpha": tg1 tag Fire {Emergency}, tg2 tag Law {Emergency,Public}
/// - system 200 "Beta":  tg1 tag Fire {Public}
async fn seed_search_dataset(db: &Db) -> (i64, i64, i64, i64) {
    let a = seed_call(db, 100, "Alpha", 1, "Fire", &["Emergency"], 1000, "a").await;
    let b = seed_call(
        db,
        100,
        "Alpha",
        2,
        "Law",
        &["Emergency", "Public"],
        2000,
        "b",
    )
    .await;
    let c = seed_call(db, 200, "Beta", 1, "Fire", &["Public"], 3000, "c").await;
    let d = seed_call(db, 100, "Alpha", 1, "Fire", &["Emergency"], 4000, "d").await;
    (a, b, c, d)
}

async fn assert_search_filters(db: &Db, a: i64, b: i64, c: i64, d: i64) {
    // No filter -> all, newest first.
    assert_eq!(ids(db, search_base()).await, vec![d, c, b, a]);

    // By system.
    assert_eq!(
        ids(
            db,
            archive::CallSearch {
                system_ref: Some(100),
                ..search_base()
            }
        )
        .await,
        vec![d, b, a]
    );
    // System + talkgroup (Ref 1 exists in both systems, so scope by system).
    assert_eq!(
        ids(
            db,
            archive::CallSearch {
                system_ref: Some(100),
                talkgroup_ref: Some(1),
                ..search_base()
            }
        )
        .await,
        vec![d, a]
    );
    // Talkgroup Ref alone.
    assert_eq!(
        ids(
            db,
            archive::CallSearch {
                talkgroup_ref: Some(2),
                ..search_base()
            }
        )
        .await,
        vec![b]
    );
    // By group — tg2 is in two groups, and each Call still arrives once
    // (`a_group_filter_cannot_multiply_a_call` says why that needs no DISTINCT).
    assert_eq!(
        ids(
            db,
            archive::CallSearch {
                group_name: Some("Emergency".into()),
                ..search_base()
            }
        )
        .await,
        vec![d, b, a]
    );
    assert_eq!(
        ids(
            db,
            archive::CallSearch {
                group_name: Some("Public".into()),
                ..search_base()
            }
        )
        .await,
        vec![c, b]
    );
    // By tag.
    assert_eq!(
        ids(
            db,
            archive::CallSearch {
                tag_name: Some("Fire".into()),
                ..search_base()
            }
        )
        .await,
        vec![d, c, a]
    );
    assert_eq!(
        ids(
            db,
            archive::CallSearch {
                tag_name: Some("Law".into()),
                ..search_base()
            }
        )
        .await,
        vec![b]
    );
    // Date range (inclusive).
    assert_eq!(
        ids(
            db,
            archive::CallSearch {
                after_ms: Some(2000),
                before_ms: Some(3000),
                ..search_base()
            }
        )
        .await,
        vec![c, b]
    );
    // Pagination.
    assert_eq!(
        ids(
            db,
            archive::CallSearch {
                limit: 2,
                ..search_base()
            }
        )
        .await,
        vec![d, c]
    );
    assert_eq!(
        ids(
            db,
            archive::CallSearch {
                limit: 2,
                offset: 1,
                ..search_base()
            }
        )
        .await,
        vec![c, b]
    );
}

fn search_base() -> archive::CallSearch {
    archive::CallSearch::default()
}

async fn ids(db: &Db, s: archive::CallSearch) -> Vec<i64> {
    page(db, &s)
        .await
        .results
        .into_iter()
        .map(|c| c.id)
        .collect()
}

/// One filtered window of the Archive, read the way every caller reads one.
async fn page(db: &Db, search: &archive::CallSearch) -> archive::SearchPage {
    archive::page(db, search).await.unwrap()
}

#[allow(clippy::too_many_arguments)]
async fn seed_call(
    db: &Db,
    system_ref: i64,
    system_label: &str,
    talkgroup_ref: i64,
    tag: &str,
    groups: &[&str],
    at_ms: i64,
    key: &str,
) -> i64 {
    let new = repo::NewCall {
        system_label: Some(system_label.into()),
        talkgroup_tag: Some(tag.into()),
        talkgroup_groups: groups.iter().map(|g| (*g).to_string()).collect(),
        ..NewCall::new(system_ref, talkgroup_ref, at_ms)
    };
    repo::insert_call(
        db,
        &new,
        common::audio_at(format!("{key}.wav")),
        &Resolved::unresolved(),
        true,
        NOW,
    )
    .await
    .unwrap()
    .id
}

// ---------------------------------------------------------------------------
// Auto-populate + blacklist policy (#8).
// ---------------------------------------------------------------------------

/// Insert a System row directly with an explicit per-system auto-populate flag
/// and blacklist (per-System policy has no surface that sets it yet — #19).
async fn seed_system(
    db: &Db,
    ext_ref: i64,
    label: &str,
    auto_populate: bool,
    blacklist: Option<&str>,
) -> system::Model {
    system::ActiveModel {
        r#ref: Set(ext_ref),
        label: Set(Some(label.into())),
        auto_populate: Set(auto_populate),
        blacklist: Set(blacklist.map(str::to_string)),
        created_at_ms: Set(NOW),
        ..Default::default()
    }
    .insert(db)
    .await
    .unwrap()
}

/// Insert a bare Talkgroup row (no labels) under `system_id`.
async fn seed_talkgroup(db: &Db, system_id: i64, ext_ref: i64) -> talkgroup::Model {
    talkgroup::ActiveModel {
        system_id: Set(system_id),
        r#ref: Set(ext_ref),
        created_at_ms: Set(NOW),
        ..Default::default()
    }
    .insert(db)
    .await
    .unwrap()
}

/// A minimal Call carrying nothing but Refs and one heard unit — the shape a bare
/// recorder upload for an unknown talkgroup produces.
fn minimal_call(system_ref: i64, talkgroup_ref: i64) -> repo::NewCall {
    repo::NewCall {
        units: vec![repo::NewCallUnit {
            unit_ref: 4242,
            label: Some("Medic 7".into()),
            ..Default::default()
        }],
        ..repo::NewCall::new(system_ref, talkgroup_ref, NOW)
    }
}

/// A brand-new Talkgroup with no recorder labels is auto-populated with rdio's
/// defaults, and the heard radio is rostered as a Unit.
#[tokio::test]
async fn auto_populate_fills_rdio_defaults_on_create() {
    let (db, _dir) = sqlite().await;

    let stored = repo::insert_call(
        &db,
        &minimal_call(11, 5),
        common::audio_at("ab/x.wav"),
        &Resolved::unresolved(),
        true,
        NOW,
    )
    .await
    .unwrap();

    let sys = system::Entity::find_by_id(stored.system_id)
        .one(&db)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(sys.label.as_deref(), Some("System 11"), "default label");

    let tg = talkgroup::Entity::find_by_id(stored.talkgroup_id)
        .one(&db)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(tg.label.as_deref(), Some("5"), "numeric Ref label");
    assert_eq!(tg.name.as_deref(), Some("Talkgroup 5"));
    let tg_tag = tag::Entity::find_by_id(tg.tag_id.unwrap())
        .one(&db)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(tg_tag.name, "Untagged");
    assert_eq!(
        repo::groups_for_talkgroup(&db, tg.id).await.unwrap(),
        vec!["Unknown".to_string()]
    );

    // The heard radio is rostered as a Unit under the system, with its alias.
    let rostered = unit::Entity::find().one(&db).await.unwrap().unwrap();
    assert_eq!(rostered.system_id, sys.id);
    assert_eq!(rostered.r#ref, 4242);
    assert_eq!(rostered.label.as_deref(), Some("Medic 7"));
}

/// Auto-populate fills *unknowns* only: a later Call for an existing curated
/// Talkgroup must not overwrite its tag/groups or add default ones.
#[tokio::test]
async fn auto_populate_leaves_existing_talkgroup_untouched() {
    let (db, _dir) = sqlite().await;

    // Curated create: tag "Fire", group "Law".
    let curated = repo::NewCall {
        talkgroup_label: Some("Dispatch".into()),
        talkgroup_tag: Some("Fire".into()),
        talkgroup_groups: vec!["Law".into()],
        ..NewCall::new(11, 5, NOW)
    };
    repo::insert_call(
        &db,
        &curated,
        common::audio_at("ab/a.wav"),
        &Resolved::unresolved(),
        true,
        NOW,
    )
    .await
    .unwrap();

    // A bare later Call for the same talkgroup must not touch it.
    repo::insert_call(
        &db,
        &minimal_call(11, 5),
        common::audio_at("ab/x.wav"),
        &Resolved::unresolved(),
        true,
        NOW + 1,
    )
    .await
    .unwrap();

    let tg = talkgroup::Entity::find()
        .filter(talkgroup::Column::Ref.eq(5))
        .one(&db)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(tg.label.as_deref(), Some("Dispatch"), "label preserved");
    let tg_tag = tag::Entity::find_by_id(tg.tag_id.unwrap())
        .one(&db)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        tg_tag.name, "Fire",
        "tag preserved, not defaulted to Untagged"
    );
    assert_eq!(
        repo::groups_for_talkgroup(&db, tg.id).await.unwrap(),
        vec!["Law".to_string()],
        "groups preserved, not polluted with Unknown"
    );
    // No default Untagged tag / Unknown group leaked into being.
    assert_eq!(tag::Entity::find().count(&db).await.unwrap(), 1);
    assert_eq!(group::Entity::find().count(&db).await.unwrap(), 1);
}

/// With auto-populate off, the per-call `call_units` detail is still recorded, but
/// the Unit roster is not touched.
#[tokio::test]
async fn insert_call_without_auto_populate_skips_unit_roster() {
    let (db, _dir) = sqlite().await;

    repo::insert_call(
        &db,
        &minimal_call(11, 5),
        common::audio_at("ab/x.wav"),
        &Resolved::unresolved(),
        false,
        NOW,
    )
    .await
    .unwrap();

    assert_eq!(
        call_unit::Entity::find().count(&db).await.unwrap(),
        1,
        "per-call detail still recorded"
    );
    assert_eq!(
        unit::Entity::find().count(&db).await.unwrap(),
        0,
        "roster untouched when auto-populate off"
    );
}

#[tokio::test]
async fn resolve_or_create_unit_is_idempotent_and_scoped() {
    let (db, _dir) = sqlite().await;
    let a = seed_system(&db, 11, "Alpha", false, None).await;
    let b = seed_system(&db, 22, "Beta", false, None).await;

    let u1 = repo::resolve_or_create_unit(&db, a.id, 4242, Some("Medic 7".into()), NOW)
        .await
        .unwrap();
    // Same (system, ref): reused, and the original alias is kept (not overwritten).
    let u2 = repo::resolve_or_create_unit(&db, a.id, 4242, Some("RENAMED".into()), NOW)
        .await
        .unwrap();
    assert_eq!(u1.id, u2.id);
    assert_eq!(
        u2.label.as_deref(),
        Some("Medic 7"),
        "alias not overwritten"
    );

    // Same Ref in a different System is a distinct Unit.
    let u3 = repo::resolve_or_create_unit(&db, b.id, 4242, None, NOW)
        .await
        .unwrap();
    assert_ne!(u1.id, u3.id);
    assert_eq!(unit::Entity::find().count(&db).await.unwrap(), 2);
}

#[tokio::test]
async fn lowest_free_system_ref_fills_the_first_gap() {
    let (db, _dir) = sqlite().await;
    assert_eq!(
        repo::lowest_free_system_ref(&db).await.unwrap(),
        1,
        "no systems -> 1"
    );

    seed_system(&db, 1, "a", false, None).await;
    seed_system(&db, 2, "b", false, None).await;
    seed_system(&db, 4, "d", false, None).await;
    assert_eq!(
        repo::lowest_free_system_ref(&db).await.unwrap(),
        3,
        "gap at 3"
    );

    seed_system(&db, 3, "c", false, None).await;
    assert_eq!(
        repo::lowest_free_system_ref(&db).await.unwrap(),
        5,
        "contiguous -> next"
    );
}

/// The auto-populate/blacklist policy over what the database actually holds —
/// [`repo::resolve_refs`] and [`repo::disposition`] composed, which is what
/// ingest does with them (#96). The *arms* are tabled in `repo`'s own unit
/// tests, over channels a test constructs; what these prove is that the read
/// finds the rows the rule is then applied to.
async fn disposition_of(
    db: &Db,
    system_ref: i64,
    talkgroup_ref: i64,
    global_auto_populate: bool,
) -> Disposition {
    let channel = repo::resolve_refs(db, system_ref, talkgroup_ref)
        .await
        .expect("resolve the channel");
    repo::disposition(&channel, talkgroup_ref, global_auto_populate)
}

#[tokio::test]
async fn disposition_unknown_system_follows_global_toggle() {
    let (db, _dir) = sqlite().await;
    // Unknown system, global on -> store & auto-populate.
    assert_eq!(
        disposition_of(&db, 99, 5, true).await,
        Disposition::Store {
            auto_populate: true
        }
    );
    // Unknown system, global off -> dropped (nothing to attach to).
    assert_eq!(
        disposition_of(&db, 99, 5, false).await,
        Disposition::Drop(DropReason::NotPopulated)
    );
}

#[tokio::test]
async fn disposition_blacklist_drops_even_with_auto_populate_on() {
    let (db, _dir) = sqlite().await;
    seed_system(&db, 11, "Alpha", true, Some("5,7")).await;

    assert_eq!(
        disposition_of(&db, 11, 5, true).await,
        Disposition::Drop(DropReason::Blacklisted),
        "blacklisted ref dropped regardless of auto-populate"
    );
    // A non-blacklisted talkgroup under the same system still stores.
    assert_eq!(
        disposition_of(&db, 11, 6, true).await,
        Disposition::Store {
            auto_populate: true
        }
    );
}

#[tokio::test]
async fn disposition_per_system_flag_populates_when_global_off() {
    let (db, _dir) = sqlite().await;
    // Global off, but this system opts in -> unknown talkgroup auto-created.
    seed_system(&db, 11, "OptIn", true, None).await;
    assert_eq!(
        disposition_of(&db, 11, 5, false).await,
        Disposition::Store {
            auto_populate: true
        }
    );

    // Global off, system opts out, unknown talkgroup -> dropped.
    let opted_out = seed_system(&db, 22, "OptOut", false, None).await;
    assert_eq!(
        disposition_of(&db, 22, 5, false).await,
        Disposition::Drop(DropReason::NotPopulated)
    );

    // ...but a *known* talkgroup under the opted-out system is always stored,
    // with auto-populate off (no unit roster).
    let known = seed_talkgroup(&db, opted_out.id, 5).await;
    assert_eq!(
        disposition_of(&db, 22, 5, false).await,
        Disposition::Store {
            auto_populate: false
        }
    );
    // ...and the channel it resolved to is carried, so dedup and the insert do
    // not have to ask again (#96, #45).
    assert_eq!(
        repo::resolve_refs(&db, 22, 5)
            .await
            .expect("resolve")
            .talkgroup_id(),
        Some(known.id)
    );
}

/// Linking a Talkgroup to a Group it's already in is a no-op (the join-table PK
/// would otherwise be violated) — the guard behind auto-populate re-linking.
#[tokio::test]
async fn link_talkgroup_group_is_idempotent() {
    let (db, _dir) = sqlite().await;
    let sys = seed_system(&db, 11, "Alpha", false, None).await;
    let tg = seed_talkgroup(&db, sys.id, 5).await;
    let grp = repo::resolve_or_create_group(&db, "Fire", NOW)
        .await
        .unwrap();

    repo::link_talkgroup_group(&db, tg.id, grp.id)
        .await
        .unwrap();
    repo::link_talkgroup_group(&db, tg.id, grp.id)
        .await
        .unwrap(); // second is a no-op

    assert_eq!(
        repo::groups_for_talkgroup(&db, tg.id).await.unwrap(),
        vec!["Fire".to_string()],
        "linked once, not duplicated"
    );
}

/// Only heard radios that carry an alias *and* a positive Ref are rostered
/// (rdio parity): a labelled non-positive Ref is skipped by the Ref guard, and an
/// anonymous positive Ref by the label guard.
#[tokio::test]
async fn only_labeled_positive_units_are_rostered() {
    let (db, _dir) = sqlite().await;
    let new = repo::NewCall {
        units: vec![
            repo::NewCallUnit {
                unit_ref: 0, // junk Ref, even with a label -> skipped
                label: Some("Ghost".into()),
                offset_ms: None,
                ..Default::default()
            },
            repo::NewCallUnit {
                unit_ref: 5000, // valid Ref but anonymous -> skipped (rdio parity)
                label: None,
                offset_ms: None,
                ..Default::default()
            },
            repo::NewCallUnit {
                unit_ref: 4242, // valid Ref + alias -> rostered
                label: Some("Medic 7".into()),
                offset_ms: None,
                ..Default::default()
            },
        ],
        ..NewCall::new(11, 5, NOW)
    };
    repo::insert_call(
        &db,
        &new,
        common::audio_at("ab/x.wav"),
        &Resolved::unresolved(),
        true,
        NOW,
    )
    .await
    .unwrap();

    // All three are still recorded as per-call detail...
    assert_eq!(call_unit::Entity::find().count(&db).await.unwrap(), 3);
    // ...but only the labelled, positive-Ref radio joins the roster.
    let rostered = unit::Entity::find().all(&db).await.unwrap();
    assert_eq!(rostered.len(), 1);
    assert_eq!(rostered[0].r#ref, 4242);
    assert_eq!(rostered[0].label.as_deref(), Some("Medic 7"));
}

/// `lowest_free_system_ref` ignores non-positive Refs (Refs always start at 1).
#[tokio::test]
async fn lowest_free_system_ref_ignores_non_positive_refs() {
    let (db, _dir) = sqlite().await;
    seed_system(&db, 0, "zero", false, None).await;
    seed_system(&db, -3, "neg", false, None).await;
    assert_eq!(
        repo::lowest_free_system_ref(&db).await.unwrap(),
        1,
        "0 and negatives don't occupy Ref 1"
    );
}

/// `recent_calls_since` backs the live-feed reconnect catch-up (#9): only Calls
/// with `id > since`, capped to the **newest** `limit`, returned oldest-first.
/// Pins the `CATCHUP_MAX_CALLS` bound the live socket relies on, over the
/// **emission** sequence a Backfill is ordered by (#94) rather than over row ids.
#[tokio::test]
async fn calls_emitted_since_is_bounded_newest_and_ascending() {
    let (db, _dir) = sqlite().await;
    // Five calls on distinct talkgroups -> sequential ids 1..=5.
    let mut ids = vec![];
    for tg in 1..=5 {
        ids.push(
            seed_call(
                &db,
                11,
                "sys",
                tg,
                "Tag",
                &["Grp"],
                NOW + tg,
                &format!("k{tg}"),
            )
            .await,
        );
    }
    assert_eq!(ids, vec![1, 2, 3, 4, 5]);
    // Emitted in the order they were stored, which is the ordinary case: ingest
    // stores and emits in one breath.
    for (id, seq) in ids.iter().zip(1..) {
        repo::emit_call(&db, *id, seq).await.unwrap();
    }

    // `emitted_seq > since`, ascending.
    assert_eq!(emitted(&db, 2, 10).await, vec![3, 4, 5]);

    // The limit keeps the NEWEST `limit` (not the oldest), still ascending.
    assert_eq!(
        emitted(&db, 0, 2).await,
        vec![4, 5],
        "newest-2, delivered oldest-first"
    );

    // Nothing newer than the last emission.
    assert!(emitted(&db, 5, 10).await.is_empty());
}

/// The Call ids in one emission window, oldest emission first.
async fn emitted(db: &Db, since: i64, limit: u64) -> Vec<i64> {
    archive::emitted_since(db, since, limit)
        .await
        .unwrap()
        .calls
        .into_iter()
        .map(|(_seq, call)| call.id)
        .collect()
}

/// A Call that is stored but has not gone out yet is **not** backfilled — a
/// **Delay** (#73) holding one back, or a Call whose emission could not be
/// recorded. Nothing has heard it, so a Listener catching up has not missed it.
#[tokio::test]
async fn a_stored_but_unemitted_call_is_not_backfilled() {
    let (db, _dir) = sqlite().await;
    let held = seed_call(&db, 11, "sys", 100, "Tag", &["Grp"], NOW, "k1").await;

    assert!(
        emitted(&db, 0, 10).await.is_empty(),
        "a stored Call with no emission has not gone out"
    );

    repo::emit_call(&db, held, 1).await.unwrap();

    assert_eq!(
        emitted(&db, 0, 10).await,
        vec![held],
        "and once it has, it backfills like any other"
    );
}

/// The sequence resumes where the last process left it, so a restart cannot hand
/// out emissions a connected Listener's cursor is already past.
#[tokio::test]
async fn the_latest_emission_is_what_the_archive_holds() {
    let (db, _dir) = sqlite().await;
    assert_eq!(
        repo::latest_emission(&db).await.unwrap(),
        0,
        "an archive that has emitted nothing starts the sequence at 1"
    );

    let call = seed_call(&db, 11, "sys", 100, "Tag", &["Grp"], NOW, "k1").await;
    repo::emit_call(&db, call, 42).await.unwrap();

    assert_eq!(repo::latest_emission(&db).await.unwrap(), 42);

    // A Call that is stored but not yet emitted has no emission, and the two
    // dialects disagree about where that sorts: SQLite puts a `NULL` last in a
    // descending sort, Postgres puts it *first*. Unfiltered, one held Call would
    // make this `0` on Postgres — and every Listener's Backfill would come back
    // empty after the next restart.
    seed_call(&db, 11, "sys", 200, "Tag", &["Grp"], NOW + 1, "k2").await;

    assert_eq!(
        repo::latest_emission(&db).await.unwrap(),
        42,
        "a Call still being held is not the archive's high-water mark"
    );
}

/// **A migration's `down` has to run**, and the newest one is the first here to
/// index a column it also drops — which SQLite refuses and Postgres allows, so
/// getting it wrong would be green on the everyday loop and red only in CI's
/// second dialect (ADR-0003, #22).
///
/// Rolled back one step and applied again, because a `down` that leaves the
/// schema unusable is the same bug as one that fails outright.
///
/// **SQLite deliberately**, not the dialect switch: Postgres drops an index with
/// the column it belongs to, so it is green whichever order the two statements
/// are written in. This is only a test on the dialect that refuses.
#[tokio::test]
async fn the_newest_migration_rolls_back_and_reapplies() {
    use sea_orm_migration::MigratorTrait;
    let dir = tempfile::tempdir().expect("tempdir");
    let url = format!("sqlite://{}?mode=rwc", dir.path().join("t.db").display());
    let db = db::connect(&url).await.expect("connect + migrate sqlite");
    // The migrator takes sea-orm's own handle rather than the composed one
    // every statement goes through (#97), so it gets a connection of its own.
    let migrating = sea_orm::Database::connect(&url).await.expect("connect");

    radio_scout::db::migration::Migrator::down(&migrating, Some(1))
        .await
        .expect("the newest migration rolls back");
    radio_scout::db::migration::Migrator::up(&migrating, None)
        .await
        .expect("and applies again");

    // Usable afterwards, not merely present: a `down` that leaves a schema
    // nothing can write to is the same bug as one that fails outright.
    let call = seed_call(&db, 11, "sys", 100, "Tag", &["Grp"], NOW, "k1").await;
    repo::emit_call(&db, call, 1).await.expect("emit");
    assert_eq!(repo::latest_emission(&db).await.unwrap(), 1);
}

/// Retention's batch delete is fed a page at a time, and an empty page must be a
/// no-op rather than an `IN ()` the dialects disagree about (#10).
#[tokio::test]
async fn deleting_no_calls_is_a_no_op() {
    let (db, _dir) = sqlite().await;
    assert_eq!(repo::delete_calls(&db, &[]).await.unwrap(), 0);
}

/// `SUM` over no rows is NULL, not 0 — a fresh install must report an empty
/// archive rather than failing the first sweep (#10). Sizes and NULL rows are
/// covered dual-dialect in [`run_retention_suite`].
#[tokio::test]
async fn total_audio_bytes_of_an_empty_archive_is_zero() {
    let (db, _dir) = sqlite().await;
    assert_eq!(repo::total_audio_bytes(&db).await.unwrap(), 0);
}

/// `m0001_init` generates its DDL from the *live* entity definitions, so adding
/// `audio_size` to `call::Model` retroactively put the column in m0001 too: a
/// fresh database already has it by the time m0003 runs, while a database
/// migrated before the field existed does not. Both must land on the same
/// schema, which is why m0003 checks before it alters (#10).
///
/// Every other test covers the fresh-database branch. This one reproduces the
/// older shape — migrate to m0002, then take the column away — and asserts the
/// migration puts it back and that retention can use it.
#[tokio::test]
async fn audio_size_migration_converges_on_databases_that_predate_the_column() {
    use radio_scout::db::migration::Migrator;
    use sea_orm::{ConnectionTrait, Database};
    use sea_orm_migration::MigratorTrait;

    let dir = tempfile::tempdir().expect("tempdir");
    let url = format!("sqlite://{}?mode=rwc", dir.path().join("t.db").display());
    let db = Database::connect(&url).await.expect("connect");

    Migrator::up(&db, Some(2)).await.expect("migrate to m0002");
    db.execute_unprepared("ALTER TABLE calls DROP COLUMN audio_size")
        .await
        .expect("reproduce the pre-m0003 schema");

    Migrator::up(&db, None)
        .await
        .expect("migrate the rest of the way");

    // The column is back, and the size cap can read it.
    repo::insert_call(
        &db,
        &repo::NewCall::new(11, 54241, NOW),
        Some(StoredAudio::written("aa/1.wav".into(), 4096)),
        &Resolved::unresolved(),
        true,
        NOW,
    )
    .await
    .expect("insert a call carrying an audio size");
    assert_eq!(repo::total_audio_bytes(&db).await.unwrap(), 4096);
}

/// The same trap, one release earlier: #8 added `systems.auto_populate` and
/// `systems.blacklist` to the *entity*, so a fresh database got them from
/// `create_table_from_entity` while a database created before #8 did not — and
/// nothing altered the existing table. The result on a real upgrade was
/// **HTTP 500 on every ingest** (`no such column: systems.auto_populate`), with
/// the recorder logging an upload error and dropping the Call.
#[tokio::test]
async fn auto_populate_migration_converges_on_databases_that_predate_the_columns() {
    use radio_scout::db::migration::Migrator;
    use sea_orm::{ConnectionTrait, Database};
    use sea_orm_migration::MigratorTrait;

    let dir = tempfile::tempdir().expect("tempdir");
    let url = format!("sqlite://{}?mode=rwc", dir.path().join("t.db").display());
    let db = Database::connect(&url).await.expect("connect");

    Migrator::up(&db, Some(2)).await.expect("migrate to m0002");
    for column in ["auto_populate", "blacklist"] {
        db.execute_unprepared(&format!("ALTER TABLE systems DROP COLUMN {column}"))
            .await
            .expect("reproduce the pre-#8 schema");
    }

    Migrator::up(&db, None)
        .await
        .expect("migrate the rest of the way");

    // Ingest works again: reading the policy is what blew up (it selects the
    // whole System row), and a Call for an unknown System lands instead.
    let channel = repo::resolve_refs(&db, 11, 54241)
        .await
        .expect("read the System the policy is decided over");
    assert!(matches!(
        repo::disposition(&channel, 54241, true),
        repo::Disposition::Store { .. }
    ));

    let stored = repo::insert_call(
        &db,
        &repo::NewCall::new(11, 54241, NOW),
        Some(StoredAudio::written("aa/1.wav".into(), 0)),
        &Resolved::unresolved(),
        true,
        NOW,
    )
    .await
    .expect("insert a call into an upgraded database");
    assert!(stored.id > 0);

    // And the columns carry their documented defaults, not NULL.
    let sys = system::Entity::find()
        .filter(system::Column::Ref.eq(11))
        .one(&db)
        .await
        .unwrap()
        .expect("the auto-created System");
    assert!(!sys.auto_populate, "per-system opt-in defaults off");
    assert_eq!(sys.blacklist, None);
}

// ---------------------------------------------------------------------------
// Archive search (#13): sort order, total count, the batched Call view, and
// cascading filter options. These run under `run_search_suite` on both dialects.
// ---------------------------------------------------------------------------

/// Playback mode walks a filtered result set forwards in time, so oldest-first
/// is a first-class sort — not just a reversed page. Alongside it, the result
/// count a paginator needs: how many Calls match, independent of the page.
async fn assert_sort_and_count(db: &Db, a: i64, b: i64, c: i64, d: i64) {
    assert_eq!(
        ids(
            db,
            archive::CallSearch {
                sort: archive::CallSort::Oldest,
                ..archive::CallSearch::default()
            }
        )
        .await,
        vec![a, b, c, d]
    );
    // Paging an oldest-first search keeps the same order.
    assert_eq!(
        ids(
            db,
            archive::CallSearch {
                sort: archive::CallSort::Oldest,
                limit: 2,
                offset: 1,
                ..archive::CallSearch::default()
            }
        )
        .await,
        vec![b, c]
    );
    // Newest-first stays the default.
    assert_eq!(
        archive::CallSearch::default().sort,
        archive::CallSort::Newest
    );

    let all = archive::CallSearch::default();
    assert_eq!(page(db, &all).await.count, 4);

    // A page window never changes the total.
    let paged = archive::CallSearch {
        limit: 2,
        offset: 2,
        ..archive::CallSearch::default()
    };
    assert_eq!(page(db, &paged).await.count, 4);

    // Filters do.
    let tagged = archive::CallSearch {
        tag_name: Some("Fire".into()),
        ..archive::CallSearch::default()
    };
    assert_eq!(page(db, &tagged).await.count, 3);

    // A talkgroup in two groups is still counted once.
    let grouped = archive::CallSearch {
        group_name: Some("Emergency".into()),
        ..archive::CallSearch::default()
    };
    assert_eq!(page(db, &grouped).await.count, 3);

    // No matches -> zero, not an error.
    let none = archive::CallSearch {
        system_ref: Some(999),
        ..archive::CallSearch::default()
    };
    assert_eq!(page(db, &none).await.count, 0);

    // An offset with no limit skips ahead and returns the rest — SQLite rejects
    // OFFSET without LIMIT, so the query supplies an unbounded one.
    assert_eq!(
        ids(
            db,
            archive::CallSearch {
                offset: 1,
                ..archive::CallSearch::default()
            }
        )
        .await,
        vec![c, b, a]
    );

    assert_the_total_describes_the_rows_above_it(db).await;
}

/// **A page and its total come from the same filter** — the property one module
/// now owns rather than three read paths each composing it (#98).
///
/// Walked over every window of every filter this dataset can express, because
/// the failure it guards against is not a wrong number in one place: it is a
/// total computed from a *different* search than the rows above it, which reads
/// as a paginator that is subtly off and nothing else. Three facts have to hold
/// together at every step, and only the last one is about arithmetic:
/// the window never moves the total, the rows never exceed the window, and
/// `hasMore` says whether walking on would find anything.
async fn assert_the_total_describes_the_rows_above_it(db: &Db) {
    for filter in [
        archive::CallSearch::default(),
        archive::CallSearch {
            tag_name: Some("Fire".into()),
            ..archive::CallSearch::default()
        },
        // The Group filter is the one that joins a many-to-many relation, so it
        // is where a total could count a join's extra rows and the page not.
        archive::CallSearch {
            group_name: Some("Emergency".into()),
            ..archive::CallSearch::default()
        },
        archive::CallSearch {
            system_ref: Some(999),
            ..archive::CallSearch::default()
        },
    ] {
        let total = page(db, &filter).await.count;

        for offset in 0..=total + 1 {
            for limit in 1..=3 {
                let window = page(
                    db,
                    &archive::CallSearch {
                        limit,
                        offset,
                        ..filter.clone()
                    },
                )
                .await;
                let shown = window.results.len() as u64;

                assert_eq!(
                    window.count, total,
                    "the window {offset}+{limit} moved the total of {filter:?}"
                );
                assert!(shown <= limit, "a page of {shown} exceeded its limit");

                // `hasMore` against the only independent oracle there is:
                // whether walking on actually finds anything. Re-deriving it
                // from `count` here would be the same arithmetic `Page` does,
                // and an assertion that recomputes its subject can never
                // disagree with it.
                let next = page(
                    db,
                    &archive::CallSearch {
                        limit,
                        offset: offset + shown,
                        ..filter.clone()
                    },
                )
                .await;
                assert_eq!(
                    window.has_more,
                    !next.results.is_empty(),
                    "hasMore is wrong about what follows \
                     ({filter:?}, offset {offset}, limit {limit})"
                );
            }
        }

        // Walking a page at a time reaches every row and stops exactly at the
        // total — the client's own loop, run against the module.
        let (mut walked, mut seen) = (0, Vec::new());
        loop {
            let step = page(
                db,
                &archive::CallSearch {
                    limit: 2,
                    offset: walked,
                    ..filter.clone()
                },
            )
            .await;
            walked += step.results.len() as u64;
            seen.extend(step.results.iter().map(|call| call.id));
            if !step.has_more {
                break;
            }
        }
        assert_eq!(walked, total, "walking {filter:?} did not reach its total");
        seen.dedup();
        assert_eq!(
            seen.len() as u64,
            total,
            "walking {filter:?} saw a Call twice"
        );
    }
}

/// A search page is denormalized in one batch rather than per Call. rdio-scanner
/// returns bare ids and makes the client fetch each Call separately (N+1 over
/// its WebSocket); a page here arrives ready to render and play.
///
/// Asserted against what [`seed_search_dataset`] wrote, rather than against a
/// second denormalizer. It used to compare each entry with a single-Call form
/// that has since been deleted (#86, whose whole point was that every caller of
/// it already held its row) — and that comparison was always the weaker one
/// anyway: an oracle that resolves the same joins the same way can only ever
/// agree, including about a join both get wrong.
async fn assert_batched_call_view(db: &Db, a: i64, b: i64, c: i64, d: i64) {
    let batched = page(db, &archive::CallSearch::default()).await.results;

    // In the order they were given, which here is the page's own newest-first.
    assert_eq!(
        batched.iter().map(|view| view.id).collect::<Vec<_>>(),
        vec![d, c, b, a],
        "a batch comes back in the order it was handed"
    );

    // The newest Call, field by field against the row that was seeded: the
    // System's label off the System, the Tag off the Talkgroup, the Group off
    // the join table, the URL off the object key.
    let newest = &batched[0];
    assert_eq!(newest.system_ref, 100);
    assert_eq!(newest.system_label.as_deref(), Some("Alpha"));
    assert_eq!(newest.talkgroup_ref, 1);
    assert_eq!(newest.talkgroup_tag.as_deref(), Some("Fire"));
    assert_eq!(newest.talkgroup_group.as_deref(), Some("Emergency"));
    assert_eq!(newest.timestamp, Some(4000));
    assert_eq!(newest.audio_url, Some(format!("/api/call/{d}/audio")));
    assert!(newest.patches.is_empty(), "nothing seeded a patch");

    // The Call under the *other* System resolves that System's own row — so one
    // batch keys per Call, rather than resolving the page once and sharing it.
    assert_eq!(batched[1].system_ref, 200);
    assert_eq!(batched[1].system_label.as_deref(), Some("Beta"));

    // A Talkgroup in several Groups carries the alphabetically-first, which is
    // the stable pick: `b` is in {Emergency, Public}.
    assert_eq!(batched[2].talkgroup_ref, 2);
    assert_eq!(batched[2].talkgroup_tag.as_deref(), Some("Law"));
    assert_eq!(batched[2].talkgroup_group.as_deref(), Some("Emergency"));

    // An empty page needs no queries and yields nothing.
    assert!(archive::stored_calls(db, &[]).await.unwrap().is_empty());
}

/// Patches ride along on a batched page, keyed to the right Call.
#[tokio::test]
async fn stored_calls_attaches_patches_to_the_right_call() {
    let (db, _dir) = sqlite().await;
    let plain = seed_call(&db, 100, "Alpha", 1, "Fire", &["Emergency"], 1000, "a").await;
    // Patch members must be Talkgroups the System knows (#81).
    let sys = repo::resolve_or_create_system(&db, 100, None, NOW)
        .await
        .unwrap();
    seed_talkgroup(&db, sys.id, 9001).await;
    seed_talkgroup(&db, sys.id, 9002).await;
    let patched = repo::insert_call(
        &db,
        &repo::NewCall {
            patches: vec![9002, 9001],
            ..NewCall::new(100, 2, 2000)
        },
        common::audio_at("p.wav"),
        &Resolved::unresolved(),
        true,
        NOW,
    )
    .await
    .unwrap()
    .id;

    let views = page(&db, &archive::CallSearch::default()).await.results;

    let by_id = |id: i64| views.iter().find(|v| v.id == id).unwrap();
    assert_eq!(by_id(patched).patches, vec![9001, 9002]); // ordered
    assert!(by_id(plain).patches.is_empty());
}

/// Cascading filters: each dimension's options are computed from the *other*
/// active filters, so picking a System narrows the Talkgroup list while the
/// System list stays switchable. Only values that actually have Calls are
/// offered — rdio-scanner builds these lists from its whole config, so it
/// happily offers Talkgroups with nothing to show.
async fn assert_cascading_filter_options(db: &Db) {
    // Unfiltered: everything with at least one Call.
    let all = archive::options(db, &archive::CallSearch::default())
        .await
        .unwrap();
    assert_eq!(
        all.systems.iter().map(|s| s.r#ref).collect::<Vec<_>>(),
        vec![100, 200]
    );
    assert_eq!(
        all.talkgroups
            .iter()
            .map(|t| (t.system_ref, t.r#ref))
            .collect::<Vec<_>>(),
        vec![(100, 1), (100, 2), (200, 1)]
    );
    assert_eq!(all.groups, vec!["Emergency", "Public"]);
    assert_eq!(all.tags, vec!["Fire", "Law"]);
    assert_eq!(all.date_start_ms, Some(1000));
    assert_eq!(all.date_stop_ms, Some(4000));

    // Pick System 100: its Talkgroups only...
    let by_system = archive::options(
        db,
        &archive::CallSearch {
            system_ref: Some(100),
            ..archive::CallSearch::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(
        by_system
            .talkgroups
            .iter()
            .map(|t| (t.system_ref, t.r#ref))
            .collect::<Vec<_>>(),
        vec![(100, 1), (100, 2)]
    );
    // ...but the System list itself stays complete, so the choice is reversible.
    assert_eq!(
        by_system
            .systems
            .iter()
            .map(|s| s.r#ref)
            .collect::<Vec<_>>(),
        vec![100, 200]
    );

    // Pick Tag "Law": only the Talkgroup/Group that carry it, and the Tag list
    // stays complete.
    let by_tag = archive::options(
        db,
        &archive::CallSearch {
            tag_name: Some("Law".into()),
            ..archive::CallSearch::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(
        by_tag
            .talkgroups
            .iter()
            .map(|t| (t.system_ref, t.r#ref))
            .collect::<Vec<_>>(),
        vec![(100, 2)]
    );
    assert_eq!(by_tag.groups, vec!["Emergency", "Public"]); // tg2 is in both
    assert_eq!(by_tag.tags, vec!["Fire", "Law"]);
    assert_eq!(
        by_tag.systems.iter().map(|s| s.r#ref).collect::<Vec<_>>(),
        vec![100]
    );

    // Talkgroup labels/tags ride along so the client needs no second lookup.
    let tg = by_tag.talkgroups.first().unwrap();
    assert_eq!(tg.tag.as_deref(), Some("Law"));
    assert_eq!(tg.label.as_deref(), Some("2"));

    // The offered date range describes what the *other* filters can reach, so
    // narrowing the range never collapses the picker's own bounds.
    let narrowed = archive::options(
        db,
        &archive::CallSearch {
            after_ms: Some(2500),
            before_ms: Some(3500),
            ..archive::CallSearch::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(narrowed.date_start_ms, Some(1000));
    assert_eq!(narrowed.date_stop_ms, Some(4000));

    // A System filter *does* move the bounds (System 200 has one Call at 3000).
    assert_eq!(by_system_bounds(db, 200).await, (Some(3000), Some(3000)));

    assert_every_dimension_cascades(db).await;
    assert_a_group_filter_cannot_multiply_a_call(db).await;
}

/// **Why the archive search does not deduplicate** (#98).
///
/// Talkgroup↔Group is many-to-many, and the search query carried a `DISTINCT`
/// under the Group filter on the strength of that: a Talkgroup in several Groups
/// would come back once per Group. True of the relation, false of the query —
/// two schema constraints make a Call unmultipliable, and this is where they are
/// written down:
///
/// 1. **`groups.name` is `UNIQUE`**, so a filter on a group *name* matches at
///    most one Group row.
/// 2. **`talkgroup_groups` is keyed `(talkgroup_id, group_id)`**, so at most one
///    link joins that Group to that Talkgroup.
///
/// Removing the `DISTINCT` failed no test, which is exactly the problem: it was
/// an unkillable mutation on the search path buying a sort buffer per group
/// filter for a case SQL cannot produce. Asserting the constraints rather than
/// the absent duplicate is deliberate — the duplicate is what *cannot* be
/// constructed, so the only honest test is of the reason it cannot. Relax either
/// constraint and this fails, which is the moment `CallQuery::rows` owes a
/// `DISTINCT` again.
///
/// The dataset already has the shape that would break: talkgroup 2 is in both
/// **Emergency** and **Public**, and Call `b` is on it.
///
/// Every probe runs in a **transaction that is rolled back**, for two reasons:
/// the rows it offers would otherwise be seen by the suites that share this
/// database, and a refused statement aborts the transaction it is in — so each
/// gets its own. Both dialects run it, because the constraints are the schema's
/// and either could be the one that stops enforcing them.
async fn assert_a_group_filter_cannot_multiply_a_call(db: &Db) {
    let a_group = |name: &str| group::ActiveModel {
        name: Set(name.to_string()),
        created_at_ms: Set(NOW),
        ..Default::default()
    };
    let (talkgroup_id, emergency) = emergency_membership(db).await;
    let a_link = |group_id| talkgroup_group::ActiveModel {
        talkgroup_id: Set(talkgroup_id),
        group_id: Set(group_id),
    };

    // The **control**, so neither refusal below can pass for the wrong reason:
    // both rows insert fine when they are not the duplicate.
    let txn = db.begin().await.unwrap();
    let fresh = a_group("Marine").insert(&txn).await.expect("a new Group");
    a_link(fresh.id)
        .insert(&txn)
        .await
        .expect("a new membership");
    txn.rollback().await.unwrap();

    // 1. A second Group by a name already taken is refused.
    let txn = db.begin().await.unwrap();
    assert!(
        a_group("Emergency").insert(&txn).await.is_err(),
        "two Groups may not share a name, or a name filter could match both"
    );
    txn.rollback().await.unwrap();

    // 2. ...and a Talkgroup may not join one Group twice, so its Calls cannot
    //    come back once per link.
    let txn = db.begin().await.unwrap();
    assert!(
        a_link(emergency).insert(&txn).await.is_err(),
        "a Talkgroup may not join one Group twice, or its Calls would double"
    );
    txn.rollback().await.unwrap();

    // ...and so the Call on a Talkgroup that *is* in two Groups still arrives
    // once under either of them.
    for name in ["Emergency", "Public"] {
        let matched = ids(
            db,
            archive::CallSearch {
                group_name: Some(name.into()),
                talkgroup_ref: Some(2),
                ..archive::CallSearch::default()
            },
        )
        .await;
        assert_eq!(matched.len(), 1, "{name} carried the Call more than once");
    }
}

/// The (talkgroup, group) ids of talkgroup 2's Emergency membership.
async fn emergency_membership(db: &Db) -> (i64, i64) {
    let group = group::Entity::find()
        .filter(group::Column::Name.eq("Emergency"))
        .one(db)
        .await
        .unwrap()
        .expect("the Emergency group");
    let talkgroup = talkgroup::Entity::find()
        .filter(talkgroup::Column::Ref.eq(2))
        .one(db)
        .await
        .unwrap()
        .expect("talkgroup 2");
    (talkgroup.id, group.id)
}

/// Every filter, against every dimension it narrows — the whole table, not the
/// three combinations somebody happened to write down (#98).
///
/// **The Group dimension is why this exists.** It was the one filter no test set,
/// and it is also the one the two facet queries are most entangled over: the
/// Group options are read through a query that joins Talkgroup→Group *itself*,
/// so a Group filter arriving on the same query would have joined them a second
/// time and SQL would have refused the duplicate alias. That never happened only
/// because the caller clears the filter first — an invariant held by a comment,
/// on a path nothing exercised.
///
/// Read as a table: rows are the filter applied, columns are what each dimension
/// then offers. **Every filter `CallSearch` carries appears as a row** — System,
/// Talkgroup, Group, Tag, both date bounds and the duration floor — because
/// "every dimension" spread across two functions is a claim nobody can check by
/// reading one of them. Every row asserts its **own** dimension is unnarrowed,
/// which is the cascade's whole promise: a choice stays reversible in one click.
async fn assert_every_dimension_cascades(db: &Db) {
    // The dataset: 100 "Alpha" {tg1 Fire [Emergency], tg2 Law [Emergency,Public]}
    //              200 "Beta"  {tg1 Fire [Public]}
    // Calls a(100,1,@1000) b(100,2,@2000) c(200,1,@3000) d(100,1,@4000).
    for (filter, expected) in [
        (
            archive::CallSearch {
                group_name: Some("Public".into()),
                ..archive::CallSearch::default()
            },
            Offered {
                // b (tg2 is in Public) and c (tg1@200 is in Public).
                systems: vec![100, 200],
                talkgroups: vec![(100, 2), (200, 1)],
                tags: vec!["Fire", "Law"],
                // Its own dimension stays whole, so Public is switchable.
                groups: vec!["Emergency", "Public"],
                dates: (Some(2000), Some(3000)),
            },
        ),
        (
            archive::CallSearch {
                group_name: Some("Emergency".into()),
                ..archive::CallSearch::default()
            },
            Offered {
                // a, b and d — all on System 100.
                systems: vec![100],
                talkgroups: vec![(100, 1), (100, 2)],
                tags: vec!["Fire", "Law"],
                groups: vec!["Emergency", "Public"],
                dates: (Some(1000), Some(4000)),
            },
        ),
        (
            archive::CallSearch {
                talkgroup_ref: Some(2),
                ..archive::CallSearch::default()
            },
            Offered {
                // Only b. A Talkgroup Ref is not scoped to a System here, but
                // only System 100 has a Talkgroup 2.
                systems: vec![100],
                talkgroups: vec![(100, 1), (100, 2), (200, 1)],
                tags: vec!["Law"],
                groups: vec!["Emergency", "Public"],
                dates: (Some(2000), Some(2000)),
            },
        ),
        (
            archive::CallSearch {
                system_ref: Some(100),
                ..archive::CallSearch::default()
            },
            Offered {
                // a, b and d.
                systems: vec![100, 200],
                talkgroups: vec![(100, 1), (100, 2)],
                tags: vec!["Fire", "Law"],
                groups: vec!["Emergency", "Public"],
                dates: (Some(1000), Some(4000)),
            },
        ),
        (
            archive::CallSearch {
                tag_name: Some("Fire".into()),
                ..archive::CallSearch::default()
            },
            Offered {
                // a, c and d — the Fire Talkgroup on each System.
                systems: vec![100, 200],
                talkgroups: vec![(100, 1), (200, 1)],
                tags: vec!["Fire", "Law"],
                // tg1@100 is in Emergency, tg1@200 in Public.
                groups: vec!["Emergency", "Public"],
                dates: (Some(1000), Some(4000)),
            },
        ),
        (
            archive::CallSearch {
                after_ms: Some(3000),
                ..archive::CallSearch::default()
            },
            Offered {
                // c and d.
                systems: vec![100, 200],
                talkgroups: vec![(100, 1), (200, 1)],
                tags: vec!["Fire"],
                groups: vec!["Emergency", "Public"],
                // Its own dimension stays whole, so the picker never collapses.
                dates: (Some(1000), Some(4000)),
            },
        ),
        (
            // The other date bound, which is a separate field and so a separate
            // chance to be dropped on the way to a facet query.
            archive::CallSearch {
                before_ms: Some(2000),
                ..archive::CallSearch::default()
            },
            Offered {
                // a and b, both on System 100.
                systems: vec![100],
                talkgroups: vec![(100, 1), (100, 2)],
                tags: vec!["Fire", "Law"],
                groups: vec!["Emergency", "Public"],
                dates: (Some(1000), Some(4000)),
            },
        ),
        (
            // The duration floor (#42) has no dimension of its own to offer, so
            // the only way to see whether it reaches the facet queries at all is
            // a value nothing matches: none of these Calls has a measured
            // duration, so every dimension must empty — including the date
            // bounds, which clear only the date filters.
            archive::CallSearch {
                min_duration_ms: Some(1),
                ..archive::CallSearch::default()
            },
            Offered {
                systems: vec![],
                talkgroups: vec![],
                tags: vec![],
                groups: vec![],
                dates: (None, None),
            },
        ),
        (
            // Two filters at once: the Group and the System narrow together, and
            // each still offers its own dimension whole.
            archive::CallSearch {
                group_name: Some("Public".into()),
                system_ref: Some(200),
                ..archive::CallSearch::default()
            },
            Offered {
                systems: vec![100, 200],
                talkgroups: vec![(200, 1)],
                tags: vec!["Fire"],
                groups: vec!["Public"],
                dates: (Some(3000), Some(3000)),
            },
        ),
        (
            // A Group nothing is in: every dimension empties, and nothing errors.
            archive::CallSearch {
                group_name: Some("Marine".into()),
                ..archive::CallSearch::default()
            },
            Offered {
                systems: vec![],
                talkgroups: vec![],
                tags: vec![],
                groups: vec!["Emergency", "Public"],
                dates: (None, None),
            },
        ),
    ] {
        let options = archive::options(db, &filter).await.unwrap();
        assert_eq!(offered(&options), expected, "cascading from {filter:?}");
    }
}

/// What each dimension offered, in one comparable value — so a case reads as a
/// row of a table rather than six assertions.
#[derive(Debug, PartialEq, Eq)]
struct Offered<'a> {
    systems: Vec<i64>,
    talkgroups: Vec<(i64, i64)>,
    tags: Vec<&'a str>,
    groups: Vec<&'a str>,
    dates: (Option<i64>, Option<i64>),
}

fn offered(options: &radio_scout::call::FilterOptions) -> Offered<'_> {
    Offered {
        systems: options.systems.iter().map(|s| s.r#ref).collect(),
        talkgroups: options
            .talkgroups
            .iter()
            .map(|t| (t.system_ref, t.r#ref))
            .collect(),
        tags: options.tags.iter().map(String::as_str).collect(),
        groups: options.groups.iter().map(String::as_str).collect(),
        dates: (options.date_start_ms, options.date_stop_ms),
    }
}

async fn by_system_bounds(db: &Db, system_ref: i64) -> (Option<i64>, Option<i64>) {
    let options = archive::options(
        db,
        &archive::CallSearch {
            system_ref: Some(system_ref),
            ..archive::CallSearch::default()
        },
    )
    .await
    .unwrap();
    (options.date_start_ms, options.date_stop_ms)
}

/// An empty archive answers with empty option lists and no date bounds rather
/// than erroring — the Search screen must render on a fresh install.
#[tokio::test]
async fn filter_options_on_an_empty_archive_are_empty() {
    let (db, _dir) = sqlite().await;
    let options = archive::options(&db, &archive::CallSearch::default())
        .await
        .unwrap();

    assert!(options.systems.is_empty());
    assert!(options.talkgroups.is_empty());
    assert!(options.groups.is_empty());
    assert!(options.tags.is_empty());
    assert_eq!(options.date_start_ms, None);
    assert_eq!(options.date_stop_ms, None);
}

/// The same trap again, for #42's eleven columns. `m0001_init` builds its DDL
/// from the live entities, so a fresh database has `calls.emergency` and the
/// rest before `m0008` runs — and every other test in this file is a fresh
/// database, which is exactly why the last two versions of this bug both
/// shipped.
///
/// Reproduce the older shape and migrate over it, then do what an operator
/// does on the morning after an upgrade: read the archive they already had, and
/// take a new Call. The **existing** rows are the point — `emergency` and
/// `encrypted` are `NOT NULL`, so a migration that added them without a default
/// would either refuse to run or leave rows SeaORM cannot deserialize, and the
/// first symptom would be an archive that 500s.
#[tokio::test]
async fn recorder_truth_migration_converges_on_databases_that_predate_the_columns() {
    use radio_scout::db::migration::Migrator;
    use sea_orm::{ConnectionTrait, Database};
    use sea_orm_migration::MigratorTrait;

    let dir = tempfile::tempdir().expect("tempdir");
    let url = format!("sqlite://{}?mode=rwc", dir.path().join("t.db").display());
    let db = Database::connect(&url).await.expect("connect");

    Migrator::up(&db, Some(7)).await.expect("migrate to m0007");
    for (table, column) in [
        ("calls", "stop_at_ms"),
        ("calls", "emergency"),
        ("calls", "encrypted"),
        ("calls", "priority"),
        ("calls", "audio_type"),
        ("calls", "site_id"),
        ("call_frequencies", "at_ms"),
        ("call_units", "tag_ota"),
        ("call_units", "emergency"),
        ("call_units", "signal_system"),
        ("call_units", "at_ms"),
    ] {
        db.execute_unprepared(&format!("ALTER TABLE {table} DROP COLUMN {column}"))
            .await
            .expect("reproduce the pre-#42 schema");
    }
    // An archive from before the upgrade, inserted through the old shape.
    db.execute_unprepared(
        "INSERT INTO systems (id, ref, label, auto_populate, created_at_ms) \
         VALUES (1, 11, 'butco', 0, 0); \
         INSERT INTO talkgroups (id, system_id, ref, created_at_ms) VALUES (1, 1, 54241, 0); \
         INSERT INTO calls (id, system_id, talkgroup_id, call_at_ms, object_key, \
                            enhancement, created_at_ms) \
         VALUES (1, 1, 1, 1000, 'aa/1.wav', 'none', 0); \
         INSERT INTO call_units (id, call_id, unit_ref) VALUES (1, 1, 4242); \
         INSERT INTO call_frequencies (id, call_id, freq) VALUES (1, 1, 774031250)",
    )
    .await
    .expect("seed the archive an operator is upgrading with");

    Migrator::up(&db, None)
        .await
        .expect("migrate the rest of the way");

    // The archive still reads, and the rows the operator already had take the
    // documented defaults rather than NULL — a `NOT NULL` column added without
    // one is what turns an upgrade into an archive that 500s.
    let existing = repo::find_call(&db, 1)
        .await
        .expect("read a Call that predates the columns")
        .expect("it is still there");
    assert!(
        !existing.emergency,
        "a Call nobody flagged is not an emergency"
    );
    assert!(!existing.encrypted);
    assert_eq!(existing.duration_ms, None, "nobody ever measured it");
    assert_eq!(existing.site_id, None);

    let views = archive::stored_calls(&db, std::slice::from_ref(&existing))
        .await
        .expect("denormalize the pre-upgrade archive");
    assert_eq!(views.len(), 1);

    // ...and a new Call lands with all of it.
    let stored = repo::insert_call(
        &db,
        &repo::NewCall {
            duration_ms: Some(8250),
            emergency: true,
            site_ref: Some(3),
            units: vec![repo::NewCallUnit {
                unit_ref: 4242,
                tag_ota: Some("MEDIC7".into()),
                emergency: true,
                ..Default::default()
            }],
            ..NewCall::new(11, 54241, NOW)
        },
        common::audio_at("aa/2.wav"),
        &Resolved::unresolved(),
        true,
        NOW,
    )
    .await
    .expect("insert a Call into an upgraded database");
    assert!(stored.emergency);
    assert_eq!(stored.duration_ms, Some(8250));
    assert!(stored.site_id.is_some(), "the Site row was created too");
}
