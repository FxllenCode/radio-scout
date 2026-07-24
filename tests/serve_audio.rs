//! `GET /api/call/{id}/audio` edge + error branches (ticket #26 hardening).
//!
//! The happy-path range serving lives in `tests/skeleton.rs`; this file covers
//! the not-found paths, the MIME default, and the **S3 presigned-redirect** path
//! — the whole S3 serving mode that had no end-to-end test.

mod common;
use common::{spawn, spawn_with_blob};

use bytes::Bytes;
use radio_scout::db::repo::{self, NewCall};
use radio_scout::{BlobStore, S3Config};
use sea_orm::DatabaseConnection;

/// Insert a Call row pointing at `object_key` (no audio object is written).
async fn insert_call(db: &DatabaseConnection, object_key: &str, mime: Option<&str>) -> i64 {
    let new = NewCall {
        system_ref: 11,
        talkgroup_ref: 54241,
        call_at_ms: 1000,
        object_key: object_key.to_string(),
        audio_mime: mime.map(str::to_string),
        ..Default::default()
    };
    repo::insert_call(db, &new, 0)
        .await
        .expect("insert call")
        .id
}

async fn get(addr: &str, path: &str) -> reqwest::Response {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none()) // observe the 307 itself
        .build()
        .unwrap()
        .get(format!("http://{addr}{path}"))
        .send()
        .await
        .expect("get audio")
}

#[tokio::test]
async fn unknown_call_id_is_404() {
    let (addr, _db, _tmp) = spawn().await;
    let resp = get(&addr, "/api/call/999999/audio").await;
    assert_eq!(resp.status(), 404);
    assert_eq!(resp.text().await.unwrap(), "call not found\n");
}

#[tokio::test]
async fn call_with_missing_audio_object_is_404() {
    let (addr, db, _tmp) = spawn().await;
    // Row exists, but nothing was ever written to the object store.
    let id = insert_call(&db, "ab/never-stored.wav", Some("audio/x-wav")).await;
    let resp = get(&addr, &format!("/api/call/{id}/audio")).await;
    assert_eq!(resp.status(), 404);
    assert_eq!(resp.text().await.unwrap(), "audio not found\n");
}

#[tokio::test]
async fn audio_without_a_stored_mime_defaults_to_octet_stream() {
    let tmp = tempfile::tempdir().unwrap();
    let blob = BlobStore::filesystem(tmp.path().join("audio")).unwrap();
    let (addr, db) = spawn_with_blob(blob.clone(), tmp.path()).await;

    blob.put("ab/clip.bin", Bytes::from_static(b"RIFFxxxx"))
        .await
        .unwrap();
    let id = insert_call(&db, "ab/clip.bin", None).await; // no MIME recorded

    let resp = get(&addr, &format!("/api/call/{id}/audio")).await;
    assert_eq!(resp.status(), 200);
    assert_eq!(
        resp.headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok()),
        Some("application/octet-stream"),
    );
    assert_eq!(resp.bytes().await.unwrap().as_ref(), b"RIFFxxxx");
}

#[tokio::test]
async fn s3_backend_redirects_to_a_presigned_url() {
    // An S3-backed store presigns offline (no network), so this exercises the
    // whole redirect path without a live bucket.
    let tmp = tempfile::tempdir().unwrap();
    let s3 = BlobStore::s3(&S3Config {
        bucket: "radio-scout".into(),
        region: "us-east-1".into(),
        endpoint: Some("http://127.0.0.1:9000".into()),
        access_key_id: "test-access".into(),
        secret_access_key: "test-secret".into(),
        allow_http: true,
    })
    .expect("s3 store");
    assert!(s3.is_presigning());
    let (addr, db) = spawn_with_blob(s3, tmp.path()).await;

    let id = insert_call(&db, "ab/deadbeef.m4a", Some("audio/mp4")).await;
    let resp = get(&addr, &format!("/api/call/{id}/audio")).await;

    assert_eq!(
        resp.status(),
        307,
        "temporary redirect to the presigned URL"
    );
    let location = resp
        .headers()
        .get("location")
        .and_then(|v| v.to_str().ok())
        .expect("Location header");
    assert!(
        location.contains("radio-scout/ab/deadbeef.m4a"),
        "points at the object: {location}"
    );
    assert!(
        location.contains("X-Amz-Signature="),
        "is a presigned URL: {location}"
    );
}
