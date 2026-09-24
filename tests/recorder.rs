//! **The recorder dashboard and RF health** (#71, spec US 50–51), driven over
//! the real WebSocket boundary by a fake Trunk Recorder.
//!
//! Every frame here is what `stat_socket.cc` really sends, with every scalar a
//! string, because that is what `boost::property_tree::write_json` produces and
//! a fixture in any other shape would prove the parser against a dialect no
//! recorder speaks. The pure halves — reading a frame, folding a dashboard, dividing
//! a chart — are unit-tested beside the code; what these prove is that a
//! recorder dialling a running Instance ends up on an Operator's screen.

use radio_scout::db::entities::frequency_health;
use sea_orm::EntityTrait;

mod common;
use common::logs::LogCapture;
use common::{CallUpload, TestApp, call_frame};

// ---------------------------------------------------------------------------
// Dialing in (spec US 50)
// ---------------------------------------------------------------------------

/// The ticket's first criterion, end to end: a recorder connects, sends what it
/// is, and the live view renders its truth.
#[tokio::test]
async fn a_recorder_dials_in_and_the_dashboard_shows_what_it_said() {
    let app = TestApp::with_key("k").await;
    app.login().await;

    let mut recorder = app.dial_recorder("k").await;
    recorder.open("butco-pi").await;
    recorder.rates("39.3").await;
    let dashboard = app
        .recorders_until(|dashboard| dashboard["recorders"][0]["systems"][0]["decodeRate"] == 39.3)
        .await;

    let seen = &dashboard["recorders"][0];
    assert_eq!(seen["name"], "butco-pi");
    assert_eq!(seen["connected"], true);
    assert_eq!(seen["captureDir"], "/captures");

    let sdr = &seen["sdrs"][0];
    assert_eq!(sdr["sourceNum"], 0);
    assert_eq!(sdr["device"], "rtl=0");
    assert_eq!(sdr["demodulators"], 4, "analog plus digital");

    let system = &seen["systems"][0];
    assert_eq!(system["shortName"], "butco");
    assert_eq!(system["sysid"], "123");
    assert_eq!(system["controlChannels"][0], 774031250.0);
    assert_eq!(system["decodeRate"], 39.3);

    let demodulators = seen["demodulators"].as_array().expect("demodulators");
    assert_eq!(demodulators.len(), 2);
    assert_eq!(demodulators[0]["state"], "recording");
    assert_eq!(demodulators[1]["state"], "available");
    assert!(
        dashboard["atMs"]
            .as_i64()
            .expect("the moment the dashboard was read")
            > 0,
        "every age on this screen is a subtraction from the server's own clock"
    );
}

/// **Why-not-recorded**, which is the ticket's second criterion. The three it
/// names are here, and so is one it does not: `no-recorder`, which is a receiver
/// that has run out of demodulators and the one an Operator can actually fix.
#[tokio::test]
async fn the_dashboard_says_why_a_transmission_was_not_recorded() {
    let app = TestApp::with_key("k").await;
    app.login().await;

    let mut recorder = app.dial_recorder("k").await;
    recorder.open("butco-pi").await;
    recorder
        .calls_active(&format!(
            "{},{},{},{}",
            call_frame("a", 101, 1, 0),
            call_frame("b", 102, 0, 5),
            call_frame("c", 103, 0, 4),
            call_frame("d", 104, 0, 6),
        ))
        .await;
    let dashboard = app
        .recorders_until(|dashboard| {
            dashboard["recorders"][0]["calls"]
                .as_array()
                .is_some_and(|calls| calls.len() == 4)
        })
        .await;

    let seen = &dashboard["recorders"][0];
    let reasons: Vec<&str> = seen["calls"]
        .as_array()
        .expect("the active calls")
        .iter()
        .map(|call| call["notRecorded"].as_str().unwrap_or("recorded"))
        .collect();
    assert_eq!(
        reasons.len(),
        4,
        "every call the recorder has in hand, recorded or not"
    );
    for expected in ["encrypted", "no-recorder", "duplicate"] {
        assert!(reasons.contains(&expected), "{reasons:?}");
    }
    assert!(
        reasons.contains(&"recorded"),
        "the one that is being recorded explains nothing: {reasons:?}"
    );

    let tally = seen["notRecorded"].as_array().expect("the tally");
    assert_eq!(tally.len(), 3, "one entry per reason, not per call");
    assert_eq!(tally[0]["count"], 1);
    assert_eq!(tally[0]["lastTalkgroupLabel"], "FIRE DISPATCH");
}

/// **Disconnect is visible** — the fourth criterion. A row that simply vanished
/// would be indistinguishable from a recorder that was never set up, which is
/// the failure this exists to avoid.
#[tokio::test]
async fn a_recorder_that_hangs_up_is_still_shown_and_marked() {
    let app = TestApp::with_key("k").await;
    app.login().await;

    let mut recorder = app.dial_recorder("k").await;
    recorder.open("butco-pi").await;
    opened(&app, &["butco-pi"]).await;
    recorder.hang_up().await;

    let dashboard = gone(&app).await;
    let seen = &dashboard["recorders"][0];
    assert_eq!(seen["name"], "butco-pi");
    assert_eq!(seen["connected"], false);
    assert!(
        seen["disconnectedAtMs"].as_i64().is_some(),
        "and when it went: {seen}"
    );
}

/// **Nothing breaks when no recorder dials in** — the fourth criterion's other
/// half, and the state every Instance is in until an Operator rebuilds Trunk
/// Recorder with the status plugin.
#[tokio::test]
async fn an_instance_nobody_dialed_into_answers_an_empty_fleet() {
    let app = TestApp::with_key("k").await;
    app.login().await;

    let dashboard = app.recorders().await;

    assert_eq!(
        dashboard["recorders"].as_array().expect("an array").len(),
        0
    );
    assert!(dashboard["atMs"].as_i64().is_some());
}

/// The credential is the **API key** the recorder already holds, and it rides in
/// the query string because Trunk Recorder's bare `statusServer` URL can carry
/// nothing else. A bad one is refused **before** the upgrade, so it is an
/// ordinary `401` rather than a frame sent down a socket already open.
#[tokio::test]
async fn a_recorder_without_a_key_is_refused_before_the_upgrade() {
    let app = TestApp::with_key("k").await;

    assert_eq!(app.dial_recorder_refused("wrong-key").await, 401);
    assert_eq!(app.dial_recorder_refused("").await, 401);
}

/// A key an Operator revoked stops being a way in — the roster is the one
/// `crate::ingest` reads, so a revoke is a revoke everywhere.
#[tokio::test]
async fn a_disabled_key_cannot_dial_in() {
    let app = TestApp::spawn().await;
    app.login().await;
    let issued = app
        .admin_post("/api/admin/api-keys", serde_json::json!({}))
        .await
        .1;
    let secret = issued["key"].as_str().expect("a key").to_string();
    let id = issued["id"].as_i64().expect("the key's id");

    app.dial_recorder(&secret).await;

    let (status, body) = app
        .admin_patch(
            &format!("/api/admin/api-keys/{id}"),
            serde_json::json!({"disabled": true}),
        )
        .await;
    assert_eq!(status, 200, "{body:?}");

    assert_eq!(app.dial_recorder_refused(&secret).await, 401);
}

/// The dashboard is the Operator's, so it takes the admin session — the
/// listener chart's rule (#62), and for its reason.
#[tokio::test]
async fn the_dashboard_is_behind_the_admin_session() {
    let app = TestApp::with_key("k").await;

    for path in ["/api/admin/recorders", "/api/admin/recorders/health"] {
        assert_eq!(app.get(path).await.status(), 401, "{path}");
    }
}

/// A refused dial says **why**, in the one vocabulary every refusal on this
/// Instance uses (#92) — and never the key, at any level, in any form.
#[tokio::test]
async fn a_refused_dial_says_why_and_never_says_the_key() {
    let app = TestApp::with_key("k").await;
    let capture = LogCapture::start();

    app.dial_recorder_refused("hunter2").await;

    let logged = capture.text();
    assert!(logged.contains("reason=invalid-recorder-key"), "{logged}");
    assert!(logged.contains("WARN"), "{logged}");
    assert!(
        !logged.contains("hunter2"),
        "a credential never reaches a log line: {logged}"
    );
}

/// A frame this release cannot read does not close the connection and does not
/// write a line per frame — a recorder speaking an unknown dialect would
/// otherwise log every three seconds for as long as it is plugged in.
#[tokio::test]
async fn an_unreadable_frame_does_not_end_the_connection() {
    let app = TestApp::with_key("k").await;
    app.login().await;

    let mut recorder = app.dial_recorder("k").await;
    recorder.send("this is not json").await;
    recorder.open("butco-pi").await;

    let dashboard = opened(&app, &["butco-pi"]).await;
    assert_eq!(dashboard["recorders"][0]["name"], "butco-pi");
}

/// **A recorder that holds its socket open and says nothing is reaped, and
/// the Operator is told** — over a real socket, with tokio's clock paused once
/// it is connected, so the shipped fifteen-second heartbeat costs nothing. It
/// answers every ping throughout: a pong is not proof of life (see
/// `recorder::ws`), and this is the case that says so end to end.
#[tokio::test]
async fn a_recorder_that_stops_talking_is_reaped_and_the_operator_is_told() {
    let app = TestApp::with_key("k").await;
    app.login().await;
    let capture = LogCapture::start();

    let mut recorder = app.dial_recorder("k").await;
    recorder.open("butco-pi").await;
    opened(&app, &["butco-pi"]).await;

    tokio::time::pause();
    let reaped = recorder
        .hung_up_on_within(std::time::Duration::from_secs(120))
        .await;
    tokio::time::resume();

    assert!(reaped, "a silent recorder is hung up on");
    let dashboard = gone(&app).await;
    assert_eq!(dashboard["recorders"][0]["name"], "butco-pi");
    let logged = capture.text();
    assert!(logged.contains("recorder stopped reporting"), "{logged}");
    assert!(logged.contains("WARN"), "{logged}");
}

/// Two recorders are two rows — an Operator with a receiver in two counties is
/// exactly who this screen is for.
#[tokio::test]
async fn two_recorders_are_two_rows() {
    let app = TestApp::with_key("k").await;
    app.login().await;

    let mut first = app.dial_recorder("k").await;
    first.open("butco-pi").await;
    let mut second = app.dial_recorder("k").await;
    second.open("hamco-pi").await;

    let dashboard = opened(&app, &["butco-pi", "hamco-pi"]).await;
    let names: Vec<&str> = dashboard["recorders"]
        .as_array()
        .expect("an array")
        .iter()
        .map(|seen| seen["name"].as_str().unwrap_or_default())
        .collect();
    assert!(names.contains(&"butco-pi"), "{names:?}");
    assert!(names.contains(&"hamco-pi"), "{names:?}");
}

// ---------------------------------------------------------------------------
// The health charts (spec US 51)
// ---------------------------------------------------------------------------

/// Trunk Recorder's own meta, with the four radio-condition numbers #42 parsed
/// around and never read.
fn tr_meta(at_s: i64, freq: i64, source: i64, errors: i64, signal: i64) -> String {
    format!(
        r#"{{"short_name":"butco","talkgroup":54241,
        "start_time":{at_s},"call_length_ms":4000,
        "freq":{freq},"freq_error":-137,"signal":{signal},"noise":-94,
        "source_num":{source},"recorder_num":3,
        "freqList":[{{"freq":{freq},"time":{at_s},"pos":0,"len":4,
                     "error_count":{errors},"spike_count":1}}]}}"#
    )
}

/// The charts, end to end: Calls arrive, and how each frequency received them
/// becomes a series an Operator can look at.
#[tokio::test]
async fn the_health_report_charts_what_the_calls_said() {
    let app = TestApp::with_key("k").await;
    app.login().await;

    // Two transmissions a minute apart, on one frequency, from one SDR.
    app.upload_tr(CallUpload::tr(&tr_meta(
        1_669_740_000,
        774031250,
        0,
        2,
        -61,
    )))
    .await;
    app.upload_tr(CallUpload::tr(&tr_meta(
        1_669_740_060,
        774031250,
        0,
        4,
        -65,
    )))
    .await;

    let (status, report) = app.admin_get("/api/admin/recorders/health").await;
    assert_eq!(status, 200, "{report:?}");

    let channel = &report["channels"][0];
    assert_eq!(channel["freq"], 774031250);
    assert_eq!(channel["sdr"], 0);
    assert_eq!(channel["samples"], 2);
    assert_eq!(channel["errorCount"], 6);
    assert_eq!(channel["airMs"], 8_000);
    assert_eq!(
        report["finestBucketMs"], 900_000,
        "a chart can never be finer than the rollup it is drawn from"
    );

    // Eight seconds of air with six errors is forty-five errors a minute.
    let rate: f64 = plotted(&channel["errorRate"])[0];
    assert!((rate - 45.0).abs() < 1e-9, "{rate}");
    // And the two signal readings average.
    let signal: f64 = plotted(&channel["signalDbm"])[0];
    assert!((signal - -63.0).abs() < 1e-9, "{signal}");
    let drift: f64 = plotted(&channel["driftHz"])[0];
    assert!((drift - -137.0).abs() < 1e-9, "signed, so it is a drift");
}

/// **Two SDRs on one frequency are two histories**, which is the whole of "a
/// dying dongle announces itself": the other one is fine, and only a per-device
/// chart can show that.
#[tokio::test]
async fn two_sdrs_on_one_frequency_are_two_channels() {
    let app = TestApp::with_key("k").await;
    app.login().await;

    app.upload_tr(CallUpload::tr(&tr_meta(
        1_669_740_000,
        774031250,
        0,
        40,
        -61,
    )))
    .await;
    app.upload_tr(CallUpload::tr(&tr_meta(
        1_669_740_060,
        774031250,
        1,
        1,
        -61,
    )))
    .await;

    let report = app.admin_get("/api/admin/recorders/health").await.1;
    let channels = report["channels"].as_array().expect("channels");

    assert_eq!(channels.len(), 2);
    // Busiest first is a tie here, so the SDR breaks it — and the errors are
    // what tell the two apart.
    assert_eq!(channels[0]["sdr"], 0);
    assert_eq!(channels[0]["errorCount"], 40);
    assert_eq!(channels[1]["sdr"], 1);
    assert_eq!(channels[1]["errorCount"], 1);
}

/// **A Copy that loses still counts** — the dying-dongle case exactly: a second
/// SDR decodes the same transmission worse, and its copy is refused as a
/// duplicate. Refused or not, it was *received*, and how badly is the evidence.
/// Before this the same copy counted only if it happened to arrive first and be
/// replaced, so the chart depended on upload order.
#[tokio::test]
async fn a_duplicate_that_lost_still_counts_toward_its_sdr() {
    let app = TestApp::with_key("k").await;
    app.login().await;

    let (_, first) = app
        .upload_tr(CallUpload::tr(&tr_meta(
            1_669_740_000,
            774031250,
            0,
            1,
            -61,
        )))
        .await;
    let (_, second) = app
        .upload_tr(CallUpload::tr(&tr_meta(
            1_669_740_000,
            774031250,
            1,
            40,
            -80,
        )))
        .await;
    assert_eq!(first, "Call imported successfully.\n");
    assert_eq!(second, "duplicate call rejected\n", "the worse copy loses");
    assert_eq!(
        app.count::<radio_scout::db::entities::call::Entity>().await,
        1
    );

    let report = app.admin_get("/api/admin/recorders/health").await.1;
    let channels = report["channels"].as_array().expect("channels");
    assert_eq!(channels.len(), 2, "one history per SDR: {channels:?}");
    let worse = channels
        .iter()
        .find(|channel| channel["sdr"] == 1)
        .expect("the losing SDR is charted");
    assert_eq!(worse["errorCount"], 40);
}

/// A duplicate's health that cannot be written is a line, not a failed upload:
/// the recorder is told the truth about its copy, and a `500` would only have
/// it retry — counting the same reception twice.
#[tokio::test]
async fn a_duplicate_whose_health_cannot_be_written_is_still_a_duplicate() {
    let app = TestApp::with_key("k").await;
    let capture = LogCapture::start();
    app.upload_tr(CallUpload::tr(&tr_meta(
        1_669_740_000,
        774031250,
        0,
        1,
        -61,
    )))
    .await;

    app.refuse_statements_on("frequency_health");
    let (status, body) = app
        .upload_tr(CallUpload::tr(&tr_meta(
            1_669_740_000,
            774031250,
            1,
            40,
            -80,
        )))
        .await;

    assert_eq!((status, body.as_str()), (200, "duplicate call rejected\n"));
    let logged = capture.text();
    assert!(
        logged.contains("receive health for a duplicate was not recorded"),
        "{logged}"
    );
    assert!(logged.contains("WARN"), "{logged}");
}

/// A bucket nothing was heard in is **`null`**, never `0`: a chart that drew "we
/// did not measure" as "a perfect zero-error minute" would say something false
/// about a receiver that may have been dead.
#[tokio::test]
async fn a_bucket_with_no_air_is_not_a_perfect_score() {
    let app = TestApp::with_key("k").await;
    app.login().await;

    app.upload_tr(CallUpload::tr(&tr_meta(
        1_669_740_000,
        774031250,
        0,
        2,
        -61,
    )))
    .await;

    // An hour of axis around one quarter-hour of traffic.
    let report = app
        .admin_get(
            "/api/admin/recorders/health\
             ?after=1669737600000&before=1669741199999&bucketMs=900000",
        )
        .await
        .1;
    let rate = report["channels"][0]["errorRate"]
        .as_array()
        .expect("a trace");

    assert_eq!(rate.len(), 4, "four quarter-hours");
    assert_eq!(
        rate.iter().filter(|value| value.is_null()).count(),
        3,
        "three of them measured nothing: {rate:?}"
    );
}

/// A Call in the rdio dialect says nothing about which SDR heard it, so its
/// channel carries no `sdr` at all rather than the sentinel the row is keyed
/// on.
#[tokio::test]
async fn a_call_with_no_sdr_charts_without_one() {
    let app = TestApp::with_key("k").await;
    app.login().await;

    app.upload_ok(CallUpload::new().set("frequency", "851012500"))
        .await;

    let report = app.admin_get("/api/admin/recorders/health").await.1;
    let channel = &report["channels"][0];

    assert_eq!(channel["freq"], 851012500);
    assert!(channel.get("sdr").is_none(), "{channel}");
}

/// A grain finer than the rollup would draw bars that are empty by
/// construction, so it is **widened** rather than refused — `MAX_BUCKETS`' own
/// bargain, and the answer says what width it really used.
#[tokio::test]
async fn a_grain_finer_than_the_rollup_is_widened() {
    let app = TestApp::with_key("k").await;
    app.login().await;
    app.upload_tr(CallUpload::tr(&tr_meta(
        1_669_740_000,
        774031250,
        0,
        2,
        -61,
    )))
    .await;

    let report = app
        .admin_get(
            "/api/admin/recorders/health\
             ?after=1669737600000&before=1669741199999&bucketMs=1000",
        )
        .await
        .1;

    assert_eq!(report["bucketMs"], 900_000);
}

/// An Instance that has taken no Calls answers a report rather than failing —
/// which is what the screen looks like on the day it is switched on.
#[tokio::test]
async fn an_empty_history_is_an_empty_report() {
    let app = TestApp::with_key("k").await;
    app.login().await;

    let report = app.admin_get("/api/admin/recorders/health").await.1;

    assert_eq!(report["channels"].as_array().expect("an array").len(), 0);
    assert_eq!(report["omitted"], 0);
    assert!(report["toMs"].as_i64().expect("an axis") > 0);
}

/// A filter that cannot be read is refused **by name**, never coerced —
/// `crate::query`'s convention, and the opposite of rdio-scanner's silent
/// shrug.
#[tokio::test]
async fn an_unreadable_filter_is_refused_by_name() {
    let app = TestApp::with_key("k").await;
    app.login().await;

    let (status, body) = app
        .admin_get("/api/admin/recorders/health?freq=not-a-number")
        .await;

    assert_eq!(status, 400);
    assert!(format!("{body}").contains("freq"), "{body:?}");
}

/// The report can be narrowed to one System, one frequency or one SDR — which
/// is what a chart does when an Operator taps a row.
#[tokio::test]
async fn a_report_can_be_narrowed() {
    let app = TestApp::with_key("k").await;
    app.login().await;
    app.upload_tr(CallUpload::tr(&tr_meta(
        1_669_740_000,
        774031250,
        0,
        2,
        -61,
    )))
    .await;
    app.upload_tr(CallUpload::tr(&tr_meta(
        1_669_740_060,
        851012500,
        1,
        2,
        -61,
    )))
    .await;

    let narrowed = app
        .admin_get("/api/admin/recorders/health?freq=851012500")
        .await
        .1;
    assert_eq!(narrowed["channels"].as_array().expect("channels").len(), 1);
    assert_eq!(narrowed["channels"][0]["freq"], 851012500);

    let by_sdr = app.admin_get("/api/admin/recorders/health?sdr=0").await.1;
    assert_eq!(by_sdr["channels"][0]["freq"], 774031250);

    let system = app
        .admin_get("/api/admin/recorders/health?system=1")
        .await
        .1;
    assert_eq!(
        system["channels"].as_array().expect("channels").len(),
        2,
        "both frequencies are on the one System Trunk Recorder named"
    );
    let nobody = app
        .admin_get("/api/admin/recorders/health?system=999")
        .await
        .1;
    assert_eq!(nobody["channels"].as_array().expect("channels").len(), 0);
}

/// **A dated report costs one statement; an undated one pays a second to
/// discover its own axis.** Asserted as the difference, `tests/archive.rs`'s
/// rule, so a screen that says where it is looking cannot start paying to be
/// told what it already said.
#[tokio::test]
async fn naming_both_bounds_saves_the_axis_query() {
    let app = TestApp::with_key("k").await;
    app.login().await;
    app.upload_tr(CallUpload::tr(&tr_meta(
        1_669_740_000,
        774031250,
        0,
        2,
        -61,
    )))
    .await;
    // Or the quiet scanner's own reads (#59) land inside the window below and
    // the difference measures a Worker rather than this handler.
    app.settle().await;

    let before = app.statements_issued();
    app.admin_get("/api/admin/recorders/health").await;
    let discovered = app.statements_issued() - before;

    let before = app.statements_issued();
    app.admin_get("/api/admin/recorders/health?after=1669737600000&before=1669741199999")
        .await;
    let dated = app.statements_issued() - before;

    assert_eq!(
        discovered,
        dated + 1,
        "one extra statement to discover the axis, and one only"
    );
}

/// The history **outlives the audio**, which is the whole reason it is a rollup
/// and not a query over `call_frequencies`: Retention takes the Call and the
/// receive conditions stay.
#[tokio::test]
async fn the_history_survives_the_calls_it_was_measured_from() {
    let app = TestApp::with_key("k").await;
    app.login().await;
    app.upload_tr(CallUpload::tr(&tr_meta(
        1_669_740_000,
        774031250,
        0,
        2,
        -61,
    )))
    .await;
    assert_eq!(app.count::<frequency_health::Entity>().await, 1);

    let system = app.the_call().await.system_id;
    let (status, body) = app
        .admin_delete(&format!("/api/admin/talkgroups/{}?force=true", {
            let talkgroup = radio_scout::db::entities::talkgroup::Entity::find()
                .one(&app.db)
                .await
                .unwrap()
                .expect("the talkgroup");
            assert_eq!(talkgroup.system_id, system);
            talkgroup.id
        }))
        .await;
    assert_eq!(status, 204, "{body:?}");

    assert_eq!(
        app.count::<radio_scout::db::entities::call::Entity>().await,
        0,
        "the Calls are gone"
    );
    assert_eq!(
        app.count::<frequency_health::Entity>().await,
        1,
        "and how the receiver was doing that afternoon is not"
    );
}

/// …and it is still bounded: a window of its own, swept by the sweeper that
/// bounds the Archive, or a quarter-hour row per frequency per SDR would
/// accumulate on a Pi's SD card for good.
#[tokio::test]
async fn old_health_is_pruned_on_its_own_window() {
    let (now_s, left) = health_left_after_sweeping(1).await;

    assert_eq!(
        left.len(),
        1,
        "the three-day-old quarter-hour went: {left:?}"
    );
    assert!(
        left[0] > (now_s - 86_400) * 1_000,
        "and the recent one stayed"
    );
}

/// `health_days = 0` is "keep forever", the reading every window in
/// `[retention]` has.
#[tokio::test]
async fn a_zero_health_window_keeps_every_row() {
    let (_, left) = health_left_after_sweeping(0).await;

    assert_eq!(left.len(), 2, "{left:?}");
}

/// Upload a Call three days old and one a minute old, let the sweeper run
/// twice under `health_days`, and say which quarter-hours are left — having
/// checked that no Call went, so the rollup is the only thing that moved.
async fn health_left_after_sweeping(health_days: u32) -> (i64, Vec<i64>) {
    let app = TestApp::builder()
        .config(move |config| {
            config.retention.health_days = health_days;
            // Fast enough to wait on in sweeps rather than in time (#93).
            config.retention.interval = std::time::Duration::from_millis(50);
            // Nothing else may prune here, or another policy could be what made
            // the row disappear.
            config.retention.days = 0;
        })
        .spawn()
        .await;
    app.create_api_key("k").await;

    let now_s = radio_scout::now_ms() / 1_000;
    for at_s in [now_s - 3 * 86_400, now_s - 60] {
        app.upload_tr(CallUpload::tr(&tr_meta(at_s, 774031250, 0, 2, -61)))
            .await;
    }
    // No count here: the sweeper is already ticking every 50ms, so the old row
    // may be gone before a count could see it. That both rows are written is
    // `a_zero_health_window_keeps_every_row`'s half of this helper.

    let sweeper = app
        .workers()
        .meter(radio_scout::retention::WORKER)
        .expect("the sweeper is a registered Worker");
    let sweeps = sweeper.load().done;
    sweeper.settled_at_least(sweeps + 2).await;

    assert_eq!(
        app.count::<radio_scout::db::entities::call::Entity>().await,
        2,
        "no Call is touched"
    );
    let left = frequency_health::Entity::find()
        .all(&app.db)
        .await
        .expect("read the rollup")
        .into_iter()
        .map(|row| row.bucket_at_ms)
        .collect();
    (now_s, left)
}

// ---------------------------------------------------------------------------
// Waiting
// ---------------------------------------------------------------------------

/// Wait until every named recorder is connected and has finished opening — its
/// demodulators are the last thing `FakeRecorder::open` sends, so once they are
/// on the dashboard everything before them is too.
async fn opened(app: &TestApp, names: &[&str]) -> serde_json::Value {
    app.recorders_until(|dashboard| {
        names.iter().all(|name| {
            dashboard["recorders"].as_array().is_some_and(|rows| {
                rows.iter().any(|row| {
                    row["name"] == *name
                        && row["connected"] == true
                        && row["demodulators"].as_array().is_some_and(|d| d.len() == 2)
                })
            })
        })
    })
    .await
}

/// The same, for a recorder that has hung up.
async fn gone(app: &TestApp) -> serde_json::Value {
    app.recorders_until(|dashboard| {
        dashboard["recorders"]
            .as_array()
            .is_some_and(|rows| rows.iter().any(|row| row["connected"] == false))
    })
    .await
}

/// The values a trace really plotted, with the `null`s dropped.
fn plotted(trace: &serde_json::Value) -> Vec<f64> {
    trace
        .as_array()
        .expect("a trace")
        .iter()
        .filter_map(serde_json::Value::as_f64)
        .collect()
}
