//! Ingest integration tests (ticket #5): hashed per-system API-key auth,
//! duplicate detection, and full-field persistence — driven over the real HTTP
//! boundary against a DB-backed app.

use std::sync::Arc;

use radio_scout::db::entities::{call_frequency, call_patch, call_unit};
use radio_scout::db::{self, repo};
use radio_scout::{AppState, BlobStore, IngestConfig, build_app};
use sea_orm::{DatabaseConnection, EntityTrait, PaginatorTrait};

/// Bring up a DB-backed app; return its address, a DB handle (for seeding keys /
/// asserting rows), and the TempDir.
async fn spawn() -> (String, DatabaseConnection, tempfile::TempDir) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let audio = Arc::new(BlobStore::filesystem(tmp.path().join("audio")).expect("blob"));
    let url = format!("sqlite://{}?mode=rwc", tmp.path().join("t.db").display());
    let dbc = db::connect(&url).await.expect("db");
    let app = build_app(AppState::new(audio, dbc.clone(), IngestConfig::default()));

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serve");
    });
    (format!("127.0.0.1:{}", addr.port()), dbc, tmp)
}

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
