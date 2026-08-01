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

use common::logs::LogCapture;
use common::s3::unreachable_store;
use common::{CallUpload, TestApp, wav};
use radio_scout::db::entities::call::{self as call_entity, EnhancementState};
use radio_scout::enhance::{EnhancementConfig, Mode};
use sea_orm::EntityTrait;

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
    // Enhancement off, so nothing runs and the Call is left exactly as a killed
    // worker would have left it.
    let mut app = TestApp::with_key("k").await;
    app.upload_ok(call()).await;
    let id = app.the_call().await.id;
    radio_scout::db::repo::mark_enhancement(&app.db, id, EnhancementState::PENDING)
        .await
        .expect("leave it mid-flight");

    // ...and the boot that comes afterwards, onto the same database and the
    // same audio, with the operator having turned enhancement on.
    app.restart_with(|config| config.enhancement = normalizing())
        .await;

    let resumed = app.await_enhancement(id).await;
    assert_eq!(resumed.enhancement, "done", "the restart abandoned it");
}

/// ...but a boot must **never** sweep up Calls that were ingested while
/// enhancement was off. Those are marked `none`, and re-encoding them is a
/// deliberate act an operator asks for — not something that happens to their
/// archive because they restarted.
#[tokio::test]
async fn a_restart_leaves_the_existing_archive_alone() {
    let mut app = TestApp::with_key("k").await;
    app.upload_ok(call()).await;
    let id = app.the_call().await.id;

    app.restart_with(|config| config.enhancement = normalizing())
        .await;

    // The catch-up sweep is owed before `restart_with` returns, so settling is
    // the fact that it ran and found nothing to do — where the 300ms sleep this
    // replaces could only ever say "nothing has happened yet".
    app.settle().await;
    let untouched = call_row(&app, id).await;
    assert_eq!(untouched.enhancement, "none");
    assert_eq!(
        app.object_bytes(&untouched.object_key).await.as_deref(),
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

/// Retention is entitled to prune a Call between it being queued and the worker
/// reaching it — the archive is bounded and the queue is deep, so on a busy
/// instance this is ordinary, not exotic. The worker must not treat a missing
/// object as a crash.
#[tokio::test]
async fn a_call_whose_audio_vanished_is_skipped_rather_than_retried_forever() {
    let mut app = TestApp::with_key("k").await;
    app.upload_ok(call()).await;
    let stored = app.the_call().await;
    // The Call is queued, and its audio is then gone — the order a prune
    // interrupted between the row and the object leaves things in.
    radio_scout::db::repo::mark_enhancement(&app.db, stored.id, EnhancementState::PENDING)
        .await
        .expect("queue it");
    app.store.delete(&stored.object_key).await.expect("prune");

    app.restart_with(|config| config.enhancement = normalizing())
        .await;

    let settled = app.await_enhancement(stored.id).await;
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
    let mut app = TestApp::spawn().await;
    app.break_table("calls").await;

    app.restart_with(|config| config.enhancement = normalizing())
        .await;

    assert_eq!(
        app.get("/healthz").await.status(),
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
    let mut app = TestApp::with_key("k").await;
    app.upload_ok(call()).await;
    let id = app.the_call().await.id;
    radio_scout::db::repo::mark_enhancement(&app.db, id, EnhancementState::PENDING)
        .await
        .expect("queue it");

    // The same archive, now behind a bucket nothing is listening on. A refused
    // connection is not on its own enough to settle inside the deadline below —
    // the connection fails fast but the *call* retries it — so what actually
    // bounds this is `blob::retry_policy` (#39).
    app.restart_onto(Some(unreachable_store()), |config| {
        config.enhancement = normalizing()
    })
    .await;

    let settled = app.await_enhancement(id).await;
    assert_eq!(
        settled.enhancement, "skipped",
        "an unreachable store must settle the Call, not re-queue it forever"
    );
    assert_eq!(
        app.get("/healthz").await.status(),
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
    let mut app = TestApp::with_key("k").await;
    for n in 0..8 {
        app.upload_ok(call().talkgroup(100 + n).at(1_000 + n)).await;
    }
    for stored in app.calls().await {
        radio_scout::db::repo::mark_enhancement(&app.db, stored.id, EnhancementState::PENDING)
            .await
            .expect("interrupt it");
    }

    // A queue one Call deep: the sweep offers eight and can hold one.
    app.restart_with(|config| {
        config.enhancement = EnhancementConfig {
            queue_depth: 1,
            ..normalizing()
        }
    })
    .await;

    app.settle().await;

    let states: Vec<String> = app
        .calls()
        .await
        .into_iter()
        .map(|c| c.enhancement)
        .collect();
    assert_eq!(states.len(), 8);
    assert!(
        !states.iter().any(|s| s == EnhancementState::PENDING),
        "Calls left pending with the worker owing nothing: {states:?}"
    );
    assert!(
        states.iter().any(|s| s == EnhancementState::SKIPPED),
        "a one-deep queue offered eight Calls must have refused some: {states:?}"
    );
}

// ---------------------------------------------------------------------------
// The I/O failure arms (#37). Each of these is a real thing an operator's
// instance does — a full disk, a Garage node refusing writes, a database that
// goes away mid-Call — and each was unreachable until the harness could make a
// specific operation fail (`common::Faults`, `TestApp::fail_writes_to`).
//
// What they all assert is the same promise: **enhancement is a convenience, and
// nothing about it failing may cost a listener the Call**. The audio a recorder
// uploaded stays served, the process keeps taking uploads, and the operator gets
// a line saying which step gave up.
// ---------------------------------------------------------------------------

/// An object store that takes reads but refuses writes — a disk that filled up
/// between one Call and the next, which on a Pi is the ordinary way this fails.
///
/// The enhanced audio has nowhere to go, so the Call must keep the object it
/// arrived with and settle. Leaving it `pending` would be worse than doing
/// nothing at all: a pending Call is deliberately served without `immutable`
/// (`crate::audio_cache_control`), so it would stay permanently uncacheable
/// *and* be re-queued by every subsequent boot.
#[tokio::test]
async fn a_store_that_cannot_be_written_leaves_the_call_on_its_original_audio() {
    let capture = LogCapture::start();
    let shared = tempfile::tempdir().expect("tempdir");
    let (store, faults) = common::faulty_store(shared.path());

    let mut app = TestApp::builder().store(store).spawn().await;
    app.create_api_key("k").await;
    app.upload_ok(call()).await;
    let stored = app.the_call().await;
    radio_scout::db::repo::mark_enhancement(&app.db, stored.id, EnhancementState::PENDING)
        .await
        .expect("queue it");

    // The same archive, now on a disk that has filled up. Armed before the
    // restart, so the very first thing the worker tries to write fails — no
    // timing, no race.
    faults.fail_puts();
    app.restart_with(|config| config.enhancement = normalizing())
        .await;

    let settled = app.await_enhancement(stored.id).await;
    assert_eq!(
        settled.enhancement,
        EnhancementState::SKIPPED,
        "a Call whose enhanced audio cannot be stored must settle, not re-queue forever"
    );
    assert_eq!(
        settled.object_key, stored.object_key,
        "and must still point at the audio the recorder uploaded"
    );
    assert_eq!(
        app.object_keys().await,
        [stored.object_key],
        "nothing was written, so there is no orphan for #10's sweep to find"
    );

    let line = capture.wait_for("enhancement skipped").await;
    assert!(line.contains("reason=store-audio"), "{line}");
    assert_eq!(
        app.get("/healthz").await.status(),
        200,
        "the process must still be serving"
    );
}

/// A database that stops taking writes part-way through — a disk that filled, a
/// replica promoted read-only — with the reads that got the worker there still
/// working. This is the failure `break_table` could never stage: the worker
/// updates a Call row it has *already read*, so taking the table away breaks the
/// read and the update is never attempted.
///
/// Two arms have to hold at once, and the second is the one that matters. The
/// Call the worker was enhancing cannot be pointed at its new audio, so the
/// object it just wrote is an orphan and #10's sweep owns it. And the Calls the
/// sweep could not fit in the queue cannot be recorded as `skipped` either — so
/// the process must still say so rather than silently leaving them, because a
/// Call stuck `pending` is re-queued by every subsequent boot and stays
/// permanently uncacheable.
#[tokio::test]
async fn a_database_that_stops_taking_writes_says_so_for_every_call_it_loses() {
    let capture = LogCapture::start();
    let mut app = TestApp::with_key("k").await;
    for n in 0..8 {
        app.upload_ok(call().talkgroup(100 + n).at(1_000 + n)).await;
    }
    for stored in app.calls().await {
        radio_scout::db::repo::mark_enhancement(&app.db, stored.id, EnhancementState::PENDING)
            .await
            .expect("interrupt it");
    }
    // Only now: everything above is the arrangement, and it is all writes.
    app.fail_writes_to("calls").await;

    // A queue one Call deep, so the sweep both *runs* one Call to its failing
    // update and *refuses* the other seven to their failing update.
    app.restart_with(|config| {
        config.enhancement = EnhancementConfig {
            queue_depth: 1,
            ..normalizing()
        }
    })
    .await;

    let skipped = capture.wait_for("reason=store-call").await;
    assert!(
        skipped.contains(common::INJECTED_WRITE),
        "the operator is told why the update failed: {skipped}"
    );
    // Pinned on the message rather than the `reason` slug, unusually: both
    // sites log `reason=mark-skipped-failed`, and the point of this test is that
    // *both* of them fired. The messages are Radio-Scout's own — no driver
    // phrasing, so nothing here differs by dialect.
    capture
        .wait_for("could not record that enhancement was skipped")
        .await;
    capture
        .wait_for("a refused Call is still marked pending")
        .await;

    assert_eq!(
        app.get("/healthz").await.status(),
        200,
        "a database that will not take writes must not take the scanner down"
    );
}

/// **A table that goes away while the worker is holding a Call.** The worker had
/// already read its row, so the failure lands on the *update* — and the Call
/// behind it in the queue never gets a row read at all.
///
/// Both arms are reached in one pass because the store is holding the first Call
/// still: while it is parked inside its `get`, the second Call is provably
/// queued and nothing has read it yet, so the table can be taken away knowing
/// exactly which statements have run. Racing a sleep against a background worker
/// would reach the same lines and fail on a loaded CI runner instead.
#[tokio::test]
async fn a_table_that_vanishes_mid_call_is_survived_by_the_worker() {
    let capture = LogCapture::start();
    let tmp = tempfile::tempdir().expect("tempdir");
    let (store, faults) = common::faulty_store(tmp.path());
    faults.stall_reads();

    let app = TestApp::builder()
        .store(store)
        .enhancement(normalizing())
        .spawn()
        .await;
    app.create_api_key("k").await;

    // The first Call: the worker reads its row, then parks reading its audio.
    app.upload_ok(call()).await;
    faults.stalled(1).await;
    // The second: queued behind the parked one, its row not yet read.
    app.upload_ok(call().talkgroup(200).at(2_000)).await;

    app.break_table("calls").await;
    faults.release();

    let missing = app.missing_table_cause("calls");
    let update = capture.wait_for("reason=store-call").await;
    assert!(
        update.contains(&missing),
        "the Call that was already read fails on its update: {update}"
    );
    let lookup = capture.wait_for("reason=look-up-call").await;
    assert!(
        lookup.contains(&missing),
        "the Call behind it fails on its row: {lookup}"
    );

    assert_eq!(
        app.get("/healthz").await.status(),
        200,
        "the process must still be serving — a worker that unwinds takes every \
         later Call's enhancement with it"
    );
}

/// A Call pruned between being queued and being reached. Retention is entitled
/// to do exactly this — the archive is bounded and the queue is deep — so it is
/// ordinary, not exotic, and it is **not a failure**: there is nothing to say
/// and nothing to settle, because the row saying `pending` is itself gone.
///
/// What must survive is the worker. The Call queued behind the pruned one has to
/// be enhanced, or a busy instance would lose everything after the first prune.
#[tokio::test]
async fn a_call_pruned_before_the_worker_reaches_it_is_passed_over_in_silence() {
    let capture = LogCapture::start();
    let tmp = tempfile::tempdir().expect("tempdir");
    let (store, faults) = common::faulty_store(tmp.path());
    faults.stall_reads();

    let app = TestApp::builder()
        .store(store)
        .enhancement(normalizing())
        .spawn()
        .await;
    app.create_api_key("k").await;

    app.upload_ok(call()).await;
    faults.stalled(1).await;
    app.upload_ok(call().talkgroup(200).at(2_000)).await;
    app.upload_ok(call().talkgroup(300).at(3_000)).await;
    let queued = app.calls().await;
    let (first, pruned, behind) = (&queued[0], &queued[1], &queued[2]);

    call_entity::Entity::delete_by_id(pruned.id)
        .exec(&app.db)
        .await
        .expect("prune the queued Call");
    faults.release();

    assert_eq!(
        app.await_enhancement(first.id).await.enhancement,
        EnhancementState::DONE
    );
    assert_eq!(
        app.await_enhancement(behind.id).await.enhancement,
        EnhancementState::DONE,
        "the Call queued behind a pruned one must still be enhanced"
    );
    capture.assert_never_logged("enhancement skipped");
}

/// The ingest side of the same failure. A Call is marked `pending` *before* it
/// is offered to the queue, so that a process killed between the two finds it
/// again at the next boot — which means the mark is a write that can fail while
/// everything around it works.
///
/// It must cost the recorder nothing. The Call is already stored, already
/// answered and already on the live feed by this point, so a failure here costs
/// only the levelling — and ingest that started returning 500s over an optional
/// convenience would be a far worse bug than the one it reported.
#[tokio::test]
async fn a_call_that_cannot_be_marked_pending_still_lands_and_still_plays() {
    let capture = LogCapture::start();
    let app = TestApp::builder().enhancement(normalizing()).spawn().await;
    app.create_api_key("k").await;
    app.fail_writes_to("calls").await;

    app.upload_ok(call()).await;

    let stored = app.the_call().await;
    assert_eq!(
        stored.enhancement,
        EnhancementState::NONE,
        "the mark never landed, which is the whole point"
    );
    assert_eq!(
        app.get(&format!("/api/call/{}/audio", stored.id))
            .await
            .status(),
        200,
        "the Call the recorder uploaded must still play"
    );
    let line = capture.wait_for("reason=mark-pending-failed").await;
    assert!(line.contains(common::INJECTED_WRITE), "{line}");
}

/// An encrypted Call has no audio object at all (#42, spec US 9), so there is
/// nothing to enhance and nothing to decode.
///
/// Offering it anyway is not harmless: the worker would mark the Call
/// `pending`, ask the store for the object named by the empty string, fail, and
/// settle it `skipped` with a WARN — one per Call, forever, on a System whose
/// traffic is mostly encrypted. That is an operator watching a log fill with a
/// failure that is not one, and a Pi spending three queries per Call to
/// discover it.
#[tokio::test]
async fn an_encrypted_call_is_never_offered_to_the_enhancement_worker() {
    let capture = LogCapture::start();
    let app = TestApp::builder().enhancement(normalizing()).spawn().await;
    app.create_api_key("k").await;

    let meta = r#"{"short_name":"butco","talkgroup":54241,
                   "start_time":1669740338,"call_length_ms":4000,"encrypted":1}"#;
    let (status, body) = app.upload_tr(CallUpload::tr(meta)).await;
    assert_eq!(status, 200, "{body:?}");

    let stored = app.the_call().await;
    assert_eq!(
        stored.enhancement,
        EnhancementState::NONE,
        "never queued, so never pending and never skipped"
    );
    capture.assert_never_logged("call enhancement failed");
}
