//! The curated configuration as one portable document (#51, spec US 47).
//!
//! What is asserted here is what an Operator could observe: a file they can
//! read, keep, diff and hand to another Instance — and an import that either
//! applies wholly or tells them which entry it would not take.
//!
//! The headline claim is a **move**, not a reload: everything below builds a
//! curated Instance, carries its document to a second one that has never seen a
//! Call, and insists the two now describe the same world. A wipe-and-restore is
//! the easier half of that and would pass while the document still leaned on
//! something local (an id, a row that happened to survive), which is the failure
//! this file exists to catch.

mod common;

use common::{CallUpload, TestApp};
use serde_json::{Value, json};

/// An app with a session open, ready to curate.
async fn curating_app() -> TestApp {
    let app = TestApp::with_key("k").await;
    app.login().await;
    app
}

/// `GET` the configuration document.
async fn export(app: &TestApp) -> Value {
    let (status, document) = app.admin_get("/api/admin/config").await;
    assert_eq!(status, 200, "export failed: {document}");
    document
}

/// `POST` one back, and insist it applied.
async fn import(app: &TestApp, document: &Value) -> Value {
    let (status, report) = app
        .admin_post("/api/admin/config/import", document.clone())
        .await;
    assert_eq!(status, 200, "import failed: {report}");
    report
}

/// Preview one instead.
async fn preview(app: &TestApp, document: &Value) -> (u16, Value) {
    app.admin_post("/api/admin/config/import?dryRun", document.clone())
        .await
}

/// Everything a Listener can select — the surface the round trip is really
/// about, read through the endpoint the panel reads.
async fn catalog(app: &TestApp) -> Value {
    app.get_json("/api/catalog").await
}

/// A System with a curated channel, a named radio, and a key — one of each
/// thing the document carries, so a field dropped anywhere fails a test here.
async fn a_curated_instance() -> TestApp {
    let app = curating_app().await;
    app.seed_system(11, true, Some("9999")).await;

    let (status, created) = app
        .admin_post(
            "/api/admin/talkgroups",
            json!({
                "systemId": system_id(&app, 11).await,
                "ref": 100,
                "label": "Fire Dispatch",
                "name": "Fulton Fire Dispatch",
                "tag": "Fire",
                "groups": ["Fire", "Dispatch"],
                "led": "red",
            }),
        )
        .await;
    assert_eq!(status, 201, "{created}");

    app.seed_talkgroup(11, 8123).await;
    let owner = created["id"].as_i64().expect("an id");
    app.admin_post(
        &format!("/api/admin/talkgroups/{owner}/members"),
        json!({"fold": [8123]}),
    )
    .await;

    app.seed_unit(11, 1200, "Engine 1").await;
    app.seed_unit_range(11, 1200, 1201, 1299).await;
    app.admin_post(
        "/api/admin/api-keys",
        json!({"label": "the pi", "systemRef": 11}),
    )
    .await;

    app
}

/// The Id of the System answering to `system_ref`.
async fn system_id(app: &TestApp, system_ref: i64) -> i64 {
    let (_, systems) = app.admin_get("/api/admin/systems").await;
    systems["results"]
        .as_array()
        .expect("systems")
        .iter()
        .find(|row| row["ref"] == system_ref)
        .expect("the System")["id"]
        .as_i64()
        .expect("an id")
}

// ---------------------------------------------------------------------------
// What comes out
// ---------------------------------------------------------------------------

/// **The document is self-contained and nested by System**, because a Ref is
/// unique only within one (CONTEXT.md) — a flat list would repeat the System on
/// every row and invite the one bug this whole feature could have.
#[tokio::test]
async fn the_document_carries_every_curated_entity_under_its_system() {
    let app = a_curated_instance().await;

    let document = export(&app).await;

    assert_eq!(document["version"], 1);
    let system = &document["systems"][0];
    assert_eq!(system["ref"], 11);
    assert_eq!(system["autoPopulate"], true);
    assert_eq!(system["blacklist"], json!([9999]));

    let talkgroup = &system["talkgroups"][0];
    assert_eq!(talkgroup["ref"], 100);
    assert_eq!(talkgroup["label"], "Fire Dispatch");
    assert_eq!(talkgroup["name"], "Fulton Fire Dispatch");
    assert_eq!(talkgroup["tag"], "Fire");
    assert_eq!(talkgroup["groups"], json!(["Dispatch", "Fire"]));
    assert_eq!(talkgroup["led"], "red");
    assert_eq!(
        talkgroup["memberRefs"],
        json!([8123]),
        "the merge is part of the configuration (#45)"
    );

    let unit = &system["units"][0];
    assert_eq!(unit["ref"], 1200);
    assert_eq!(unit["label"], "Engine 1");
    assert_eq!(unit["ranges"], json!([{"from": 1201, "to": 1299}]));

    // The roster is the roster — the harness's own key and the one first run
    // provisions are in it too, which is right and is why this asserts the
    // *shape* of every entry rather than a list of one.
    let keys = document["apiKeys"].as_array().expect("api keys");
    assert!(
        keys.contains(&json!({"label": "the pi", "systemRef": 11, "disabled": false})),
        "{keys:?}"
    );
    for key in keys {
        let fields: Vec<&String> = key.as_object().expect("an entry").keys().collect();
        assert!(
            fields
                .iter()
                .all(|field| ["label", "systemRef", "disabled"].contains(&field.as_str())),
            "a key entry carries something it should not: {fields:?}"
        );
    }
}

/// **No secret leaves, in any form.** `api_keys.key_hash` is unsalted SHA-256,
/// so an exported hash is offline-crackable for any Operator who chose a key
/// they could remember — and this file gets emailed, committed and pasted.
///
/// Asserted over the **whole serialized document** rather than the one field it
/// would live on, because a field added later that happened to carry it would
/// pass a narrower test.
#[tokio::test]
async fn no_secret_and_no_hash_is_anywhere_in_the_document() {
    let app = curating_app().await;
    app.seed_system(11, true, None).await;
    let (_, issued) = app
        .admin_post("/api/admin/api-keys", json!({"label": "the pi"}))
        .await;
    let secret = issued["key"]
        .as_str()
        .expect("the one sight of it")
        .to_owned();

    let document = export(&app).await;

    let text = serde_json::to_string(&document).expect("serialize");
    assert!(!text.contains(&secret), "the key itself is in the document");
    let hash = format!(
        "{:x}",
        <sha2::Sha256 as sha2::Digest>::digest(secret.as_bytes())
    );
    assert!(!text.contains(&hash), "the hash is in the document");
    assert!(!text.contains("keyHash"), "a field for it exists: {text}");
}

/// **Only Units carrying curation.** An Instance rosters a Unit per radio it has
/// ever heard (#47), and a bare number re-rosters from the first Call that keys
/// — so carrying ten thousand of them costs file size and an Operator's
/// attention for nothing they curated.
#[tokio::test]
async fn a_unit_nobody_curated_is_left_out() {
    let app = curating_app().await;
    app.seed_system(11, true, None).await;
    app.seed_unit(11, 1200, "Engine 1").await;
    app.seed_unit(11, 4471, None).await;
    app.seed_unit(11, 4472, None).await;
    app.seed_unit_range(11, 4472, 5000, 5099).await;

    let document = export(&app).await;

    let refs: Vec<i64> = document["systems"][0]["units"]
        .as_array()
        .expect("units")
        .iter()
        .map(|unit| unit["ref"].as_i64().expect("a ref"))
        .collect();
    assert_eq!(
        refs,
        vec![1200, 4472],
        "a name or a Range is curation; a bare roster entry is not"
    );
}

/// **The document is a pure function of the configuration**, which is what makes
/// it diffable in git (spec US 47's "versionable"). No timestamp, no ids, and a
/// deterministic order — so two exports of the same Instance are the same bytes,
/// and a diff shows what an Operator changed rather than when they looked.
#[tokio::test]
async fn two_exports_of_one_instance_are_identical_bytes() {
    let app = a_curated_instance().await;

    let first = serde_json::to_string(&export(&app).await).expect("serialize");
    let second = serde_json::to_string(&export(&app).await).expect("serialize");

    assert_eq!(first, second);
}

/// The download names itself, so a browser's Downloads folder is readable and
/// two backups do not collide.
#[tokio::test]
async fn the_export_names_the_file_it_downloads_as() {
    let app = curating_app().await;

    let response = app
        .admin_verb(reqwest::Method::GET, "/api/admin/config", None)
        .send()
        .await
        .expect("a request");

    let disposition = response
        .headers()
        .get("content-disposition")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    assert!(disposition.contains("attachment"), "{disposition}");
    assert!(disposition.contains("radio-scout-config"), "{disposition}");
    assert!(disposition.ends_with(".json\""), "{disposition}");
}

// ---------------------------------------------------------------------------
// The move
// ---------------------------------------------------------------------------

/// **The headline claim**: a curated setup moves to an Instance that has never
/// seen a Call, and the two then describe the same world.
///
/// Asserted twice over, because each half would pass alone while the other was
/// broken: the **document** round-trips to identical bytes (so nothing was
/// dropped on the way in *or* leaned on something local on the way out), and the
/// **catalog** matches (so what a Listener can actually select is the same,
/// which is the point of carrying any of it).
#[tokio::test]
async fn a_curated_configuration_moves_to_another_instance_whole() {
    let source = a_curated_instance().await;
    let document = export(&source).await;

    let target = curating_app().await;
    let report = import(&target, &document).await;

    assert_eq!(report["dryRun"], false, "{report}");
    assert_eq!(
        report["rejected"],
        json!([]),
        "nothing about a document we just wrote should be refusable: {report}"
    );
    assert_eq!(
        export(&target).await,
        document,
        "the document does not round-trip"
    );
    assert_eq!(
        catalog(&target).await,
        catalog(&source).await,
        "the catalog does not round-trip"
    );
}

/// Importing the same document twice changes nothing the second time — the
/// property that makes a restore safe to retry after a half-finished one.
#[tokio::test]
async fn importing_a_document_twice_is_a_no_op() {
    let source = a_curated_instance().await;
    let document = export(&source).await;
    let target = curating_app().await;
    import(&target, &document).await;

    let again = import(&target, &document).await;

    assert_eq!(again["systems"]["created"], 0, "{again}");
    assert_eq!(again["talkgroups"]["created"], 0, "{again}");
    assert_eq!(again["talkgroups"]["updated"], 0, "{again}");
    assert_eq!(again["units"]["created"], 0, "{again}");
    assert_eq!(export(&target).await, document);
}

/// **An import never deletes.** Absence means "not mentioned", because inferring
/// deletion from absence is the rdio-scanner failure the whole curation surface
/// exists to not repeat — and here it would be a truncated file destroying a
/// county's worth of curation, with the Calls behind it.
#[tokio::test]
async fn an_import_leaves_what_the_document_never_mentioned() {
    let app = a_curated_instance().await;
    let document = export(&app).await;
    let (status, kept) = app
        .admin_post(
            "/api/admin/talkgroups",
            json!({"systemId": system_id(&app, 11).await, "ref": 700, "label": "EMS"}),
        )
        .await;
    assert_eq!(status, 201, "{kept}");

    // The document was taken before 700 existed, so it says nothing about it.
    import(&app, &document).await;

    let refs: Vec<i64> = export(&app).await["systems"][0]["talkgroups"]
        .as_array()
        .expect("talkgroups")
        .iter()
        .map(|row| row["ref"].as_i64().expect("a ref"))
        .collect();
    assert!(refs.contains(&700), "a row nobody mentioned was deleted");
}

/// A key comes back as a **row with a new secret**, shown once — the roster's
/// shape and scoping survive a move, and the Operator is told which recorder
/// each new secret belongs to.
#[tokio::test]
async fn an_imported_key_is_re_issued_and_shown_once() {
    let source = a_curated_instance().await;
    let document = export(&source).await;
    let target = curating_app().await;

    let report = import(&target, &document).await;

    let issued = &report["apiKeys"][0];
    assert_eq!(issued["label"], "the pi");
    assert_eq!(issued["systemRef"], 11);
    let secret = issued["key"].as_str().expect("the one sight of it");
    assert!(!secret.is_empty());

    // And it really works, which is the only proof that matters for a key.
    let (status, _) = target
        .upload(CallUpload::new().key(secret).talkgroup(100))
        .await;
    assert_eq!(status, 200, "the re-issued key does not authorize ingest");
}

// ---------------------------------------------------------------------------
// The preview, and what it refuses
// ---------------------------------------------------------------------------

/// A dry run walks the identical path and rolls back, so its report is a promise
/// about the real run rather than a separate estimate of it (#18's rule).
#[tokio::test]
async fn a_dry_run_reports_what_the_real_import_then_does_and_writes_nothing() {
    let source = a_curated_instance().await;
    let document = export(&source).await;
    let target = curating_app().await;

    let (status, previewed) = preview(&target, &document).await;

    assert_eq!(status, 200, "{previewed}");
    assert_eq!(previewed["dryRun"], true);
    assert_eq!(
        export(&target).await["systems"],
        json!([]),
        "a preview wrote something"
    );

    let real = import(&target, &document).await;
    assert_eq!(real["systems"], previewed["systems"]);
    assert_eq!(real["talkgroups"], previewed["talkgroups"]);
    assert_eq!(real["units"], previewed["units"]);
}

/// **A refused entry names where it is**, which is a JSON document's answer to
/// the CSV importer's line number — and the rest of the document still applies,
/// because one bad LED must not cost an Operator their whole restore.
#[tokio::test]
async fn a_bad_entry_is_rejected_by_path_and_the_rest_applies() {
    let app = curating_app().await;

    let report = import(
        &app,
        &json!({
            "version": 1,
            "systems": [{
                "ref": 11,
                "talkgroups": [
                    {"ref": 100, "label": "Fire", "led": "puce"},
                    {"ref": 200, "label": "EMS"},
                ],
            }],
        }),
    )
    .await;

    let rejected = &report["rejected"][0];
    assert_eq!(rejected["at"], "systems[0].talkgroups[0]");
    assert_eq!(rejected["reason"], "unknown-led");
    assert!(
        rejected["detail"]
            .as_str()
            .expect("a sentence")
            .contains("puce"),
        "{rejected}"
    );
    assert_eq!(report["talkgroups"]["created"], 1, "the good row applied");
}

/// A document from a version this Instance does not understand is refused whole,
/// rather than half-applied by guessing. rdio's own version check is commented
/// out in `import-export-config.component.ts`, so it accepts any release's file.
#[tokio::test]
async fn a_document_from_an_unknown_version_is_refused() {
    let app = curating_app().await;

    let (status, refused) = app
        .admin_post(
            "/api/admin/config/import",
            json!({"version": 99, "systems": []}),
        )
        .await;

    assert_eq!(status, 400, "{refused}");
    assert_eq!(refused["error"], "unknown-document-version");
    assert!(
        refused["detail"]
            .as_str()
            .expect("a sentence")
            .contains("99"),
        "{refused}"
    );
}

/// A body that is not a configuration document at all is refused **by name**,
/// with a sentence — an Operator who picked the wrong file gets told that, not
/// serde's opinion of their JSON through a bare 422.
#[tokio::test]
async fn a_body_that_is_not_a_document_is_refused_by_name() {
    let app = curating_app().await;

    let (status, refused) = app
        .admin_post("/api/admin/config/import", json!({"nonsense": true}))
        .await;

    assert_eq!(status, 400, "{refused}");
    assert_eq!(refused["error"], "malformed-document");
    assert!(
        refused["detail"]
            .as_str()
            .expect("a sentence")
            .contains("version"),
        "{refused}"
    );
}

/// **A document claiming to be ours is held to our shape**, so a hand-edited
/// file with a misspelled key is caught rather than half-applied — the strictness
/// every other body on this surface has.
#[tokio::test]
async fn a_misspelled_field_in_our_own_version_is_refused() {
    let app = curating_app().await;

    let (status, refused) = app
        .admin_post(
            "/api/admin/config/import",
            json!({"version": 1, "systems": [{"ref": 11, "talkgruops": []}]}),
        )
        .await;

    assert_eq!(status, 400, "{refused}");
    assert_eq!(refused["error"], "malformed-document");
    assert!(
        refused["detail"]
            .as_str()
            .expect("a sentence")
            .contains("talkgruops"),
        "it names the field: {refused}"
    );
}

/// **A document from a newer release says so**, rather than complaining about
/// the field that release added. That ordering is the whole point of a version:
/// applied the other way round, an Operator upgrading gets "unknown field
/// `downstreams`" and no idea that their instance is the old one.
#[tokio::test]
async fn a_newer_document_names_its_version_not_its_new_fields() {
    let app = curating_app().await;

    let (status, refused) = app
        .admin_post(
            "/api/admin/config/import",
            json!({"version": 2, "systems": [], "downstreams": [{"url": "…"}]}),
        )
        .await;

    assert_eq!(status, 400, "{refused}");
    assert_eq!(refused["error"], "unknown-document-version");
    assert!(
        !refused["detail"]
            .as_str()
            .expect("a sentence")
            .contains("downstreams"),
        "it complained about the new field instead: {refused}"
    );
}

// ---------------------------------------------------------------------------
// What it costs
// ---------------------------------------------------------------------------

/// **The export is one query per table, never one per row** (#86's rule).
///
/// A county's configuration is hundreds of Talkgroups and thousands of Units,
/// and the per-row shape is thousands of round trips on a Pi — an answer that is
/// entirely correct and only slow, which is exactly the class of bug the
/// statement counter exists to make visible from outside.
///
/// Asserted as *equal cost at two sizes* rather than a pinned number, so adding
/// a table to the document is free to cost one more statement and only a per-row
/// read fails.
#[tokio::test]
async fn exporting_a_big_configuration_costs_what_a_small_one_does() {
    let app = curating_app().await;
    app.seed_system(11, true, None).await;
    app.seed_talkgroup(11, 100).await;
    app.seed_unit(11, 1200, "Engine 1").await;

    let before = app.statements_issued();
    export(&app).await;
    let small = app.statements_issued() - before;

    for r#ref in 200..260 {
        app.seed_talkgroup(11, r#ref).await;
        app.seed_unit(11, 4000 + r#ref, "Portable").await;
    }

    let before = app.statements_issued();
    export(&app).await;
    let big = app.statements_issued() - before;

    assert_eq!(
        small, big,
        "the export reads per row: {small} statements for 1 channel, {big} for 61"
    );
}

// ---------------------------------------------------------------------------
// Importing over a configuration that already exists
// ---------------------------------------------------------------------------

/// **The document is the authority for the rows it names.** A restore onto an
/// Instance that has drifted has to move it back, not merge with it — which is
/// the difference between this and the CSV importer, where a blank cell means
/// "leave alone" because a spreadsheet cannot spell the difference.
#[tokio::test]
async fn a_document_moves_what_the_instance_had_differently() {
    let app = curating_app().await;
    app.seed_system(11, false, None).await;
    app.seed_talkgroup(11, 100).await;
    app.seed_unit(11, 1200, "Old name").await;

    let report = import(
        &app,
        &json!({
            "version": 1,
            "systems": [{
                "ref": 11,
                "label": "Fulton",
                "autoPopulate": true,
                "blacklist": [9999],
                "talkgroups": [{"ref": 100, "label": "Fire Dispatch", "led": "red"}],
                "units": [{"ref": 1200, "label": "Engine 1"}],
            }],
        }),
    )
    .await;

    assert_eq!(report["systems"]["updated"], 1, "{report}");
    assert_eq!(report["talkgroups"]["updated"], 1, "{report}");
    assert_eq!(report["units"]["updated"], 1, "{report}");

    let system = &export(&app).await["systems"][0];
    assert_eq!(system["label"], "Fulton");
    assert_eq!(system["autoPopulate"], true);
    assert_eq!(system["blacklist"], json!([9999]));
    assert_eq!(system["talkgroups"][0]["label"], "Fire Dispatch");
    assert_eq!(system["talkgroups"][0]["led"], "red");
    assert_eq!(system["units"][0]["label"], "Engine 1");
}

/// A member Ref another channel already holds is spoken for, and taking it would
/// break that channel to fix this one — the same refusal the browser and the CSV
/// make, reported here at the entry that asked for it.
#[tokio::test]
async fn a_member_ref_another_channel_holds_is_rejected_by_path() {
    let app = curating_app().await;
    app.seed_talkgroup(11, 200).await;
    app.seed_member_ref(11, 200, 8123).await;

    let report = import(
        &app,
        &json!({
            "version": 1,
            "systems": [{
                "ref": 11,
                "talkgroups": [{"ref": 100, "memberRefs": [8123]}],
            }],
        }),
    )
    .await;

    let rejected = &report["rejected"][0];
    assert_eq!(rejected["at"], "systems[0].talkgroups[0]");
    assert_eq!(rejected["reason"], "member-ref-owned-elsewhere");
    assert_eq!(
        app.member_refs(11, 200).await,
        vec![8123],
        "the other channel's merge is untouched"
    );
    assert_eq!(
        report["talkgroups"]["created"], 1,
        "the channel itself still applied — only its merge was refused"
    );
}

/// A Range colliding with one the System already owns is refused at its entry,
/// because a Ref inside two Ranges belongs to whichever row the query returns
/// first — one radio attributing to two apparatus depending on the day.
#[tokio::test]
async fn an_overlapping_range_is_rejected_by_path() {
    let app = curating_app().await;
    app.seed_unit(11, 4400, "Ladder 2").await;
    app.seed_unit_range(11, 4400, 4400, 4499).await;

    let report = import(
        &app,
        &json!({
            "version": 1,
            "systems": [{
                "ref": 11,
                "units": [{"ref": 1200, "label": "Engine 1",
                           "ranges": [{"from": 4450, "to": 4550}]}],
            }],
        }),
    )
    .await;

    let rejected = &report["rejected"][0];
    assert_eq!(rejected["at"], "systems[0].units[0]");
    assert_eq!(rejected["reason"], "range-overlaps");
    assert!(
        rejected["detail"]
            .as_str()
            .expect("a sentence")
            .contains("4400"),
        "it names the Range in the way: {rejected}"
    );
}

/// **A disabled key comes back disabled.** The flag is the whole point of
/// carrying a revoked-but-remembered key across: re-issuing it *enabled* would
/// quietly re-open a door the Operator had shut.
#[tokio::test]
async fn a_disabled_key_is_re_issued_still_disabled() {
    let app = curating_app().await;

    let report = import(
        &app,
        &json!({
            "version": 1,
            "systems": [],
            "apiKeys": [{"label": "the old laptop", "disabled": true}],
        }),
    )
    .await;

    let issued = report["apiKeys"]
        .as_array()
        .expect("issued keys")
        .iter()
        .find(|key| key["label"] == "the old laptop")
        .expect("the key");
    assert_eq!(issued["disabled"], true, "{issued}");

    // And it really is refused, which is the only proof that matters.
    let secret = issued["key"].as_str().expect("the one sight of it");
    let (status, _) = app
        .upload(CallUpload::new().key(secret).talkgroup(100))
        .await;
    assert_eq!(status, 401, "a disabled key still authorized ingest");
}

/// **A preview says how many keys it would issue**, which is the only honest
/// thing it can say about a credential it must not create. Without the count a
/// preview of a six-key roster reports nothing at all and the real run then
/// issues six.
#[tokio::test]
async fn a_preview_counts_the_keys_it_would_issue_without_minting_one() {
    let source = a_curated_instance().await;
    let document = export(&source).await;
    let target = curating_app().await;
    let keys_before = target
        .count::<radio_scout::db::entities::api_key::Entity>()
        .await;

    let (_, previewed) = preview(&target, &document).await;

    assert_eq!(previewed["apiKeysToIssue"], 1, "{previewed}");
    assert_eq!(previewed["apiKeys"], json!([]), "a preview mints nothing");
    assert_eq!(
        target
            .count::<radio_scout::db::entities::api_key::Entity>()
            .await,
        keys_before
    );

    let real = import(&target, &document).await;
    assert_eq!(
        real["apiKeysToIssue"], 1,
        "and the real run does what it said"
    );
    assert_eq!(real["apiKeys"].as_array().expect("issued").len(), 1);
}

/// **Names are trimmed, and a blank one is punctuation.** A document is
/// hand-edited far more often than a form is mistyped, and `"Fire "` reaching
/// the database beside `"Fire"` is two identical rows in a panel nobody can tell
/// apart — the failure the forms' own trimming already prevents.
#[tokio::test]
async fn the_names_a_document_carries_are_trimmed_and_never_blank() {
    let app = curating_app().await;
    app.admin_post("/api/admin/groups", json!({"name": "Fire"}))
        .await;

    let report = import(
        &app,
        &json!({
            "version": 1,
            "groups": ["  Fire  ", "   "],
            "systems": [{
                "ref": 11,
                "label": "  Fulton  ",
                "talkgroups": [{"ref": 100, "label": " Fire Dispatch ", "groups": ["Fire ", ""]}],
                "units": [{"ref": 1200, "label": "  Engine 1  "}],
            }],
        }),
    )
    .await;

    assert_eq!(
        report["groupsCreated"], 0,
        "\"  Fire  \" is the Fire we had"
    );
    let (_, groups) = app.admin_get("/api/admin/groups").await;
    assert_eq!(
        groups["results"].as_array().expect("groups").len(),
        1,
        "a blank entry became a Group: {groups}"
    );

    let system = &export(&app).await["systems"][0];
    assert_eq!(system["label"], "Fulton");
    assert_eq!(system["talkgroups"][0]["label"], "Fire Dispatch");
    assert_eq!(system["talkgroups"][0]["groups"], json!(["Fire"]));
    assert_eq!(system["units"][0]["label"], "Engine 1");
}

// ---------------------------------------------------------------------------
// What counts as already right
// ---------------------------------------------------------------------------
//
// `unchanged` is the count a restore is judged by — it is what makes re-running
// a half-finished one safe, and what an Operator reads to mean "nothing moved".
// The comparison behind it is a conjunction over every field, so it is asserted
// **one field at a time**: a test that changes all of them at once passes just
// as well if the comparison lost a clause, or read `||` where it means `&&`.

/// One System, one channel, one radio, every field filled — the document the
/// per-field cases below change exactly one thing in.
fn a_full_document() -> Value {
    json!({
        "version": 1,
        "systems": [{
            "ref": 11,
            "label": "Fulton",
            "autoPopulate": true,
            "blacklist": [9999],
            "enhancement": true,
            "talkgroups": [{
                "ref": 100,
                "label": "Fire Dispatch",
                "name": "Fulton Fire Dispatch",
                "tag": "Fire",
                "groups": ["Dispatch", "Fire"],
                "led": "red",
                "enhancement": false,
            }],
            "units": [{
                "ref": 1200,
                "label": "Engine 1",
                "ranges": [{"from": 1201, "to": 1299}],
            }],
        }],
    })
}

/// `a_full_document()` with one field of its System, Talkgroup or Unit replaced.
fn changed(at: &str, field: &str, value: Value) -> Value {
    let mut document = a_full_document();
    let entry = match at {
        "system" => &mut document["systems"][0],
        "talkgroup" => &mut document["systems"][0]["talkgroups"][0],
        _ => &mut document["systems"][0]["units"][0],
    };
    entry[field] = value;
    document
}

/// **Every field is part of "already right".** One at a time, because a
/// conjunction that lost a clause still answers correctly whenever some *other*
/// field also moved — and the case an Operator actually hits is the one where
/// exactly one thing drifted.
#[rstest::rstest]
#[case::system_label("system", "label", json!("Renamed"), "systems")]
#[case::system_auto_populate("system", "autoPopulate", json!(false), "systems")]
#[case::system_blacklist("system", "blacklist", json!([1234]), "systems")]
#[case::system_enhancement("system", "enhancement", json!(false), "systems")]
#[case::talkgroup_label("talkgroup", "label", json!("Renamed"), "talkgroups")]
#[case::talkgroup_name("talkgroup", "name", json!("Renamed"), "talkgroups")]
#[case::talkgroup_tag("talkgroup", "tag", json!("Law"), "talkgroups")]
#[case::talkgroup_groups("talkgroup", "groups", json!(["Fire"]), "talkgroups")]
#[case::talkgroup_led("talkgroup", "led", json!("blue"), "talkgroups")]
#[case::talkgroup_enhancement("talkgroup", "enhancement", json!(true), "talkgroups")]
#[case::unit_label("unit", "label", json!("Renamed"), "units")]
#[case::unit_ranges("unit", "ranges", json!([{"from": 1201, "to": 1250}]), "units")]
#[tokio::test]
async fn changing_any_one_field_makes_its_row_updated(
    #[case] at: &str,
    #[case] field: &str,
    #[case] value: Value,
    #[case] kind: &str,
) {
    let app = curating_app().await;
    import(&app, &a_full_document()).await;

    let report = import(&app, &changed(at, field, value)).await;

    assert_eq!(
        report[kind]["updated"], 1,
        "changing {at}.{field} did not count as a change: {report}"
    );
    assert_eq!(report[kind]["unchanged"], 0, "{report}");
}

/// ...and a document nothing moved in reports every row as already right, which
/// is the other half of the same claim: a comparison that answered `false` too
/// eagerly would pass every case above while making a restore look like a
/// rewrite.
#[tokio::test]
async fn a_document_that_changed_nothing_leaves_every_row_unchanged() {
    let app = curating_app().await;
    import(&app, &a_full_document()).await;

    let report = import(&app, &a_full_document()).await;

    for kind in ["systems", "talkgroups", "units"] {
        assert_eq!(report[kind]["unchanged"], 1, "{kind}: {report}");
        assert_eq!(report[kind]["created"], 0, "{kind}: {report}");
        assert_eq!(report[kind]["updated"], 0, "{kind}: {report}");
    }
}

/// Every field survives a create, too — the arm the per-field cases above never
/// reach, since they import over rows that already exist.
#[tokio::test]
async fn a_document_creates_rows_carrying_every_field_it_named() {
    let app = curating_app().await;

    import(&app, &a_full_document()).await;

    assert_eq!(export(&app).await["systems"], a_full_document()["systems"]);
}

/// **A Group nobody is in still travels.** An Operator who made "Rescue" before
/// assigning it meant to keep it, and a restore that dropped every empty
/// category would quietly undo an afternoon of setting them up — the same
/// argument the Talkgroup listing makes for showing a Group with no channels.
#[tokio::test]
async fn a_group_and_a_tag_nothing_uses_still_travel() {
    let app = curating_app().await;

    let report = import(
        &app,
        &json!({
            "version": 1,
            "groups": ["Rescue"],
            "tags": ["Aircraft"],
            "systems": [],
        }),
    )
    .await;

    assert_eq!(report["groupsCreated"], 1, "{report}");
    assert_eq!(report["tagsCreated"], 1, "{report}");

    let document = export(&app).await;
    assert_eq!(document["groups"], json!(["Rescue"]));
    assert_eq!(document["tags"], json!(["Aircraft"]));

    // ...and a second import creates neither again, which is what makes a
    // retried restore a no-op for them too.
    let again = import(&app, &document).await;
    assert_eq!(again["groupsCreated"], 0, "{again}");
    assert_eq!(again["tagsCreated"], 0, "{again}");
}
