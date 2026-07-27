//! The integration harness's own tests (ticket #21).
//!
//! Every other `tests/*.rs` binary trusts [`common::TestApp`] to bring up the
//! real app and to tell the truth about what it observed. That trust is only
//! worth something if it is checked somewhere: a harness that silently shares a
//! database between two apps, or whose `stored` helper answers `true` for a key
//! that was never written, turns a green suite into a lie. So the harness gets
//! the same treatment as the code it tests — driven over the real HTTP + WS
//! boundary, asserting on rows, objects, and frames.
//!
//! This file is also the harness's documentation by example: the shapes here are
//! the ones the rest of the suite is written in.

mod common;
use common::{CallUpload, TestApp, frame_within, next_json, no_frame_within, subscribe};

use std::time::Duration;

use rstest::rstest;

use radio_scout::IngestConfig;
use radio_scout::db::DbBackend;
use radio_scout::db::entities::{call, call_patch, system};
use sea_orm::ConnectionTrait;

// ---------------------------------------------------------------------------
// Bring-up: the real app, on a real port, over a temp SQLite DB + temp store.
// ---------------------------------------------------------------------------

/// The app the harness spawns is the app the binary serves — same router, same
/// routes, reachable over real TCP.
#[tokio::test]
async fn spawn_serves_the_real_app_over_http() {
    let app = TestApp::spawn().await;

    let resp = app.get("/healthz").await;
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.text().await.expect("body"), "ok");
}

/// Two apps share nothing. A suite runs its tests concurrently, so a Call
/// ingested by one must be invisible to the other — in the database *and* in the
/// object store.
#[tokio::test]
async fn two_apps_share_no_database_and_no_store() {
    let one = TestApp::with_key("k").await;
    let other = TestApp::with_key("k").await;
    assert_ne!(one.addr, other.addr, "each app gets its own port");

    one.upload_ok(CallUpload::new()).await;

    assert_eq!(one.count::<call::Entity>().await, 1);
    assert_eq!(other.count::<call::Entity>().await, 0, "separate databases");
    assert_eq!(one.object_keys().await.len(), 1);
    assert!(other.object_keys().await.is_empty(), "separate stores");
}

/// The handle owns its temp directory, so a test never has to keep a `_tmp`
/// binding alive by hand — and nothing is left behind in `/tmp` when it drops.
#[tokio::test]
async fn the_temp_directory_lives_and_dies_with_the_handle() {
    let path = {
        let app = TestApp::spawn().await;
        let path = app.path().to_path_buf();
        assert!(path.exists(), "the base dir exists while the app does");
        path
    };
    assert!(!path.exists(), "and is cleaned up when the handle drops");
}

// ---------------------------------------------------------------------------
// Synthetic Calls: what `CallUpload` puts on the wire.
// ---------------------------------------------------------------------------

/// The defaults are a complete, valid recorder upload — so a test that cares
/// about one field says only that field.
#[tokio::test]
async fn a_default_upload_is_a_complete_valid_call() {
    let app = TestApp::with_key("k").await;

    app.upload_ok(CallUpload::new()).await;

    let stored = app.the_call().await;
    let system = app.system_of(&stored).await;
    let talkgroup = app.talkgroup_of(&stored).await;
    assert_eq!(system.r#ref, 11);
    assert_eq!(talkgroup.r#ref, 54241);
    assert_eq!(stored.call_at_ms, 1000);
    assert_eq!(stored.audio_mime.as_deref(), Some("audio/x-wav"));
    assert_eq!(
        stored.audio_size,
        Some(CallUpload::DEFAULT_AUDIO.len() as i64)
    );
}

/// `set` overrides a default in place and `remove` drops it, so a test can build
/// the exact upload it means — including the invalid ones.
#[tokio::test]
async fn set_overrides_and_remove_drops_a_field() {
    let app = TestApp::with_key("k").await;

    app.upload_ok(CallUpload::new().set("systemLabel", "RSP25MTL").at(7_000))
        .await;
    let stored = app.the_call().await;
    assert_eq!(stored.call_at_ms, 7_000);
    assert_eq!(
        app.system_of(&stored).await.label.as_deref(),
        Some("RSP25MTL")
    );

    // Removing the talkgroup is how the load-bearing rdio 417 gets exercised.
    let (status, body) = app.upload(CallUpload::new().remove("talkgroup")).await;
    assert_eq!(status, 417);
    assert!(
        body.to_lowercase()
            .starts_with("incomplete call data: no talkgroup"),
        "{body:?}"
    );
}

/// A field set twice keeps its original position, so `audio_first` really does
/// describe part order rather than accidentally reordering the text fields.
#[tokio::test]
async fn the_audio_part_can_lead_the_way_a_recorder_sends_it() {
    let app = TestApp::with_key("k").await;

    // Trunk Recorder puts the audio part first and the metadata after it, which
    // is what makes `audioType`/`audioName` win over the part's own headers.
    app.upload_ok(
        CallUpload::new()
            .audio_first()
            .audio_unlabelled(b"audio-bytes")
            .set("audioType", "audio/mpeg")
            .set("audioName", "clip.mp3"),
    )
    .await;

    let stored = app.the_call().await;
    assert_eq!(stored.audio_mime.as_deref(), Some("audio/mpeg"));
    assert_eq!(stored.audio_name.as_deref(), Some("clip.mp3"));
}

/// An upload with no audio at all is a case the ingest contract has an answer
/// for, so the builder has to be able to express it.
#[tokio::test]
async fn an_upload_can_carry_no_audio() {
    let app = TestApp::with_key("k").await;

    let (status, body) = app.upload(CallUpload::new().no_audio()).await;

    assert_eq!(status, 417);
    assert!(
        body.to_lowercase()
            .starts_with("incomplete call data: no audio"),
        "{body:?}"
    );
}

/// The Trunk-Recorder-native dialect is the other ingest endpoint, so it is the
/// other thing the harness must be able to drive — same builder, same knobs,
/// a different set of defaults and a different route.
#[tokio::test]
async fn upload_tr_drives_the_trunk_recorder_native_endpoint() {
    let app = TestApp::with_key("k").await;

    let (status, body) = app
        .upload_tr(CallUpload::tr(
            r#"{"short_name":"butco","talkgroup":54241,"start_time":1669740338}"#,
        ))
        .await;

    assert_eq!(status, 200, "{body:?}");
    assert!(body.contains("Call imported successfully."), "{body:?}");
    assert_eq!(app.the_call().await.call_at_ms, 1669740338000);

    // And the incomplete shapes it has to be able to express.
    let (status, body) = app.upload_tr(CallUpload::tr("{}").remove("meta")).await;
    assert_eq!(status, 417);
    assert!(
        body.to_lowercase()
            .starts_with("incomplete call data: no meta"),
        "{body:?}"
    );
}

// ---------------------------------------------------------------------------
// Assertions: rows, objects, frames.
// ---------------------------------------------------------------------------

/// `count` reads whatever entity it is asked for, which is how a test says "the
/// child rows landed too" without spelling out a query.
#[tokio::test]
async fn count_reads_any_entity() {
    let app = TestApp::with_key("k").await;

    app.upload_ok(CallUpload::new().set("patches", "[100, 200]"))
        .await;

    assert_eq!(app.count::<call::Entity>().await, 1);
    assert_eq!(app.count::<call_patch::Entity>().await, 2);
    assert_eq!(app.count::<system::Entity>().await, 1);
}

/// `calls` is ordered oldest-first, so a retention or archive assertion can name
/// positions instead of sorting at every call site.
#[tokio::test]
async fn calls_are_returned_oldest_first() {
    let app = TestApp::with_key("k").await;

    for at_ms in [3_000, 1_000, 2_000] {
        app.upload_ok(CallUpload::new().talkgroup(at_ms).at(at_ms))
            .await;
    }

    let stored: Vec<i64> = app.calls().await.iter().map(|c| c.call_at_ms).collect();
    assert_eq!(stored, vec![1_000, 2_000, 3_000]);
}

/// The store the app writes to is the store the test reads back, so "the audio
/// really was written" is one call rather than a second blob store nobody wired
/// up.
#[tokio::test]
async fn stored_objects_are_observable_through_the_handle() {
    let app = TestApp::with_key("k").await;

    app.upload_ok(CallUpload::new().audio(b"the-real-bytes"))
        .await;

    let key = app.the_call().await.object_key;
    assert!(app.stored(&key).await, "ingest wrote {key}");
    assert_eq!(app.object_keys().await, vec![key.clone()]);
    assert_eq!(
        app.object_bytes(&key).await.as_deref(),
        Some(b"the-real-bytes".as_slice())
    );
    assert!(!app.stored("nothing/here.wav").await, "and only that");
}

/// A test that needs an object without an ingest — the orphan-GC and
/// audio-serving cases — writes one directly.
#[tokio::test]
async fn an_object_can_be_seeded_without_an_ingest() {
    let app = TestApp::spawn().await;

    app.put_object("ab/clip.wav", b"0123456789").await;

    assert!(app.stored("ab/clip.wav").await);
    assert_eq!(app.count::<call::Entity>().await, 0, "no row, just bytes");
}

/// A Call row can be seeded straight into the database, for the read surfaces
/// (archive search, audio serving) that care about rows rather than about how
/// they got there.
#[tokio::test]
async fn a_call_row_can_be_seeded_without_an_upload() {
    let app = TestApp::spawn().await;

    let id = app
        .seed_call(radio_scout::db::repo::NewCall {
            system_ref: 11,
            talkgroup_ref: 54241,
            call_at_ms: 1_000,
            object_key: "ab/clip.wav".into(),
            audio_mime: Some("audio/x-wav".into()),
            ..Default::default()
        })
        .await;

    assert_eq!(app.the_call().await.id, id);
}

/// A System with a blacklist (or with auto-populate forced on) has no admin
/// surface yet — per-System policy is a database row, so #19's admin surface is
/// where an operator sets one — so tests seed it directly.
#[tokio::test]
async fn a_system_can_be_seeded_with_its_ingest_policy() {
    let app = TestApp::with_key("k").await;
    app.seed_system(11, false, Some("54241")).await;

    app.upload_ok(CallUpload::new().talkgroup(54241)).await;
    app.upload_ok(CallUpload::new().talkgroup(99999)).await;

    assert_eq!(
        app.count::<call::Entity>().await,
        1,
        "the blacklisted talkgroup was dropped, the other was not"
    );
}

// ---------------------------------------------------------------------------
// The WebSocket half of the seam.
// ---------------------------------------------------------------------------

/// The live feed is driven over a real socket: connect, subscribe, and see the
/// Call an upload pushes.
#[tokio::test]
async fn a_subscribed_socket_sees_the_call_an_upload_pushes() {
    let app = TestApp::with_key("k").await;
    let mut ws = app.connect_ws().await;

    subscribe(&mut ws, r#"{"t":"sub","sel":{"11":{"54241":true}}}"#).await;
    app.upload_ok(CallUpload::new()).await;

    let frame = next_json(&mut ws).await;
    assert_eq!(frame["t"], "call");
    assert_eq!(frame["call"]["systemRef"], 11);
    assert_eq!(frame["call"]["talkgroupRef"], 54241);
}

/// The other half of the assertion — a frame that must *not* arrive — needs a
/// bounded wait, or "filtered correctly" and "hung forever" look the same.
#[tokio::test]
async fn a_frame_that_should_not_arrive_can_be_asserted_absent() {
    let app = TestApp::with_key("k").await;
    let mut ws = app.connect_ws().await;

    subscribe(&mut ws, r#"{"t":"sub","sel":{"11":{"99999":true}}}"#).await;
    app.upload_ok(CallUpload::new().talkgroup(54241)).await;

    no_frame_within(&mut ws, Duration::from_millis(400)).await;
}

/// `frame_within` answers the same question with the frame in hand, for the
/// tests that need to look at what did arrive after something else did not.
#[tokio::test]
async fn frame_within_hands_back_what_arrived() {
    let app = TestApp::with_key("k").await;
    let mut ws = app.connect_ws().await;

    subscribe(&mut ws, r#"{"t":"sub","all":true}"#).await;
    app.upload_ok(CallUpload::new().talkgroup(4242)).await;

    let frame = frame_within(&mut ws, Duration::from_millis(400))
        .await
        .expect("all:true delivers everything");
    assert_eq!(frame["call"]["talkgroupRef"], 4242);
    assert!(
        frame_within(&mut ws, Duration::from_millis(200))
            .await
            .is_none(),
        "and nothing else follows"
    );
}

/// The greeting is the first thing on the wire, so a test that asserts about it
/// can ask for it rather than racing the frames behind it.
#[tokio::test]
async fn the_hello_greeting_is_available_or_consumed_on_request() {
    let app = TestApp::spawn().await;

    let (_ws, hello) = app.connect_ws_with_hello().await;
    assert_eq!(hello["t"], "hello");

    // `connect_ws` swallows it, leaving the socket ready for protocol traffic.
    let mut ws = app.connect_ws().await;
    subscribe(&mut ws, r#"{"t":"sub","all":true}"#).await;
}

// ---------------------------------------------------------------------------
// The knobs: every configuration the suite needs, on one builder.
// ---------------------------------------------------------------------------

/// The ingest config reaches the app — otherwise the auto-populate and dedup
/// tests would be asserting against the defaults and passing for the wrong
/// reason.
#[tokio::test]
async fn the_builder_plumbs_the_ingest_config() {
    let app = TestApp::builder()
        .ingest(IngestConfig {
            auto_populate: false,
            ..Default::default()
        })
        .spawn()
        .await;
    app.create_api_key("k").await;

    app.upload_ok(CallUpload::new()).await;

    assert_eq!(
        app.count::<system::Entity>().await,
        0,
        "auto-populate off drops the unknown System"
    );
}

/// The heartbeat period reaches the app, which the client learns from the
/// greeting — so a test can drive reaping in milliseconds instead of the
/// production 30 s.
#[tokio::test]
async fn the_builder_plumbs_the_heartbeat_period() {
    let app = TestApp::builder()
        .heartbeat(Duration::from_millis(120))
        .spawn()
        .await;

    let (_ws, hello) = app.connect_ws_with_hello().await;

    assert_eq!(hello["heartbeatMs"], 120);
}

/// A caller-supplied blob store replaces the temp filesystem one — this is how
/// the S3 serving mode gets an end-to-end test without a running S3.
#[tokio::test]
async fn the_builder_accepts_a_caller_supplied_store() {
    let s3 = radio_scout::BlobStore::s3(&radio_scout::S3Config {
        bucket: "radio-scout".into(),
        region: "us-east-1".into(),
        endpoint: Some("http://localhost:3900".into()),
        access_key_id: "GKtestaccesskey".into(),
        secret_access_key: "testsecretkey".into(),
        allow_http: true,
    })
    .expect("s3 store");
    let app = TestApp::builder().store(s3).spawn().await;
    let id = app
        .seed_call(radio_scout::db::repo::NewCall {
            system_ref: 11,
            talkgroup_ref: 54241,
            call_at_ms: 1_000,
            object_key: "ab/clip.m4a".into(),
            audio_mime: Some("audio/mp4".into()),
            ..Default::default()
        })
        .await;

    let resp = app
        .get_without_redirects(&format!("/api/call/{id}/audio"))
        .await;

    assert_eq!(
        resp.status(),
        307,
        "the S3 backend presigns rather than proxying"
    );
}

/// The dual-dialect run (#22, ADR-0003/0009) is the *whole* suite a second time,
/// not one hand-written Postgres test: with `TEST_POSTGRES_URL` set, every
/// `TestApp::spawn` in every binary lands on Postgres; with it unset — the
/// everyday loop, and any machine without Docker — the default stays SQLite.
///
/// This is the one assertion that catches a dual-dialect job silently running
/// SQLite twice, which is a green CI that proves half of what it claims. That
/// each app gets its **own** Postgres database is proven by
/// `two_apps_share_no_database_and_no_store`, which runs on whichever dialect it
/// was given.
#[tokio::test]
async fn spawn_runs_on_the_dialect_the_run_was_given() {
    let app = TestApp::spawn().await;

    let expected = match std::env::var("TEST_POSTGRES_URL") {
        Ok(_) => DbBackend::Postgres,
        Err(_) => DbBackend::Sqlite,
    };
    assert_eq!(app.db.get_database_backend(), expected);
}

/// A per-test Postgres database is named by rewriting **only** the database
/// name out of the server URL the run was handed (#22). The parts around it are
/// load-bearing: credentials live before the path, and connection parameters
/// (`sslmode`, `options`) live after it — a rewrite that ate either would send
/// the whole dual-dialect run at the wrong server, or at the right one
/// unencrypted.
#[rstest]
#[case::a_database_named_in_the_path(
    "postgres://u:p@host:5432/postgres",
    "postgres://u:p@host:5432/rs_test_1"
)]
#[case::parameters_after_it(
    "postgres://u:p@host:5432/postgres?sslmode=disable",
    "postgres://u:p@host:5432/rs_test_1?sslmode=disable"
)]
#[case::no_database_at_all("postgres://host", "postgres://host/rs_test_1")]
#[case::no_database_but_parameters(
    "postgres://host?sslmode=disable",
    "postgres://host/rs_test_1?sslmode=disable"
)]
fn a_per_test_database_url_replaces_only_the_database_name(
    #[case] server: &str,
    #[case] expected: &str,
) {
    assert_eq!(common::database_url_in(server, "rs_test_1"), expected);
}

/// A test can name the database it wants, rather than taking the one the run
/// would have chosen. Proven here with a second SQLite file — the per-call-site
/// override, distinct from the whole-suite dialect switch
/// `spawn_runs_on_the_dialect_the_run_was_given` covers.
#[tokio::test]
async fn the_builder_accepts_a_caller_supplied_database() {
    let elsewhere = tempfile::tempdir().expect("tempdir");
    let path = elsewhere.path().join("elsewhere.db");
    let app = TestApp::builder()
        .database_url(format!("sqlite://{}?mode=rwc", path.display()))
        .spawn()
        .await;
    app.create_api_key("k").await;

    app.upload_ok(CallUpload::new()).await;

    assert_eq!(app.count::<call::Entity>().await, 1);
    assert!(path.exists(), "the app used the database it was handed");
    assert!(
        !app.path().join("t.db").exists(),
        "and not the one it would have made"
    );
}

/// An API key is the precondition of nearly every ingest test, so it is one
/// call — scoped to a System when the test is about scoping.
#[tokio::test]
async fn api_keys_can_be_seeded_globally_or_per_system() {
    let app = TestApp::spawn().await;
    app.create_api_key("global").await;
    app.create_api_key_for_system("sys11", 11).await;

    let (status, _) = app.upload(CallUpload::new().key("global").system(22)).await;
    assert_eq!(status, 200, "a global key grants every system");
    let (status, _) = app.upload(CallUpload::new().key("sys11").system(11)).await;
    assert_eq!(status, 200, "a scoped key grants its own system");
    let (status, _) = app.upload(CallUpload::new().key("sys11").system(33)).await;
    assert_eq!(status, 401, "and no other");
}

// ---------------------------------------------------------------------------
// The raw escape hatches, for the tests that are about the wire itself.
// ---------------------------------------------------------------------------

/// The golden suite replays captured recorder bytes verbatim, so the harness has
/// to be able to POST a body it did not build.
#[tokio::test]
async fn a_raw_body_can_be_posted_with_its_own_headers() {
    let app = TestApp::spawn().await;

    let (status, body) = app
        .post_bytes(
            "/api/call-upload",
            "text/plain",
            b"not a multipart body".to_vec(),
        )
        .await;

    assert_eq!(status, 400, "{body:?}");
}

/// Range requests are a first-class part of the audio contract (ADR-0002), so
/// asking for one is a first-class part of the harness.
#[tokio::test]
async fn a_range_request_is_one_call() {
    let app = TestApp::with_key("k").await;
    app.upload_ok(CallUpload::new().audio(b"0123456789")).await;
    let id = app.the_call().await.id;

    let resp = app
        .get_range(&format!("/api/call/{id}/audio"), "bytes=2-5")
        .await;

    assert_eq!(resp.status(), 206);
    assert_eq!(resp.bytes().await.expect("body").as_ref(), b"2345");
}

/// JSON read surfaces are asked for as JSON, with the 200 checked on the way
/// through — a search that 500s should not fail as "not an array".
#[tokio::test]
async fn get_json_checks_the_status_on_the_way_past() {
    let app = TestApp::spawn().await;

    let page = app.get_json("/api/calls").await;

    assert!(page["results"].is_array(), "{page}");
}
