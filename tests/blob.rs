//! Blob-store tests (ticket #4): the filesystem backend, orphan-GC, and S3
//! presigning. The S3 test signs offline (SigV4 is computed locally, no network),
//! so it runs everywhere; real S3 I/O against Garage/MinIO is CI/manual (needs a
//! running store — no Docker here).

use std::collections::HashSet;

use bytes::Bytes;
use radio_scout::blob::{BlobStore, S3Config, orphan_gc};

async fn fs_store() -> (BlobStore, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = BlobStore::filesystem(dir.path().join("audio")).expect("blob store");
    (store, dir)
}

#[tokio::test]
async fn filesystem_roundtrip_size_and_range() {
    let (store, _dir) = fs_store().await;
    let data = Bytes::from_static(b"0123456789");

    store.put("ab/one.wav", data.clone()).await.unwrap();
    assert_eq!(store.size("ab/one.wav").await.unwrap(), Some(10));
    assert_eq!(store.get("ab/one.wav").await.unwrap().unwrap(), data);
    assert_eq!(
        store.get_range("ab/one.wav", 2, 5).await.unwrap(),
        Bytes::from_static(b"234"),
    );

    // Absent objects report None rather than erroring.
    assert_eq!(store.get("ab/missing.wav").await.unwrap(), None);
    assert_eq!(store.size("ab/missing.wav").await.unwrap(), None);

    // The filesystem backend proxies (never presigns).
    assert!(!store.is_presigning());
    assert!(store.presigned_get_url("ab/one.wav").await.is_none());
}

#[tokio::test]
async fn delete_is_idempotent_and_list_enumerates() {
    let (store, _dir) = fs_store().await;
    store
        .put("aa/1.wav", Bytes::from_static(b"x"))
        .await
        .unwrap();
    store
        .put("bb/2.wav", Bytes::from_static(b"y"))
        .await
        .unwrap();

    let mut keys = store.list_keys().await.unwrap();
    keys.sort();
    assert_eq!(keys, vec!["aa/1.wav".to_string(), "bb/2.wav".to_string()]);

    store.delete("aa/1.wav").await.unwrap();
    store.delete("aa/1.wav").await.unwrap(); // deleting a missing object is fine
    assert_eq!(
        store.list_keys().await.unwrap(),
        vec!["bb/2.wav".to_string()]
    );
}

#[tokio::test]
async fn orphan_gc_deletes_only_unreferenced() {
    let (store, _dir) = fs_store().await;
    for key in ["aa/1.wav", "bb/2.wav", "cc/3.wav"] {
        store.put(key, Bytes::from_static(b"x")).await.unwrap();
    }

    let referenced: HashSet<String> = ["aa/1.wav".to_string(), "cc/3.wav".to_string()]
        .into_iter()
        .collect();
    let mut deleted = orphan_gc(&store, &referenced).await.unwrap();
    deleted.sort();
    assert_eq!(deleted, vec!["bb/2.wav".to_string()]);

    let mut remaining = store.list_keys().await.unwrap();
    remaining.sort();
    assert_eq!(
        remaining,
        vec!["aa/1.wav".to_string(), "cc/3.wav".to_string()]
    );
}

#[tokio::test]
async fn s3_backend_presigns_get_urls_offline() {
    let store = BlobStore::s3(&S3Config {
        bucket: "radio-scout".into(),
        region: "us-east-1".into(),
        endpoint: Some("http://localhost:3900".into()), // Garage-style endpoint
        access_key_id: "GKtestaccesskey".into(),
        secret_access_key: "testsecretkey".into(),
        allow_http: true,
    })
    .expect("build s3 store");

    assert!(store.is_presigning());

    let url = store
        .presigned_get_url("ab/call.m4a")
        .await
        .expect("s3 presigns")
        .expect("signed url");
    assert!(url.contains("radio-scout"), "url names the bucket: {url}");
    assert!(url.contains("ab/call.m4a"), "url names the key: {url}");
    assert!(url.contains("X-Amz-Signature"), "url is signed: {url}");
}
