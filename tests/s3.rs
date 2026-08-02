//! The S3 backend against a store that answers (ticket #35).
//!
//! `tests/blob.rs` covers the same surface offline: SigV4 is computed locally,
//! so it runs everywhere and proves nothing about a round trip. This file is the
//! other half — every call here reaches a real Garage/MinIO and fails if the
//! object never lands. That backend is what a hosted or NAS-backed install runs
//! on (ADR-0002), and range requests are the half iOS `<audio>` will not play
//! without.
//!
//! **These tests skip when the run was given no store**, saying so — the same
//! posture as the dual-dialect Postgres run (#22). See
//! `docs/agents/real-s3.md` for the one command that provides one.

mod common;
use common::{CallUpload, TestApp, header_of};

use std::collections::HashSet;

use bytes::Bytes;
use radio_scout::blob::AudioStore;
use radio_scout::blob::orphan_gc;
use radio_scout::db::repo::NewCall;
use radio_scout::now_ms;

/// A cutoff an hour in the future: everything these tests write is older, so the
/// orphan-GC write grace period no longer protects any of it.
fn past_grace() -> i64 {
    now_ms() + 3_600_000
}

/// The whole object contract in one round trip, against a server: an object that
/// lands, reports its own size, comes back byte-identical, serves a byte range,
/// and stops existing when deleted.
///
/// Every one of these is a code path the offline test cannot reach — `size` is
/// a `HEAD`, `get_range` is a `Range:` header the server has to honour, and the
/// absent-object answers are S3's `404` mapped to `None` rather than a local
/// `ENOENT`.
#[tokio::test]
async fn objects_round_trip_through_a_real_bucket() {
    let Some(store) = common::s3::test_bucket().await else {
        return;
    };
    let data = Bytes::from_static(b"0123456789");

    store.put("ab/one.wav", data.clone()).await.unwrap();

    assert_eq!(store.size("ab/one.wav").await.unwrap(), Some(10));
    assert_eq!(store.get("ab/one.wav").await.unwrap().unwrap(), data);
    assert_eq!(
        store.get_range("ab/one.wav", 2, 5).await.unwrap(),
        Bytes::from_static(b"234"),
    );

    // A key nothing was ever written to is `None`, not an error.
    assert_eq!(store.get("ab/missing.wav").await.unwrap(), None);
    assert_eq!(store.size("ab/missing.wav").await.unwrap(), None);

    store.delete("ab/one.wav").await.unwrap();
    assert_eq!(store.size("ab/one.wav").await.unwrap(), None);
    // S3 answers a `DELETE` of an absent key with a 204, so this is not the
    // filesystem's "missing is not an error" branch — it is the server's.
    store.delete("ab/one.wav").await.unwrap();
}

/// Orphan-GC over a real bucket: it lists what the *server* holds, judges each
/// object by the *server's* `Last-Modified`, and the objects it reclaims are
/// gone from the bucket afterwards.
///
/// The offline test judges timestamps the local filesystem wrote moments
/// earlier; here they cross the wire as RFC 1123 seconds, which is where a
/// resolution or clock-skew bug would live.
#[tokio::test]
async fn orphan_gc_reclaims_only_unreferenced_objects_in_a_real_bucket() {
    let Some(store) = common::s3::test_bucket().await else {
        return;
    };
    for key in ["aa/1.wav", "bb/2.wav", "cc/3.wav"] {
        store.put(key, Bytes::from_static(b"xx")).await.unwrap();
    }

    let referenced: HashSet<String> = ["aa/1.wav".to_string(), "cc/3.wav".to_string()]
        .into_iter()
        .collect();
    let outcome = orphan_gc(&store, &referenced, past_grace()).await.unwrap();

    let reclaimed: Vec<String> = outcome.reclaimed.iter().map(|o| o.key.clone()).collect();
    assert_eq!(reclaimed, vec!["bb/2.wav".to_string()]);
    assert_eq!(outcome.bytes(), 2, "the size the server reported");
    assert_eq!(outcome.errors, 0);

    let mut remaining = store.list_keys().await.unwrap();
    remaining.sort();
    assert_eq!(
        remaining,
        vec!["aa/1.wav".to_string(), "cc/3.wav".to_string()],
        "the bucket itself, re-listed"
    );
}

/// The write half of the operator's actual question — "does a Call from my
/// recorder land in my bucket?"
///
/// Every other test here drives [`BlobStore`](radio_scout::BlobStore) directly.
/// This one goes in at the recorder's own boundary, so what reaches the store is
/// whatever the ingest handler decided to write: the sharded key it minted, and
/// the bytes it pulled off the multipart part.
#[tokio::test]
async fn an_ingested_call_lands_in_the_bucket() {
    let Some(store) = common::s3::test_bucket().await else {
        return;
    };
    let app = TestApp::builder().store(store).spawn().await;
    app.create_api_key("k").await;

    app.upload_ok(CallUpload::new()).await;

    let call = app.the_call().await;
    assert_eq!(
        app.object_bytes(&call.object_key).await.as_deref(),
        Some(CallUpload::DEFAULT_AUDIO),
        "the audio the recorder sent, read back out of the bucket"
    );
    assert_eq!(
        app.object_keys().await,
        vec![call.object_key.clone()],
        "the row's key is the only object in the bucket"
    );
}

/// The presigned redirect, **followed**.
///
/// `tests/serve_audio.rs` proves the app answers `307` with a signed-looking
/// `Location`; only a store that answers can prove that URL is one a browser
/// gets audio from. A signature the store rejects looks identical from here
/// until something fetches it — and what fetches it in production is an iOS
/// `<audio>` element, which reports the failure as silence.
///
/// So this asserts on the bytes at the other end, and then on a **ranged** fetch
/// of the same URL: with the S3 backend the range request never reaches
/// Radio-Scout at all, so the app's own range code (ADR-0002) is not what keeps
/// iOS playing — the presigned URL has to.
#[tokio::test]
async fn the_presigned_redirect_is_followable_to_the_real_audio() {
    let Some(store) = common::s3::test_bucket().await else {
        return;
    };
    let audio = Bytes::from_static(b"RIFF....WAVEfmt 0123456789");
    let app = TestApp::builder().store(store).spawn().await;
    app.put_object("ab/deadbeef.m4a", &audio).await;
    let id = app
        .seed_call(NewCall {
            system_ref: 11,
            talkgroup_ref: 54241,
            call_at_ms: 1000,
            object_key: "ab/deadbeef.m4a".to_string(),
            audio_mime: Some("audio/mp4".to_string()),
            ..Default::default()
        })
        .await;

    let redirect = app
        .get_without_redirects(&format!("/api/call/{id}/audio"))
        .await;
    assert_eq!(redirect.status(), 307);
    let location = header_of(&redirect, "location")
        .expect("a Location header")
        .to_string();

    // The harness's own client, exactly as every other request in the suite is
    // made — the URL just happens to point at the store rather than at the app.
    let direct = app
        .client()
        .get(&location)
        .send()
        .await
        .expect("follow the presigned URL");
    assert_eq!(direct.status(), 200, "the store honoured the signature");
    assert_eq!(direct.bytes().await.unwrap(), audio);

    let ranged = app
        .client()
        .get(&location)
        .header(reqwest::header::RANGE, "bytes=16-25")
        .send()
        .await
        .expect("range-request the presigned URL");
    assert_eq!(ranged.status(), 206, "a signed URL is still range-servable");
    assert_eq!(
        header_of(&ranged, "content-range"),
        Some("bytes 16-25/26"),
        "the store's own range accounting"
    );
    assert_eq!(
        ranged.bytes().await.unwrap(),
        Bytes::from_static(b"0123456789")
    );

    // ...and the *reused* signature is one the store still accepts (#31). This
    // is what makes prefetch worth anything here: the second request hands back
    // the URL the first one minted — byte-identical, so the client's cache hits
    // — and a store has to honour it, which nothing offline can prove. A cache
    // that quietly served a signature the store rejects would look exactly like
    // this until something fetched it.
    // A second apart: SigV4 stamps `X-Amz-Date` to the second, so signing twice
    // inside one second would match whether or not anything was cached.
    tokio::time::sleep(std::time::Duration::from_millis(1_100)).await;
    let again = app
        .get_without_redirects(&format!("/api/call/{id}/audio"))
        .await;
    assert_eq!(
        header_of(&again, "location"),
        Some(location.as_str()),
        "the same signature, so the element's request is a cache hit"
    );
    let reused = app
        .client()
        .get(&location)
        .send()
        .await
        .expect("follow the reused presigned URL");
    assert_eq!(
        reused.status(),
        200,
        "the store honoured the reused signature"
    );
    assert_eq!(
        header_of(&reused, "cache-control"),
        Some("private, max-age=604800, immutable"),
        "the store's own response carries the caching promise"
    );
    assert_eq!(reused.bytes().await.unwrap(), audio);
}

/// The other half of making prefetch pay off on S3 (#31): the **object** carries
/// the caching promise, not just the redirect to it.
///
/// A stable signed URL is necessary and not sufficient. With a presigned
/// redirect the store answers the client directly, so the `Cache-Control`
/// `serve_audio` puts on its own responses is never seen — and a browser given
/// no freshness information falls back to a heuristic, which for an object
/// written moments ago is zero. The element would then revalidate every
/// prefetched Call rather than playing it from cache, and all the stable URL
/// would have bought is a 304 instead of silence.
///
/// Only a real store can show this: the attribute is set on the way in and read
/// back off the store's own response, and neither end of that exists offline.
#[tokio::test]
async fn a_stored_object_carries_the_cache_control_a_prefetch_needs() {
    let Some(store) = common::s3::test_bucket().await else {
        return;
    };
    let app = TestApp::builder().store(store).spawn().await;
    app.create_api_key("k").await;
    app.upload_ok(CallUpload::new().audio_named(b"RIFF....WAVE", "a.wav", "audio/x-wav"))
        .await;
    let call = app.the_call().await;

    let redirect = app
        .get_without_redirects(&format!("/api/call/{}/audio", call.id))
        .await;
    let location = header_of(&redirect, "location").expect("a Location header");

    let direct = app
        .client()
        .get(location)
        .send()
        .await
        .expect("follow the presigned URL");

    assert_eq!(direct.status(), 200);
    assert_eq!(
        header_of(&direct, "cache-control"),
        Some("private, max-age=604800, immutable"),
        "an ingested Call's object is as cacheable as the proxied path says it is"
    );
}
