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

use radio_scout::db::entities::{
    call, call_frequency, call_patch, call_unit, system, tag, talkgroup,
};
use radio_scout::db::repo;
use sea_orm::{ColumnTrait, DatabaseConnection, EntityTrait, PaginatorTrait, QueryFilter};

mod common;
use common::spawn;

/// Read a captured multipart body from `tests/fixtures/`.
fn fixture(name: &str) -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    std::fs::read(&path).unwrap_or_else(|e| panic!("read fixture {}: {e}", path.display()))
}

/// POST a raw captured body to `path` with the recorder's exact `Content-Type`
/// (boundary) and `User-Agent`, mimicking a real recorder on the wire.
async fn post_raw(
    addr: &str,
    path: &str,
    content_type: &str,
    user_agent: &str,
    body: Vec<u8>,
) -> (u16, String) {
    let resp = reqwest::Client::new()
        .post(format!("http://{addr}{path}"))
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

/// The one call in the DB (each golden test ingests exactly one).
async fn the_call(db: &DatabaseConnection) -> call::Model {
    call::Entity::find()
        .one(db)
        .await
        .unwrap()
        .expect("exactly one call ingested")
}

async fn the_talkgroup(db: &DatabaseConnection, id: i64) -> talkgroup::Model {
    talkgroup::Entity::find_by_id(id)
        .one(db)
        .await
        .unwrap()
        .unwrap()
}

async fn units_for(db: &DatabaseConnection, call_id: i64) -> Vec<call_unit::Model> {
    call_unit::Entity::find()
        .filter(call_unit::Column::CallId.eq(call_id))
        .all(db)
        .await
        .unwrap()
}

// ---- Trunk Recorder (rdioscanner_uploader) -> /api/call-upload --------------

#[tokio::test]
async fn golden_trunk_recorder_plugin_upload() {
    let (addr, db, _tmp) = spawn().await;
    // The plugin's key is per-system; scope it to the system id it sends (8).
    repo::create_api_key(&db, "tr-plugin-key", Some(8), None, 0)
        .await
        .unwrap();

    let (status, body) = post_raw(
        &addr,
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
    let call = the_call(&db).await;
    assert_eq!(call.call_at_ms, 1669740338000, "dateTime seconds -> ms");
    assert_eq!(call.frequency, Some(774031250), "scalar call frequency");

    let sys = system::Entity::find_by_id(call.system_id)
        .one(&db)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(sys.r#ref, 8);
    assert_eq!(sys.label.as_deref(), Some("butco"));

    // rdio mapping: talkgroupLabel->label, talkgroupName->name, talkgroupTag->tag.
    let tg = the_talkgroup(&db, call.talkgroup_id).await;
    assert_eq!(tg.r#ref, 54241);
    assert_eq!(tg.label.as_deref(), Some("TDB A1"));
    assert_eq!(tg.name.as_deref(), Some("Fire Department Dispatch A1"));
    let tg_tag = tag::Entity::find_by_id(tg.tag_id.expect("tag"))
        .one(&db)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(tg_tag.name, "Fire Dispatch");
    assert_eq!(
        repo::groups_for_talkgroup(&db, tg.id).await.unwrap(),
        vec!["Fire".to_string()]
    );

    // Detailed `frequencies` array -> one call_frequency row (kept distinct from
    // the scalar `frequency`; rdio clobbers one with the other, we keep both).
    let freqs = call_frequency::Entity::find().all(&db).await.unwrap();
    assert_eq!(freqs.len(), 1);
    assert_eq!(freqs[0].freq, 774031250);
    assert_eq!(freqs[0].len_ms, Some(5760));
    assert_eq!(freqs[0].error_count, Some(2));
    assert_eq!(freqs[0].spike_count, Some(0));

    // `sources` array -> two units; the second carries its tag as the label.
    let units = units_for(&db, call.id).await;
    assert_eq!(units.len(), 2);
    let tagged = units.iter().find(|u| u.unit_ref == 1610051).unwrap();
    assert_eq!(tagged.label.as_deref(), Some("Engine 5"));
    let untagged = units.iter().find(|u| u.unit_ref == 1610092).unwrap();
    assert_eq!(untagged.label, None);

    // Empty `patches` array -> no patch rows.
    assert_eq!(call_patch::Entity::find().count(&db).await.unwrap(), 0);

    // Full round-trip: the stored WAV serves back byte-for-byte.
    let audio = reqwest::get(format!("http://{addr}/api/call/{}/audio", call.id))
        .await
        .unwrap();
    assert_eq!(audio.status(), 200);
    let served = audio.bytes().await.unwrap();
    assert!(served.starts_with(b"RIFF"), "served a WAV container");
    assert_eq!(served.len(), 60, "the fixture's 60-byte WAV");
}

// ---- SDRTrunk -> /api/call-upload ------------------------------------------

#[tokio::test]
async fn golden_sdrtrunk_upload() {
    let (addr, db, _tmp) = spawn().await;
    repo::create_api_key(&db, "sdrtrunk-key", Some(11), None, 0)
        .await
        .unwrap();

    let (status, body) = post_raw(
        &addr,
        "/api/call-upload",
        "multipart/form-data; boundary=--sdrtrunk-sdrtrunk-sdrtrunk",
        "sdrtrunk",
        fixture("sdrtrunk-call-upload.multipart"),
    )
    .await;

    assert_eq!(status, 200, "{body:?}");
    assert_eq!(body, "Call imported successfully.\n");

    let call = the_call(&db).await;
    assert_eq!(call.call_at_ms, 1763216122000, "dateTime seconds -> ms");
    assert_eq!(call.frequency, Some(851000000));
    // SDRTrunk's singular `source` is the call's primary source unit.
    assert_eq!(call.source_ref, Some(1610092));

    let sys = system::Entity::find_by_id(call.system_id)
        .one(&db)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(sys.r#ref, 11);
    assert_eq!(sys.label.as_deref(), Some("metropd"));

    let tg = the_talkgroup(&db, call.talkgroup_id).await;
    assert_eq!(tg.r#ref, 54241);
    assert_eq!(tg.label.as_deref(), Some("PD Disp"));
    assert_eq!(tg.name, None, "SDRTrunk sends no talkgroupName");
    assert_eq!(tg.tag_id, None, "SDRTrunk sends no talkgroupTag");
    assert_eq!(
        repo::groups_for_talkgroup(&db, tg.id).await.unwrap(),
        vec!["Law Dispatch".to_string()]
    );

    // `talkerAlias` is not modelled (rdio-scanner ignores it too) — it must be
    // silently dropped, never rejected.
    assert_eq!(call_patch::Entity::find().count(&db).await.unwrap(), 0);
}

// ---- Trunk Recorder native (.wav + .json meta) -> trunk-recorder-call-upload

#[tokio::test]
async fn golden_trunk_recorder_native_meta_upload() {
    let (addr, db, _tmp) = spawn().await;
    // Native TR has no numeric system id (short_name only) -> global key.
    repo::create_api_key(&db, "tr-native-key", None, None, 0)
        .await
        .unwrap();

    let (status, body) = post_raw(
        &addr,
        "/api/trunk-recorder-call-upload",
        "multipart/form-data; boundary=------------------------0f1e2d3c4b5a69788796a5b4",
        "TrunkRecorder1.0",
        fixture("trunk-recorder-native-meta.multipart"),
    )
    .await;

    assert_eq!(status, 200, "{body:?}");
    assert_eq!(body, "Call imported successfully.\n");

    let call = the_call(&db).await;
    // The guarded bug: start_time is used, NOT now() (rdio's `// DBEUG` line).
    assert_eq!(call.call_at_ms, 1669740338000, "start_time used, not now()");

    let sys = system::Entity::find_by_id(call.system_id)
        .one(&db)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        sys.label.as_deref(),
        Some("butco"),
        "system from short_name"
    );

    let tg = the_talkgroup(&db, call.talkgroup_id).await;
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
    // talkgroup_group_tag is the "-" placeholder -> dropped, no tag.
    assert_eq!(tg.tag_id, None, "\"-\" group_tag dropped");
    assert_eq!(
        repo::groups_for_talkgroup(&db, tg.id).await.unwrap(),
        vec!["EMS".to_string()]
    );

    let freqs = call_frequency::Entity::find().all(&db).await.unwrap();
    assert_eq!(freqs.len(), 1);
    assert_eq!(freqs[0].freq, 771093750);
    assert_eq!(freqs[0].error_count, Some(3));
    assert_eq!(freqs[0].spike_count, Some(1));

    let units = units_for(&db, call.id).await;
    assert_eq!(units.len(), 1);
    assert_eq!(units[0].unit_ref, 1610092);

    assert_eq!(call_patch::Entity::find().count(&db).await.unwrap(), 2);
}

// ---- Parity: rdio drops empty / "-" placeholder talkgroup fields -----------

/// rdio-scanner's `ParseMultipartContent` stores `talkgroupLabel/Name/Tag/Group`
/// only when `len > 0 && != "-"` (`parsers.go`). Recorders (Trunk Recorder always,
/// SDRTrunk often) send those parts even when the talkgroup is unknown — as empty
/// strings or a `"-"` placeholder. We must drop them to NULL like rdio, not
/// persist `""`/`"-"` as a bogus label/tag/group.
#[tokio::test]
async fn golden_generic_upload_drops_empty_and_dash_talkgroup_fields() {
    let (addr, db, _tmp) = spawn().await;
    repo::create_api_key(&db, "k", None, None, 0).await.unwrap();

    let audio = reqwest::multipart::Part::bytes(b"RIFFxxxx".to_vec())
        .file_name("a.wav")
        .mime_str("audio/x-wav")
        .unwrap();
    let form = reqwest::multipart::Form::new()
        .text("key", "k")
        .text("system", "8")
        .text("talkgroup", "54241")
        .text("timestamp", "1000")
        .text("talkgroupLabel", "") // empty
        .text("talkgroupName", "-") // placeholder
        .text("talkgroupTag", "-") // placeholder
        .text("talkgroupGroup", "-") // placeholder
        .part("audio", audio);
    let resp = reqwest::Client::new()
        .post(format!("http://{addr}/api/call-upload"))
        .multipart(form)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);

    let call = the_call(&db).await;
    let tg = the_talkgroup(&db, call.talkgroup_id).await;
    assert_eq!(tg.label, None, "empty talkgroupLabel -> NULL");
    assert_eq!(tg.name, None, "\"-\" talkgroupName -> NULL");
    assert_eq!(tg.tag_id, None, "\"-\" talkgroupTag -> no tag");
    assert!(
        repo::groups_for_talkgroup(&db, tg.id)
            .await
            .unwrap()
            .is_empty(),
        "\"-\" talkgroupGroup -> no group"
    );
}
