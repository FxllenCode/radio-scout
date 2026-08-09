//! Talkgroup CSV import (#18, spec US 37) over the real HTTP boundary:
//! `POST /api/admin/talkgroups/import`.
//!
//! The behaviors that matter here are the ones rdio-scanner gets wrong — an
//! import that duplicates on re-run, erases what it didn't mention, or half-
//! applies — so most of these assert against the *stored rows*, not the report.

mod common;
use common::TestApp;

use radio_scout::db::Db;
use radio_scout::db::entities::{group, talkgroup, talkgroup_group};
use radio_scout::db::repo::{self, NewCall};
use sea_orm::{ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, QueryOrder};
use serde_json::Value;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// An app with an admin session already open.
///
/// The importer is the oldest route on the `/api/admin/` surface, and since #19
/// that surface is gated: a bare `TestApp::spawn` reaches it with a 401. That
/// the guard is real is asserted in `tests/admin.rs`; here it is a precondition.
async fn admin_app() -> TestApp {
    let app = TestApp::spawn().await;
    app.login().await;
    app
}

/// POST a CSV body, returning the status and parsed JSON.
async fn import(app: &TestApp, query: &str, csv: &str) -> (u16, Value) {
    import_bytes(app, query, csv.as_bytes().to_vec()).await
}

async fn import_bytes(app: &TestApp, query: &str, body: Vec<u8>) -> (u16, Value) {
    let (status, body) = app
        .post_admin_bytes(
            &format!("/api/admin/talkgroups/import{query}"),
            "text/csv",
            body,
        )
        .await;
    (
        status,
        serde_json::from_str(&body).unwrap_or_else(|e| panic!("json body ({e}): {body:?}")),
    )
}

/// POST a CSV expecting it to succeed, returning the report.
async fn import_ok(app: &TestApp, query: &str, csv: &str) -> Value {
    let (status, body) = import(app, query, csv).await;
    assert_eq!(status, 200, "import failed: {body}");
    body
}

/// The stored Talkgroup for (system ref, talkgroup ref).
async fn stored_talkgroup(
    db: &Db,
    system_ref: i64,
    talkgroup_ref: i64,
) -> Option<talkgroup::Model> {
    let system = radio_scout::db::entities::system::Entity::find()
        .filter(radio_scout::db::entities::system::Column::Ref.eq(system_ref))
        .one(db)
        .await
        .expect("system query")?;
    talkgroup::Entity::find()
        .filter(talkgroup::Column::SystemId.eq(system.id))
        .filter(talkgroup::Column::Ref.eq(talkgroup_ref))
        .one(db)
        .await
        .expect("talkgroup query")
}

/// The Talkgroup for (system, ref), panicking if the import didn't make one.
async fn must_store(db: &Db, system_ref: i64, talkgroup_ref: i64) -> talkgroup::Model {
    stored_talkgroup(db, system_ref, talkgroup_ref)
        .await
        .unwrap_or_else(|| panic!("no talkgroup {system_ref}/{talkgroup_ref}"))
}

/// The Tag name on a stored Talkgroup.
async fn tag_of(db: &Db, tg: &talkgroup::Model) -> Option<String> {
    let tag_id = tg.tag_id?;
    radio_scout::db::entities::tag::Entity::find_by_id(tag_id)
        .one(db)
        .await
        .expect("tag query")
        .map(|t| t.name)
}

/// The Group names linked to a stored Talkgroup, sorted.
async fn groups_of(db: &Db, talkgroup_id: i64) -> Vec<String> {
    let mut names = Vec::new();
    for link in talkgroup_group::Entity::find()
        .filter(talkgroup_group::Column::TalkgroupId.eq(talkgroup_id))
        .all(db)
        .await
        .expect("links")
    {
        let g = group::Entity::find_by_id(link.group_id)
            .one(db)
            .await
            .expect("group")
            .expect("group row");
        names.push(g.name);
    }
    names.sort();
    names
}

/// How many Talkgroups exist in total — the count that catches duplication.
async fn talkgroup_count(db: &Db) -> usize {
    talkgroup::Entity::find()
        .all(db)
        .await
        .expect("talkgroups")
        .len()
}

/// Ingest one Call, so a System and an auto-populated Talkgroup exist to curate.
async fn seed_call(db: &Db, system_ref: i64, label: &str, talkgroup_ref: i64) {
    repo::insert_call(
        db,
        &NewCall {
            system_label: Some(label.into()),
            ..NewCall::new(system_ref, talkgroup_ref, 1_000)
        },
        common::audio_at(format!("k/{system_ref}-{talkgroup_ref}.wav")),
        &repo::Resolved::default(),
        true,
        0,
    )
    .await
    .expect("seed call");
}

// ---------------------------------------------------------------------------
// The acceptance criterion
// ---------------------------------------------------------------------------

/// The ticket, end to end: a `ref,label,group,tag,led` CSV bulk-sets all four.
#[tokio::test]
async fn csv_bulk_sets_labels_groups_tags_and_leds() {
    let app = admin_app().await;

    let report = import_ok(
        &app,
        "?system=11",
        "ref,label,name,group,tag,led\n\
         54241,TDB A1,Butler Dispatch,Fire,Dispatch,red\n\
         54242,TDB A2,Butler Fireground,Fire,Fireground,orange\n\
         54301,PD 1,Butler Police,Law,Dispatch,blue\n",
    )
    .await;

    assert_eq!(report["talkgroupsCreated"], 3);
    assert_eq!(report["rows"], 3);
    assert_eq!(report["rejected"], serde_json::json!([]));

    let tg = must_store(&app.db, 11, 54241).await;
    assert_eq!(tg.label.as_deref(), Some("TDB A1"));
    assert_eq!(tg.name.as_deref(), Some("Butler Dispatch"));
    assert_eq!(tg.led.as_deref(), Some("red"));
    assert_eq!(tag_of(&app.db, &tg).await.as_deref(), Some("Dispatch"));
    assert_eq!(groups_of(&app.db, tg.id).await, vec!["Fire".to_string()]);

    let pd = must_store(&app.db, 11, 54301).await;
    assert_eq!(pd.led.as_deref(), Some("blue"));
    assert_eq!(groups_of(&app.db, pd.id).await, vec!["Law".to_string()]);

    // Tags and Groups are created once and shared, not per row.
    assert_eq!(report["tagsCreated"], 2); // Dispatch, Fireground
    assert_eq!(report["groupsCreated"], 2); // Fire, Law
}

/// Curating what a recorder auto-populated (#8) is the actual US 37 story:
/// tidy the archive you already have.
#[tokio::test]
async fn import_curates_auto_populated_talkgroups() {
    let app = admin_app().await;
    seed_call(&app.db, 11, "butco", 54241).await;

    // Auto-populate gave it rdio's defaults.
    let before = must_store(&app.db, 11, 54241).await;
    assert_eq!(before.label.as_deref(), Some("54241"));
    assert_eq!(before.led, None);

    let report = import_ok(&app, "?system=11", "ref,label,led\n54241,TDB A1,green\n").await;
    assert_eq!(report["talkgroupsUpdated"], 1);
    assert_eq!(report["talkgroupsCreated"], 0);

    let after = must_store(&app.db, 11, 54241).await;
    assert_eq!(after.id, before.id, "curation must not replace the row");
    assert_eq!(after.label.as_deref(), Some("TDB A1"));
    assert_eq!(after.led.as_deref(), Some("green"));
    // The Call still points at it.
    assert_eq!(talkgroup_count(&app.db).await, 1);
}

// ---------------------------------------------------------------------------
// Idempotence — rdio's `unshift` bug
// ---------------------------------------------------------------------------

/// rdio's importer `unshift`s every row, so importing the same file twice
/// duplicates every Talkgroup. Ours upserts on (System, Ref).
#[tokio::test]
async fn re_importing_the_same_file_changes_nothing() {
    let app = admin_app().await;
    let csv = "ref,label,group,tag,led\n\
               54241,TDB A1,Fire,Dispatch,red\n\
               54242,TDB A2,Fire,Fireground,orange\n";

    let first = import_ok(&app, "?system=11", csv).await;
    assert_eq!(first["talkgroupsCreated"], 2);
    assert_eq!(talkgroup_count(&app.db).await, 2);

    let second = import_ok(&app, "?system=11", csv).await;
    assert_eq!(second["talkgroupsCreated"], 0);
    assert_eq!(second["talkgroupsUpdated"], 0);
    assert_eq!(second["talkgroupsUnchanged"], 2, "a re-import is a no-op");
    assert_eq!(talkgroup_count(&app.db).await, 2, "no duplicates");

    // And nothing about the rows moved.
    let tg = must_store(&app.db, 11, 54241).await;
    assert_eq!(tg.label.as_deref(), Some("TDB A1"));
    assert_eq!(groups_of(&app.db, tg.id).await, vec!["Fire".to_string()]);
}

/// The same Talkgroup Ref in two Systems is two Talkgroups — Refs are unique
/// only within a System, so an import must not collapse them.
#[tokio::test]
async fn the_same_ref_in_two_systems_stays_two_talkgroups() {
    let app = admin_app().await;

    import_ok(
        &app,
        "",
        "system,ref,label\n\
         11,100,Butler Dispatch\n\
         22,100,Warren Dispatch\n",
    )
    .await;

    assert_eq!(talkgroup_count(&app.db).await, 2);
    assert_eq!(
        must_store(&app.db, 11, 100).await.label.as_deref(),
        Some("Butler Dispatch")
    );
    assert_eq!(
        must_store(&app.db, 22, 100).await.label.as_deref(),
        Some("Warren Dispatch")
    );
}

// ---------------------------------------------------------------------------
// Blank cells leave curation alone
// ---------------------------------------------------------------------------

/// A narrow CSV must not erase what it doesn't mention. This is what makes a
/// two-column `ref,led` file safe to run over a fully-curated archive.
#[tokio::test]
async fn a_blank_cell_leaves_the_stored_value_alone() {
    let app = admin_app().await;
    import_ok(
        &app,
        "?system=11",
        "ref,label,name,group,tag,led\n54241,TDB A1,Butler Dispatch,Fire,Dispatch,red\n",
    )
    .await;

    // Now import only the LED column.
    let report = import_ok(&app, "?system=11", "ref,led\n54241,cyan\n").await;
    assert_eq!(report["talkgroupsUpdated"], 1);

    let tg = must_store(&app.db, 11, 54241).await;
    assert_eq!(tg.led.as_deref(), Some("cyan"), "the named column moved");
    assert_eq!(tg.label.as_deref(), Some("TDB A1"), "label survived");
    assert_eq!(tg.name.as_deref(), Some("Butler Dispatch"), "name survived");
    assert_eq!(tag_of(&app.db, &tg).await.as_deref(), Some("Dispatch"));
    assert_eq!(groups_of(&app.db, tg.id).await, vec!["Fire".to_string()]);

    // An explicitly empty cell in a column that *is* present is equally quiet.
    import_ok(&app, "?system=11", "ref,label,led\n54241,,magenta\n").await;
    let tg = must_store(&app.db, 11, 54241).await;
    assert_eq!(
        tg.label.as_deref(),
        Some("TDB A1"),
        "blank cell cleared nothing"
    );
    assert_eq!(tg.led.as_deref(), Some("magenta"));
}

/// Every curatable field moves when the CSV names it — checked one at a time,
/// so a field that silently never updates can't hide behind its neighbours.
#[tokio::test]
async fn each_named_column_updates_an_existing_talkgroup() {
    let app = admin_app().await;
    import_ok(
        &app,
        "?system=11",
        "ref,label,name,tag,led\n54241,Old label,Old name,Old tag,red\n",
    )
    .await;

    for (csv, check) in [
        ("ref,label\n54241,New label\n", "label"),
        ("ref,name\n54241,New name\n", "name"),
        ("ref,tag\n54241,New tag\n", "tag"),
        ("ref,led\n54241,cyan\n", "led"),
    ] {
        let report = import_ok(&app, "?system=11", csv).await;
        assert_eq!(report["talkgroupsUpdated"], 1, "{check} did not update");
    }

    let tg = must_store(&app.db, 11, 54241).await;
    assert_eq!(tg.label.as_deref(), Some("New label"));
    assert_eq!(tg.name.as_deref(), Some("New name"));
    assert_eq!(tg.led.as_deref(), Some("cyan"));
    assert_eq!(tag_of(&app.db, &tg).await.as_deref(), Some("New tag"));

    // Re-importing the same values is a no-op, per field.
    let report = import_ok(
        &app,
        "?system=11",
        "ref,label,name,tag,led\n54241,New label,New name,New tag,cyan\n",
    )
    .await;
    assert_eq!(report["talkgroupsUnchanged"], 1);
}

// ---------------------------------------------------------------------------
// Groups are a set the CSV owns
// ---------------------------------------------------------------------------

/// A Talkgroup can be in several Groups; rdio's importer only ever assigned one.
#[tokio::test]
async fn a_row_can_put_a_talkgroup_in_several_groups() {
    let app = admin_app().await;
    import_ok(
        &app,
        "?system=11",
        "ref,label,group\n54241,TDB A1,\"Fire;EMS;Butler County\"\n",
    )
    .await;

    let tg = must_store(&app.db, 11, 54241).await;
    assert_eq!(
        groups_of(&app.db, tg.id).await,
        vec![
            "Butler County".to_string(),
            "EMS".to_string(),
            "Fire".to_string()
        ]
    );
}

/// A non-empty Group cell is the whole set, so re-importing with fewer Groups
/// is how a Talkgroup gets removed from one. (rdio has no way to do this at all.)
#[tokio::test]
async fn re_importing_narrower_groups_removes_the_others() {
    let app = admin_app().await;
    import_ok(&app, "?system=11", "ref,group\n54241,\"Fire;EMS\"\n").await;
    let tg = must_store(&app.db, 11, 54241).await;
    assert_eq!(groups_of(&app.db, tg.id).await.len(), 2);

    let report = import_ok(&app, "?system=11", "ref,group\n54241,Fire\n").await;
    assert_eq!(report["talkgroupsUpdated"], 1, "a group change is a change");
    assert_eq!(groups_of(&app.db, tg.id).await, vec!["Fire".to_string()]);

    // The now-unused Group row itself survives — other Talkgroups may use it,
    // and orphan cleanup is retention's job, not an import's.
    assert!(
        group::Entity::find()
            .filter(group::Column::Name.eq("EMS"))
            .one(&app.db)
            .await
            .expect("group query")
            .is_some()
    );
}

// ---------------------------------------------------------------------------
// Systems
// ---------------------------------------------------------------------------

/// An operator can curate ahead of the first Call: a `system` Ref creates the
/// System, and the report says so, so a mistyped column is visible.
#[tokio::test]
async fn a_system_ref_may_be_created_and_is_reported() {
    let app = admin_app().await;
    let report = import_ok(&app, "?system=77", "ref,label\n54241,TDB A1\n").await;

    assert_eq!(report["systemsCreated"], 1);
    assert_eq!(
        must_store(&app.db, 77, 54241).await.label.as_deref(),
        Some("TDB A1")
    );
}

/// A System *label* never creates one: inventing a Ref for a mistyped name
/// would scatter Talkgroups into a System nothing ever ingests into.
#[tokio::test]
async fn an_unknown_system_label_rejects_its_rows_and_creates_nothing() {
    let app = admin_app().await;
    seed_call(&app.db, 11, "butco", 1).await;

    let report = import_ok(
        &app,
        "",
        "system,ref,label\n\
         butco,54241,TDB A1\n\
         typo,54242,Nowhere\n",
    )
    .await;

    assert_eq!(report["talkgroupsCreated"], 1);
    assert_eq!(report["systemsCreated"], 0);
    assert_eq!(report["rejected"][0]["line"], 3);
    assert_eq!(report["rejected"][0]["reason"], "unknown-system");
    assert_eq!(report["rejected"][0]["detail"], "typo");

    // The good row landed in the real System; the bad one made nothing.
    assert_eq!(
        must_store(&app.db, 11, 54241).await.label.as_deref(),
        Some("TDB A1")
    );
    assert_eq!(talkgroup_count(&app.db).await, 2); // the seeded tg 1 + 54241
}

/// A Trunk Recorder `short_name` is the System name an operator actually knows.
#[tokio::test]
async fn a_system_label_resolves_to_the_existing_system() {
    let app = admin_app().await;
    seed_call(&app.db, 11, "butco", 1).await;

    import_ok(&app, "?system=butco", "ref,label\n54241,TDB A1\n").await;
    assert_eq!(
        must_store(&app.db, 11, 54241).await.label.as_deref(),
        Some("TDB A1")
    );
    assert_eq!(talkgroup_count(&app.db).await, 2);
}

// ---------------------------------------------------------------------------
// Dry run
// ---------------------------------------------------------------------------

/// A dry run reports exactly what the real run does, and writes nothing.
#[tokio::test]
async fn dry_run_reports_the_real_run_and_writes_nothing() {
    let app = admin_app().await;
    let csv = "ref,label,group,tag,led\n\
               54241,TDB A1,Fire,Dispatch,red\n\
               54242,TDB A2,Fire,Fireground,purple\n";

    let dry = import_ok(&app, "?system=11&dryRun=true", csv).await;
    assert_eq!(dry["dryRun"], true);
    assert_eq!(
        talkgroup_count(&app.db).await,
        0,
        "a dry run writes nothing"
    );
    assert_eq!(
        group::Entity::find()
            .all(&app.db)
            .await
            .expect("groups")
            .len(),
        0,
        "not even the Groups it would have made"
    );

    let real = import_ok(&app, "?system=11", csv).await;
    assert_eq!(real["dryRun"], false);

    // Every count and rejection matches the promise the dry run made.
    for field in [
        "rows",
        "talkgroupsCreated",
        "talkgroupsUpdated",
        "talkgroupsUnchanged",
        "systemsCreated",
        "tagsCreated",
        "groupsCreated",
        "rejected",
        "layout",
    ] {
        assert_eq!(dry[field], real[field], "dry run mispredicted {field}");
    }
    assert_eq!(talkgroup_count(&app.db).await, 1); // the purple row was rejected
}

/// `?dryRun` with no value is the spelling an operator reaches for first. It
/// must mean "preview", never "write" — this is the one mistake here that
/// can't be undone.
#[tokio::test]
async fn dry_run_is_a_flag_not_a_value() {
    let csv = "ref,label\n54241,TDB A1\n";

    for query in [
        "?system=11&dryRun",
        "?system=11&dryRun=",
        "?system=11&dryRun=true",
    ] {
        let app = admin_app().await;
        let report = import_ok(&app, query, csv).await;
        assert_eq!(report["dryRun"], true, "query={query}");
        assert_eq!(
            talkgroup_count(&app.db).await,
            0,
            "query={query} wrote rows"
        );
    }

    // Only an explicit denial turns it back off.
    for query in [
        "?system=11",
        "?system=11&dryRun=false",
        "?system=11&dryRun=0",
        "?system=11&dryRun=no",
        "?system=11&dryRun=off",
    ] {
        let app = admin_app().await;
        let report = import_ok(&app, query, csv).await;
        assert_eq!(report["dryRun"], false, "query={query}");
        assert_eq!(
            talkgroup_count(&app.db).await,
            1,
            "query={query} wrote nothing"
        );
    }
}

/// A blank `?system=` is no default at all, not a System named "".
#[tokio::test]
async fn a_blank_system_parameter_is_no_default() {
    let app = admin_app().await;
    let report = import_ok(&app, "?system=", "ref,label\n54241,TDB A1\n").await;

    assert_eq!(report["rejected"][0]["reason"], "no-system");
    assert_eq!(talkgroup_count(&app.db).await, 0);
}

// ---------------------------------------------------------------------------
// Consistency with auto-populate (#8)
// ---------------------------------------------------------------------------

/// A Talkgroup the import *invents* must come out indistinguishable from one a
/// recorder would have auto-populated. Otherwise it has no Tag and no Group,
/// and since ingest leaves existing Talkgroups alone, its Calls would be
/// missing from the archive's Tag/Group filter facets forever.
#[tokio::test]
async fn an_invented_talkgroup_gets_the_same_defaults_ingest_would_give_it() {
    let app = admin_app().await;
    import_ok(&app, "?system=11", "ref,label,led\n54241,TDB A1,red\n").await;

    let tg = must_store(&app.db, 11, 54241).await;
    assert_eq!(tag_of(&app.db, &tg).await.as_deref(), Some("Untagged"));
    assert_eq!(groups_of(&app.db, tg.id).await, vec!["Unknown".to_string()]);
    // What the CSV *did* say still wins over the defaults.
    assert_eq!(tg.label.as_deref(), Some("TDB A1"));
    assert_eq!(tg.led.as_deref(), Some("red"));

    // A Call for it then lands in the facets a listener filters by.
    seed_call(&app.db, 11, "butco", 54241).await;
    let filters: Value = app.get_json("/api/calls/filters").await;
    assert!(
        filters["groups"]
            .as_array()
            .unwrap()
            .contains(&Value::String("Unknown".into())),
        "an invented talkgroup's calls must be filterable: {filters}"
    );
}

/// Curating an *existing* Talkgroup keeps "blank means leave alone" — the
/// defaults are for creation only, and must never overwrite curation.
#[tokio::test]
async fn defaults_never_overwrite_an_existing_talkgroup() {
    let app = admin_app().await;
    import_ok(&app, "?system=11", "ref,tag,group\n54241,Dispatch,Fire\n").await;

    // A later narrow import mentions neither tag nor group.
    let report = import_ok(&app, "?system=11", "ref,led\n54241,red\n").await;
    assert_eq!(report["talkgroupsUpdated"], 1);

    let tg = must_store(&app.db, 11, 54241).await;
    assert_eq!(tag_of(&app.db, &tg).await.as_deref(), Some("Dispatch"));
    assert_eq!(groups_of(&app.db, tg.id).await, vec!["Fire".to_string()]);
}

/// Curating ahead of the first Call, then receiving it: the recorder must land
/// on the Talkgroup the operator already curated, not a second one.
#[tokio::test]
async fn a_call_arriving_after_curation_lands_on_the_curated_talkgroup() {
    let app = admin_app().await;
    import_ok(&app, "?system=11", "ref,label,led\n54241,TDB A1,green\n").await;
    let curated = must_store(&app.db, 11, 54241).await;

    seed_call(&app.db, 11, "butco", 54241).await;

    let after = must_store(&app.db, 11, 54241).await;
    assert_eq!(after.id, curated.id, "ingest must reuse the curated row");
    assert_eq!(after.label.as_deref(), Some("TDB A1"), "curation survives");
    assert_eq!(after.led.as_deref(), Some("green"));
    assert_eq!(talkgroup_count(&app.db).await, 1, "no shadow talkgroup");
}

// ---------------------------------------------------------------------------
// Reporting instead of silence
// ---------------------------------------------------------------------------

/// rdio drops unparseable rows without a word. Every dropped row here is
/// reported with the line number and a machine-readable reason, in file order.
#[tokio::test]
async fn rejected_rows_are_reported_in_file_order() {
    let app = admin_app().await;
    let report = import_ok(
        &app,
        "?system=11",
        "ref,label,led\n\
         54241,Good,red\n\
         nope,Bad ref,red\n\
         54243,Bad led,chartreuse\n\
         ,No ref,red\n\
         54245,Also good,blue\n",
    )
    .await;

    assert_eq!(report["rows"], 5);
    assert_eq!(report["talkgroupsCreated"], 2);
    let rejected = report["rejected"].as_array().expect("rejected array");
    assert_eq!(
        rejected
            .iter()
            .map(|r| (r["line"].as_u64().unwrap(), r["reason"].as_str().unwrap()))
            .collect::<Vec<_>>(),
        vec![
            (3, "ref-not-a-number"),
            (4, "led-not-in-palette"),
            (5, "missing-ref"),
        ]
    );

    // Only the good rows landed.
    assert_eq!(talkgroup_count(&app.db).await, 2);
    assert!(stored_talkgroup(&app.db, 11, 54243).await.is_none());
}

/// A whole-file problem is a 400 naming the problem, not a 200 that did nothing.
#[tokio::test]
async fn unreadable_files_are_named_400s() {
    let app = admin_app().await;

    for (csv, reason) in [
        ("foo,bar\nx,y\n", "no-ref-column"),
        ("", "empty-csv"),
        ("ref,label\n", "empty-csv"),
    ] {
        let (status, body) = import(&app, "?system=11", csv).await;
        assert_eq!(status, 400, "csv={csv:?} body={body}");
        assert_eq!(body["error"], reason, "csv={csv:?}");
        assert!(body["detail"].is_string(), "a 400 explains itself");
    }

    // A Latin-1 export: rdio would silently store mojibake.
    let (status, body) = import_bytes(
        &app,
        "?system=11",
        b"ref,label\n54241,C\xf4t\xe9\n".to_vec(),
    )
    .await;
    assert_eq!(status, 400);
    assert_eq!(body["error"], "malformed-csv");

    assert_eq!(talkgroup_count(&app.db).await, 0, "nothing was written");
}

/// With no System named anywhere, rows are rejected rather than guessed into
/// whichever System happens to exist.
#[tokio::test]
async fn rows_with_no_system_are_rejected() {
    let app = admin_app().await;
    let report = import_ok(&app, "", "ref,label\n54241,TDB A1\n").await;

    assert_eq!(report["talkgroupsCreated"], 0);
    assert_eq!(report["rejected"][0]["reason"], "no-system");
    assert_eq!(talkgroup_count(&app.db).await, 0);
}

/// A dead database is a 500 — not a 200 with a report claiming an import that
/// never happened. What failed is written down here, not handed to the client
/// (ADR-0011 rule 4).
#[tokio::test]
async fn a_broken_database_is_a_server_error_not_a_false_report() {
    let capture = common::logs::LogCapture::start();
    let app = admin_app().await;
    app.db.clone().close().await.expect("close the pool");

    let (status, body) = app
        .post_admin_bytes(
            "/api/admin/talkgroups/import?system=11",
            "text/csv",
            b"ref,label\n54241,TDB A1\n".to_vec(),
        )
        .await;
    assert_eq!(status, 500);
    assert!(body.starts_with("internal error (request id: "), "{body:?}");

    let line = capture.only_line_containing("stage=import-talkgroups");
    assert!(
        line.contains(" ERROR "),
        "a 500 should say what failed: {line}"
    );
}

// ---------------------------------------------------------------------------
// Recorder-format compatibility
// ---------------------------------------------------------------------------

/// The headerless RadioReference / Trunk Recorder export an operator already
/// feeds to rdio-scanner, imported unchanged.
#[tokio::test]
async fn a_headerless_radioreference_export_imports_unchanged() {
    let app = admin_app().await;
    let report = import_ok(
        &app,
        "?system=11",
        "54241,D3E1,TDB A1,D,Butler Dispatch,Fire Dispatch,Butler County\n\
         54242,D3E2,TDB A2,D,Butler Fireground,Fire-Tac,Butler County\n",
    )
    .await;

    assert_eq!(report["layout"], "radio-reference");
    assert_eq!(report["talkgroupsCreated"], 2);

    let tg = must_store(&app.db, 11, 54241).await;
    assert_eq!(tg.label.as_deref(), Some("TDB A1"));
    assert_eq!(tg.name.as_deref(), Some("Butler Dispatch"));
    assert_eq!(tag_of(&app.db, &tg).await.as_deref(), Some("Fire Dispatch"));
    assert_eq!(
        groups_of(&app.db, tg.id).await,
        vec!["Butler County".to_string()]
    );
}

/// A quoted comma inside a field. rdio's regex split turns this into two
/// fields and shifts every column after it.
#[tokio::test]
async fn quoted_commas_do_not_shift_the_columns() {
    let app = admin_app().await;
    import_ok(
        &app,
        "?system=11",
        "54241,D3E1,\"Dispatch, North\",D,\"Butler, North\",Fire Dispatch,Butler County\n",
    )
    .await;

    let tg = must_store(&app.db, 11, 54241).await;
    assert_eq!(tg.label.as_deref(), Some("Dispatch, North"));
    assert_eq!(tg.name.as_deref(), Some("Butler, North"));
    assert_eq!(tag_of(&app.db, &tg).await.as_deref(), Some("Fire Dispatch"));
}

// ---------------------------------------------------------------------------
// The curated LED reaches the client
// ---------------------------------------------------------------------------

/// An imported LED is only worth storing if a listener sees it: it must ride
/// the same denormalized Call the live feed and the archive deliver (#11/#14).
#[tokio::test]
async fn an_imported_led_rides_the_call_payload() {
    let app = admin_app().await;
    seed_call(&app.db, 11, "butco", 54241).await;

    // Before curation there is no color, and the client falls back.
    let page: Value = app.get_json("/api/calls").await;
    assert!(page["results"][0].get("led").is_none());

    import_ok(&app, "?system=11", "ref,led\n54241,magenta\n").await;

    let page: Value = app.get_json("/api/calls").await;
    assert_eq!(page["results"][0]["led"], "magenta");
}

// ---------------------------------------------------------------------------
// Scale
// ---------------------------------------------------------------------------

/// A full county talkgroup list in one request — the real shape of US 37 —
/// lands in one transaction with the query count bounded by the *distinct*
/// Systems/Tags/Groups rather than by the row count.
#[tokio::test]
async fn a_full_county_list_imports_in_one_pass() {
    let app = admin_app().await;

    let mut csv = String::from("ref,label,group,tag,led\n");
    for i in 0..500 {
        let led = ["red", "blue", "green", "yellow"][i % 4];
        csv.push_str(&format!(
            "{},TG {i},Butler County,Dispatch,{led}\n",
            54000 + i
        ));
    }

    let report = import_ok(&app, "?system=11", &csv).await;
    assert_eq!(report["rows"], 500);
    assert_eq!(report["talkgroupsCreated"], 500);
    assert_eq!(report["tagsCreated"], 1, "one Tag, not 500");
    assert_eq!(report["groupsCreated"], 1, "one Group, not 500");
    assert_eq!(report["rejected"], serde_json::json!([]));
    assert_eq!(talkgroup_count(&app.db).await, 500);

    // Ordering is stable and every row landed with its own values.
    let rows = talkgroup::Entity::find()
        .order_by_asc(talkgroup::Column::Ref)
        .all(&app.db)
        .await
        .expect("talkgroups");
    assert_eq!(rows[0].label.as_deref(), Some("TG 0"));
    assert_eq!(rows[499].label.as_deref(), Some("TG 499"));
    assert_eq!(rows[499].led.as_deref(), Some("yellow"));
}

// ---------------------------------------------------------------------------
// Units (#47, spec US 43): `POST /api/admin/units/import`
// ---------------------------------------------------------------------------

/// POST a unit CSV, returning the status and parsed JSON.
async fn import_units(app: &TestApp, query: &str, csv: &str) -> (u16, Value) {
    let (status, body) = app
        .post_admin_bytes(
            &format!("/api/admin/units/import{query}"),
            "text/csv",
            csv.as_bytes().to_vec(),
        )
        .await;
    (
        status,
        serde_json::from_str(&body).unwrap_or_else(|e| panic!("json body ({e}): {body:?}")),
    )
}

async fn import_units_ok(app: &TestApp, query: &str, csv: &str) -> Value {
    let (status, body) = import_units(app, query, csv).await;
    assert_eq!(status, 200, "unit import failed: {body}");
    body
}

/// A fleet's numbering scheme in one paste (spec US 43): the apparatus, its
/// name, and the block of portables that answer to it.
///
/// rdio-scanner has no unit import at all — units are typed one at a time into
/// an admin table, which is why nobody's units are named.
#[tokio::test]
async fn a_unit_csv_creates_units_and_their_ranges() {
    let app = admin_app().await;

    let report = import_units_ok(
        &app,
        "?system=11",
        "ref,label,memberRefs\n\
         1200,Engine 1,1201-1299;4471\n\
         1300,Ladder 3,\n",
    )
    .await;

    assert_eq!(report["rows"], 2);
    assert_eq!(report["unitsCreated"], 2);
    assert_eq!(report["rangesAdded"], 2);
    assert_eq!(report["rejected"], serde_json::json!([]));

    let engine = app.unit_by_ref(11, 1200).await.expect("Engine 1");
    assert_eq!(engine.label.as_deref(), Some("Engine 1"));
    // The whole block reads back through the view a Listener reaches.
    let unit = app.get_json("/api/unit/11/1200").await;
    assert_eq!(
        unit["memberRefs"],
        serde_json::json!([{ "from": 1201, "to": 1299 }, { "from": 4471, "to": 4471 }])
    );
}

/// The names an operator writes reach the wire (spec US 42): a Call already in
/// the Archive, keyed by a portable inside the block, reads as the apparatus the
/// moment the CSV lands.
#[tokio::test]
async fn an_imported_unit_names_the_calls_already_archived() {
    let app = admin_app().await;
    app.seed_call(
        NewCall {
            units: vec![repo::NewCallUnit {
                unit_ref: 1250,
                ..Default::default()
            }],
            ..NewCall::new(11, 54241, 1000)
        },
        common::audio_at("k/1.wav"),
    )
    .await;

    import_units_ok(
        &app,
        "?system=11",
        "ref,label,memberRefs\n1200,Engine 1,1201-1299\n",
    )
    .await;

    let row = &app.get_json("/api/calls").await["results"][0];
    assert_eq!(row["unitRef"], 1200);
    assert_eq!(row["unitLabel"], "Engine 1");
}

/// Re-importing the same file is a no-op — the property that makes "fix the
/// rejected lines and import again" safe.
#[tokio::test]
async fn re_importing_a_unit_csv_changes_nothing() {
    let app = admin_app().await;
    let csv = "ref,label,memberRefs\n1200,Engine 1,1201-1299;4471\n";

    import_units_ok(&app, "?system=11", csv).await;
    let again = import_units_ok(&app, "?system=11", csv).await;

    assert_eq!(again["unitsCreated"], 0);
    assert_eq!(again["unitsUpdated"], 0);
    assert_eq!(again["unitsUnchanged"], 1);
    assert_eq!(again["rangesAdded"], 0);
    assert_eq!(again["rangesRemoved"], 0);
}

/// A non-empty member-Refs cell **is** the set, so dropping a span from the
/// file is how an operator takes it back; `-` is the empty set, and a blank
/// cell says nothing at all. The same three sentences the Talkgroup importer
/// speaks (#45), because an operator should not have to learn two dialects.
#[tokio::test]
async fn the_member_refs_cell_is_the_whole_truth_for_the_row() {
    let app = admin_app().await;
    import_units_ok(
        &app,
        "?system=11",
        "ref,label,memberRefs\n1200,Engine 1,1201-1299;4471\n",
    )
    .await;

    // A blank cell leaves the Ranges alone...
    import_units_ok(&app, "?system=11", "ref,label\n1200,Engine One\n").await;
    assert_eq!(
        app.get_json("/api/unit/11/1200").await["memberRefs"],
        serde_json::json!([{ "from": 1201, "to": 1299 }, { "from": 4471, "to": 4471 }])
    );

    // ...a narrower cell drops what it no longer names...
    let narrowed = import_units_ok(
        &app,
        "?system=11",
        "ref,label,memberRefs\n1200,Engine One,1201-1299\n",
    )
    .await;
    assert_eq!(narrowed["rangesRemoved"], 1);
    assert_eq!(
        app.get_json("/api/unit/11/1200").await["memberRefs"],
        serde_json::json!([{ "from": 1201, "to": 1299 }])
    );

    // ...and `-` takes them all back.
    let emptied = import_units_ok(
        &app,
        "?system=11",
        "ref,label,memberRefs\n1200,Engine One,-\n",
    )
    .await;
    assert_eq!(emptied["rangesRemoved"], 1);
    assert!(
        app.get_json("/api/unit/11/1200")
            .await
            .get("memberRefs")
            .is_none()
    );
}

/// A Range that collides with one already owned is **refused, naming the Range
/// in the way** (#45) — because "that overlaps something" is not actionable on a
/// fleet with forty of them, and because a Ref inside two Ranges would attribute
/// one radio's Calls to two apparatus depending on the day.
#[tokio::test]
async fn a_range_overlapping_another_units_is_a_reported_row() {
    let app = admin_app().await;

    let report = import_units_ok(
        &app,
        "?system=11",
        "ref,label,memberRefs\n\
         1200,Engine 1,1201-1299\n\
         1400,Ladder 3,1250-1350\n",
    )
    .await;

    assert_eq!(report["unitsCreated"], 2, "the Unit itself is fine");
    assert_eq!(
        report["rejected"],
        serde_json::json!([{ "line": 3, "reason": "range-overlaps", "detail": "1201-1299" }])
    );
    assert!(
        app.get_json("/api/unit/11/1400")
            .await
            .get("memberRefs")
            .is_none(),
        "no half-applied Range"
    );
}

/// Rows that cannot be read are reported one by one, with the line number an
/// operator can open — never silently dropped, which is what rdio does.
#[tokio::test]
async fn unreadable_unit_rows_are_reported_per_row() {
    let app = admin_app().await;

    let report = import_units_ok(
        &app,
        "?system=11",
        "ref,label,memberRefs\n\
         notanumber,Engine 1,\n\
         1300,Ladder 3,12ab-1300\n\
         1400,Rescue 4,\n",
    )
    .await;

    assert_eq!(report["rows"], 3);
    assert_eq!(report["unitsCreated"], 1, "the good row still landed");
    assert_eq!(
        report["rejected"],
        serde_json::json!([
            { "line": 2, "reason": "ref-not-a-number", "detail": "notanumber" },
            { "line": 3, "reason": "member-ref-not-a-number", "detail": "12ab-1300" },
        ])
    );
}

/// A dry run walks the identical path and rolls it back, so the report is a
/// promise about the real run rather than a separate estimate of it.
#[tokio::test]
async fn a_unit_dry_run_reports_the_real_run_and_writes_nothing() {
    let app = admin_app().await;
    let csv = "ref,label,memberRefs\n1200,Engine 1,1201-1299\n";

    let dry = import_units_ok(&app, "?system=11&dryRun=true", csv).await;
    assert_eq!(dry["dryRun"], true);
    assert_eq!(dry["unitsCreated"], 1);
    assert_eq!(dry["rangesAdded"], 1);
    assert!(app.unit_by_ref(11, 1200).await.is_none(), "nothing written");

    let real = import_units_ok(&app, "?system=11", csv).await;
    assert_eq!(real["dryRun"], false);
    assert_eq!(real["unitsCreated"], dry["unitsCreated"]);
    assert_eq!(real["rangesAdded"], dry["rangesAdded"]);
    assert!(app.unit_by_ref(11, 1200).await.is_some());
}

/// A file with no `ref` column is a whole-file 400 naming what was missing —
/// there is no positional fallback here, because no third party exports unit
/// lists and guessing at columns would silently rename a fleet.
#[tokio::test]
async fn a_unit_csv_with_no_ref_column_is_a_named_400() {
    let app = admin_app().await;

    let (status, body) = import_units(&app, "?system=11", "1200,Engine 1\n").await;

    assert_eq!(status, 400);
    assert_eq!(body["error"], "no-unit-ref-column");
    assert!(
        body["detail"].as_str().expect("detail").contains("header"),
        "{body}"
    );
}

/// Rows naming no System are rejected rather than scattered into one invented
/// for them — a unit Ref means nothing without the System it is unique within.
#[tokio::test]
async fn unit_rows_with_no_system_are_rejected() {
    let app = admin_app().await;

    let report = import_units_ok(&app, "", "ref,label\n1200,Engine 1\n").await;

    assert_eq!(report["unitsCreated"], 0);
    assert_eq!(report["rejected"][0]["reason"], "no-system");
}

/// The importer is admin-gated like every other route under the prefix (#19).
#[tokio::test]
async fn the_unit_importer_is_behind_the_admin_session() {
    let app = TestApp::spawn().await;

    let (status, _) = app
        .post_bytes(
            "/api/admin/units/import",
            "text/csv",
            b"ref\n1200\n".to_vec(),
        )
        .await;

    assert_eq!(status, 401);
}

/// A file with a header and nothing under it, and a file with nothing at all,
/// are both "no data rows" — not a silent success reporting zero.
#[tokio::test]
async fn a_unit_csv_with_no_data_rows_is_a_named_400() {
    let app = admin_app().await;

    for empty in ["", "ref,label\n"] {
        let (status, body) = import_units(&app, "?system=11", empty).await;
        assert_eq!(status, 400, "{empty:?} -> {body}");
        assert_eq!(body["error"], "empty-csv", "{empty:?}");
    }
}

/// The row-level readings a unit CSV can refuse, one at a time — each reported
/// with the line an operator opens rather than dropped.
#[tokio::test]
async fn every_unreadable_unit_cell_is_its_own_reported_row() {
    let app = admin_app().await;

    let report = import_units_ok(
        &app,
        "?system=11",
        "ref,label,memberRefs\n\
         ,Engine 1,\n\
         1300,Ladder 3,1301-13ab\n",
    )
    .await;

    assert_eq!(
        report["rejected"],
        serde_json::json!([
            { "line": 2, "reason": "missing-ref" },
            { "line": 3, "reason": "member-ref-not-a-number", "detail": "1301-13ab" },
        ])
    );
}

/// A per-row `system` column, so one file can carry a whole instance's fleets —
/// and a mistyped System *label* rejects its rows rather than inventing a
/// System nothing will ever ingest into (the Talkgroup importer's rule, #18).
#[tokio::test]
async fn a_unit_row_may_name_its_own_system_and_a_bad_name_rejects_it() {
    let app = admin_app().await;
    app.seed_unit(11, 1, "placeholder").await; // brings System 11 into being

    let report = import_units_ok(
        &app,
        "",
        "system,ref,label\n\
         11,1200,Engine 1\n\
         nosuchcounty,1300,Ladder 3\n",
    )
    .await;

    assert_eq!(report["unitsCreated"], 1);
    assert_eq!(
        report["rejected"],
        serde_json::json!([
            { "line": 3, "reason": "unknown-system", "detail": "nosuchcounty" }
        ])
    );
    assert_eq!(
        app.unit_by_ref(11, 1200).await.expect("Engine 1").label,
        Some("Engine 1".to_string())
    );
}

/// A stray separator in the member-Refs cell is punctuation, not a Range of
/// nothing — the same reading the Talkgroup importer gives its own cell.
#[tokio::test]
async fn empty_entries_in_a_unit_cell_are_punctuation_not_ranges() {
    let app = admin_app().await;

    let report = import_units_ok(
        &app,
        "?system=11",
        "ref,label,memberRefs\n1200,Engine 1,;1201-1299;;\n",
    )
    .await;

    assert_eq!(report["rangesAdded"], 1);
    assert_eq!(report["rejected"], serde_json::json!([]));
}

/// A unit import that cannot reach the database is a 500 naming its own stage,
/// never a report claiming rows landed (ADR-0011 rule 4).
#[tokio::test]
async fn a_broken_database_is_a_server_error_not_a_false_unit_report() {
    let capture = common::logs::LogCapture::start();
    let app = admin_app().await;
    app.db.clone().close().await.expect("close the pool");

    let (status, body) = app
        .post_admin_bytes(
            "/api/admin/units/import?system=11",
            "text/csv",
            b"ref,label\n1200,Engine 1\n".to_vec(),
        )
        .await;

    assert_eq!(status, 500);
    assert!(body.starts_with("internal error (request id: "), "{body:?}");
    let line = capture.only_line_containing("stage=import-units");
    assert!(
        line.contains(" ERROR "),
        "a 500 should say what failed: {line}"
    );
}

/// **Setting a Unit's Ranges reads the System's spans once, not once per span.**
///
/// The overlap guarantee (#45) is a comparison against every Range the System
/// already owns, and the obvious way to write it — ask, insert, ask again — is a
/// full read of `unit_refs` per row of a fleet CSV. Correct, and quadratic: a
/// county list of five hundred apparatus would spend a thousand table reads on a
/// Pi to write five hundred rows.
///
/// Asserted as a *slope* rather than a total: seven more Ranges must cost seven
/// more statements — the seven inserts and nothing else. A pinned total would
/// break every time something unrelated changed.
#[tokio::test]
async fn a_units_ranges_cost_one_statement_each_to_write() {
    async fn spent(app: &TestApp, spans: usize) -> u64 {
        let cell = (0..spans)
            .map(|n| format!("{}-{}", 1000 + n * 10, 1005 + n * 10))
            .collect::<Vec<_>>()
            .join(";");
        app.settle().await;
        let before = app.statements_issued();
        let report =
            import_units_ok(app, "?system=11", &format!("ref,memberRefs\n500,{cell}\n")).await;
        assert_eq!(report["rangesAdded"], spans as u64, "{report}");
        app.statements_issued() - before
    }

    // Two apps, because a Unit that already owns spans is a different write.
    let app = admin_app().await;
    // Postgres inserts with `RETURNING`, so sea-orm gets the stored row back in
    // the same statement; SQLite needs a `SELECT` after it (ADR-0003, and the
    // same dialect difference `tests/ingest.rs` spells out).
    let read_back = match app.db.get_database_backend() {
        sea_orm::DatabaseBackend::Postgres => 0,
        _ => 1,
    };
    let one = spent(&app, 1).await;
    let eight = spent(&admin_app().await, 8).await;

    assert_eq!(
        eight - one,
        7 * (1 + read_back),
        "seven more Ranges, seven more inserts — and no second read of the \
         System's spans ({one} for one, {eight} for eight)"
    );
}
