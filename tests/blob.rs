//! Blob-store tests (ticket #4): the filesystem backend, orphan-GC, and S3
//! presigning. The S3 test signs offline (SigV4 is computed locally, no
//! network), so it runs everywhere — and proves nothing about a round trip.
//! That half is `tests/s3.rs` (#35), which needs a store that answers and skips
//! when the run was not given one.

mod common;

use std::collections::HashSet;

use bytes::Bytes;
use common::s3::{FlakyStore, unreachable_store};
use radio_scout::blob::{BlobStore, S3Config, orphan_gc};
use radio_scout::now_ms;

/// A cutoff an hour in the past: everything written by these tests is newer, so
/// nothing has aged out of the grace period yet.
fn within_grace() -> i64 {
    now_ms() - 3_600_000
}

/// A cutoff an hour in the future: everything written by these tests is older,
/// so the grace period no longer protects anything.
fn past_grace() -> i64 {
    now_ms() + 3_600_000
}

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
    let outcome = orphan_gc(&store, &referenced, past_grace()).await.unwrap();

    let mut reclaimed: Vec<String> = outcome.reclaimed.iter().map(|o| o.key.clone()).collect();
    reclaimed.sort();
    assert_eq!(reclaimed, vec!["bb/2.wav".to_string()]);
    assert_eq!(outcome.bytes(), 1);
    assert_eq!(outcome.errors, 0);

    let mut remaining = store.list_keys().await.unwrap();
    remaining.sort();
    assert_eq!(
        remaining,
        vec!["aa/1.wav".to_string(), "cc/3.wav".to_string()]
    );
}

/// The race orphan-GC has to survive: ingest writes the audio object *before*
/// inserting its row (ADR-0002), so for that window the object legitimately has
/// no row. Reclaiming it would delete a live Call's audio, so anything written
/// inside the grace period is left alone.
#[tokio::test]
async fn orphan_gc_spares_objects_still_inside_the_write_grace_period() {
    let (store, _dir) = fs_store().await;
    store
        .put("aa/mid-ingest.wav", Bytes::from_static(b"x"))
        .await
        .unwrap();

    let outcome = orphan_gc(&store, &HashSet::new(), within_grace())
        .await
        .unwrap();

    assert!(outcome.reclaimed.is_empty(), "{outcome:?}");
    assert_eq!(store.list_keys().await.unwrap(), vec!["aa/mid-ingest.wav"]);

    // Once it ages past the grace period it is reclaimed.
    let outcome = orphan_gc(&store, &HashSet::new(), past_grace())
        .await
        .unwrap();
    assert_eq!(outcome.reclaimed.len(), 1);
    assert!(store.list_keys().await.unwrap().is_empty());
}

#[tokio::test]
async fn list_objects_reports_size_and_write_time() {
    let (store, _dir) = fs_store().await;
    store
        .put("aa/1.wav", Bytes::from_static(b"0123456789"))
        .await
        .unwrap();

    let objects = store.list_objects().await.unwrap();
    assert_eq!(objects.len(), 1);
    assert_eq!(objects[0].key, "aa/1.wav");
    assert_eq!(objects[0].size, 10);
    // Written during this test, so it sits between the two bounds.
    assert!(objects[0].last_modified_ms > within_grace());
    assert!(objects[0].last_modified_ms < past_grace());
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

    let signed = store
        .presigned_get_url("ab/call.m4a")
        .await
        .expect("s3 presigns")
        .expect("signed url");
    let url = signed.url;
    assert!(url.contains("radio-scout"), "url names the bucket: {url}");
    assert!(url.contains("ab/call.m4a"), "url names the key: {url}");
    assert!(url.contains("X-Amz-Signature"), "url is signed: {url}");
    // ...and it says how long a redirect to it may be cached (#31): the
    // signature's 300s life, less the margin never given away.
    assert_eq!(signed.max_age_secs, 240);
}

/// **What a write asks a store to remember about an object** (#31).
///
/// With a presigned redirect the store answers the listener directly, so the
/// `Cache-Control` `serve_audio` puts on its own responses is never seen — and a
/// browser given no freshness information revalidates, which is what the whole
/// prefetch is trying to avoid. The header therefore has to be stored *on the
/// object*, at write time.
///
/// `tests/s3.rs` reads it back off a store that answers, which is the real proof
/// and the one that skips wherever no store is running — i.e. the everyday loop
/// and every contributor without Docker. This is the half that runs everywhere:
/// the write is intercepted, so what it asked for is observable without anything
/// to ask. The put is armed to fail precisely because there is no store behind
/// this endpoint to talk to; the recording happens first.
#[tokio::test]
async fn an_s3_write_asks_the_store_to_store_the_cache_control() {
    let (store, faults) = common::Faults::wrapping(
        BlobStore::s3(&S3Config {
            bucket: "radio-scout".into(),
            region: "us-east-1".into(),
            endpoint: Some("http://localhost:3900".into()),
            access_key_id: "GKtestaccesskey".into(),
            secret_access_key: "testsecretkey".into(),
            allow_http: true,
        })
        .expect("build s3 store"),
    );
    faults.fail_puts();

    let _ = store.put("ab/call.m4a", Bytes::from_static(b"audio")).await;

    let asked = faults.puts();
    assert_eq!(asked.len(), 1, "one write reached the store");
    assert_eq!(
        asked[0]
            .attributes
            .get(&object_store::Attribute::CacheControl),
        Some(&object_store::AttributeValue::from(
            "private, max-age=604800, immutable"
        )),
        "an object a client fetches directly carries its own caching promise"
    );
}

/// ...and the filesystem backend asks for nothing, which is not an optimisation:
/// `object_store` specifies that a backend which cannot honour an attribute
/// returns an error, and `LocalFileSystem` cannot store one. It needs none —
/// nothing fetches a local object directly.
#[tokio::test]
async fn a_filesystem_write_asks_for_no_attributes() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (store, faults) =
        common::Faults::wrapping(BlobStore::filesystem(dir.path()).expect("fs store"));

    store
        .put("ab/call.wav", Bytes::from_static(b"audio"))
        .await
        .expect("a filesystem write succeeds");

    assert!(
        faults.puts()[0].attributes.is_empty(),
        "nothing a LocalFileSystem would refuse"
    );
}

/// **A store having a blip is ridden out, not given up on** (#39).
///
/// The half of the retry policy that bounding it could have broken: the point of
/// four retries rather than none is that a store which answers `503` while it
/// restarts or sheds load still hands over the object. A policy tuned only for
/// "fail fast" would turn every transient hiccup into a Call skipped and an
/// upload the recorder has to send again.
///
/// The count matters as much as the bytes: two requests for one `get` is the
/// retry, observed from the server's side rather than inferred.
#[tokio::test]
async fn a_store_that_fails_once_is_retried_and_answers() {
    let audio = Bytes::from_static(b"the second attempt's bytes");
    let flaky = FlakyStore::start(1, audio.clone()).await;

    let got = flaky
        .store()
        .get("ab/call.m4a")
        .await
        .expect("a blip must not become a failure")
        .expect("the object is there");

    assert_eq!(got, audio);
    assert_eq!(flaky.requests(), 2, "the 503 must have been retried once");
}

/// **A store that is not there surfaces as an error in seconds** (#39).
///
/// `object_store`'s shipped retry policy would sleep for up to the better part
/// of a minute first, in randomized draws — so "how long until a dead store is
/// reported" was a variable with a tail past any deadline a caller had set, and
/// on a Pi it is a worker slot held for minutes over one Call. `src/blob.rs`
/// overrides it; this is the proof the override actually reaches the client
/// rather than sitting in a constant nobody passed anywhere.
///
/// Asserted two ways, because each catches what the other cannot: the error
/// `object_store` hands back **names the policy it exhausted**, which is exact
/// and does not care how loaded the machine is — and the call really does
/// return in seconds, which is the property the ticket is about and which a
/// string cannot demonstrate.
///
/// The string half is deliberately coupled to `object_store`'s own error
/// `Display`. If a version bump reformats it this goes red, which is the right
/// outcome and not a flake: it is deterministic, and "did our policy still reach
/// the client" is exactly the question a bump should make someone re-answer.
#[tokio::test]
async fn an_unreachable_store_gives_up_within_its_retry_policy() {
    let store = unreachable_store();

    let started = std::time::Instant::now();
    let error = store
        .get("ab/call.m4a")
        .await
        .expect_err("a store nothing is listening on cannot answer");
    let elapsed = started.elapsed();

    let message = error.to_string();
    assert!(
        message.contains("max_retries: 4"),
        "the store's own words must name our retry ceiling, not object_store's 10: {message}"
    );
    assert!(
        message.contains("retry_timeout: 5s"),
        "and our timeout, not object_store's 180s: {message}"
    );
    // The policy's own ceiling is five seconds of retrying; the bound here is
    // deliberately looser than that, because a bound that a loaded CI box can
    // brush against is the flake this ticket exists to remove. It is still far
    // below the minute-plus tail of the schedule we replaced.
    assert!(
        elapsed < std::time::Duration::from_secs(10),
        "gave up only after {elapsed:?}"
    );
}
