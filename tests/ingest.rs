//! Ingest integration tests (ticket #5): hashed per-system API-key auth,
//! duplicate detection, and full-field persistence — driven over the real HTTP
//! boundary against a DB-backed app.

use radio_scout::db::entities::{
    api_key, call, call_frequency, call_patch, call_unit, system, tag, talkgroup,
};
use radio_scout::db::repo;
use sea_orm::{ActiveModelTrait, EntityTrait, PaginatorTrait, Set};

mod common;
use common::spawn;

fn form(key: &str, system: i64, talkgroup: i64, timestamp_ms: i64) -> reqwest::multipart::Form {
    let audio = reqwest::multipart::Part::bytes(b"audio-bytes".to_vec())
        .file_name("a.wav")
        .mime_str("audio/x-wav")
        .unwrap();
    reqwest::multipart::Form::new()
        .text("key", key.to_string())
        .text("system", system.to_string())
        .text("talkgroup", talkgroup.to_string())
        .text("timestamp", timestamp_ms.to_string())
        .part("audio", audio)
}

async fn post(addr: &str, form: reqwest::multipart::Form) -> (u16, String) {
    let resp = reqwest::Client::new()
        .post(format!("http://{addr}/api/call-upload"))
        .multipart(form)
        .send()
        .await
        .expect("upload");
    let status = resp.status().as_u16();
    (status, resp.text().await.unwrap_or_default())
}

#[tokio::test]
async fn ingest_requires_a_valid_api_key() {
    let (addr, db, _tmp) = spawn().await;
    repo::create_api_key(&db, "good-key", None, None, 0)
        .await
        .unwrap();

    let (status, body) = post(&addr, form("good-key", 11, 54241, 1000)).await;
    assert_eq!(status, 200);
    assert!(body.contains("Call imported successfully."), "{body:?}");

    let (status, body) = post(&addr, form("wrong-key", 11, 54241, 2000)).await;
    assert_eq!(status, 401);
    assert!(
        body.contains("Invalid API key for system 11 talkgroup 54241."),
        "{body:?}"
    );

    // No key field at all -> rejected.
    let no_key = reqwest::multipart::Form::new()
        .text("system", "11")
        .text("talkgroup", "54241")
        .part(
            "audio",
            reqwest::multipart::Part::bytes(b"x".to_vec())
                .file_name("a.wav")
                .mime_str("audio/x-wav")
                .unwrap(),
        );
    let (status, _) = post(&addr, no_key).await;
    assert_eq!(status, 401);
}

#[tokio::test]
async fn api_key_is_scoped_to_its_system() {
    let (addr, db, _tmp) = spawn().await;
    repo::create_api_key(&db, "sys11-key", Some(11), None, 0)
        .await
        .unwrap();

    let (status, _) = post(&addr, form("sys11-key", 11, 54241, 1000)).await;
    assert_eq!(status, 200, "key grants its own system");

    let (status, body) = post(&addr, form("sys11-key", 22, 54241, 1000)).await;
    assert_eq!(status, 401, "key denied for another system");
    assert!(body.contains("Invalid API key for system 22"), "{body:?}");
}

#[tokio::test]
async fn duplicate_calls_within_the_window_are_rejected() {
    let (addr, db, _tmp) = spawn().await;
    repo::create_api_key(&db, "k", None, None, 0).await.unwrap();

    let (status, body) = post(&addr, form("k", 11, 54241, 1000)).await;
    assert_eq!(status, 200);
    assert!(body.contains("Call imported successfully."));

    // Same system+talkgroup at the same time -> duplicate (still HTTP 200).
    let (status, body) = post(&addr, form("k", 11, 54241, 1000)).await;
    assert_eq!(status, 200);
    assert!(body.contains("duplicate call rejected"), "{body:?}");

    // A different talkgroup is not a duplicate.
    let (status, body) = post(&addr, form("k", 11, 99999, 1000)).await;
    assert_eq!(status, 200);
    assert!(body.contains("Call imported successfully."));

    // The same talkgroup well outside the ~500ms window is not a duplicate.
    let (status, body) = post(&addr, form("k", 11, 54241, 1000 + 5000)).await;
    assert_eq!(status, 200);
    assert!(body.contains("Call imported successfully."), "{body:?}");
}

#[tokio::test]
async fn ingest_persists_the_full_field_set() {
    let (addr, db, _tmp) = spawn().await;
    repo::create_api_key(&db, "k", None, None, 0).await.unwrap();

    let audio = reqwest::multipart::Part::bytes(b"audio-bytes".to_vec())
        .file_name("call.m4a")
        .mime_str("audio/mp4")
        .unwrap();
    let form = reqwest::multipart::Form::new()
        .text("key", "k")
        .text("system", "11")
        .text("systemLabel", "RSP25MTL")
        .text("talkgroup", "54241")
        .text("talkgroupLabel", "TDB A1")
        .text("talkgroupTag", "Fire dispatch")
        .text("talkgroupGroup", "Fire")
        .text("timestamp", "1000")
        .text(
            "frequencies",
            r#"[{"freq":774031250,"pos":0,"len":1.5,"dbm":-50,"errorCount":1,"spikeCount":0}]"#,
        )
        .text("sources", r#"[{"src":4424000,"pos":0,"tag":"Engine 1"}]"#)
        .text("patches", "[100, 200]")
        .part("audio", audio);

    let (status, body) = post(&addr, form).await;
    assert_eq!(status, 200, "{body:?}");
    assert!(body.contains("Call imported successfully."));

    assert_eq!(call_frequency::Entity::find().count(&db).await.unwrap(), 1);
    assert_eq!(call_unit::Entity::find().count(&db).await.unwrap(), 1);
    assert_eq!(call_patch::Entity::find().count(&db).await.unwrap(), 2);
}

async fn post_tr(addr: &str, meta_json: &str, audio: &[u8]) -> (u16, String) {
    let audio = reqwest::multipart::Part::bytes(audio.to_vec())
        .file_name("call.m4a")
        .mime_str("audio/mp4")
        .unwrap();
    let form = reqwest::multipart::Form::new()
        .text("key", "k")
        .text("meta", meta_json.to_string())
        .part("audio", audio);
    let resp = reqwest::Client::new()
        .post(format!("http://{addr}/api/trunk-recorder-call-upload"))
        .multipart(form)
        .send()
        .await
        .expect("tr upload");
    (
        resp.status().as_u16(),
        resp.text().await.unwrap_or_default(),
    )
}

#[tokio::test]
async fn trunk_recorder_upload_persists_call_and_maps_meta() {
    let (addr, db, _tmp) = spawn().await;
    repo::create_api_key(&db, "k", None, None, 0).await.unwrap();

    let meta = r#"{
      "short_name":"butco","talkgroup":54241,
      "talkgroup_tag":"TDB A1","talkgroup_description":"Fire Dispatch A1",
      "talkgroup_group":"Fire","talkgroup_group_tag":"Fire Dispatch",
      "start_time":1669740338,"freq":774031250,
      "freqList":[{"freq":774031250,"pos":0,"len":1.5,"error_count":1,"spike_count":0}],
      "srcList":[{"src":4424000,"pos":0,"tag":"Engine 1"}],
      "patched_talkgroups":[100,200]
    }"#;

    let (status, body) = post_tr(&addr, meta, b"audio-bytes").await;
    assert_eq!(status, 200, "{body:?}");
    assert!(body.contains("Call imported successfully."));

    // The timestamp is start_time (NOT now) — the bug this ticket guards against.
    let stored = call::Entity::find().one(&db).await.unwrap().unwrap();
    assert_eq!(
        stored.call_at_ms, 1669740338000,
        "start_time used, not now()"
    );

    // rdio field mapping: talkgroup_tag->label, description->name, group_tag->tag.
    let tg = talkgroup::Entity::find_by_id(stored.talkgroup_id)
        .one(&db)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(tg.r#ref, 54241);
    assert_eq!(tg.label.as_deref(), Some("TDB A1"));
    assert_eq!(tg.name.as_deref(), Some("Fire Dispatch A1"));
    let tag = tag::Entity::find_by_id(tg.tag_id.unwrap())
        .one(&db)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(tag.name, "Fire Dispatch");

    // System resolved from short_name.
    let sys = system::Entity::find_by_id(stored.system_id)
        .one(&db)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(sys.label.as_deref(), Some("butco"));

    // Child rows from freqList / srcList / patched_talkgroups.
    assert_eq!(call_frequency::Entity::find().count(&db).await.unwrap(), 1);
    assert_eq!(call_patch::Entity::find().count(&db).await.unwrap(), 2);
    let unit = call_unit::Entity::find().one(&db).await.unwrap().unwrap();
    assert_eq!(unit.unit_ref, 4424000);
    assert_eq!(unit.label.as_deref(), Some("Engine 1"));
}

#[tokio::test]
async fn trunk_recorder_missing_talkgroup_is_incomplete() {
    let (addr, db, _tmp) = spawn().await;
    repo::create_api_key(&db, "k", None, None, 0).await.unwrap();

    let (status, body) = post_tr(&addr, r#"{"short_name":"butco","start_time":1000}"#, b"x").await;
    assert_eq!(status, 417);
    assert!(
        body.to_lowercase()
            .starts_with("incomplete call data: no talkgroup"),
        "{body:?}"
    );
}

#[tokio::test]
async fn trunk_recorder_converges_with_generic_upload_by_label() {
    let (addr, db, _tmp) = spawn().await;
    repo::create_api_key(&db, "k", None, None, 0).await.unwrap();

    // A generic upload creates System ref=11 with label "butco".
    let generic = reqwest::multipart::Form::new()
        .text("key", "k")
        .text("system", "11")
        .text("systemLabel", "butco")
        .text("talkgroup", "100")
        .text("timestamp", "1000")
        .part(
            "audio",
            reqwest::multipart::Part::bytes(b"x".to_vec())
                .file_name("a.wav")
                .mime_str("audio/x-wav")
                .unwrap(),
        );
    let (status, _) = post(&addr, generic).await;
    assert_eq!(status, 200);

    // A TR upload with the matching short_name lands on that same System.
    let (status, _) = post_tr(
        &addr,
        r#"{"short_name":"butco","talkgroup":200,"start_time":2}"#,
        b"y",
    )
    .await;
    assert_eq!(status, 200);

    assert_eq!(
        system::Entity::find().count(&db).await.unwrap(),
        1,
        "TR + generic uploads for the same label share one System"
    );
    let sys = system::Entity::find().one(&db).await.unwrap().unwrap();
    assert_eq!(sys.r#ref, 11, "TR reuses the generic upload's Ref");
    let calls = call::Entity::find().all(&db).await.unwrap();
    assert_eq!(calls.len(), 2);
    assert!(calls.iter().all(|c| c.system_id == sys.id));
}

#[tokio::test]
async fn trunk_recorder_same_short_name_reuses_one_system() {
    let (addr, db, _tmp) = spawn().await;
    repo::create_api_key(&db, "k", None, None, 0).await.unwrap();

    // First upload synthesizes a Ref for "newsys"; the second finds it by label.
    post_tr(
        &addr,
        r#"{"short_name":"newsys","talkgroup":1,"start_time":1}"#,
        b"a",
    )
    .await;
    post_tr(
        &addr,
        r#"{"short_name":"newsys","talkgroup":2,"start_time":2}"#,
        b"b",
    )
    .await;

    assert_eq!(
        system::Entity::find().count(&db).await.unwrap(),
        1,
        "the same short_name maps to one System (stable synthetic Ref)"
    );
}

/// A **disabled** API key is denied even though its hash matches (ADR-0008). This
/// is the load-bearing security branch `authorize_ingest` guards — a revoked key
/// must not ingest.
#[tokio::test]
async fn disabled_api_key_is_rejected() {
    let (addr, db, _tmp) = spawn().await;
    api_key::ActiveModel {
        key_hash: Set(repo::hash_key("revoked-key")),
        label: Set(None),
        system_ref: Set(None),
        disabled: Set(true),
        created_at_ms: Set(0),
        ..Default::default()
    }
    .insert(&db)
    .await
    .unwrap();

    let (status, body) = post(&addr, form("revoked-key", 11, 54241, 1000)).await;
    assert_eq!(status, 401, "disabled key is denied");
    assert!(
        body.contains("Invalid API key for system 11 talkgroup 54241."),
        "{body:?}"
    );
    // And nothing was stored.
    assert_eq!(call::Entity::find().count(&db).await.unwrap(), 0);
}

/// A call with a talkgroup but no audio part is incomplete (417) — checked before
/// auth, so it never touches the DB.
#[tokio::test]
async fn upload_without_audio_is_incomplete() {
    let (addr, db, _tmp) = spawn().await;
    repo::create_api_key(&db, "k", None, None, 0).await.unwrap();

    let no_audio = reqwest::multipart::Form::new()
        .text("key", "k")
        .text("system", "11")
        .text("talkgroup", "54241")
        .text("timestamp", "1000"); // no audio part at all
    let (status, body) = post(&addr, no_audio).await;
    assert_eq!(status, 417);
    assert!(
        body.to_lowercase()
            .starts_with("incomplete call data: no audio"),
        "{body:?}"
    );
}

/// An empty audio part is treated as no audio (417).
#[tokio::test]
async fn empty_audio_is_incomplete() {
    let (addr, db, _tmp) = spawn().await;
    repo::create_api_key(&db, "k", None, None, 0).await.unwrap();

    let empty = reqwest::multipart::Form::new()
        .text("key", "k")
        .text("system", "11")
        .text("talkgroup", "54241")
        .text("timestamp", "1000")
        .part(
            "audio",
            reqwest::multipart::Part::bytes(Vec::new())
                .file_name("a.wav")
                .mime_str("audio/x-wav")
                .unwrap(),
        );
    let (status, body) = post(&addr, empty).await;
    assert_eq!(status, 417);
    assert!(
        body.to_lowercase()
            .starts_with("incomplete call data: no audio"),
        "{body:?}"
    );
}

/// Trunk Recorder native upload with unparseable `meta` JSON → 417 `Invalid call
/// data` (the exact rdio string), before auth.
#[tokio::test]
async fn trunk_recorder_invalid_meta_json_is_rejected() {
    let (addr, _db, _tmp) = spawn().await;
    let (status, body) = post_tr(&addr, "this is not json {", b"audio").await;
    assert_eq!(status, 417);
    assert_eq!(body, "Invalid call data\n");
}

/// Trunk Recorder native upload with no `meta` part at all → incomplete (417).
#[tokio::test]
async fn trunk_recorder_without_meta_is_incomplete() {
    let (addr, _db, _tmp) = spawn().await;
    let audio = reqwest::multipart::Part::bytes(b"audio".to_vec())
        .file_name("call.wav")
        .mime_str("audio/x-wav")
        .unwrap();
    let form = reqwest::multipart::Form::new()
        .text("key", "k")
        .part("audio", audio); // no `meta`
    let resp = reqwest::Client::new()
        .post(format!("http://{addr}/api/trunk-recorder-call-upload"))
        .multipart(form)
        .send()
        .await
        .unwrap();
    let status = resp.status().as_u16();
    let body = resp.text().await.unwrap();
    assert_eq!(status, 417);
    assert!(
        body.to_lowercase()
            .starts_with("incomplete call data: no meta"),
        "{body:?}"
    );
}

/// The rdio-compatible field aliases are honored: `patched_talkgroups` (== the
/// `patches` array) and `audioType`/`audioName` (the MIME + filename carried as
/// form fields rather than on the audio part).
#[tokio::test]
async fn field_aliases_are_accepted() {
    let (addr, db, _tmp) = spawn().await;
    repo::create_api_key(&db, "k", None, None, 0).await.unwrap();

    // Audio part carries neither filename nor MIME; the fields supply both.
    // Order matches real recorders (Trunk Recorder): the audio part comes first,
    // then the metadata fields, so the fields win (last-write).
    let audio = reqwest::multipart::Part::bytes(b"audio-bytes".to_vec());
    let form = reqwest::multipart::Form::new()
        .part("audio", audio)
        .text("key", "k")
        .text("system", "11")
        .text("talkgroup", "54241")
        .text("timestamp", "1000")
        .text("patched_talkgroups", "[100, 200]")
        .text("audioType", "audio/mpeg")
        .text("audioName", "clip.mp3");
    let (status, body) = post(&addr, form).await;
    assert_eq!(status, 200, "{body:?}");

    assert_eq!(
        call_patch::Entity::find().count(&db).await.unwrap(),
        2,
        "patched_talkgroups is the patches alias"
    );
    let stored = call::Entity::find().one(&db).await.unwrap().unwrap();
    assert_eq!(
        stored.audio_mime.as_deref(),
        Some("audio/mpeg"),
        "audioType"
    );
    assert_eq!(stored.audio_name.as_deref(), Some("clip.mp3"), "audioName");
}
