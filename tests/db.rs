//! Data-layer integration tests (ticket #3): migrations, resolve-or-create, the
//! Call insert with child rows, and the archive-search aggregation query.
//!
//! SQLite runs everywhere (a fresh temp-file DB per test). The aggregation query
//! also runs against Postgres when `TEST_POSTGRES_URL` is set — CI provisions a
//! fresh Postgres and sets it (ADR-0003: every migration and query is exercised
//! on both dialects). Locally that test skips (no Docker here).

use radio_scout::db::entities::{
    api_key, call_frequency, call_patch, call_unit, group, system, tag, talkgroup, unit,
};
use radio_scout::db::repo::{Disposition, DropReason};
use radio_scout::db::{self, repo};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, PaginatorTrait, QueryFilter,
    Set,
};

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

    let stored = repo::insert_call(&db, &new, true, NOW).await.unwrap();

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
    repo::insert_call(db, &new, true, NOW).await.unwrap().id
}

// ---------------------------------------------------------------------------
// Auto-populate + blacklist policy (#8).
// ---------------------------------------------------------------------------

/// Insert a System row directly with an explicit per-system auto-populate flag
/// and blacklist (the ingest pipeline can't set these yet — that's config #17).
async fn seed_system(
    db: &DatabaseConnection,
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
async fn seed_talkgroup(db: &DatabaseConnection, system_id: i64, ext_ref: i64) -> talkgroup::Model {
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
        system_ref,
        talkgroup_ref,
        call_at_ms: NOW,
        object_key: "ab/x.wav".into(),
        units: vec![repo::NewCallUnit {
            unit_ref: 4242,
            label: Some("Medic 7".into()),
            offset_ms: None,
        }],
        ..Default::default()
    }
}

/// A brand-new Talkgroup with no recorder labels is auto-populated with rdio's
/// defaults, and the heard radio is rostered as a Unit.
#[tokio::test]
async fn auto_populate_fills_rdio_defaults_on_create() {
    let (db, _dir) = sqlite().await;

    let stored = repo::insert_call(&db, &minimal_call(11, 5), true, NOW)
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
        system_ref: 11,
        talkgroup_ref: 5,
        talkgroup_label: Some("Dispatch".into()),
        talkgroup_tag: Some("Fire".into()),
        talkgroup_groups: vec!["Law".into()],
        call_at_ms: NOW,
        object_key: "ab/a.wav".into(),
        ..Default::default()
    };
    repo::insert_call(&db, &curated, true, NOW).await.unwrap();

    // A bare later Call for the same talkgroup must not touch it.
    repo::insert_call(&db, &minimal_call(11, 5), true, NOW + 1)
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

    repo::insert_call(&db, &minimal_call(11, 5), false, NOW)
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

#[tokio::test]
async fn disposition_unknown_system_follows_global_toggle() {
    let (db, _dir) = sqlite().await;
    // Unknown system, global on -> store & auto-populate.
    assert_eq!(
        repo::ingest_disposition(&db, 99, 5, true).await.unwrap(),
        Disposition::Store {
            auto_populate: true
        }
    );
    // Unknown system, global off -> dropped (nothing to attach to).
    assert_eq!(
        repo::ingest_disposition(&db, 99, 5, false).await.unwrap(),
        Disposition::Drop(DropReason::NotPopulated)
    );
}

#[tokio::test]
async fn disposition_blacklist_drops_even_with_auto_populate_on() {
    let (db, _dir) = sqlite().await;
    seed_system(&db, 11, "Alpha", true, Some("5,7")).await;

    assert_eq!(
        repo::ingest_disposition(&db, 11, 5, true).await.unwrap(),
        Disposition::Drop(DropReason::Blacklisted),
        "blacklisted ref dropped regardless of auto-populate"
    );
    // A non-blacklisted talkgroup under the same system still stores.
    assert_eq!(
        repo::ingest_disposition(&db, 11, 6, true).await.unwrap(),
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
        repo::ingest_disposition(&db, 11, 5, false).await.unwrap(),
        Disposition::Store {
            auto_populate: true
        }
    );

    // Global off, system opts out, unknown talkgroup -> dropped.
    let opted_out = seed_system(&db, 22, "OptOut", false, None).await;
    assert_eq!(
        repo::ingest_disposition(&db, 22, 5, false).await.unwrap(),
        Disposition::Drop(DropReason::NotPopulated)
    );

    // ...but a *known* talkgroup under the opted-out system is always stored,
    // with auto-populate off (no unit roster).
    seed_talkgroup(&db, opted_out.id, 5).await;
    assert_eq!(
        repo::ingest_disposition(&db, 22, 5, false).await.unwrap(),
        Disposition::Store {
            auto_populate: false
        }
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
        system_ref: 11,
        talkgroup_ref: 5,
        call_at_ms: NOW,
        object_key: "ab/x.wav".into(),
        units: vec![
            repo::NewCallUnit {
                unit_ref: 0, // junk Ref, even with a label -> skipped
                label: Some("Ghost".into()),
                offset_ms: None,
            },
            repo::NewCallUnit {
                unit_ref: 5000, // valid Ref but anonymous -> skipped (rdio parity)
                label: None,
                offset_ms: None,
            },
            repo::NewCallUnit {
                unit_ref: 4242, // valid Ref + alias -> rostered
                label: Some("Medic 7".into()),
                offset_ms: None,
            },
        ],
        ..Default::default()
    };
    repo::insert_call(&db, &new, true, NOW).await.unwrap();

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
