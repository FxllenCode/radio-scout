//! `GET /api/call/{id}/audio` edge + error branches (ticket #26 hardening).
//!
//! The happy-path range serving lives in `tests/skeleton.rs`; this file covers
//! the not-found paths, the MIME default, and the **S3 presigned-redirect** path
//! — the whole S3 serving mode that had no end-to-end test.

mod common;
use common::{TestApp, header_of};

use rstest::rstest;

use radio_scout::blob::AudioStore;
use radio_scout::db::entities::call::EnhancementState;
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
    let s3 = s3_store();
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
    // The redirect is cacheable for a slice of the signature's own lifetime
    // (#31) — see `the_presigned_redirect_is_cacheable_and_stable` for why, and
    // for the bound.
    assert_eq!(
        header_of(&resp, "cache-control"),
        Some("private, max-age=240"),
        "cacheable, but for less than the signature has left"
    );
}

/// Next-Call prefetch has to pay off on the S3 backend too (#31).
///
/// The client warms `GET /api/call/{id}/audio` ahead of time, and the `<audio>`
/// element then asks for it again — ranged. On the filesystem backend the second
/// request is a cache hit. On S3 the route 307s, and while every request signed
/// a *fresh* URL the element asked for a different one than the prefetch warmed:
/// every prefetched Call was downloaded twice, on the deployment ADR-0002
/// recommends for scale-up, and a phone paid for it.
///
/// So the signature is reused for a slice of its own lifetime, and the redirect
/// says how long that is. What must never happen is a cached redirect outliving
/// its signature and handing out 403s, which is why the advertised `max-age` is
/// the remaining validity *minus* a margin rather than the validity itself.
#[tokio::test]
async fn the_presigned_redirect_is_cacheable_and_stable() {
    let app = TestApp::builder().store(s3_store()).spawn().await;
    let id = insert_call(&app, "ab/deadbeef.m4a", Some("audio/mp4")).await;

    let first = app.get_without_redirects(&audio(id)).await;
    // A full second apart, deliberately. SigV4 stamps `X-Amz-Date` to the
    // second, so two signings inside the same second are byte-identical anyway
    // and comparing them would prove nothing — this is what makes the assertion
    // below discriminate between a cache and a coincidence.
    tokio::time::sleep(std::time::Duration::from_millis(1_100)).await;
    let second = app.get_without_redirects(&audio(id)).await;

    assert_eq!(
        header_of(&first, "location"),
        header_of(&second, "location"),
        "the element must be redirected to the URL the prefetch already warmed"
    );
    // A signature is minted for 300s and a redirect to it advertised for the 240
    // that leaves after the margin. A full second has passed, so a *reused* one
    // must advertise strictly less than 240 — the second, independent proof that
    // nothing re-signed, and the invariant that matters either way: what a client
    // is told to cache always expires before what it points at.
    let reused = max_age_of(&second);
    assert!(
        (1..240).contains(&reused),
        "a reused signature counts down what is left of it, got {reused}"
    );
}

/// The `max-age` a response's `Cache-Control` advertises.
fn max_age_of(resp: &reqwest::Response) -> u64 {
    header_of(resp, "cache-control")
        .expect("Cache-Control")
        .split("max-age=")
        .nth(1)
        .expect("a max-age directive")
        .split(',')
        .next()
        .expect("the max-age value")
        .trim()
        .parse()
        .expect("max-age is a number")
}

/// ...but not for a Call still queued for enhancement (#20). The worker points
/// the row at a **different object** when it finishes, so a redirect cached for
/// four minutes would keep sending the listener to the un-levelled audio long
/// after the levelled version existed. The redirect inherits the same 30s the
/// proxied response already uses for a pending Call.
#[tokio::test]
async fn a_pending_calls_redirect_is_not_cached_past_its_enhancement() {
    let app = TestApp::builder().store(s3_store()).spawn().await;
    let id = app
        .seed_call(NewCall {
            system_ref: 11,
            talkgroup_ref: 54241,
            call_at_ms: 1000,
            object_key: "ab/pending.m4a".into(),
            audio_mime: Some("audio/mp4".into()),
            ..Default::default()
        })
        .await;
    radio_scout::db::repo::mark_enhancement(&app.db, id, EnhancementState::PENDING)
        .await
        .expect("mark pending");

    let resp = app.get_without_redirects(&audio(id)).await;

    assert_eq!(resp.status(), 307);
    assert_eq!(
        header_of(&resp, "cache-control"),
        Some("private, max-age=30"),
        "the object key is about to change"
    );
}

/// An S3-backed store that presigns offline (no network), so the whole redirect
/// path is exercised without a live bucket.
fn s3_store() -> BlobStore {
    BlobStore::s3(&S3Config {
        bucket: "radio-scout".into(),
        region: "us-east-1".into(),
        endpoint: Some("http://127.0.0.1:9000".into()),
        access_key_id: "test-access".into(),
        secret_access_key: "test-secret".into(),
        allow_http: true,
    })
    .expect("s3 store")
}

// ---------------------------------------------------------------------------
// The store failing underneath a listener (#37).
//
// These are the arms that separate "gone" from "broken". A 404 tells a client
// to stop asking; a 500 with a ref tells it to try again and tells the operator
// where to look. Getting that backwards is how a transient Garage outage turns
// into every client quietly deciding the archive is empty.
// ---------------------------------------------------------------------------

/// A store that stats an object happily and then refuses to hand over its
/// bytes — a Garage node shedding load, a disk throwing read errors.
///
/// Both halves of the audio contract have to answer the same way: the whole-body
/// GET a desktop makes, and the ranged GET iOS `<audio>` makes (ADR-0002). They
/// are separate code paths and separate stages in the log, so they are asserted
/// separately.
#[rstest]
#[case::whole_body(None, "read-audio")]
#[case::ranged(Some("bytes=0-3"), "read-audio-range")]
#[tokio::test]
async fn a_store_that_refuses_to_read_is_a_server_error_not_a_missing_call(
    #[case] range: Option<&str>,
    #[case] stage: &str,
) {
    let capture = common::logs::LogCapture::start();
    let tmp = tempfile::tempdir().expect("tempdir");
    let (store, faults) = common::faulty_store(tmp.path());
    let app = TestApp::builder().store(store).spawn().await;
    let id = insert_call(&app, "aa/1.wav", Some("audio/wav")).await;
    app.put_object("aa/1.wav", b"audio-bytes").await;

    faults.fail_reads();
    let resp = match range {
        Some(range) => app.get_range(&audio(id), range).await,
        None => app.get(&audio(id)).await,
    };

    assert_eq!(resp.status(), 500, "a broken store is not a missing Call");
    let request_id = common::request_id_of(&resp);
    let body = resp.text().await.expect("body");
    assert_eq!(body, format!("internal error (ref: {request_id})\n"));

    let line = capture.only_line_containing("server error");
    assert!(line.contains(&format!("stage={stage}")), "{line}");
    assert!(
        line.contains(common::INJECTED_IO),
        "the cause travels: {line}"
    );
}

/// The object pruned between the stat that sized it and the read that would
/// have served it. Retention and orphan-GC both run while listeners are
/// listening, so this window is ordinary rather than exotic.
///
/// It is a **404**, not a 500: the object really is gone, the client should stop
/// asking, and nothing needs an operator's attention. What separates it from the
/// test above is only what the store *said* — an error there, nothing here — so
/// the store is told to say it (#97). Before that seam existed the same window
/// had to be staged by parking a real read inside the store and pruning a real
/// object while it was held, which meant the fault machinery had to know that
/// serving stats before it reads.
#[tokio::test]
async fn audio_pruned_between_the_stat_and_the_read_is_a_404() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (store, faults) = common::faulty_store(tmp.path());
    let app = TestApp::builder().store(store).spawn().await;
    let id = insert_call(&app, "aa/1.wav", Some("audio/wav")).await;
    app.put_object("aa/1.wav", b"audio-bytes").await;

    faults.hide_reads();
    let resp = app.get(&audio(id)).await;

    assert_eq!(resp.status(), 404);
    assert_eq!(resp.text().await.expect("body"), "audio not found\n");
}
