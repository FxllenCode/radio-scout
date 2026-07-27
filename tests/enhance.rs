//! Audio enhancement over the real boundary (#20, spec US 33-34, ADR-0006 as
//! amended).
//!
//! The unit tests in `src/enhance.rs` prove the DSP — that a quiet Call and a
//! loud one come out level, that rumble is filtered, that nothing clips. What
//! they cannot prove is the part an operator actually experiences: that a
//! recorder's upload is answered *before* any of it happens, that the object a
//! Call points at is replaced afterwards, and that a Call whose audio could not
//! be enhanced still plays.
//!
//! So these drive a real app: POST to the real ingest endpoint, then assert on
//! the row, the stored object, and the bytes `GET /api/call/{id}/audio` hands
//! back.

mod common;

use common::{CallUpload, TestApp};
use radio_scout::db::entities::call::EnhancementState;
use radio_scout::enhance::{EnhancementConfig, Mode};

/// The bytes a recorder would send: two seconds of 16 kHz tone, deliberately
/// quiet, so that "it was levelled" is visible in the result.
fn quiet_wav() -> Vec<u8> {
    wav(&tone(1000.0, 16_000, 0.02), 16_000)
}

fn tone(freq: f64, rate: u32, amplitude: f64) -> Vec<f32> {
    let n = (2.0 * rate as f64) as usize;
    (0..n)
        .map(|i| {
            let t = i as f64 / rate as f64;
            (amplitude * (std::f64::consts::TAU * freq * t).sin()) as f32
        })
        .collect()
}

fn wav(samples: &[f32], rate: u32) -> Vec<u8> {
    let data: Vec<u8> = samples
        .iter()
        .flat_map(|s| ((s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16).to_le_bytes())
        .collect();
    let mut out = Vec::new();
    out.extend(b"RIFF");
    out.extend(((36 + data.len()) as u32).to_le_bytes());
    out.extend(b"WAVEfmt ");
    out.extend(16u32.to_le_bytes());
    out.extend(1u16.to_le_bytes());
    out.extend(1u16.to_le_bytes());
    out.extend(rate.to_le_bytes());
    out.extend((rate * 2).to_le_bytes());
    out.extend(2u16.to_le_bytes());
    out.extend(16u16.to_le_bytes());
    out.extend(b"data");
    out.extend((data.len() as u32).to_le_bytes());
    out.extend(data);
    out
}

/// An instance with enhancement turned on, the way an operator would.
fn normalizing() -> EnhancementConfig {
    EnhancementConfig {
        mode: Mode::Normalize,
        ..EnhancementConfig::default()
    }
}

/// An upload carrying real audio, on the key every test here registers.
fn call() -> CallUpload {
    CallUpload::new().key("k").audio(&quiet_wav())
}

/// **The whole feature, end to end.** A recorder uploads, is told the Call
/// landed, and the Call is then replaced by an enhanced version of itself —
/// under the same id, so every URL a client already holds still resolves.
///
/// The object key must *change* rather than being written over: ADR-0002's
/// ordering depends on an object never being mutated in place, and #10's
/// orphan-GC is what reclaims the original.
#[tokio::test]
async fn an_enhanced_call_replaces_its_audio_under_the_same_id() {
    let app = TestApp::builder().enhancement(normalizing()).spawn().await;
    app.create_api_key("k").await;

    app.upload_ok(call()).await;
    let original = app.the_call().await;
    let enhanced = app.await_enhancement(original.id).await;

    assert_ne!(
        enhanced.object_key, original.object_key,
        "the enhanced audio must be a new object, never a rewrite of the old one"
    );
    assert_eq!(enhanced.id, original.id, "the Call keeps its identity");
    assert!(
        app.stored(&enhanced.object_key).await,
        "the row points at an object that is not there"
    );
}

/// The listener-visible half: what `GET /api/call/{id}/audio` hands back is the
/// enhanced audio, not the bytes the recorder sent.
///
/// Asserted through the HTTP endpoint rather than the store, because that is
/// the only path a browser has — a row pointing at a correct object that the
/// endpoint does not serve would be a passing test and a silent bug.
#[tokio::test]
async fn the_audio_endpoint_serves_the_enhanced_bytes() {
    let app = TestApp::builder().enhancement(normalizing()).spawn().await;
    app.create_api_key("k").await;

    app.upload_ok(call()).await;
    let id = app.the_call().await.id;
    app.await_enhancement(id).await;

    let served = app
        .get(&format!("/api/call/{id}/audio"))
        .await
        .bytes()
        .await
        .expect("audio body");
    assert_eq!(
        sample_rate_of(&served),
        8_000,
        "the endpoint is still serving the recorder's audio"
    );
    assert_ne!(
        served.as_ref(),
        quiet_wav().as_slice(),
        "the bytes did not change at all"
    );
}

/// The sample rate out of a WAV header — bytes 24..28 of a canonical file.
fn sample_rate_of(bytes: &[u8]) -> u32 {
    u32::from_le_bytes(bytes[24..28].try_into().expect("a WAV header"))
}

/// **The property the whole design turns on.** Enhancement must not put itself
/// between a recorder and its `200`, or between a Call and the live feed.
///
/// rdio-scanner converts inline (`server/controller.go:335`), so a slow encoder
/// delays every upload behind it. Here the Call reaches a listening socket
/// while enhancement has not started — which is why a deep queue costs nothing.
#[tokio::test]
async fn a_call_reaches_the_live_feed_before_it_is_enhanced() {
    let app = TestApp::builder().enhancement(normalizing()).spawn().await;
    app.create_api_key("k").await;
    let mut ws = app.connect_ws().await;
    common::subscribe(&mut ws, r#"{"t":"sub","all":true}"#).await;

    app.upload_ok(call()).await;

    let frame = common::next_json(&mut ws).await;
    assert_eq!(frame["t"], "call", "the live feed waited for enhancement");
    // ...and the URL it was given keeps working after the object underneath it
    // is replaced, because it names the Call rather than the object.
    let url = frame["call"]["audioUrl"]
        .as_str()
        .expect("an audio url")
        .to_string();
    let id = app.the_call().await.id;
    app.await_enhancement(id).await;
    assert_eq!(
        app.get(&url).await.status(),
        200,
        "the URL stopped resolving"
    );
}

/// The row records what happened, including the length — which ingest cannot
/// know, because it has bytes rather than samples. Every Call before #20 has a
/// `NULL` here.
#[tokio::test]
async fn an_enhanced_call_records_its_state_format_and_length() {
    let app = TestApp::builder().enhancement(normalizing()).spawn().await;
    app.create_api_key("k").await;

    app.upload_ok(call()).await;
    let enhanced = app.await_enhancement(app.the_call().await.id).await;

    assert_eq!(enhanced.enhancement, "done");
    assert_eq!(enhanced.audio_mime.as_deref(), Some("audio/wav"));
    assert!(
        enhanced.object_key.ends_with(".wav"),
        "{}",
        enhanced.object_key
    );
    let duration = enhanced.duration_ms.expect("a measured duration");
    assert!(
        (duration - 2_000).abs() < 100,
        "{duration}ms for a two-second Call"
    );
    assert_eq!(
        enhanced.audio_size,
        Some(
            app.object_bytes(&enhanced.object_key)
                .await
                .expect("stored")
                .len() as i64
        ),
        "the size on the row must be the size of the object retention will sum"
    );
}

/// What ships. Enhancement is off, so a Call is stored exactly as it arrived
/// and nothing marks it otherwise — the state every Call that predates #20 has.
#[tokio::test]
async fn without_enhancement_a_call_keeps_the_audio_it_arrived_with() {
    let app = TestApp::with_key("k").await;

    app.upload_ok(call()).await;

    let stored = app.the_call().await;
    assert_eq!(stored.enhancement, "none");
    assert_eq!(
        app.object_bytes(&stored.object_key).await.as_deref(),
        Some(quiet_wav().as_slice()),
        "passthrough must be byte-for-byte what the recorder sent"
    );
}

/// **A Call queued for enhancement must not be cached as immutable.**
///
/// `Cache-Control: immutable` is a promise that the bytes behind a URL will
/// never change, and for a passthrough Call it is true. For a Call waiting to
/// be enhanced it is precisely false — the object is about to be replaced — and
/// a client that cached it during the window would keep the un-levelled version
/// for a week.
#[tokio::test]
async fn a_call_awaiting_enhancement_is_not_cached_as_immutable() {
    let app = TestApp::with_key("k").await;
    app.upload_ok(call()).await;
    let id = app.the_call().await.id;
    radio_scout::db::repo::mark_enhancement(&app.db, id, EnhancementState::PENDING)
        .await
        .expect("mark pending");

    let response = app.get(&format!("/api/call/{id}/audio")).await;

    let cache_control = common::header_of(&response, "cache-control").expect("a cache-control");
    assert!(
        !cache_control.contains("immutable"),
        "promised immutability for audio about to be replaced: {cache_control}"
    );
    assert_eq!(response.status(), 200, "it must still play meanwhile");
}

/// ...while a Call nothing is going to touch keeps the long immutable cache the
/// client's prefetch depends on (#14).
#[tokio::test]
async fn a_settled_call_is_still_cached_immutably() {
    let app = TestApp::builder().enhancement(normalizing()).spawn().await;
    app.create_api_key("k").await;
    app.upload_ok(call()).await;
    let id = app.the_call().await.id;
    app.await_enhancement(id).await;

    let response = app.get(&format!("/api/call/{id}/audio")).await;

    let cache_control = common::header_of(&response, "cache-control").expect("a cache-control");
    assert!(cache_control.contains("immutable"), "{cache_control}");
}

/// A recorder mid-crash sends something that is not audio. The Call is already
/// stored and already on the live feed, so the only right answer is to leave it
/// exactly as it is — playable, with whatever the recorder sent — and record
/// that enhancement was skipped (ADR-0011 rule 3).
#[tokio::test]
async fn a_call_that_cannot_be_enhanced_keeps_working() {
    let app = TestApp::builder().enhancement(normalizing()).spawn().await;
    app.create_api_key("k").await;
    let not_audio = b"<!doctype html><title>502 Bad Gateway</title>".to_vec();

    app.upload_ok(CallUpload::new().key("k").audio(&not_audio))
        .await;
    let settled = app.await_enhancement(app.the_call().await.id).await;

    assert_eq!(settled.enhancement, "skipped");
    assert_eq!(
        app.object_bytes(&settled.object_key).await,
        Some(not_audio),
        "a Call that could not be enhanced must keep the audio it arrived with"
    );
    assert_eq!(
        app.get(&format!("/api/call/{}/audio", settled.id))
            .await
            .status(),
        200,
        "and must still be servable"
    );
}

/// Scope, over the wire: a System an operator has switched enhancement off for
/// keeps its passthrough audio while the instance enhances everything else.
/// This is the knob US 34 exists for — one chatty System must not eat a Pi.
#[tokio::test]
async fn a_system_can_opt_out_while_the_instance_enhances() {
    let app = TestApp::builder().enhancement(normalizing()).spawn().await;
    app.create_api_key("k").await;
    app.seed_system(11, false, None).await;
    set_system_enhancement(&app, 11, Some(false)).await;

    app.upload_ok(call().system(11)).await;
    app.upload_ok(call().system(22).talkgroup(999).at(1)).await;

    let calls = app.calls().await;
    let mut opted_out = None;
    for stored in &calls {
        if app.system_of(stored).await.r#ref == 11 {
            opted_out = Some(stored.id);
        }
    }
    let settled = app
        .await_enhancement(opted_out.expect("the opted-out System's Call"))
        .await;
    assert_eq!(
        settled.enhancement, "none",
        "an opted-out System must not be queued at all"
    );
    assert_eq!(
        app.object_bytes(&settled.object_key).await.as_deref(),
        Some(quiet_wav().as_slice())
    );
}

/// Set a System's enhancement column, the way an admin surface eventually will.
async fn set_system_enhancement(app: &TestApp, system_ref: i64, enhancement: Option<bool>) {
    use radio_scout::db::entities::system;
    use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
    system::Entity::update_many()
        .col_expr(
            system::Column::Enhancement,
            sea_orm::sea_query::Expr::value(enhancement),
        )
        .filter(system::Column::Ref.eq(system_ref))
        .exec(&app.db)
        .await
        .expect("set system enhancement");
}

/// **A restart must not lose a Call mid-enhancement.**
///
/// The queue is in memory, so a process that stops with work in it forgets that
/// work. What survives is the row: a Call marked `pending` is one the worker
/// was going to reach, and the next boot picks it up. Without this, a restart
/// during a busy stretch would leave a scattering of un-levelled Calls with
/// nothing anywhere to fix them.
#[tokio::test]
async fn a_restart_resumes_the_calls_it_was_part_way_through() {
    let shared = tempfile::tempdir().expect("tempdir");
    let url = shared_database_url(&shared).await;
    let store =
        || radio_scout::BlobStore::filesystem(shared.path().join("audio")).expect("shared store");

    // The instance that took the Call in — enhancement off, so nothing runs and
    // the Call is left exactly as a killed worker would have left it.
    let before = TestApp::builder()
        .database_url(url.clone())
        .store(store())
        .spawn()
        .await;
    before.create_api_key("k").await;
    before.upload_ok(call()).await;
    let id = before.the_call().await.id;
    radio_scout::db::repo::mark_enhancement(&before.db, id, EnhancementState::PENDING)
        .await
        .expect("leave it mid-flight");

    // ...and the instance that comes up afterwards, onto the same database and
    // the same audio.
    let after = TestApp::builder()
        .database_url(url)
        .store(store())
        .enhancement(normalizing())
        .spawn()
        .await;

    let resumed = after.await_enhancement(id).await;
    assert_eq!(resumed.enhancement, "done", "the restart abandoned it");
}

/// ...but a boot must **never** sweep up Calls that were ingested while
/// enhancement was off. Those are marked `none`, and re-encoding them is a
/// deliberate act an operator asks for — not something that happens to their
/// archive because they restarted.
#[tokio::test]
async fn a_restart_leaves_the_existing_archive_alone() {
    let shared = tempfile::tempdir().expect("tempdir");
    let url = shared_database_url(&shared).await;
    let store =
        || radio_scout::BlobStore::filesystem(shared.path().join("audio")).expect("shared store");

    let before = TestApp::builder()
        .database_url(url.clone())
        .store(store())
        .spawn()
        .await;
    before.create_api_key("k").await;
    before.upload_ok(call()).await;
    let id = before.the_call().await.id;

    let after = TestApp::builder()
        .database_url(url)
        .store(store())
        .enhancement(normalizing())
        .spawn()
        .await;

    // Long enough that a catch-up sweep would have finished if it were going to
    // touch this Call; the assertion is that nothing happened at all.
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    let untouched = call_row(&after, id).await;
    assert_eq!(untouched.enhancement, "none");
    assert_eq!(
        after.object_bytes(&untouched.object_key).await.as_deref(),
        Some(quiet_wav().as_slice()),
        "an existing archive was rewritten by a restart"
    );
}

async fn call_row(app: &TestApp, id: i64) -> radio_scout::db::entities::call::Model {
    use sea_orm::EntityTrait;
    radio_scout::db::entities::call::Entity::find_by_id(id)
        .one(&app.db)
        .await
        .expect("read call")
        .expect("the Call exists")
}

/// A database two apps in one test can share — Postgres when the run was given
/// one, SQLite otherwise, so this works in both dialects (#22).
async fn shared_database_url(dir: &tempfile::TempDir) -> String {
    match common::postgres_server() {
        Some(server) => common::create_test_database(&server).await,
        None => format!(
            "sqlite://{}?mode=rwc",
            dir.path().join("shared.db").display()
        ),
    }
}

/// Retention is entitled to prune a Call between it being queued and the worker
/// reaching it — the archive is bounded and the queue is deep, so on a busy
/// instance this is ordinary, not exotic. The worker must not treat a missing
/// object as a crash.
#[tokio::test]
async fn a_call_whose_audio_vanished_is_skipped_rather_than_retried_forever() {
    let shared = tempfile::tempdir().expect("tempdir");
    let url = shared_database_url(&shared).await;
    let store =
        || radio_scout::BlobStore::filesystem(shared.path().join("audio")).expect("shared store");

    let before = TestApp::builder()
        .database_url(url.clone())
        .store(store())
        .spawn()
        .await;
    before.create_api_key("k").await;
    before.upload_ok(call()).await;
    let stored = before.the_call().await;
    // The Call is queued, and its audio is then gone — the order a prune
    // interrupted between the row and the object leaves things in.
    radio_scout::db::repo::mark_enhancement(&before.db, stored.id, EnhancementState::PENDING)
        .await
        .expect("queue it");
    before
        .store
        .delete(&stored.object_key)
        .await
        .expect("prune");

    let after = TestApp::builder()
        .database_url(url)
        .store(store())
        .enhancement(normalizing())
        .spawn()
        .await;

    let settled = after.await_enhancement(stored.id).await;
    assert_eq!(
        settled.enhancement, "skipped",
        "a Call with no audio must settle, or every boot re-queues it forever"
    );
}

/// Boot must survive a database the catch-up sweep cannot read — a half-applied
/// migration, or a hand-edited archive. Enhancement is a background convenience
/// and must never be the reason a scanner refuses to come up; the sweep says
/// what happened and the process serves anyway.
#[tokio::test]
async fn a_database_the_sweep_cannot_read_does_not_stop_the_boot() {
    let shared = tempfile::tempdir().expect("tempdir");
    let url = shared_database_url(&shared).await;
    let store =
        || radio_scout::BlobStore::filesystem(shared.path().join("audio")).expect("shared store");

    let before = TestApp::builder()
        .database_url(url.clone())
        .store(store())
        .spawn()
        .await;
    before.break_table("calls").await;

    let after = TestApp::builder()
        .database_url(url)
        .store(store())
        .enhancement(normalizing())
        .spawn()
        .await;

    assert_eq!(
        after.get("/healthz").await.status(),
        200,
        "a sweep that could not run took the scanner down with it"
    );
}

/// The ingest side's own failure path. Deciding whether to enhance needs the
/// System and Talkgroup rows; with them gone the decision cannot be made — and
/// the Call, which is already stored and already answered, must simply not be
/// enhanced. Ingest itself must not start failing over it.
#[tokio::test]
async fn an_undecidable_scope_leaves_the_call_alone_without_failing_ingest() {
    let app = TestApp::builder().enhancement(normalizing()).spawn().await;
    app.create_api_key("k").await;
    app.upload_ok(call()).await;
    app.await_enhancement(app.the_call().await.id).await;

    // A Call id nothing knows about: the scope lookup finds no row, which is
    // the same shape as a Call pruned between insert and decision.
    let scope = radio_scout::db::repo::enhancement_scope(&app.db, 999_999)
        .await
        .expect("a missing Call is not an error");

    assert_eq!(scope, None, "a Call that is not there has no scope");
}

/// An object store that cannot be reached — an S3 endpoint that is down, a
/// Garage node being restarted. The worker must give up on that Call, say so,
/// and keep serving the queue; a background task that unwinds takes every
/// later Call's enhancement with it.
///
/// Reachable deterministically because a Call queued at boot is reached before
/// anything else can race it: the store is dead from the start, so the very
/// first thing the worker does with it fails.
#[tokio::test]
async fn a_store_that_cannot_be_read_skips_the_call_and_keeps_going() {
    let shared = tempfile::tempdir().expect("tempdir");
    let url = shared_database_url(&shared).await;

    let before = TestApp::builder()
        .database_url(url.clone())
        .store(radio_scout::BlobStore::filesystem(shared.path().join("audio")).expect("store"))
        .spawn()
        .await;
    before.create_api_key("k").await;
    before.upload_ok(call()).await;
    let id = before.the_call().await.id;
    radio_scout::db::repo::mark_enhancement(&before.db, id, EnhancementState::PENDING)
        .await
        .expect("queue it");

    // The same archive, now behind a bucket nothing is listening on.
    let unreachable = radio_scout::BlobStore::s3(&radio_scout::S3Config {
        bucket: "radio-scout".into(),
        region: "us-east-1".into(),
        // Port 1 refuses immediately, so this fails fast rather than hanging.
        endpoint: Some("http://127.0.0.1:1".into()),
        access_key_id: "test-access".into(),
        secret_access_key: "test-secret".into(),
        allow_http: true,
    })
    .expect("s3 store");
    let after = TestApp::builder()
        .database_url(url)
        .store(unreachable)
        .enhancement(normalizing())
        .spawn()
        .await;

    let settled = after.await_enhancement(id).await;
    assert_eq!(
        settled.enhancement, "skipped",
        "an unreachable store must settle the Call, not re-queue it forever"
    );
    assert_eq!(
        after.get("/healthz").await.status(),
        200,
        "the process must still be serving"
    );
}

/// **A restart that interrupted more Calls than the queue is deep.**
///
/// The boot sweep offers every `pending` Call at once, so a shallow queue
/// refuses most of them. Those refusals have to be *recorded*: a Call left
/// `pending` is re-queued by every subsequent boot and — because a pending Call
/// is deliberately served without `immutable` — stays permanently uncacheable.
/// So the sweep sheds exactly the way ingest does, and nothing is left in
/// limbo.
#[tokio::test]
async fn a_sweep_that_overflows_the_queue_leaves_nothing_pending() {
    let shared = tempfile::tempdir().expect("tempdir");
    let url = shared_database_url(&shared).await;
    let store =
        || radio_scout::BlobStore::filesystem(shared.path().join("audio")).expect("shared store");

    let before = TestApp::builder()
        .database_url(url.clone())
        .store(store())
        .spawn()
        .await;
    before.create_api_key("k").await;
    for n in 0..8 {
        before
            .upload_ok(call().talkgroup(100 + n).at(1_000 + n))
            .await;
    }
    for stored in before.calls().await {
        radio_scout::db::repo::mark_enhancement(&before.db, stored.id, EnhancementState::PENDING)
            .await
            .expect("interrupt it");
    }

    // A queue one Call deep: the sweep offers eight and can hold one.
    let after = TestApp::builder()
        .database_url(url)
        .store(store())
        .enhancement(EnhancementConfig {
            queue_depth: 1,
            ..normalizing()
        })
        .spawn()
        .await;

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    loop {
        let states: Vec<String> = after
            .calls()
            .await
            .into_iter()
            .map(|c| c.enhancement)
            .collect();
        if !states.iter().any(|s| s == EnhancementState::PENDING) {
            assert_eq!(states.len(), 8);
            assert!(
                states.iter().any(|s| s == EnhancementState::SKIPPED),
                "a one-deep queue offered eight Calls must have refused some: {states:?}"
            );
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "Calls left pending after the sweep: {states:?}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
}
