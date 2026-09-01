//! Listener counts over time (#62, spec US 41): the count a live-feed
//! connection joins, the **Worker** that writes it down, the chart it is read
//! back as, and the promise that nothing else is kept.
//!
//! Driven over the real HTTP + WebSocket boundary via the integration harness
//! (ADR-0009), because "a Listener" is a connection and the only honest way to
//! make one is to open one.

mod common;
use common::TestApp;

use std::time::Duration;

use radio_scout::db::entities::listener_sample;
use radio_scout::listeners::WORKER;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, Set};
use serde_json::Value;

/// An Instance that samples fast enough for a test to watch it, rather than at
/// the minute a scanner uses.
///
/// The interval is a *configuration edit* (#90), not a subsystem assembled by
/// hand — and the wait below is [`radio_scout::worker::Meter::settled_at_least`]
/// rather than a sleep, so what a test asserts is that samples were taken and
/// not that some number of milliseconds went by (#93).
async fn sampling_app() -> TestApp {
    TestApp::builder()
        .config(|config| config.listeners.interval = Duration::from_millis(20))
        .spawn()
        .await
}

/// Wait until the sampler has written `n` rows.
async fn samples_taken(app: &TestApp, n: u64) {
    app.workers()
        .meter(WORKER)
        .expect("the sampler is a registered Worker")
        .settled_at_least(n)
        .await;
}

/// The peaks a chart is drawn from.
fn values(series: &Value) -> Vec<i64> {
    series["values"]
        .as_array()
        .expect("values")
        .iter()
        .map(|value| value.as_i64().expect("a count"))
        .collect()
}

/// **Somebody listening is somebody counted**, and the count reaches the chart.
///
/// The whole feature end to end: a socket is opened, the sampler ticks, and an
/// Operator asking "was anybody on?" is answered from rows rather than from a
/// guess.
#[tokio::test]
async fn a_connected_listener_is_counted_and_charted() {
    let app = sampling_app().await;
    let _listener = app.connect_ws().await;

    // Two, not one: the first may have been taken before the socket landed.
    samples_taken(&app, 2).await;
    app.login().await;

    let series = app.get_json("/api/admin/listeners").await;

    assert_eq!(
        values(&series).into_iter().max(),
        Some(1),
        "one Listener was on, and the chart says so"
    );
}

/// A Listener who arrived and left between two ticks is still a Listener who
/// was there — which is the entire reason a sample is a peak rather than a
/// reading, and the thing an instantaneous count would silently lose.
#[tokio::test]
async fn a_listener_who_left_before_the_tick_is_still_in_the_chart() {
    let app = TestApp::builder()
        // Long enough that the connection below is certainly opened and closed
        // inside one window: an instantaneous reading would see nobody.
        .config(|config| config.listeners.interval = Duration::from_millis(400))
        .spawn()
        .await;

    {
        let _brief = app.connect_ws_with_hello().await;
    }

    samples_taken(&app, 1).await;
    app.login().await;

    let series = app.get_json("/api/admin/listeners").await;

    assert_eq!(
        values(&series).into_iter().max(),
        Some(1),
        "they were here, however briefly"
    );
}

/// An empty stretch reads zero rather than being missing, so a client draws a
/// chart rather than having an arm for a hole in one — and an Instance that was
/// not running genuinely had nobody listening to it.
#[tokio::test]
async fn a_stretch_with_no_samples_reads_zero() {
    let app = TestApp::spawn().await;
    app.login().await;

    let series = app.get_json("/api/admin/listeners").await;

    let values = values(&series);
    assert!(!values.is_empty(), "an axis, even with nothing on it");
    assert!(values.iter().all(|peak| *peak == 0));
}

/// **The chart is the Operator's.** The Archive is open because listening is
/// open (ADR-0008); how many people take that up is not a fact a Listener gets
/// to publish on the Operator's behalf.
#[tokio::test]
async fn the_listener_chart_is_behind_the_admin_session() {
    let app = TestApp::spawn().await;

    let response = app.get("/api/admin/listeners").await;

    assert_eq!(response.status(), 401);
}

/// **Counts, never identities** (ADR-0011 rule 5) — asserted against the table
/// itself rather than against what happens to be written today.
///
/// A column added later that could name somebody — an address, a session, a
/// user agent, a Talkgroup on a channel with one listener — fails here, which
/// is the only place it could be noticed: every other test would go on passing,
/// and the row would look perfectly ordinary in the Logs view.
#[tokio::test]
async fn a_listener_sample_has_nothing_in_it_that_could_name_anybody() {
    use sea_orm::Iterable;

    let columns: Vec<String> = listener_sample::Column::iter()
        .map(|column| format!("{column:?}"))
        .collect();

    assert_eq!(
        columns,
        vec!["Id", "AtMs", "Listeners"],
        "a listener sample is a time and a number, and must stay that way"
    );
}

/// The samples are bounded by a window of their own, swept by the same sweeper
/// that bounds the Archive and the log — or a quarter of minutes would
/// accumulate forever on a Pi's SD card.
#[tokio::test]
async fn old_samples_are_pruned_on_their_own_window() {
    let app = TestApp::builder()
        .config(|config| {
            config.retention.listener_days = 1;
            // Fast enough to watch: the shipped cadence is an hour, and the
            // wait below is for *sweeps* rather than for a duration (#93).
            config.retention.interval = Duration::from_millis(50);
            // Nothing else may prune here, or an unrelated policy could be what
            // made the row disappear.
            config.retention.days = 0;
            config.listeners.enabled = false;
        })
        .spawn()
        .await;

    let now_ms = radio_scout::now_ms();
    for at_ms in [now_ms - 3 * 86_400_000, now_ms - 60_000] {
        listener_sample::Entity::insert(listener_sample::ActiveModel {
            at_ms: Set(at_ms),
            listeners: Set(2),
            ..Default::default()
        })
        .exec(&app.db)
        .await
        .expect("seed a sample");
    }

    // The boot sweep has already run by the time `spawn` returns, so the rows
    // above post-date it: sweep again on the same Instance.
    let sweeper = app
        .workers()
        .meter(radio_scout::retention::WORKER)
        .expect("the sweeper is a registered Worker");
    let sweeps = sweeper.load().done;
    sweeper.settled_at_least(sweeps + 2).await;

    let left: Vec<i64> = listener_sample::Entity::find()
        .all(&app.db)
        .await
        .expect("read samples")
        .into_iter()
        .map(|sample| sample.at_ms)
        .collect();

    assert_eq!(left, vec![now_ms - 60_000], "the three-day-old row went");
}

/// Switched off, nothing is recorded at all — and the Worker is absent rather
/// than merely quiet, so a Pi that will never have a listener is not woken once
/// a minute forever.
#[tokio::test]
async fn a_disabled_sampler_records_nothing() {
    let app = TestApp::builder()
        .config(|config| {
            config.listeners.enabled = false;
            config.listeners.interval = Duration::from_millis(10);
        })
        .spawn()
        .await;
    let _listener = app.connect_ws().await;
    app.settle().await;

    assert!(
        app.workers().meter(WORKER).is_none(),
        "no sampler is registered at all"
    );
    assert_eq!(app.count::<listener_sample::Entity>().await, 0);
}

/// The chart is bucketed the way the density ribbon is, by the same axis — so
/// an Operator asking for an hour of ten-minute buckets gets six of them.
#[tokio::test]
async fn the_chart_buckets_the_range_it_is_asked_for() {
    let app = TestApp::builder()
        .config(|config| config.listeners.enabled = false)
        .spawn()
        .await;
    app.login().await;

    for (at_ms, listeners) in [(1_000, 3), (1_500, 9), (5_500, 2)] {
        listener_sample::Entity::insert(listener_sample::ActiveModel {
            at_ms: Set(at_ms),
            listeners: Set(listeners),
            ..Default::default()
        })
        .exec(&app.db)
        .await
        .expect("seed a sample");
    }

    let series = app
        .get_json("/api/admin/listeners?after=1000&before=5999&bucketMs=1000")
        .await;

    assert_eq!(series["fromMs"], 1000);
    assert_eq!(series["toMs"], 6000);
    // The busier of the two samples sharing the first bucket is what shows: the
    // question is how many were on at once, not on average.
    assert_eq!(values(&series), vec![9, 0, 0, 0, 2]);
}

/// **A sample that cannot be written is a gap in a chart, not a scanner
/// falling over.** WARN and carry on: the next tick tries again, and nothing a
/// Listener can hear depends on it (ADR-0011 rule 7).
#[tokio::test]
async fn a_sample_that_cannot_be_written_is_reported_and_survived() {
    let capture = common::logs::LogCapture::start();
    let app = sampling_app().await;
    app.refuse_statements_on("listener_samples");

    samples_taken(&app, 2).await;

    assert!(
        capture
            .text()
            .contains("a listener count could not be recorded"),
        "{}",
        capture.text()
    );
    // ...and the Worker is still going, rather than having ended on the first
    // refusal: a database that comes back finds the sampler still sampling.
    assert!(
        app.workers()
            .meter(WORKER)
            .expect("the sampler")
            .load()
            .done
            >= 2
    );
}

/// A bad parameter is named, the way every other read surface names one.
#[tokio::test]
async fn malformed_chart_parameters_are_rejected_with_a_reason() {
    let app = TestApp::spawn().await;
    app.login().await;

    for (query, expect) in [
        ("?after=yesterday", "after"),
        ("?bucketMs=hourly", "bucketMs"),
    ] {
        let response = app.get(&format!("/api/admin/listeners{query}")).await;
        assert_eq!(response.status(), 400, "GET /api/admin/listeners{query}");
        let body = response.text().await.expect("a body");
        assert!(body.contains(expect), "{body:?} should name {expect:?}");
    }
}

/// The count is a *connection*, so a Listener who hangs up stops being counted
/// — the guard's whole job, and the thing a pair of hand-written calls would
/// eventually forget on one of the ways a socket can end.
#[tokio::test]
async fn a_listener_who_hangs_up_stops_being_counted() {
    let app = sampling_app().await;

    {
        let _listener = app.connect_ws_with_hello().await;
        samples_taken(&app, 2).await;
    }

    // Long enough after the socket closed that several windows have been and
    // gone with nobody on them.
    let taken = app
        .workers()
        .meter(WORKER)
        .expect("the sampler")
        .load()
        .done;
    samples_taken(&app, taken + 4).await;
    app.login().await;

    let quiet = listener_sample::Entity::find()
        .filter(listener_sample::Column::Listeners.eq(0))
        .all(&app.db)
        .await
        .expect("read samples");

    assert!(
        !quiet.is_empty(),
        "the windows after the hang-up have nobody in them"
    );
}
