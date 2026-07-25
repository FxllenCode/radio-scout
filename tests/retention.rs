//! Retention integration tests (ticket #10, spec US 41): age-based pruning, the
//! optional size cap, and orphan-GC — driven over the real ingest endpoint and
//! the real blob store, so the assertions are on what an operator actually sees
//! (rows gone, audio objects gone, playback 404s).

use radio_scout::blob::BlobStore;
use radio_scout::db::entities::{call, call_frequency, call_patch, call_unit};
use radio_scout::db::repo;
use radio_scout::now_ms;
use radio_scout::retention::{self, RetentionConfig};
use rstest::rstest;
use sea_orm::{DatabaseConnection, EntityTrait, PaginatorTrait, QueryOrder};

mod common;
use common::spawn_with_store;

/// A round wall-clock instant the age/size tests treat as "now" — they compare
/// against `call_at_ms`, which the test controls, so a fixed instant keeps them
/// deterministic. The orphan-GC tests can't: they compare against object write
/// times, which the store stamps from the real system clock, so those pass
/// [`now_ms`] instead.
const NOW: i64 = 1_700_000_000_000;
const DAY: i64 = 86_400_000;

/// Upload one call of `size` bytes for `talkgroup` at `call_at_ms`.
async fn upload(addr: &str, talkgroup: i64, call_at_ms: i64, size: usize) {
    let audio = reqwest::multipart::Part::bytes(vec![1u8; size])
        .file_name("call.wav")
        .mime_str("audio/x-wav")
        .unwrap();
    let form = reqwest::multipart::Form::new()
        .text("key", "k")
        .text("system", "11")
        .text("talkgroup", talkgroup.to_string())
        .text("timestamp", call_at_ms.to_string())
        .part("audio", audio);
    let resp = reqwest::Client::new()
        .post(format!("http://{addr}/api/call-upload"))
        .multipart(form)
        .send()
        .await
        .expect("upload");
    assert_eq!(resp.status().as_u16(), 200);
}

/// Every stored call, oldest first.
async fn calls(db: &DatabaseConnection) -> Vec<call::Model> {
    call::Entity::find()
        .order_by_asc(call::Column::CallAtMs)
        .all(db)
        .await
        .unwrap()
}

/// Whether an object is still in the store.
async fn stored(store: &BlobStore, key: &str) -> bool {
    store.size(key).await.unwrap().is_some()
}

#[tokio::test]
async fn sweep_prunes_calls_older_than_the_retention_window() {
    let (addr, db, store, _tmp) = spawn_with_store().await;
    repo::create_api_key(&db, "k", None, None, 0).await.unwrap();

    // Two calls well past a 7-day window, one comfortably inside it.
    upload(&addr, 100, NOW - 30 * DAY, 8).await;
    upload(&addr, 200, NOW - 10 * DAY, 8).await;
    upload(&addr, 300, NOW - DAY, 8).await;

    let before = calls(&db).await;
    assert_eq!(before.len(), 3);
    let aged_out_keys: Vec<String> = before[..2].iter().map(|c| c.object_key.clone()).collect();
    let kept = before[2].clone();

    let config = RetentionConfig {
        days: 7,
        max_size_bytes: None,
        ..Default::default()
    };
    let report = retention::sweep(&db, &store, &config, NOW).await.unwrap();

    assert_eq!(report.aged_out, 2);
    assert_eq!(report.bytes_freed, 16);

    // The metadata rows went with the audio — not one without the other.
    let remaining = calls(&db).await;
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].id, kept.id);
    for key in &aged_out_keys {
        assert!(!stored(&store, key).await, "{key} should be deleted");
    }
    assert!(stored(&store, &kept.object_key).await, "kept call's audio");
}

#[tokio::test]
async fn pruned_calls_stop_serving_audio() {
    let (addr, db, store, _tmp) = spawn_with_store().await;
    repo::create_api_key(&db, "k", None, None, 0).await.unwrap();
    upload(&addr, 100, NOW - 30 * DAY, 8).await;

    let id = calls(&db).await[0].id;
    let url = format!("http://{addr}/api/call/{id}/audio");
    assert_eq!(
        reqwest::get(&url).await.unwrap().status().as_u16(),
        200,
        "audio serves before the sweep"
    );

    retention::sweep(&db, &store, &RetentionConfig::default(), NOW)
        .await
        .unwrap();

    assert_eq!(reqwest::get(&url).await.unwrap().status().as_u16(), 404);
}

/// The cap rdio-scanner doesn't have: a busy system can blow past the disk
/// between age-prunes, so the oldest Calls go until the archive fits.
#[tokio::test]
async fn size_cap_drops_the_oldest_until_the_archive_fits() {
    let (addr, db, store, _tmp) = spawn_with_store().await;
    repo::create_api_key(&db, "k", None, None, 0).await.unwrap();

    // Five 1000-byte calls, all recent — 5000 bytes total.
    for i in 0..5 {
        upload(&addr, 100 + i, NOW - (5 - i) * 60_000, 1000).await;
    }
    let before = calls(&db).await;
    assert_eq!(before.len(), 5);

    // Age pruning off, so only the cap is under test. 5000 > 2500, and it takes
    // three of the oldest to get to 2000.
    let config = RetentionConfig {
        days: 0,
        max_size_bytes: Some(2500),
        ..Default::default()
    };
    let report = retention::sweep(&db, &store, &config, NOW).await.unwrap();

    assert_eq!(report.over_cap, 3);
    assert_eq!(report.aged_out, 0);
    assert_eq!(report.bytes_freed, 3000);

    // The two newest survive, with their audio.
    let remaining = calls(&db).await;
    let kept: Vec<i64> = remaining.iter().map(|c| c.id).collect();
    assert_eq!(kept, vec![before[3].id, before[4].id]);
    for gone in &before[..3] {
        assert!(!stored(&store, &gone.object_key).await);
    }
    for keep in &before[3..] {
        assert!(stored(&store, &keep.object_key).await);
    }
    assert!(repo::total_audio_bytes(&db).await.unwrap() <= 2500);
}

/// An archive already inside the cap is left completely alone — including the
/// boundary. `max_size_gb` is a *limit*, so sitting exactly on it is legal; a
/// cap that pruned at equality would quietly cost the operator one call per
/// sweep forever.
#[rstest]
#[case(1000, 4096, "well under the cap")]
#[case(1000, 1000, "exactly at the cap")]
#[tokio::test]
async fn size_cap_leaves_an_archive_that_already_fits(
    #[case] size: usize,
    #[case] cap: u64,
    #[case] context: &str,
) {
    let (addr, db, store, _tmp) = spawn_with_store().await;
    repo::create_api_key(&db, "k", None, None, 0).await.unwrap();
    upload(&addr, 100, NOW - 60_000, size).await;

    let config = RetentionConfig {
        days: 0,
        max_size_bytes: Some(cap),
        ..Default::default()
    };
    let report = retention::sweep(&db, &store, &config, NOW).await.unwrap();

    assert!(report.is_noop(), "{context}: {report:?}");
    assert_eq!(
        call::Entity::find().count(&db).await.unwrap(),
        1,
        "{context}"
    );
}

/// Pruning pages through the archive instead of issuing one unbounded DELETE
/// (rdio's approach), so a big sweep never holds a long write lock. More calls
/// than fit in a batch must still all go.
#[tokio::test]
async fn pruning_pages_through_batches_until_the_archive_is_within_policy() {
    let (addr, db, store, _tmp) = spawn_with_store().await;
    repo::create_api_key(&db, "k", None, None, 0).await.unwrap();
    for i in 0..5 {
        upload(&addr, 100 + i, NOW - (30 - i) * DAY, 8).await;
    }

    let config = RetentionConfig {
        days: 7,
        max_size_bytes: None,
        batch_size: 2, // three passes for five calls
        ..Default::default()
    };
    let report = retention::sweep(&db, &store, &config, NOW).await.unwrap();

    assert_eq!(report.aged_out, 5);
    assert_eq!(call::Entity::find().count(&db).await.unwrap(), 0);
    assert!(store.list_keys().await.unwrap().is_empty());
}

/// Child rows (frequencies / units / patches) go with their Call — the schema's
/// foreign keys are RESTRICT, so a prune that forgot them would fail outright.
#[tokio::test]
async fn pruning_takes_a_calls_child_rows_with_it() {
    let (addr, db, store, _tmp) = spawn_with_store().await;
    repo::create_api_key(&db, "k", None, None, 0).await.unwrap();

    let audio = reqwest::multipart::Part::bytes(vec![1u8; 8])
        .file_name("call.wav")
        .mime_str("audio/x-wav")
        .unwrap();
    let form = reqwest::multipart::Form::new()
        .text("key", "k")
        .text("system", "11")
        .text("talkgroup", "54241")
        .text("timestamp", (NOW - 30 * DAY).to_string())
        .text(
            "frequencies",
            r#"[{"freq":774031250,"pos":0,"len":1.5,"dbm":-50}]"#,
        )
        .text("sources", r#"[{"src":4424000,"pos":0,"tag":"Engine 1"}]"#)
        .text("patches", "[100, 200]")
        .part("audio", audio);
    let status = reqwest::Client::new()
        .post(format!("http://{addr}/api/call-upload"))
        .multipart(form)
        .send()
        .await
        .unwrap()
        .status()
        .as_u16();
    assert_eq!(status, 200);
    assert_eq!(call_frequency::Entity::find().count(&db).await.unwrap(), 1);

    let report = retention::sweep(&db, &store, &RetentionConfig::default(), NOW)
        .await
        .unwrap();

    assert_eq!(report.aged_out, 1);
    assert_eq!(call::Entity::find().count(&db).await.unwrap(), 0);
    assert_eq!(call_frequency::Entity::find().count(&db).await.unwrap(), 0);
    assert_eq!(call_unit::Entity::find().count(&db).await.unwrap(), 0);
    assert_eq!(call_patch::Entity::find().count(&db).await.unwrap(), 0);
}

/// The residue of an ingest that stored its audio and then failed to insert the
/// row (ADR-0002's write-object-then-row ordering makes that the failure mode).
/// Nothing points at it, so nothing can ever play it — the sweep reclaims it.
#[tokio::test]
async fn orphan_gc_reclaims_audio_no_call_row_points_at() {
    let (addr, db, store, _tmp) = spawn_with_store().await;
    repo::create_api_key(&db, "k", None, None, 0).await.unwrap();
    upload(&addr, 100, now_ms(), 8).await;
    let live = calls(&db).await[0].clone();

    store
        .put("zz/orphan.wav", bytes::Bytes::from_static(b"0123456789"))
        .await
        .unwrap();

    // No age or size pruning; grace already elapsed for everything written.
    let config = RetentionConfig {
        days: 0,
        max_size_bytes: None,
        orphan_grace: std::time::Duration::ZERO,
        ..Default::default()
    };
    let report = retention::sweep(&db, &store, &config, now_ms() + 60_000)
        .await
        .unwrap();

    assert_eq!(report.orphans, 1);
    assert_eq!(report.bytes_freed, 10);
    assert!(!stored(&store, "zz/orphan.wav").await);

    // The live Call is untouched — it has a row, so it is not an orphan.
    assert!(stored(&store, &live.object_key).await);
    assert_eq!(call::Entity::find().count(&db).await.unwrap(), 1);
}

/// One object the store refuses to delete must not stop the sweep. The Call's
/// **row** is gone either way, so it no longer shows up or plays; the stranded
/// audio is now an orphan and a later sweep retries it. Anything else would let a
/// single bad object wedge retention — and the disk — forever.
///
/// Unix-only: the failure is induced by making the shard directory read-only,
/// which the kernel ignores for root, so the test opts out there.
#[cfg(unix)]
#[tokio::test]
async fn an_undeletable_object_is_counted_and_the_sweep_carries_on() {
    use std::os::unix::fs::PermissionsExt;

    let (addr, db, store, tmp) = spawn_with_store().await;
    repo::create_api_key(&db, "k", None, None, 0).await.unwrap();
    upload(&addr, 100, now_ms() - 30 * DAY, 8).await;

    // Both objects live in the aged-out Call's shard directory: one is its audio
    // (hit by the prune), one is an orphan (hit by the GC pass).
    let object_key = calls(&db).await[0].object_key.clone();
    let shard = object_key.split('/').next().unwrap().to_string();
    store
        .put(
            &format!("{shard}/orphan.wav"),
            bytes::Bytes::from_static(b"x"),
        )
        .await
        .unwrap();

    let shard_dir = tmp.path().join("audio").join(&shard);
    let restore = std::fs::metadata(&shard_dir).unwrap().permissions();
    let mut readonly = restore.clone();
    readonly.set_mode(0o555);
    std::fs::set_permissions(&shard_dir, readonly).unwrap();

    // Root bypasses directory permissions, so there'd be nothing to observe.
    if std::fs::write(shard_dir.join("probe"), b"x").is_ok() {
        std::fs::set_permissions(&shard_dir, restore).unwrap();
        return;
    }

    let config = RetentionConfig {
        days: 7,
        max_size_bytes: None,
        orphan_grace: std::time::Duration::ZERO,
        ..Default::default()
    };
    let report = retention::sweep(&db, &store, &config, now_ms() + 60_000).await;
    std::fs::set_permissions(&shard_dir, restore).unwrap();
    let report = report.expect("an undeletable object must not fail the sweep");

    // The Call is pruned as far as any listener is concerned...
    assert_eq!(report.aged_out, 1);
    assert_eq!(call::Entity::find().count(&db).await.unwrap(), 0);
    // ...but no bytes came back, and every stranded object was counted: the
    // Call's own audio during the prune, then it and the orphan during the GC.
    assert_eq!(report.bytes_freed, 0);
    assert_eq!(report.orphans, 0);
    assert_eq!(report.object_errors, 3);
    assert!(stored(&store, &object_key).await);
}

/// Audio ingest wrote moments ago but hasn't inserted a row for yet looks
/// exactly like an orphan. The grace period is what stops the GC from deleting a
/// live Call's audio out from under it.
#[tokio::test]
async fn orphan_gc_spares_audio_written_within_the_grace_period() {
    let (_addr, db, store, _tmp) = spawn_with_store().await;
    store
        .put("zz/mid-ingest.wav", bytes::Bytes::from_static(b"x"))
        .await
        .unwrap();

    let config = RetentionConfig {
        days: 0,
        max_size_bytes: None,
        orphan_grace: std::time::Duration::from_secs(3600),
        ..Default::default()
    };
    let report = retention::sweep(&db, &store, &config, now_ms())
        .await
        .unwrap();

    assert!(report.is_noop(), "{report:?}");
    assert!(stored(&store, "zz/mid-ingest.wav").await);
}

#[tokio::test]
async fn retention_days_zero_keeps_everything() {
    let (addr, db, store, _tmp) = spawn_with_store().await;
    repo::create_api_key(&db, "k", None, None, 0).await.unwrap();
    upload(&addr, 100, NOW - 3650 * DAY, 8).await;

    let config = RetentionConfig {
        days: 0,
        max_size_bytes: None,
        ..Default::default()
    };
    let report = retention::sweep(&db, &store, &config, NOW).await.unwrap();

    assert_eq!(report.aged_out, 0);
    assert_eq!(call::Entity::find().count(&db).await.unwrap(), 1);
}
