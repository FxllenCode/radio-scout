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
        serde_json::json!({
            "systems": [],
            "activityWindowMs": 24 * 60 * 60 * 1000_i64,
            "sharing": true,
        }),
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
            "activityWindowMs": 24 * 60 * 60 * 1000_i64,
            "sharing": true,
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

// ---------------------------------------------------------------------------
// Activity: how busy each Talkgroup has been (#57, spec US 29).
// ---------------------------------------------------------------------------

/// Seed a Call on a Talkgroup at a given instant, so a test can put traffic
/// inside the activity window and outside it.
async fn seed_call_at(app: &TestApp, system_ref: i64, talkgroup_ref: i64, at_ms: i64) {
    app.seed_call(
        NewCall::new(system_ref, talkgroup_ref, at_ms),
        common::audio_at(format!("k/{system_ref}-{talkgroup_ref}-{at_ms}.wav")),
    )
    .await;
}

/// The panel row needs a reason to be sorted "most active" and a last-heard age
/// to show, and only the server can count what it has. Bounded to a window
/// rather than the whole Archive: an unbounded `MAX`/`COUNT` is a scan of every
/// Call ever stored, on every app open, on a Pi.
#[tokio::test]
async fn a_talkgroup_carries_how_busy_it_has_been() {
    let app = TestApp::spawn().await;
    let now = radio_scout::now_ms();
    seed_talkgroup(&app, 100, "Alpha", 1, "Alpha Fire", "Fire", &[]).await;
    seed_call_at(&app, 100, 1, now - 60_000).await;
    seed_call_at(&app, 100, 1, now - 3_600_000).await;
    // **Yesterday, and therefore outside the window** — the one Call that
    // proves the cutoff is `now - ACTIVITY_WINDOW_MS` rather than any other
    // arithmetic over the same two numbers. The seeding Call at epoch 0 cannot:
    // it is outside every cutoff a mutation could produce.
    seed_call_at(&app, 100, 1, now - 2 * 24 * 60 * 60 * 1_000).await;

    let catalog = catalog(&app).await;
    let talkgroup = &catalog["systems"][0]["talkgroups"][0];
    assert_eq!(
        talkgroup["recentCalls"], 2,
        "the two inside the window; the two-day-old Call and the epoch-0 seeding Call are outside it"
    );
    assert_eq!(
        talkgroup["lastCallAtMs"],
        serde_json::json!(now - 60_000),
        "the newest inside the window — what a last-heard age is a subtraction from"
    );
}

/// A Talkgroup nothing has been heard on lately says nothing rather than
/// claiming zero, so a quiet row is drawn as quiet and the JSON a fresh
/// instance serves is unchanged.
#[tokio::test]
async fn a_quiet_talkgroup_carries_no_activity_at_all() {
    let app = TestApp::spawn().await;
    seed_talkgroup(&app, 100, "Alpha", 1, "Alpha Fire", "Fire", &[]).await;

    let talkgroup = &catalog(&app).await["systems"][0]["talkgroups"][0];
    assert_eq!(talkgroup.get("recentCalls"), None);
    assert_eq!(talkgroup.get("lastCallAtMs"), None);
}

/// How long "recently" is, on the wire (#57).
///
/// The panel says "12 calls in the last 24 hours" out loud, and the only way
/// that sentence stays true when the window moves is for the window to be the
/// server's answer rather than a second constant in the client.
#[tokio::test]
async fn the_catalog_says_how_long_recently_is() {
    let app = TestApp::spawn().await;

    assert_eq!(
        catalog(&app).await["activityWindowMs"],
        serde_json::json!(24 * 60 * 60 * 1000_i64),
    );
}

/// Activity is one grouped query, not one per Talkgroup (#86's rule): a county
/// panel is 400+ rows, and an N+1 here would be invisible from outside because
/// the answer is correct.
#[tokio::test]
async fn activity_costs_the_same_however_many_talkgroups_there_are() {
    let app = TestApp::spawn().await;
    let now = radio_scout::now_ms();
    for r#ref in 1..=3 {
        seed_talkgroup(&app, 100, "Alpha", r#ref, "Small", "Fire", &[]).await;
        seed_call_at(&app, 100, r#ref, now - 60_000).await;
    }

    let before = app.statements_issued();
    catalog(&app).await;
    let small = app.statements_issued() - before;

    for r#ref in 4..=30 {
        seed_talkgroup(&app, 100, "Alpha", r#ref, "Big", "Fire", &[]).await;
        seed_call_at(&app, 100, r#ref, now - 60_000).await;
    }

    let before = app.statements_issued();
    catalog(&app).await;
    assert_eq!(
        app.statements_issued() - before,
        small,
        "ten times the Talkgroups, the same number of round trips"
    );
}

/// **What this Instance offers, asked once on app open** (#64, spec US 32).
///
/// The share control is drawn from this: a control that is offered and then
/// refused is a control that lies, and an Operator who closed sharing closed it
/// for a reason. On by default, because an instance as it ships already serves
/// its whole Archive to anyone who asks.
#[tokio::test]
async fn the_catalog_says_whether_this_instance_shares() {
    let mut app = TestApp::spawn().await;
    assert_eq!(catalog(&app).await["sharing"], true);

    app.restart_with(|config| config.share.enabled = false)
        .await;

    assert_eq!(catalog(&app).await["sharing"], false);
}
