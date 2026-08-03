//! The selection catalog (#12, spec US 19–21): `GET /api/catalog`, the Systems
//! and Talkgroups the Talkgroups panel offers.
//!
//! Driven over the real HTTP boundary via the integration harness (ADR-0009).

mod common;
use common::logs::LogCapture;
use common::{CallUpload, TestApp, request_id_of};

use radio_scout::db::repo::NewCall;
use serde_json::Value;

#[tokio::test]
async fn a_fresh_instance_offers_an_empty_catalog() {
    let app = TestApp::spawn().await;

    assert_eq!(
        app.get_json("/api/catalog").await,
        serde_json::json!({ "systems": [] }),
        "zero-config first run has nothing to select yet"
    );
}

/// Seed a Talkgroup the way a curated archive has one.
async fn seed_talkgroup(
    app: &TestApp,
    system_ref: i64,
    system_label: &str,
    talkgroup_ref: i64,
    label: &str,
    tag: &str,
    groups: &[&str],
) {
    app.seed_call(
        NewCall {
            system_label: Some(system_label.into()),
            talkgroup_label: Some(label.into()),
            talkgroup_tag: Some(tag.into()),
            talkgroup_groups: groups.iter().map(|g| (*g).to_string()).collect(),
            ..NewCall::new(system_ref, talkgroup_ref, 0)
        },
        common::audio_at(format!("k/{system_ref}-{talkgroup_ref}.wav")),
    )
    .await;
}

/// The catalog as the panel reads it.
async fn catalog(app: &TestApp) -> Value {
    app.get_json("/api/catalog").await
}

#[tokio::test]
async fn a_talkgroup_carries_what_the_panel_groups_and_labels_it_by() {
    let app = TestApp::spawn().await;
    seed_talkgroup(&app, 100, "Alpha", 1, "Alpha Fire", "Fire", &["Emergency"]).await;

    assert_eq!(
        catalog(&app).await,
        serde_json::json!({
            "systems": [{
                "ref": 100,
                "label": "Alpha",
                "talkgroups": [{
                    "ref": 1,
                    "label": "Alpha Fire",
                    "name": "Talkgroup 1",
                    "tag": "Fire",
                    "groups": ["Emergency"],
                }],
            }],
        })
    );
}

#[tokio::test]
async fn a_talkgroup_in_several_groups_lists_them_all() {
    let app = TestApp::spawn().await;
    seed_talkgroup(
        &app,
        100,
        "Alpha",
        2,
        "Alpha Law",
        "Law",
        &["Public", "Emergency"],
    )
    .await;

    let catalog = catalog(&app).await;
    assert_eq!(
        catalog["systems"][0]["talkgroups"][0]["groups"],
        serde_json::json!(["Emergency", "Public"]),
        "sorted, so a Group category row is stable between reloads"
    );
}

/// The panel is a list a listener scans by eye, so it is ordered by what they
/// read — the label — with the Ref breaking ties (and standing in for a System
/// or Talkgroup that has no label at all).
#[tokio::test]
async fn systems_and_talkgroups_are_ordered_by_label() {
    let app = TestApp::spawn().await;
    seed_talkgroup(&app, 200, "Beta", 7, "Zulu", "Ops", &[]).await;
    seed_talkgroup(&app, 200, "Beta", 3, "Alpha Bravo", "Ops", &[]).await;
    seed_talkgroup(&app, 100, "Countywide", 1, "Fire Dispatch", "Fire", &[]).await;

    let catalog = catalog(&app).await;
    let systems = catalog["systems"].as_array().expect("systems");
    assert_eq!(
        systems.iter().map(|s| &s["label"]).collect::<Vec<_>>(),
        vec!["Beta", "Countywide"],
        "Beta before Countywide, though its Ref is higher"
    );
    assert_eq!(
        systems[0]["talkgroups"]
            .as_array()
            .expect("talkgroups")
            .iter()
            .map(|t| &t["label"])
            .collect::<Vec<_>>(),
        vec!["Alpha Bravo", "Zulu"],
    );
}

/// Auto-populate (#8) is the whole point: a recorder pointed at a fresh instance
/// fills the panel without anyone configuring anything.
#[tokio::test]
async fn an_ingested_call_puts_its_talkgroup_in_the_catalog() {
    let app = TestApp::with_key("k").await;

    app.upload_ok(CallUpload::new()).await;

    let catalog = catalog(&app).await;
    let talkgroup = &catalog["systems"][0]["talkgroups"][0];
    assert_eq!(catalog["systems"][0]["ref"], 11);
    assert_eq!(talkgroup["ref"], 54241);
    assert_eq!(
        talkgroup["tag"], "Untagged",
        "auto-populate's default Tag, so the Tag row is never empty"
    );
    assert_eq!(talkgroup["groups"], serde_json::json!(["Unknown"]));
}

/// A Talkgroup whose Calls have all aged out (#10) is still selectable — the
/// listener's selection is about what *will* arrive, not what is in the archive.
/// rdio's equivalent surface is its whole config, so it has this property too;
/// deriving the panel from `GET /api/calls/filters` would have lost it.
#[tokio::test]
async fn a_talkgroup_outlives_its_calls() {
    let app = TestApp::spawn().await;
    seed_talkgroup(&app, 100, "Alpha", 1, "Alpha Fire", "Fire", &["Emergency"]).await;
    let call = app.the_call().await;

    radio_scout::db::repo::delete_calls(&app.db, &[call.id])
        .await
        .expect("prune the call");

    assert_eq!(
        catalog(&app).await["systems"][0]["talkgroups"][0]["label"],
        "Alpha Fire"
    );
}

/// A dead database is a 500 with a correlation ref, never an empty catalog —
/// which the panel would draw as "no systems yet" and a listener would read as
/// their archive having vanished (ADR-0011 rule 4).
#[tokio::test]
async fn a_broken_database_is_a_server_error_not_an_empty_catalog() {
    let capture = LogCapture::start();
    let app = TestApp::spawn().await;
    app.db.clone().close().await.expect("close the pool");

    let resp = app.get("/api/catalog").await;

    assert_eq!(resp.status(), 500);
    let request_id = request_id_of(&resp);
    assert_eq!(
        resp.text().await.expect("body"),
        format!("internal error (request id: {request_id})\n"),
        "the client is told the ref and nothing else"
    );

    let line = capture.only_line_containing("stage=load-catalog");
    assert!(line.contains(" ERROR "), "{line}");
    assert!(line.contains(&format!("request_id={request_id}")), "{line}");
    assert!(
        line.contains("cause="),
        "the operator is told what failed: {line}"
    );
}
