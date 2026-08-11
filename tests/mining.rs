//! **Mining** (#48, spec US 13): what SDRTrunk buried in the audio, folded into
//! the Archive.
//!
//! `src/mining/mod.rs`'s own tests are the dialect — which frame means what, over
//! values constructed by hand. This file is the two places it *runs*: on the
//! ingest path, where a Call is mined as it arrives, and in the sweep
//! sweep, over an Archive that was stored before any of this existed. Both are
//! driven through a real Instance with real MP3 bytes over real HTTP, because
//! what the pure tests cannot prove is that the probe, the dialect and the
//! writers all agree about the same file.

mod common;

use common::{CallUpload, SdrTrunkMp3, TestApp, no_frame_within, subscribe};
use radio_scout::db::entities::site;
use radio_scout::db::repo::{NewCall, NewCallUnit};
use rstest::rstest;
use sea_orm::ConnectionTrait;

/// The comment run SDRTrunk writes for a channel configured with a tower, a
/// decoder and a frequency — `AudioMetadataUtils.getMetadataMap`'s order.
const FULL_COMMENT: &str = "Date:2026-08-11 09:00:00.000;System:Fulton;Site:Downtown;\
                            Name:Control 1;Decoder:P25 Phase 1;Frequency:851012500;";

/// The one Call an app holds, as a Listener is shown it.
async fn the_wire_call(app: &TestApp) -> serde_json::Value {
    let page = app.get_json("/api/calls").await;
    page["results"]
        .as_array()
        .and_then(|calls| calls.first())
        .cloned()
        .expect("one Call in the Archive")
}

// ---------------------------------------------------------------------------
// Mining on the ingest path (#48). Not an off-path worker, deliberately: the
// live-feed frame is published at ingest and nothing republishes one (#46), so
// a name that arrives afterwards never reaches the Listener who heard the Call.
// ---------------------------------------------------------------------------

/// **The whole point of the ticket.** SDRTrunk has an alias list an Operator
/// curated, and its rdio broadcaster sends none of it — `FormField` has no
/// field for a configured radio name at all. It writes it into the MP3, and
/// this is where that becomes a **Unit** with a name.
#[tokio::test]
async fn an_sdrtrunk_upload_names_the_radio_no_wire_field_could_have() {
    let app = TestApp::with_key("k").await;
    let mp3 = SdrTrunkMp3::new().radio("1234567 Engine 1");

    app.upload_ok(CallUpload::sdrtrunk(&mp3)).await;
    app.settle().await;

    let unit = app
        .unit_by_ref(11, 1234567)
        .await
        .expect("the radio was rostered");
    assert_eq!(
        unit.label.as_deref(),
        Some("Engine 1"),
        "the roster takes the alias SDRTrunk had configured"
    );
    assert_eq!(
        the_wire_call(&app).await["unitLabel"],
        "Engine 1",
        "and the Listener sees it on the Call itself"
    );
}

/// The alias is the **configured** one, so it lands in `label`; `talkerAlias`
/// — the name the radio put over the air — stays in `tag_ota` beside it.
/// CONTEXT.md keeps the two apart because when they disagree, the disagreement
/// is the information.
#[tokio::test]
async fn the_mined_alias_is_the_configured_one_and_never_the_over_the_air_one() {
    let app = TestApp::with_key("k").await;
    let mp3 = SdrTrunkMp3::new().radio("1234567 Engine 1");

    app.upload_ok(CallUpload::sdrtrunk(&mp3).set("talkerAlias", "ENG1"))
        .await;
    app.settle().await;

    let detail = app
        .get_json(&format!("/api/call/{}", app.the_call().await.id))
        .await;
    let heard = &detail["units"][0];
    assert_eq!(heard["label"], "Engine 1", "SDRTrunk's own alias list");
    assert_eq!(heard["tagOta"], "ENG1", "what the radio broadcast");
}

/// A tower's name, out of the audio — and a **Site** minted to hold it, because
/// SDRTrunk sends no `site` field for a Ref to be paired with.
#[tokio::test]
async fn a_mined_tower_becomes_a_site_a_listener_can_read() {
    let app = TestApp::with_key("k").await;
    let mp3 = SdrTrunkMp3::new().comment(FULL_COMMENT);

    app.upload_ok(CallUpload::sdrtrunk(&mp3)).await;
    app.settle().await;

    let call = the_wire_call(&app).await;
    assert_eq!(call["siteLabel"], "Downtown");
    assert_eq!(
        call["siteRef"], 1,
        "the Ref is this Instance's own, minted lowest-free — nobody outside \
         it ever numbered this tower"
    );
}

/// Two Calls off the same tower are one **Site**, and a second tower is a
/// second one — so the minted Refs are an identity rather than a counter that
/// runs away with the traffic.
#[tokio::test]
async fn the_same_tower_heard_twice_is_one_site() {
    let app = TestApp::with_key("k").await;
    let downtown = SdrTrunkMp3::new().comment("Site:Downtown;");
    let ridge = SdrTrunkMp3::new().comment("Site:North Ridge;");

    app.upload_ok(CallUpload::sdrtrunk(&downtown).at(1_000))
        .await;
    app.upload_ok(CallUpload::sdrtrunk(&downtown).at(60_000))
        .await;
    app.upload_ok(CallUpload::sdrtrunk(&ridge).at(120_000))
        .await;
    app.settle().await;

    assert_eq!(app.count::<site::Entity>().await, 2);
    // **The minted Refs are the lowest free ones, in order.** The second tower
    // has to step over the first: a mint that did not search would hand out the
    // same number twice, and `idx_sites_system_ref` would refuse the row.
    assert_eq!(
        app.site_refs(11).await,
        vec![(1, "Downtown".to_string()), (2, "North Ridge".to_string())]
    );
}

/// What SDRTrunk was demodulating fills the column Trunk Recorder's
/// `audio_type` fills — the same question in two recorders' vocabularies.
#[tokio::test]
async fn the_decoder_fills_what_the_recorder_was_demodulating() {
    let app = TestApp::with_key("k").await;
    let mp3 = SdrTrunkMp3::new().comment(FULL_COMMENT);

    app.upload_ok(CallUpload::sdrtrunk(&mp3)).await;
    app.settle().await;

    assert_eq!(
        app.the_call().await.audio_type.as_deref(),
        Some("P25 Phase 1")
    );
}

/// **Mining fills and never overwrites.** The wire is the recorder speaking
/// now; the container is a snapshot it wrote earlier, and where both answer the
/// live one wins. Here the upload names a frequency and the ID3 names a
/// different one.
#[tokio::test]
async fn the_wire_wins_wherever_the_container_also_answered() {
    let app = TestApp::with_key("k").await;
    let mp3 = SdrTrunkMp3::new().comment("Frequency:99000000;");

    app.upload_ok(CallUpload::sdrtrunk(&mp3).set("frequency", "851012500"))
        .await;
    app.settle().await;

    assert_eq!(app.the_call().await.frequency, Some(851012500));
}

/// A **Unit** an Operator has named keeps that name. Auto-populate fills
/// unknowns and never rewrites curation (#8), and mining is subject to the same
/// rule — otherwise every upload would undo the admin surface (#49).
#[tokio::test]
async fn a_radio_somebody_named_is_not_renamed_by_its_own_recorder() {
    let app = TestApp::with_key("k").await;
    app.seed_unit(11, 1234567, "Battalion 3").await;
    let mp3 = SdrTrunkMp3::new().radio("1234567 Engine 1");

    app.upload_ok(CallUpload::sdrtrunk(&mp3)).await;
    app.settle().await;

    let unit = app.unit_by_ref(11, 1234567).await.expect("the seeded Unit");
    assert_eq!(unit.label.as_deref(), Some("Battalion 3"));
    assert_eq!(
        the_wire_call(&app).await["unitLabel"],
        "Battalion 3",
        "and the curated name is what a Listener is shown"
    );
}

/// ...and a **Site** somebody named keeps its name, for the same reason.
#[tokio::test]
async fn a_tower_somebody_named_is_not_renamed_either() {
    let app = TestApp::with_key("k").await;
    // A Ref-identified tower, named by hand, that a later upload also names.
    app.upload_ok(CallUpload::new().set("site", "4").at(1_000))
        .await;
    app.name_site(11, 4, "Courthouse").await;
    let mp3 = SdrTrunkMp3::new().comment("Site:Downtown;");

    app.upload_ok(CallUpload::sdrtrunk(&mp3).set("site", "4").at(60_000))
        .await;
    app.settle().await;

    assert_eq!(the_wire_call(&app).await["siteLabel"], "Courthouse");
}

/// The tag is one moment's snapshot, and a Call lists the radios *it* heard. A
/// name is applied only to the radio the Call actually carries — otherwise one
/// apparatus's name lands on another's, which is worse than no name at all.
#[tokio::test]
async fn a_name_is_never_hung_on_a_radio_the_call_did_not_hear() {
    let app = TestApp::with_key("k").await;
    let mp3 = SdrTrunkMp3::new().radio("999 Engine 1");

    app.upload_ok(CallUpload::sdrtrunk(&mp3)).await; // the wire says radio 1234567
    app.settle().await;

    assert!(
        app.unit_by_ref(11, 999).await.is_none(),
        "the radio the container named was never heard on this Call"
    );
    assert!(
        app.unit_by_ref(11, 1234567).await.is_none(),
        "...and the radio that *was* heard is still anonymous, so the roster \
         has nothing to hold (#8: a Unit exists once something names it)"
    );
    assert_eq!(
        the_wire_call(&app).await["unitLabel"],
        serde_json::Value::Null,
        "and the Listener is shown no name rather than the wrong one"
    );
}

/// The gate. An Archive is full of MP3s, and `TPE1` on any of them parses as a
/// radio id followed by a name — "50 Cent" is a perfectly good radio 50.
/// Nothing is mined unless the recorder identified itself.
#[tokio::test]
async fn an_mp3_that_does_not_say_sdrtrunk_wrote_it_is_not_mined() {
    let app = TestApp::with_key("k").await;
    let mp3 = SdrTrunkMp3::new()
        .anonymous()
        .radio("1234567 Engine 1")
        .comment("Site:Downtown;");

    app.upload_ok(CallUpload::sdrtrunk(&mp3)).await;
    app.settle().await;

    assert_eq!(app.the_call().await.site_id, None, "no tower was believed");
    assert!(
        app.unit_by_ref(11, 1234567).await.is_none(),
        "and no radio was named, so none was rostered"
    );
}

/// Nothing here may cost a recorder anything. The probe was already running for
/// the Call's duration (#42), so mining is the *same* pass — and an upload with
/// nothing to mine must issue exactly the statements it always did.
#[tokio::test]
async fn mining_a_call_with_nothing_in_it_costs_no_statements_at_all() {
    let app = TestApp::with_key("k").await;
    // Everything exists, so this measures the steady state rather than
    // auto-populate. The radio is the one the SDRTrunk field set names.
    let bare = SdrTrunkMp3::new();
    app.upload_ok(CallUpload::sdrtrunk(&bare).at(1_000)).await;
    app.settle().await;

    let before = app.statements_issued();
    app.upload_ok(CallUpload::sdrtrunk(&bare).at(60_000)).await;
    app.settle().await;
    let bare_cost = app.statements_issued() - before;

    // A plain rdio upload naming the same radio, on the same rows: what a Call
    // costs when no container is mined at all.
    let before = app.statements_issued();
    app.upload_ok(CallUpload::new().set("unit", "1234567").at(120_000))
        .await;
    app.settle().await;
    let plain_cost = app.statements_issued() - before;

    assert_eq!(
        bare_cost, plain_cost,
        "an SDRTrunk Call with nothing configured must cost what any other \
         Call costs — mining rides the probe ingest was already running"
    );
}

// ---------------------------------------------------------------------------
// The Mining sweep Worker (#48): the Archive that was already there.
// ---------------------------------------------------------------------------

/// A Call stored the way one was before any of this existed: audio in the
/// store, and nothing has ever looked inside it.
async fn seed_unmined(app: &TestApp, at_ms: i64, mp3: &SdrTrunkMp3) -> i64 {
    let key = format!("archive/{at_ms}.mp3");
    app.put_object(&key, &mp3.bytes()).await;
    app.seed_call(
        NewCall {
            units: vec![NewCallUnit {
                unit_ref: 1234567,
                ..Default::default()
            }],
            ..NewCall::new(11, 54241, at_ms)
        },
        common::audio_at(key),
    )
    .await
}

/// The other half of the ticket. An Operator who has run SDRTrunk for a year
/// has an Archive full of MP3s with their configured radio aliases and tower
/// names inside, and nothing has ever read one.
#[tokio::test]
async fn the_archive_that_was_already_there_gets_mined() {
    let mut app = TestApp::with_key("k").await;
    let mp3 = SdrTrunkMp3::new()
        .radio("1234567 Engine 1")
        .comment(FULL_COMMENT);
    let id = seed_unmined(&app, 1_000, &mp3).await;

    // A restart is a boot sweep, which is the sweeper's shape: an Instance that
    // restarts more often than the interval still makes progress.
    app.restart().await;
    app.settle().await;

    let call = app.the_call().await;
    assert_eq!(call.audio_type.as_deref(), Some("P25 Phase 1"));
    assert!(call.site_id.is_some(), "a tower was named");
    assert_eq!(
        app.unit_by_ref(11, 1234567)
            .await
            .expect("the radio was rostered")
            .label
            .as_deref(),
        Some("Engine 1")
    );
    assert_eq!(
        app.get_json(&format!("/api/call/{id}")).await["siteLabel"],
        "Downtown"
    );
}

/// **Resumable, and it never reads a Call twice.** With a batch of one, each
/// sweep takes exactly one Call — and the Call the previous sweep finished is
/// not the one it takes.
#[tokio::test]
async fn a_sweep_resumes_where_the_last_one_stopped() {
    let mut app = TestApp::builder()
        .config(|config| config.mining.batch_size = 1)
        .spawn()
        .await;
    let older = seed_unmined(&app, 1_000, &SdrTrunkMp3::new().radio("1 Engine 1")).await;
    let newer = seed_unmined(&app, 60_000, &SdrTrunkMp3::new().radio("1 Ladder 9")).await;

    app.restart().await;
    app.settle().await;

    // Newest first: the half of an Archive somebody is most likely listening to.
    assert!(app.mined_at(newer).await.is_some(), "the newer was taken");
    assert!(app.mined_at(older).await.is_none(), "and only the newer");

    app.restart().await;
    app.settle().await;

    assert!(
        app.mined_at(older).await.is_some(),
        "the next sweep picked up where that one stopped"
    );
}

/// A Call whose audio could not be read is left **unstamped**, so the next
/// sweep tries again — losing a Call to mining forever because a store was
/// briefly unreachable is exactly what the stamp must not do.
#[tokio::test]
async fn a_call_the_store_would_not_answer_for_is_tried_again() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (store, faults) = common::faulty_store(dir.path());
    let mut app = TestApp::builder().store(store).spawn().await;
    let id = seed_unmined(&app, 1_000, &SdrTrunkMp3::new().radio("1234567 Engine 1")).await;

    faults.fail_reads();
    app.restart().await;
    app.settle().await;
    assert!(
        app.mined_at(id).await.is_none(),
        "a Call that could not be read must not be stamped as looked-at"
    );

    faults.allow_reads();
    app.restart().await;
    app.settle().await;

    assert!(app.mined_at(id).await.is_some());
    assert_eq!(
        app.unit_by_ref(11, 1234567)
            .await
            .expect("rostered on the retry")
            .label
            .as_deref(),
        Some("Engine 1")
    );
}

/// A Call the sweep read and found nothing in is stamped all the same — the
/// only thing that makes the sweep terminate. Otherwise every Trunk Recorder
/// Call in the Archive is re-read on every tick, forever.
#[tokio::test]
async fn a_call_with_nothing_in_it_is_never_read_a_second_time() {
    let mut app = TestApp::with_key("k").await;
    let key = "archive/plain.wav";
    app.put_object(key, &common::silence_ms(500)).await;
    let id = app
        .seed_call(NewCall::new(11, 54241, 1_000), common::audio_at(key))
        .await;

    app.restart().await;
    app.settle().await;

    assert!(
        app.mined_at(id).await.is_some(),
        "\"there was nothing in it\" is an answer, and the column records it"
    );
}

/// **Mining is a read.** The Call's audio object is the same bytes afterwards,
/// which is what lets `Cache-Control: immutable` stay true for a Call somebody
/// is already holding a URL for.
#[tokio::test]
async fn mining_never_touches_the_audio() {
    let mut app = TestApp::with_key("k").await;
    let mp3 = SdrTrunkMp3::new().radio("1234567 Engine 1");
    seed_unmined(&app, 1_000, &mp3).await;

    app.restart().await;
    app.settle().await;

    assert_eq!(
        app.object_bytes("archive/1000.mp3").await,
        Some(mp3.bytes()),
        "the object is byte-for-byte what it was"
    );
    assert_eq!(
        app.object_keys().await.len(),
        1,
        "and there is only the one"
    );
}

/// **Nothing is published for a mined Call.** The Listener already has it; a
/// second live frame would play it twice — the same rule a **Replacement**
/// follows (#46).
#[tokio::test]
async fn a_mined_call_is_never_pushed_to_the_live_feed() {
    let mut app = TestApp::with_key("k").await;
    seed_unmined(&app, 1_000, &SdrTrunkMp3::new().radio("1234567 Engine 1")).await;
    app.restart().await;

    let mut ws = app.connect_ws().await;
    subscribe(&mut ws, r#"{"t":"sub","sel":{"11":{"54241":true}}}"#).await;
    app.settle().await;

    no_frame_within(&mut ws, std::time::Duration::from_millis(200)).await;
}

/// **Turned off, nothing sweeps.** An Operator whose audio is on metered object
/// storage gets to decide not to pay for a read per stored Call.
#[tokio::test]
async fn an_operator_can_turn_the_sweep_off() {
    let mut app = TestApp::builder()
        .config(|config| config.mining.sweep = false)
        .spawn()
        .await;
    let id = seed_unmined(&app, 1_000, &SdrTrunkMp3::new().radio("1234567 Engine 1")).await;

    app.restart().await;
    app.settle().await;

    assert!(app.mined_at(id).await.is_none());
    assert!(
        app.workers()
            .loads()
            .iter()
            .all(|reading| reading.name != radio_scout::mining::sweep::WORKER),
        "and there is no Worker to show on a status page"
    );
}

/// **The two writers must not drift.** Mining at ingest assembles a Call that
/// does not exist yet; the sweep edits one that has existed for a year. They
/// are different code, and a field taught to one and not the other is invisible
/// from either side — the failure `tests/trplugin.rs` exists to catch between
/// the upload script and the plugin, in a different place.
///
/// So: the same MP3 down both paths, and the same Call out.
#[tokio::test]
async fn both_paths_land_the_identical_call() {
    let mut app = TestApp::with_key("k").await;
    let mp3 = SdrTrunkMp3::new()
        .radio("1234567 Engine 1")
        .comment(FULL_COMMENT);

    // Down the ingest path, mined in the same pass that reads the duration.
    app.upload_ok(CallUpload::sdrtrunk(&mp3).at(1_000)).await;
    app.settle().await;
    let ingested = mined_shape(&app, app.the_call().await.id).await;

    // ...and the same file, stored the way it would have been before #48 and
    // swept afterwards. Far enough away in time to be its own transmission.
    let seeded = seed_unmined(&app, 3_600_000, &mp3).await;
    app.restart().await;
    app.settle().await;
    let backfilled = mined_shape(&app, seeded).await;

    assert_eq!(ingested, backfilled);
}

/// Everything **Mining** can put on a Call, as a Listener is shown it.
async fn mined_shape(app: &TestApp, id: i64) -> serde_json::Value {
    let detail = app.get_json(&format!("/api/call/{id}")).await;
    serde_json::json!({
        "frequency": detail["frequency"],
        "siteLabel": detail["siteLabel"],
        "unitLabel": detail["unitLabel"],
        "units": detail["units"],
    })
}

/// A tower a recorder identified by **Ref** gets its name from the container —
/// the case where both halves arrive, and the Ref decides *which* tower while
/// the name decides what it is called.
#[tokio::test]
async fn a_tower_named_by_ref_gains_the_name_the_audio_carried() {
    let app = TestApp::with_key("k").await;
    let mp3 = SdrTrunkMp3::new().comment("Site:Downtown;");

    // Discovered and named in one go.
    app.upload_ok(CallUpload::sdrtrunk(&mp3).set("site", "4").at(1_000))
        .await;
    app.settle().await;

    let call = the_wire_call(&app).await;
    assert_eq!(call["siteRef"], 4, "the Recorder's Ref, not a minted one");
    assert_eq!(call["siteLabel"], "Downtown");

    // ...and a tower that has been a bare number for a year — every Site in
    // every archive that predates #48 — is named the first time a Call names it.
    app.upload_ok(CallUpload::new().set("site", "9").at(60_000))
        .await;
    app.upload_ok(
        CallUpload::sdrtrunk(&SdrTrunkMp3::new().comment("Site:North Ridge;"))
            .set("site", "9")
            .at(120_000),
    )
    .await;
    app.settle().await;

    let named = &app.get_json("/api/calls").await["results"][0];
    assert_eq!(named["siteRef"], 9);
    assert_eq!(named["siteLabel"], "North Ridge");
}

/// The sweep's own "nothing to hang a name on" arms. Each leaves the Call
/// alone and stamps it, so a sweep never comes back to it — and none of them
/// puts a name where it does not belong.
#[rstest]
#[case::the_tag_names_no_radio("Site:Downtown;", None, None)]
#[case::a_radio_the_call_never_heard("Site:Downtown;", Some("999 Engine 1"), None)]
#[tokio::test]
async fn the_sweep_leaves_a_radio_alone_when_it_cannot_name_it(
    #[case] comment: &str,
    #[case] radio: Option<&str>,
    #[case] expected: Option<&str>,
) {
    let mut app = TestApp::with_key("k").await;
    let mut mp3 = SdrTrunkMp3::new().comment(comment);
    if let Some(radio) = radio {
        mp3 = mp3.radio(radio);
    }
    let id = seed_unmined(&app, 1_000, &mp3).await;

    app.restart().await;
    app.settle().await;

    assert!(
        app.mined_at(id).await.is_some(),
        "and it is never read again"
    );
    assert_eq!(
        app.get_json(&format!("/api/call/{id}")).await["units"][0]["label"].as_str(),
        expected
    );
}

/// A radio this Call's Recorder already named keeps that name, on the backfill
/// path as on the ingest one — the Recorder said it about *this* transmission,
/// and the container is the older answer.
#[tokio::test]
async fn the_sweep_never_renames_a_radio_the_recorder_already_named() {
    let mut app = TestApp::with_key("k").await;
    let key = "archive/named.mp3";
    app.put_object(key, &SdrTrunkMp3::new().radio("1234567 Engine 1").bytes())
        .await;
    let id = app
        .seed_call(
            NewCall {
                units: vec![NewCallUnit {
                    unit_ref: 1234567,
                    label: Some("Battalion 3".into()),
                    ..Default::default()
                }],
                ..NewCall::new(11, 54241, 1_000)
            },
            common::audio_at(key),
        )
        .await;

    app.restart().await;
    app.settle().await;

    assert_eq!(
        app.get_json(&format!("/api/call/{id}")).await["units"][0]["label"],
        "Battalion 3"
    );
}

/// **Auto-populate gates the roster on the sweep path too** (#8). With the
/// instance-wide flag off, only a System that opted back in gets its radios
/// rostered — the Call's own `call_units` row is named either way, because that
/// records what the Recorder said rather than creating an entity.
#[rstest]
#[case::the_system_opted_back_in(true, Some("Engine 1"))]
#[case::nobody_wants_new_entities(false, None)]
#[tokio::test]
async fn the_sweep_rosters_only_where_auto_populate_says_so(
    #[case] system_populates: bool,
    #[case] rostered: Option<&str>,
) {
    let mut app = TestApp::builder()
        .ingest(radio_scout::IngestConfig {
            auto_populate: false,
            ..Default::default()
        })
        .spawn()
        .await;
    app.seed_system(11, system_populates, None).await;
    let id = seed_unmined(&app, 1_000, &SdrTrunkMp3::new().radio("1234567 Engine 1")).await;

    app.restart().await;
    app.settle().await;

    assert_eq!(
        app.unit_by_ref(11, 1234567)
            .await
            .and_then(|unit| unit.label)
            .as_deref(),
        rostered
    );
    assert_eq!(
        app.get_json(&format!("/api/call/{id}")).await["units"][0]["label"],
        "Engine 1",
        "the Call still records what its Recorder called the radio"
    );
}

/// **The other half of "a Ref identifies and a name names"**, on the sweep's
/// path. A Call that already points at a tower keeps that tower — but the tower
/// still gains a name if it has none.
///
/// This is the divergence `both_paths_land_the_identical_call` cannot catch:
/// SDRTrunk sends no `site`, so no Call that goes down both paths ever has one,
/// and the two writers could disagree here forever without a test noticing.
#[tokio::test]
async fn the_sweep_names_a_tower_the_call_already_points_at() {
    let mut app = TestApp::with_key("k").await;
    // A Ref-identified, unnamed tower — every Site in an archive predating #48.
    app.upload_ok(CallUpload::new().set("site", "4").at(1_000))
        .await;
    let key = "archive/tower.mp3";
    app.put_object(key, &SdrTrunkMp3::new().comment("Site:Downtown;").bytes())
        .await;
    let id = app
        .seed_call(
            NewCall {
                site_ref: Some(4),
                ..NewCall::new(11, 54241, 3_600_000)
            },
            common::audio_at(key),
        )
        .await;

    app.restart().await;
    app.settle().await;

    let call = app.get_json(&format!("/api/call/{id}")).await;
    assert_eq!(call["siteRef"], 4, "the Recorder's tower, not a minted one");
    assert_eq!(call["siteLabel"], "Downtown", "named from the audio");
}

/// **What one Call costs the sweep, exactly.**
///
/// The sweep reads its Calls one at a time by construction — an object each, a
/// row each — so the invariant `tests/live.rs` holds the Backfill to (a page
/// costs a constant) is the wrong one here. What must not happen is a *hidden*
/// per-Call query creeping in behind the obvious ones, which is the N+1 #86
/// deleted from the denormalizer. So the marginal cost of one mined Call is
/// pinned outright, with its breakdown, the way `tests/ingest.rs` pins one
/// steady-state upload.
///
/// Measured as the **difference between two sweeps** rather than against a
/// single reading: a sweep only runs at boot here (the interval is the shipped
/// thirty seconds), and a boot issues statements of its own that have nothing
/// to do with mining.
#[tokio::test]
async fn mining_one_stored_call_costs_a_fixed_number_of_statements() {
    let mut app = TestApp::builder()
        .config(|config| config.mining.batch_size = 1)
        .spawn()
        .await;
    // Two Calls off the same tower and the same radio, so the second one is the
    // steady state: nothing is created for it, only filled in.
    let mp3 = SdrTrunkMp3::new()
        .radio("1234567 Engine 1")
        .comment(FULL_COMMENT);
    seed_unmined(&app, 1_000, &mp3).await;
    seed_unmined(&app, 60_000, &mp3).await;

    // The first sweep takes the newer Call and creates the Site and the Unit.
    app.restart().await;
    app.settle().await;

    // The second takes the older one, with everything it needs already there.
    let before = app.statements_issued();
    app.restart().await;
    app.settle().await;
    let mining_a_call = app.statements_issued() - before;

    // The third finds nothing left — the same boot, and no Call.
    let before = app.statements_issued();
    app.restart().await;
    app.settle().await;
    let mining_nothing = app.statements_issued() - before;

    // SQLite reads a row back after each write; Postgres returns it from the
    // statement itself (ADR-0003). Two writes here — the Call and its
    // `call_units` row — so two statements of dialect difference.
    let read_backs = match app.db.get_database_backend() {
        sea_orm::DatabaseBackend::Postgres => 0,
        _ => 2,
    };

    assert_eq!(
        mining_a_call - mining_nothing,
        6 + read_backs,
        "one mined Call, over a sweep that mined none: the Site resolved by \
         name; the Call row updated; the `call_units` row found and updated (+ \
         a read-back each on SQLite); the Unit the roster resolves the radio \
         to; and the statement that stamps the page looked-at, which the empty \
         sweep skips. A number that grows here is a query running once per Call \
         over an archive of hundreds of thousands. \
         ({mining_a_call} with a Call, {mining_nothing} without)"
    );
}

/// ...and a tower somebody *has* named keeps its name on the sweep's path too.
/// The mirror of `a_tower_somebody_named_is_not_renamed_either`, which is the
/// ingest side of the same rule.
#[tokio::test]
async fn the_sweep_never_renames_a_tower_somebody_named() {
    let mut app = TestApp::with_key("k").await;
    app.upload_ok(CallUpload::new().set("site", "4").at(1_000))
        .await;
    app.name_site(11, 4, "Courthouse").await;
    let key = "archive/named-tower.mp3";
    app.put_object(key, &SdrTrunkMp3::new().comment("Site:Downtown;").bytes())
        .await;
    let id = app
        .seed_call(
            NewCall {
                site_ref: Some(4),
                ..NewCall::new(11, 54241, 3_600_000)
            },
            common::audio_at(key),
        )
        .await;

    app.restart().await;
    app.settle().await;

    assert_eq!(
        app.get_json(&format!("/api/call/{id}")).await["siteLabel"],
        "Courthouse"
    );
}
