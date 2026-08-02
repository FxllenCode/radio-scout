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
use common::{
    ADMIN_PASSWORD, CallUpload, TestApp, frame_within, next_json, no_frame_within, subscribe,
};

use std::time::Duration;

use bytes::Bytes;
use rstest::rstest;

use radio_scout::IngestConfig;
use radio_scout::blob::AudioStore;
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
    // Patch rows exist only for Talkgroups the System knows (#81).
    app.seed_talkgroup(11, 100).await;
    app.seed_talkgroup(11, 200).await;

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

/// A Talkgroup can be seeded so a test can say what a System already knows —
/// which is what decides patch membership (#81) — without minting a Call to
/// teach it, and `patch_refs` reads back the members that survived.
///
/// Seeding is deliberately invisible to the Call under test: the System it
/// creates carries the same label ingest would have defaulted to, so an upload
/// that arrives afterwards behaves exactly as it would have without the seed.
#[tokio::test]
async fn a_talkgroup_can_be_seeded_and_its_patch_members_read_back() {
    let app = TestApp::with_key("k").await;
    app.seed_talkgroup(11, 300).await;

    app.upload_ok(CallUpload::new().set("patches", "[300, 1610092]"))
        .await;

    let call = app.the_call().await;
    assert_eq!(
        app.patch_refs(call.id).await,
        vec![300],
        "the seeded Talkgroup is a member; the unknown Ref is not"
    );
    assert_eq!(
        app.system_of(&call).await.label.as_deref(),
        Some("System 11"),
        "seeding left the System exactly as the upload would have created it"
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

// ---------------------------------------------------------------------------
// Fault injection (#37, reshaped by #97): a store and a statement that can be
// made to fail, both by naming what should fail rather than by damaging the
// thing underneath.
// ---------------------------------------------------------------------------

/// **The store seam.** Every worker that writes audio has an error arm that is
/// unreachable while the only store in the suite is a filesystem that works —
/// so the handling of a refused write is shipped untested. A store that fails
/// when told is what makes those arms ordinary tests.
#[tokio::test]
async fn a_store_can_be_told_to_fail_its_writes() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (store, faults) = common::faulty_store(tmp.path());

    store
        .put("aa/1.wav", Bytes::from_static(b"before"))
        .await
        .expect("a healthy store takes a write");
    faults.fail_puts();
    let refused = store.put("aa/2.wav", Bytes::from_static(b"after")).await;

    assert!(refused.is_err(), "a store told to fail must refuse");
    assert_eq!(
        store.list_keys().await.expect("list"),
        ["aa/1.wav"],
        "and must not have written the object it refused"
    );
}

/// **A store that stats an object and then does not hand it over.**
///
/// Both halves of that are real — a Garage node shedding load answers the read
/// with an error, and an object pruned between the stat and the read is simply
/// gone — and serving has to tell them apart: one is a 500 an operator must act
/// on, the other a 404 the client should stop asking about.
///
/// Neither is reachable through a store that works, and **no real store can be
/// made to fail one and not the other**: a filesystem object made unreadable
/// fails its stat too, because `LocalFileSystem` opens the file to size it.
/// Under the store, the previous answer had to encode `serve::audio`'s own call
/// order ("a `head` is never failed, only a `get`") and park a real read while a
/// real object was pruned. Named at the interface, it is two lines and no
/// timing.
#[tokio::test]
async fn a_store_can_stat_an_object_it_will_not_hand_over() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (store, faults) = common::faulty_store(tmp.path());
    store
        .put("aa/1.wav", Bytes::from_static(b"bytes"))
        .await
        .expect("write");

    faults.fail_reads();
    assert_eq!(
        store.size("aa/1.wav").await.expect("stat"),
        Some(5),
        "the stat still answers, which is what makes the read the failing step"
    );
    assert!(store.get("aa/1.wav").await.is_err(), "broken");

    faults.hide_reads();
    assert_eq!(
        store
            .get("aa/1.wav")
            .await
            .expect("a hidden read is not an error"),
        None,
        "gone, which is a different answer from broken"
    );
}

/// **The statement seam** (#97): a failing statement is chosen by *naming* it.
///
/// What it replaces is `DROP TABLE` — dialect-specific DDL that took every
/// statement touching the table with it, so a read that had to succeed first
/// could not, and whose failure could only be recognised by matching a driver's
/// own wording, one phrasing per dialect. The refusal here is Radio-Scout's own
/// string on both, and the table is named in the SQL the same way on both, so
/// nothing about this test knows which dialect it is running on.
#[tokio::test]
async fn a_statement_naming_a_refused_table_is_refused() {
    let app = TestApp::with_key("k").await;
    app.upload_ok(CallUpload::new()).await;
    let stored = app.the_call().await;

    app.refuse_statements_on("calls");

    let refused = radio_scout::db::repo::find_call(&app.db, stored.id)
        .await
        .expect_err("a statement naming a refused table must fail");
    assert!(
        refused.to_string().contains(common::REFUSED),
        "the refusal says it was injected, whatever the dialect: {refused}"
    );
    assert!(
        radio_scout::db::repo::count_api_keys(&app.db).await.is_ok(),
        "a table nobody named still answers"
    );
}

/// **Refusing one kind of statement**, and why taking the table away was not
/// enough. A dropped table breaks the *first* statement that touches it — but
/// every failure worth reaching on the write side happens after a read *and* an
/// insert of the same table have already succeeded, so it always failed the
/// wrong one. Refusing updates while reads and inserts still work is what puts
/// the failure where it belongs.
#[tokio::test]
async fn a_table_can_be_told_to_refuse_its_updates() {
    let app = TestApp::with_key("k").await;
    app.upload_ok(CallUpload::new()).await;
    let stored = app.the_call().await;

    app.refuse_updates_to("calls");

    // Reads are untouched...
    assert_eq!(app.the_call().await.id, stored.id, "reads still work");
    // ...and so are inserts, or nothing could be arranged before the failure.
    app.upload_ok(CallUpload::new().at(2_000)).await;
    assert_eq!(app.count::<call::Entity>().await, 2, "inserts still work");

    let refused = radio_scout::db::repo::mark_enhancement(
        &app.db,
        stored.id,
        call::EnhancementState::SKIPPED,
    )
    .await;

    let error = refused.expect_err("an update must be refused").to_string();
    assert!(
        error.contains(common::REFUSED),
        "the refusal must say it was injected, on either dialect: {error}"
    );
    assert_eq!(
        app.calls().await[0].enhancement,
        call::EnhancementState::NONE,
        "and must leave the row it refused alone"
    );
}

/// **Counting statements** (#86), the other half of the same seam: the decorator
/// that can refuse a statement is also the one thing that sees every statement,
/// so it is what can say how many there have been.
///
/// This is what makes "does this path cost more per Call?" a question a test can
/// ask at all — an N+1 is invisible from outside, because the answer is right
/// and only the number of round-trips behind it is wrong. Monotonic, so a test
/// measures a stretch of the app's life by sampling either side of it.
#[tokio::test]
async fn the_statements_an_app_issues_are_counted() {
    let app = TestApp::with_key("k").await;

    let before = app.statements_issued();
    app.upload_ok(CallUpload::new()).await;
    let after_ingest = app.statements_issued();
    assert!(
        after_ingest > before,
        "ingesting a Call issues statements: {before} -> {after_ingest}"
    );

    // A request that touches no row moves nothing, so the count is of what the
    // app really asked the database and not of how long it has been up.
    app.get("/healthz").await;
    assert_eq!(
        app.statements_issued(),
        after_ingest,
        "a request that reads no row costs no statement"
    );

    // Statements inside a transaction are counted too — ingest's insert is one,
    // so a seam that missed them would undercount exactly where the writes are.
    let txn = app.db.begin().await.expect("begin");
    radio_scout::db::repo::count_api_keys(&txn)
        .await
        .expect("a statement inside the transaction");
    txn.rollback().await.expect("rollback");
    assert!(
        app.statements_issued() > after_ingest,
        "a statement issued inside a transaction is still a statement"
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

// ---------------------------------------------------------------------------
// A spawned app is the Instance the binary boots (#90). Everything below is
// about the assembly rather than about a route: the workers that used to be
// missing from the harness entirely, the configuration seam that replaced
// hand-wiring each subsystem, and the restart that replaced standing a second
// app up on a hand-shared database.
// ---------------------------------------------------------------------------

/// **The Retention sweeper runs**, which the harness used to omit — so every
/// test in the suite ran against an instance with no sweeper, and #10's
/// behaviour was only ever exercised by calling `sweep` directly.
///
/// The policy is turned off by default here (`baseline_config`) because the
/// suite dates its fixtures in 1970; a test that wants pruning asks for it, and
/// then it happens on the next boot exactly as it does on an operator's.
#[tokio::test]
async fn a_spawned_app_runs_the_retention_sweeper() {
    let mut app = TestApp::with_key("k").await;
    app.upload_ok(CallUpload::new()).await;
    assert_eq!(app.count::<call::Entity>().await, 1);

    // A week's policy, and a Call from 1970: the boot sweep prunes it.
    app.restart_with(|config| config.retention.days = 7).await;
    app.settle().await;

    assert_eq!(
        app.count::<call::Entity>().await,
        0,
        "the sweeper never ran on a spawned app"
    );
}

/// **`settle` is how the suite waits** (#93) — and the reason it can be
/// believed is that a Worker owes its work from where the work is *handed
/// over*, not from wherever the runtime gets round to polling it.
///
/// So: a Call is offered to enhancement inside the ingest the test already
/// awaited, and published to the push sender there too. Both are therefore
/// already owed by the time `upload_ok` has returned, and there is no window in
/// which an Instance reads idle because a worker has not woken up yet. That is
/// the whole difference between this and a sleep, and it is what makes a test
/// that asserts *absence* honest rather than optimistic.
#[tokio::test]
async fn work_is_owed_from_the_moment_it_is_handed_over() {
    let app = TestApp::builder()
        .enhancement(radio_scout::enhance::EnhancementConfig {
            mode: radio_scout::enhance::Mode::Normalize,
            ..Default::default()
        })
        .spawn()
        .await;
    app.create_api_key("k").await;

    app.upload_ok(CallUpload::new()).await;
    let depth: u64 = app
        .workers()
        .loads()
        .iter()
        .map(|reading| reading.load.depth)
        .sum();

    assert!(
        depth > 0,
        "an ingested Call left every Worker reading idle: {:?}",
        app.workers().loads()
    );

    app.settle().await;
    assert_ne!(
        app.the_call().await.enhancement,
        radio_scout::db::entities::call::EnhancementState::PENDING,
        "settling returned with the Call still pending"
    );
}

/// **A restart does not stop the logging** (#93). The log writer is the one
/// Worker that belongs to the *process* rather than to a run — the subscriber
/// feeding it outlives the restart — so `restart` stops the other three and
/// carries this one across.
///
/// Being still *listed* proves nothing; being still *draining* is the claim, so
/// the assertion is a line written after the restart arriving in the Logs view
/// an operator reads. Get this wrong and every stored log line after the first
/// restart disappears, silently, which is exactly the sort of thing a suite
/// notices years later.
#[tokio::test]
async fn a_restart_leaves_the_operator_log_still_being_written() {
    let mut app = TestApp::spawn().await;
    let _sink = app.store_logs();

    app.restart().await;
    app.get("/healthz").await;

    app.await_logged("request").await;
}

/// **Every Worker is named on the registry**, because a status surface (#70)
/// reads it through `AppState` and can never see the `Instance` that owns the
/// handles. An Instance with enhancement off runs three; turning it on is what
/// adds the fourth — a surface must show what is running, not a row of zeroes
/// for what is not.
#[tokio::test]
async fn the_registry_names_the_workers_this_instance_is_running() {
    let app = TestApp::spawn().await;
    let names = |app: &TestApp| -> Vec<&'static str> {
        app.workers()
            .loads()
            .iter()
            .map(|reading| reading.name)
            .collect()
    };

    assert_eq!(
        names(&app),
        vec![
            radio_scout::logsink::WORKER,
            radio_scout::retention::WORKER,
            radio_scout::push::WORKER,
        ],
        "the shipped default runs three: enhancement is off"
    );

    let mut app = app;
    app.restart_with(|config| {
        config.enhancement.mode = radio_scout::enhance::Mode::Normalize;
    })
    .await;

    assert_eq!(
        names(&app),
        vec![
            // The log writer belongs to the process rather than to a run, so a
            // restart carries it across — and it stays the oldest.
            radio_scout::logsink::WORKER,
            radio_scout::retention::WORKER,
            radio_scout::push::WORKER,
            radio_scout::enhance::WORKER,
        ],
    );
}

/// **A restart is one handle**, and it is a real one: the archive and the audio
/// survive, and the port does not.
#[tokio::test]
async fn a_restart_keeps_the_archive_and_moves_to_a_new_port() {
    let mut app = TestApp::with_key("k").await;
    app.upload_ok(CallUpload::new()).await;
    let key = app.the_call().await.object_key;
    let before = app.addr.clone();

    app.restart().await;

    assert_ne!(app.addr, before, "a stopped app released its port");
    assert_eq!(app.get("/healthz").await.status(), 200);
    assert_eq!(app.count::<call::Entity>().await, 1, "the archive survived");
    assert!(app.stored(&key).await, "the audio survived");
}

/// **A shut admin surface is a provisioning outcome**, reachable from the
/// harness by arranging its cause: an env file that cannot be written means the
/// generated password exists only inside the process, so none is set.
///
/// This is what `.admin(AdminAuth::locked())` used to fake. Faking it proved
/// the state was refusable; this proves a boot can arrive at it — and that the
/// same boot takes Web Push with it, because the two generated credentials
/// share the file that could not be written.
#[tokio::test]
async fn an_app_that_could_not_save_its_credentials_has_none() {
    let app = TestApp::builder().without_credentials().spawn().await;

    assert_eq!(
        app.login_as(ADMIN_PASSWORD).await.status(),
        401,
        "an unprovisioned admin surface let somebody in"
    );
    assert_eq!(app.get("/api/push/key").await.status(), 404);
    assert_eq!(
        app.get("/healthz").await.status(),
        200,
        "...and the scanner keeps scanning, which is the whole bargain"
    );
}

/// ...and so does a configuration **file**, resolved the way a boot resolves
/// one. A test about configuration should be able to write the thing an
/// operator writes.
#[tokio::test]
async fn a_configuration_file_reaches_the_running_app() {
    let app = TestApp::builder()
        .toml("[ingest]\nauto_populate = false\n")
        .spawn()
        .await;
    app.create_api_key("k").await;

    app.upload_ok(CallUpload::new().set("source", 4242)).await;

    assert_eq!(
        app.count::<radio_scout::db::entities::unit::Entity>().await,
        0,
        "`auto_populate = false` never left the file"
    );
}

/// The clock is an input (#90), so a harness can put an app at a moment of its
/// choosing rather than at whatever time the test machine says it is.
#[tokio::test]
async fn a_spawned_app_reads_the_clock_it_was_given() {
    let app = TestApp::builder()
        .clock(radio_scout::Clock::frozen(1_700_000_000_000))
        .spawn()
        .await;
    app.create_api_key("k").await;

    app.upload_ok(CallUpload::new()).await;

    assert_eq!(app.the_call().await.created_at_ms, 1_700_000_000_000);
}
