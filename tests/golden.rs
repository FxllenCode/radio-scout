//! Recorder-compatibility golden suite (ticket #7).
//!
//! Each test replays a byte-accurate captured recorder payload
//! (`tests/fixtures/*.multipart`, provenance in `tests/fixtures/README.md`) over
//! the real HTTP boundary and asserts BOTH the exact rdio-compatible response
//! string + status code AND the parsed rows in the database. Together they guard
//! the drop-in-replacement promise against regression (spec "Testing Decisions",
//! ADR-0009): a change that silently breaks Trunk Recorder or SDRTrunk
//! compatibility fails here.
//!
//! Posting the raw fixture bytes (rather than a reqwest-built form) is the point:
//! it exercises our multipart reader against each recorder's real field order,
//! boundary style, and part-header quirks — e.g. SDRTrunk's four-dash body
//! delimiter and filename-before-name audio part, Trunk Recorder's hard-coded
//! `application/octet-stream` audio part.

use std::path::Path;

use radio_scout::db::entities::{call_frequency, call_patch, call_unit, unit};
use radio_scout::db::repo;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

mod common;
use common::{CallUpload, TestApp};

/// Read a captured multipart body from `tests/fixtures/`.
fn fixture(name: &str) -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    std::fs::read(&path).unwrap_or_else(|e| panic!("read fixture {}: {e}", path.display()))
}

/// POST a raw captured body to `path` with the recorder's exact `Content-Type`
/// (boundary) and `User-Agent`, mimicking a real recorder on the wire.
///
/// The harness's `post_bytes` would do all of this but the `User-Agent`, and the
/// header is part of what a recorder puts on the wire — so the golden suite
/// spells the request out itself.
async fn post_raw(
    app: &TestApp,
    path: &str,
    content_type: &str,
    user_agent: &str,
    body: Vec<u8>,
) -> (u16, String) {
    let resp = app
        .client()
        .post(app.url(path))
        .header(reqwest::header::CONTENT_TYPE, content_type)
        .header(reqwest::header::USER_AGENT, user_agent)
        .body(body)
        .send()
        .await
        .expect("send raw upload");
    (
        resp.status().as_u16(),
        resp.text().await.unwrap_or_default(),
    )
}

async fn units_for(app: &TestApp, call_id: i64) -> Vec<call_unit::Model> {
    call_unit::Entity::find()
        .filter(call_unit::Column::CallId.eq(call_id))
        .all(&app.db)
        .await
        .unwrap()
}

// ---- Trunk Recorder (rdioscanner_uploader) -> /api/call-upload --------------

#[tokio::test]
async fn golden_trunk_recorder_plugin_upload() {
    // The plugin's key is per-system; scope it to the system id it sends (8).
    let app = TestApp::spawn().await;
    app.create_api_key_for_system("tr-plugin-key", 8).await;

    let (status, body) = post_raw(
        &app,
        "/api/call-upload",
        "multipart/form-data; boundary=------------------------d1e2f3a4b5c6d7e8f9a0b1c2",
        "TrunkRecorder1.0",
        fixture("trunk-recorder-call-upload.multipart"),
    )
    .await;

    // Exact rdio-compatible response.
    assert_eq!(status, 200, "{body:?}");
    assert_eq!(body, "Call imported successfully.\n");

    // Parse correctness.
    let call = app.the_call().await;
    assert_eq!(call.call_at_ms, 1669740338000, "dateTime seconds -> ms");
    assert_eq!(call.frequency, Some(774031250), "scalar call frequency");

    let sys = app.system_of(&call).await;
    assert_eq!(sys.r#ref, 8);
    assert_eq!(sys.label.as_deref(), Some("butco"));

    // rdio mapping: talkgroupLabel->label, talkgroupName->name, talkgroupTag->tag.
    let tg = app.talkgroup_of(&call).await;
    assert_eq!(tg.r#ref, 54241);
    assert_eq!(tg.label.as_deref(), Some("TDB A1"));
    assert_eq!(tg.name.as_deref(), Some("Fire Department Dispatch A1"));
    let tg_tag = app.tag_of(&tg).await;
    assert_eq!(tg_tag.name, "Fire Dispatch");
    assert_eq!(
        repo::groups_for_talkgroup(&app.db, tg.id).await.unwrap(),
        vec!["Fire".to_string()]
    );

    // Detailed `frequencies` array -> one call_frequency row (kept distinct from
    // the scalar `frequency`; rdio clobbers one with the other, we keep both).
    let freqs = call_frequency::Entity::find().all(&app.db).await.unwrap();
    assert_eq!(freqs.len(), 1);
    assert_eq!(freqs[0].freq, 774031250);
    assert_eq!(freqs[0].len_ms, Some(5760));
    assert_eq!(freqs[0].error_count, Some(2));
    assert_eq!(freqs[0].spike_count, Some(0));

    // `sources` array -> two units; the second carries its tag as the label.
    let units = units_for(&app, call.id).await;
    assert_eq!(units.len(), 2);
    let tagged = units.iter().find(|u| u.unit_ref == 1610051).unwrap();
    assert_eq!(tagged.label.as_deref(), Some("Engine 5"));
    let untagged = units.iter().find(|u| u.unit_ref == 1610092).unwrap();
    assert_eq!(untagged.label, None);

    // Auto-populate (#8): a heard radio joins the Unit roster only when it carries
    // an alias (rdio parity) — so "Engine 5" is rostered, but the anonymous
    // 1610092 is not, even though both appear in the per-call `call_units` above.
    let roster = unit::Entity::find()
        .filter(unit::Column::SystemId.eq(call.system_id))
        .all(&app.db)
        .await
        .unwrap();
    assert_eq!(roster.len(), 1, "only the aliased source is rostered");
    assert_eq!(roster[0].r#ref, 1610051);
    assert_eq!(roster[0].label.as_deref(), Some("Engine 5"));

    // Empty `patches` array -> no patch rows.
    assert_eq!(app.count::<call_patch::Entity>().await, 0);

    // Full round-trip: the stored WAV serves back byte-for-byte.
    let audio = app.get(&format!("/api/call/{}/audio", call.id)).await;
    assert_eq!(audio.status(), 200);
    let served = audio.bytes().await.unwrap();
    assert!(served.starts_with(b"RIFF"), "served a WAV container");
    assert_eq!(served.len(), 60, "the fixture's 60-byte WAV");
}

// ---- SDRTrunk -> /api/call-upload ------------------------------------------

#[tokio::test]
async fn golden_sdrtrunk_upload() {
    let app = TestApp::spawn().await;
    app.create_api_key_for_system("sdrtrunk-key", 11).await;

    let (status, body) = post_raw(
        &app,
        "/api/call-upload",
        "multipart/form-data; boundary=--sdrtrunk-sdrtrunk-sdrtrunk",
        "sdrtrunk",
        fixture("sdrtrunk-call-upload.multipart"),
    )
    .await;

    assert_eq!(status, 200, "{body:?}");
    assert_eq!(body, "Call imported successfully.\n");

    let call = app.the_call().await;
    assert_eq!(call.call_at_ms, 1763216122000, "dateTime seconds -> ms");
    assert_eq!(call.frequency, Some(851000000));
    // SDRTrunk's singular `source` is the call's primary source unit.
    assert_eq!(call.source_ref, Some(1610092));

    let sys = app.system_of(&call).await;
    assert_eq!(sys.r#ref, 11);
    assert_eq!(sys.label.as_deref(), Some("metropd"));

    let tg = app.talkgroup_of(&call).await;
    assert_eq!(tg.r#ref, 54241);
    assert_eq!(tg.label.as_deref(), Some("PD Disp"));
    // SDRTrunk sends no talkgroupName / talkgroupTag; auto-populate (#8) fills
    // rdio-scanner's defaults on create rather than leaving them NULL.
    assert_eq!(tg.name.as_deref(), Some("Talkgroup 54241"), "default name");
    let tg_tag = app.tag_of(&tg).await;
    assert_eq!(tg_tag.name, "Untagged", "default Untagged tag");
    assert_eq!(
        repo::groups_for_talkgroup(&app.db, tg.id).await.unwrap(),
        vec!["Law Dispatch".to_string()]
    );

    // `talkerAlias` is not modelled (rdio-scanner ignores it too) — it must be
    // silently dropped, never rejected.
    assert_eq!(app.count::<call_patch::Entity>().await, 0);
}

/// SDRTrunk broadcasting a Call on a **patch group** (#81).
///
/// `getPatches()` (`RdioScannerBroadcaster.java:546-574`) concatenates the patch
/// group, then every patched Talkgroup, then every patched **radio**, into one
/// flat array with no separator and no type marker — and sends it under the same
/// `patches` field name Trunk Recorder's uploader uses. Nothing on the wire says
/// where the talkgroups stop, so the split is recovered the way rdio-scanner
/// recovers it (`call.go:572-582`): a ref is a patch member only if the System
/// has a Talkgroup for it. The radios trailing behind are dropped — never stored
/// as Talkgroup Refs, and never fanned out to a listener who selected that
/// number.
#[tokio::test]
async fn golden_sdrtrunk_patched_upload_drops_the_trailing_radio_ids() {
    let app = TestApp::spawn().await;
    app.create_api_key_for_system("sdrtrunk-key", 11).await;
    // What the System knows: 54241 and 54242 are channels it has heard. 54000 is
    // the patch group itself — this Call's own Talkgroup, which auto-populate
    // creates on the way past. 1610051 and 1610092 are radios and stay unknown.
    // The System is created here rather than by the upload, so it is created
    // with the `systemLabel` the fixture sends.
    repo::resolve_or_create_system(&app.db, 11, Some("metropd".into()), 0)
        .await
        .expect("seed the fixture's system");
    app.seed_talkgroup(11, 54241).await;
    app.seed_talkgroup(11, 54242).await;

    let (status, body) = post_raw(
        &app,
        "/api/call-upload",
        "multipart/form-data; boundary=--sdrtrunk-sdrtrunk-sdrtrunk",
        "sdrtrunk",
        fixture("sdrtrunk-call-upload-patched.multipart"),
    )
    .await;

    // The wire contract is untouched by the classification.
    assert_eq!(status, 200, "{body:?}");
    assert_eq!(body, "Call imported successfully.\n");

    let call = app.the_call().await;
    assert_eq!(
        app.talkgroup_of(&call).await.r#ref,
        54000,
        "the Call rides on the patch group"
    );
    assert_eq!(
        app.system_of(&call).await.label.as_deref(),
        Some("metropd"),
        "seeding the System did not displace the fixture's systemLabel"
    );
    assert_eq!(
        app.patch_refs(call.id).await,
        vec![54000, 54241, 54242],
        "the patch group and its Talkgroups; 1610051 and 1610092 are radios"
    );
}

// ---- Trunk Recorder native (.wav + .json meta) -> trunk-recorder-call-upload

#[tokio::test]
async fn golden_trunk_recorder_native_meta_upload() {
    // Native TR has no numeric system id (short_name only) -> global key.
    let app = TestApp::with_key("tr-native-key").await;
    // The meta's `patched_talkgroups` are `[54155, 54156]`, and a patch ref is
    // stored only for a Talkgroup its System has (#81). 54155 is this Call's own
    // Talkgroup, which auto-populate creates on the way past — so only 54156 has
    // to be seeded. Native TR has no numeric System Ref, so the upload finds its
    // System by matching `short_name` against the label; seed that System first.
    repo::resolve_or_create_system(&app.db, 1, Some("butco".into()), 0)
        .await
        .expect("seed the short_name's system");
    app.seed_talkgroup(1, 54156).await;

    let (status, body) = post_raw(
        &app,
        "/api/trunk-recorder-call-upload",
        "multipart/form-data; boundary=------------------------0f1e2d3c4b5a69788796a5b4",
        "TrunkRecorder1.0",
        fixture("trunk-recorder-native-meta.multipart"),
    )
    .await;

    assert_eq!(status, 200, "{body:?}");
    assert_eq!(body, "Call imported successfully.\n");

    let call = app.the_call().await;
    // The guarded bug: start_time is used, NOT now() (rdio's `// DBEUG` line).
    assert_eq!(call.call_at_ms, 1669740338000, "start_time used, not now()");

    let sys = app.system_of(&call).await;
    assert_eq!(
        sys.label.as_deref(),
        Some("butco"),
        "system from short_name"
    );

    let tg = app.talkgroup_of(&call).await;
    assert_eq!(tg.r#ref, 54155);
    assert_eq!(
        tg.label.as_deref(),
        Some("EMS DISP"),
        "talkgroup_tag->label"
    );
    assert_eq!(
        tg.name.as_deref(),
        Some("EMS Dispatch"),
        "talkgroup_description->name"
    );
    // talkgroup_group_tag is the "-" placeholder -> dropped (parsers.go), then
    // auto-populate (#8) fills rdio's default `Untagged` tag on create.
    let tg_tag = app.tag_of(&tg).await;
    assert_eq!(tg_tag.name, "Untagged", "\"-\" group_tag -> default tag");
    assert_eq!(
        repo::groups_for_talkgroup(&app.db, tg.id).await.unwrap(),
        vec!["EMS".to_string()]
    );

    let freqs = call_frequency::Entity::find().all(&app.db).await.unwrap();
    assert_eq!(freqs.len(), 1);
    assert_eq!(freqs[0].freq, 771093750);
    assert_eq!(freqs[0].error_count, Some(3));
    assert_eq!(freqs[0].spike_count, Some(1));

    let units = units_for(&app, call.id).await;
    assert_eq!(units.len(), 1);
    assert_eq!(units[0].unit_ref, 1610092);

    assert_eq!(app.count::<call_patch::Entity>().await, 2);
}

// ---- Parity: rdio drops empty / "-" placeholder talkgroup fields -----------

/// rdio-scanner reads `talkgroupLabel/Name/Tag/Group` only when
/// `len > 0 && != "-"` (`parsers.go`), then auto-populate fills the blanks with
/// its defaults (`controller.go`). Recorders (Trunk Recorder always, SDRTrunk
/// often) send those parts even for an unknown talkgroup — as empty strings or a
/// `"-"` placeholder. We must never persist `""`/`"-"` as a bogus label/tag/group:
/// they are dropped, then #8 auto-populate substitutes rdio's defaults on create.
#[tokio::test]
async fn golden_generic_upload_drops_empty_and_dash_talkgroup_fields() {
    let app = TestApp::with_key("k").await;

    app.upload_ok(
        CallUpload::new()
            .system(8)
            .audio(b"RIFFxxxx")
            .set("talkgroupLabel", "") // empty
            .set("talkgroupName", "-") // placeholder
            .set("talkgroupTag", "-") // placeholder
            .set("talkgroupGroup", "-"), // placeholder
    )
    .await;

    let call = app.the_call().await;
    // Every placeholder field was dropped, then auto-populated with rdio's default
    // — never persisted as the literal "" / "-".
    let tg = app.talkgroup_of(&call).await;
    assert_eq!(tg.label.as_deref(), Some("54241"), "empty label -> Ref");
    assert_eq!(
        tg.name.as_deref(),
        Some("Talkgroup 54241"),
        "\"-\" name -> default"
    );
    let tg_tag = app.tag_of(&tg).await;
    assert_eq!(tg_tag.name, "Untagged", "\"-\" tag -> Untagged");
    assert_eq!(
        repo::groups_for_talkgroup(&app.db, tg.id).await.unwrap(),
        vec!["Unknown".to_string()],
        "\"-\" group -> Unknown"
    );
}
