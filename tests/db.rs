//! Data-layer integration tests (ticket #3): migrations, resolve-or-create, the
//! Call insert with child rows, and the archive-search aggregation query.
//!
//! SQLite runs everywhere (a fresh temp-file DB per test). The aggregation query
//! also runs against Postgres when `TEST_POSTGRES_URL` is set — CI provisions a
//! fresh Postgres and sets it (ADR-0003: every migration and query is exercised
//! on both dialects). Locally that test skips (no Docker here).

use radio_scout::db::entities::{call_frequency, call_patch, call_unit, system, talkgroup};
use radio_scout::db::{self, repo};
use sea_orm::{DatabaseConnection, EntityTrait, PaginatorTrait};

const NOW: i64 = 1_700_000_000_000;

/// A fresh SQLite database (temp file so a connection pool shares one DB) with
/// migrations applied. The TempDir must outlive the connection.
async fn sqlite() -> (DatabaseConnection, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let url = format!("sqlite://{}?mode=rwc", dir.path().join("t.db").display());
    let db = db::connect(&url).await.expect("connect + migrate sqlite");
    (db, dir)
}

#[tokio::test]
async fn migrations_apply_and_tables_are_queryable() {
    let (db, _dir) = sqlite().await;
    // Every table exists and is empty on a fresh DB.
    assert_eq!(system::Entity::find().count(&db).await.unwrap(), 0);
    assert_eq!(talkgroup::Entity::find().count(&db).await.unwrap(), 0);
    assert_eq!(call_patch::Entity::find().count(&db).await.unwrap(), 0);
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

    // A Talkgroup Ref is unique only within its System.
    let other = repo::resolve_or_create_system(&db, 22, None, NOW)
        .await
        .unwrap();
    let tg_a = repo::resolve_or_create_talkgroup(&db, a.id, 5, None, None, NOW)
        .await
        .unwrap();
    let tg_b = repo::resolve_or_create_talkgroup(&db, other.id, 5, None, None, NOW)
        .await
        .unwrap();
    assert_ne!(
        tg_a.id, tg_b.id,
        "same Ref in different Systems -> distinct"
    );
}

#[tokio::test]
async fn insert_call_persists_call_with_children() {
    let (db, _dir) = sqlite().await;

    let new = repo::NewCall {
        system_ref: 11,
        system_label: Some("Alpha".into()),
        talkgroup_ref: 100,
        talkgroup_label: Some("Dispatch".into()),
        talkgroup_tag: Some("Fire".into()),
        talkgroup_groups: vec!["Emergency".into(), "Public".into()],
        call_at_ms: NOW,
        frequency: Some(774_031_250),
        source_ref: Some(4_424_000),
        object_key: "ab/abcd.wav".into(),
        audio_mime: Some("audio/x-wav".into()),
        audio_name: Some("audio.wav".into()),
        patches: vec![200, 300],
        units: vec![repo::NewCallUnit {
            unit_ref: 4_424_000,
            label: Some("Engine 1".into()),
            offset_ms: Some(0),
        }],
        frequencies: vec![repo::NewCallFrequency {
            freq: 774_031_250,
            ..Default::default()
        }],
        ..Default::default()
    };

    let stored = repo::insert_call(&db, &new, NOW).await.unwrap();

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

    // Child rows.
    assert_eq!(call_patch::Entity::find().count(&db).await.unwrap(), 2);
    assert_eq!(call_unit::Entity::find().count(&db).await.unwrap(), 1);
    assert_eq!(call_frequency::Entity::find().count(&db).await.unwrap(), 1);

    // Patch archive helper.
    let patched = repo::calls_patched_to(&db, 200).await.unwrap();
    assert_eq!(patched.len(), 1);
    assert_eq!(patched[0].id, stored.id);
}

#[tokio::test]
async fn search_calls_filters_on_sqlite() {
    let (db, _dir) = sqlite().await;
    run_search_suite(&db).await;
}

/// The archive-search + Group aggregation on Postgres — the highest
/// dialect-divergence risk. Runs only when CI provides a fresh Postgres.
#[tokio::test]
async fn search_calls_filters_on_postgres_when_available() {
    let Ok(url) = std::env::var("TEST_POSTGRES_URL") else {
        eprintln!("skipping Postgres dual-dialect test: TEST_POSTGRES_URL unset (needs Docker/CI)");
        return;
    };
    let db = db::connect(&url).await.expect("connect + migrate postgres");
    run_search_suite(&db).await;
}

/// Seed a self-contained dataset and assert cascading filters + DISTINCT +
/// newest-first ordering. Run identically on both dialects.
async fn run_search_suite(db: &DatabaseConnection) {
    // system 100 "Alpha": tg1 tag Fire {Emergency}, tg2 tag Law {Emergency,Public}
    // system 200 "Beta":  tg1 tag Fire {Public}
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

    // No filter -> all, newest first.
    assert_eq!(ids(db, search_base()).await, vec![d, c, b, a]);

    // By system.
    assert_eq!(
        ids(
            db,
            repo::CallSearch {
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
            repo::CallSearch {
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
            repo::CallSearch {
                talkgroup_ref: Some(2),
                ..search_base()
            }
        )
        .await,
        vec![b]
    );
    // By group — tg2 is in two groups but DISTINCT keeps each call once.
    assert_eq!(
        ids(
            db,
            repo::CallSearch {
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
            repo::CallSearch {
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
            repo::CallSearch {
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
            repo::CallSearch {
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
            repo::CallSearch {
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
            repo::CallSearch {
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
            repo::CallSearch {
                limit: 2,
                offset: 1,
                ..search_base()
            }
        )
        .await,
        vec![c, b]
    );
}

fn search_base() -> repo::CallSearch {
    repo::CallSearch::default()
}

async fn ids(db: &DatabaseConnection, s: repo::CallSearch) -> Vec<i64> {
    repo::search_calls(db, &s)
        .await
        .unwrap()
        .into_iter()
        .map(|c| c.id)
        .collect()
}

#[allow(clippy::too_many_arguments)]
async fn seed_call(
    db: &DatabaseConnection,
    system_ref: i64,
    system_label: &str,
    talkgroup_ref: i64,
    tag: &str,
    groups: &[&str],
    at_ms: i64,
    key: &str,
) -> i64 {
    let new = repo::NewCall {
        system_ref,
        system_label: Some(system_label.into()),
        talkgroup_ref,
        talkgroup_tag: Some(tag.into()),
        talkgroup_groups: groups.iter().map(|g| (*g).to_string()).collect(),
        call_at_ms: at_ms,
        object_key: format!("{key}.wav"),
        ..Default::default()
    };
    repo::insert_call(db, &new, NOW).await.unwrap().id
}
