//! `GET /api/call/{id}/audio` edge + error branches (ticket #26 hardening).
//!
//! The happy-path range serving lives in `tests/skeleton.rs`; this file covers
//! the not-found paths, the MIME default, and the **S3 presigned-redirect** path
//! — the whole S3 serving mode that had no end-to-end test.

mod common;
use common::{TestApp, header_of};

use radio_scout::db::repo::NewCall;
use radio_scout::{BlobStore, S3Config};

/// A Call row pointing at `object_key`. No audio object is written unless the
/// test writes one.
async fn insert_call(app: &TestApp, object_key: &str, mime: Option<&str>) -> i64 {
    app.seed_call(NewCall {
        system_ref: 11,
        talkgroup_ref: 54241,
        call_at_ms: 1000,
        object_key: object_key.to_string(),
        audio_mime: mime.map(str::to_string),
        ..Default::default()
    })
    .await
}

/// The audio endpoint for a Call.
fn audio(id: i64) -> String {
    format!("/api/call/{id}/audio")
}

#[tokio::test]
async fn unknown_call_id_is_404() {
    let app = TestApp::spawn().await;

    let resp = app.get(&audio(999999)).await;

    assert_eq!(resp.status(), 404);
    assert_eq!(resp.text().await.unwrap(), "call not found\n");
}

#[tokio::test]
async fn call_with_missing_audio_object_is_404() {
    let app = TestApp::spawn().await;
    // Row exists, but nothing was ever written to the object store.
    let id = insert_call(&app, "ab/never-stored.wav", Some("audio/x-wav")).await;

    let resp = app.get(&audio(id)).await;

    assert_eq!(resp.status(), 404);
    assert_eq!(resp.text().await.unwrap(), "audio not found\n");
}

#[tokio::test]
async fn audio_without_a_stored_mime_defaults_to_octet_stream() {
    let app = TestApp::spawn().await;
    app.put_object("ab/clip.bin", b"RIFFxxxx").await;
    let id = insert_call(&app, "ab/clip.bin", None).await; // no MIME recorded

    let resp = app.get(&audio(id)).await;

    assert_eq!(resp.status(), 200);
    assert_eq!(
        header_of(&resp, "content-type"),
        Some("application/octet-stream")
    );
    assert_eq!(resp.bytes().await.unwrap().as_ref(), b"RIFFxxxx");
}

/// A Call's audio never changes once stored, so it is cached hard — which is
/// what makes the client's next-Call prefetch (#14) pay off: the prefetch warms
/// the HTTP cache and the `<audio>` element then starts from disk instead of the
/// network. A media element re-requests by range, so the ranged response has to
/// carry the same headers or the cache is bypassed on the very request that
/// matters.
#[tokio::test]
async fn audio_is_cacheable_so_a_prefetched_call_starts_instantly() {
    const CACHE: &str = "private, max-age=604800, immutable";
    let app = TestApp::spawn().await;
    app.put_object("ab/clip.m4a", b"0123456789").await;
    let id = insert_call(&app, "ab/clip.m4a", Some("audio/mp4")).await;

    let full = app.get(&audio(id)).await;
    assert_eq!(full.status(), 200);
    assert_eq!(header_of(&full, "cache-control"), Some(CACHE));

    let ranged = app.get_range(&audio(id), "bytes=2-5").await;
    assert_eq!(ranged.status(), 206);
    assert_eq!(header_of(&ranged, "cache-control"), Some(CACHE));
}

#[tokio::test]
async fn s3_backend_redirects_to_a_presigned_url() {
    // An S3-backed store presigns offline (no network), so this exercises the
    // whole redirect path without a live bucket.
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
    let app = TestApp::builder().store(s3).spawn().await;
    let id = insert_call(&app, "ab/deadbeef.m4a", Some("audio/mp4")).await;

    let resp = app.get_without_redirects(&audio(id)).await;

    assert_eq!(
        resp.status(),
        307,
        "temporary redirect to the presigned URL"
    );
    let location = header_of(&resp, "location").expect("Location header");
    assert!(
        location.contains("radio-scout/ab/deadbeef.m4a"),
        "points at the object: {location}"
    );
    assert!(
        location.contains("X-Amz-Signature="),
        "is a presigned URL: {location}"
    );
    // The bytes are immutable, but the *signature* on this Location expires —
    // caching the redirect would outlive it and start handing out 403s.
    assert!(
        header_of(&resp, "cache-control").is_none(),
        "the presigned redirect itself must not be cached"
    );
}
