//! **The Delay policy** (#73, spec US 62) — a Call stored on arrival, published
//! late, flagged, and surviving a restart.
//!
//! Every test here runs on a clock that only moves when it is told to
//! ([`TestApp::advance`]), because the thing under test *is* the passage of
//! time: "not yet" and "now" are both assertions, and neither may be a sleep.

mod common;

use std::time::Duration;

use common::{
    CallUpload, FILTER_BUDGET, Peer, Sink, TestApp, next_json, no_frame_within, page_out, subscribe,
};
use radio_scout::Clock;
use serde_json::json;

/// When every test's Instance thinks it is — a real-looking instant, so a
/// Call's arrival and its recorder's timestamp are both plausible.
const NOW: i64 = 1_790_000_000_000;
const SYSTEM: i64 = 11;
const MINUTE: Duration = Duration::from_secs(60);

/// An Instance whose clock stands still at [`NOW`] until a test moves it, with
/// the default upload key registered and an Operator signed in.
async fn an_instance() -> TestApp {
    let app = TestApp::builder().clock(Clock::frozen(NOW)).spawn().await;
    app.create_api_key("k").await;
    app.login().await;
    app
}

/// An Instance whose System [`SYSTEM`] delays every Call by `minutes`, set the
/// way an Operator sets it.
async fn delayed_by(minutes: u32) -> TestApp {
    let app = an_instance().await;
    let (status, body) = app
        .admin_post(
            "/api/admin/systems",
            json!({ "ref": SYSTEM, "delayMinutes": minutes }),
        )
        .await;
    assert_eq!(status, 201, "the System was refused: {body}");
    app
}

const ALL: &str = r#"{"t":"sub","all":true}"#;

// ---------------------------------------------------------------------------
// Stored on arrival, published late
// ---------------------------------------------------------------------------

/// The tracer: a Call on a delayed System is not on the live feed when it
/// arrives, and is — flagged — once its Delay has passed.
#[tokio::test]
async fn a_delayed_call_goes_out_when_its_delay_has_passed_and_says_so() {
    let app = delayed_by(10).await;
    let mut ws = app.connect_ws().await;
    subscribe(&mut ws, ALL).await;

    app.upload_ok(CallUpload::new()).await;
    app.settle().await;
    no_frame_within(&mut ws, FILTER_BUDGET).await;

    app.advance(10 * MINUTE).await;

    let frame = next_json(&mut ws).await;
    assert_eq!(frame["call"]["talkgroupRef"], 54241);
    assert_eq!(frame["call"]["delayed"], true, "{frame}");
}

// ---------------------------------------------------------------------------
// Waiting answers exactly like not there
// ---------------------------------------------------------------------------

/// The Talkgroup Refs a search page holds, in the order it answered.
fn refs(page: &serde_json::Value) -> Vec<i64> {
    page["results"]
        .as_array()
        .expect("a page of calls")
        .iter()
        .map(|call| call["talkgroupRef"].as_i64().expect("a talkgroup ref"))
        .collect()
}

/// How many Calls the density ribbon counted.
fn counted(activity: &serde_json::Value) -> i64 {
    activity["values"]
        .as_array()
        .expect("the axis's buckets")
        .iter()
        .map(|bucket| bucket.as_i64().unwrap_or_default())
        .sum()
}

/// What the Talkgroup facet offers.
fn offered(filters: &serde_json::Value) -> Vec<i64> {
    filters["talkgroups"]
        .as_array()
        .expect("the talkgroup facet")
        .iter()
        .map(|option| option["ref"].as_i64().expect("a ref"))
        .collect()
}

/// Every read that answers with **many** Calls leaves a waiting one out — the
/// page, the total behind it, the filter options and the density ribbon — and
/// takes it in once it has gone out. A count that moved while it waited would
/// announce that something just happened on a channel an Operator delayed
/// precisely so nobody would know yet.
#[tokio::test]
async fn every_archive_surface_leaves_a_waiting_call_out_until_it_goes_out() {
    let app = delayed_by(10).await;
    app.upload_ok(CallUpload::new().talkgroup(54241).at(NOW - 30_000))
        .await;

    let page = app.get_json("/api/calls").await;
    assert_eq!(refs(&page), Vec::<i64>::new(), "search");
    assert_eq!(page["count"], 0, "and the total");
    assert_eq!(
        offered(&app.get_json("/api/calls/filters").await),
        Vec::<i64>::new(),
        "the cascading filter options"
    );
    assert_eq!(
        counted(&app.get_json("/api/calls/activity?buckets=4").await),
        0,
        "the density ribbon"
    );

    app.advance(10 * MINUTE).await;

    let page = app.get_json("/api/calls").await;
    assert_eq!(refs(&page), vec![54241], "search, once it has gone out");
    assert_eq!(page["count"], 1);
    assert_eq!(
        page["results"][0]["delayed"], true,
        "flagged in the Archive too"
    );
    assert_eq!(
        offered(&app.get_json("/api/calls/filters").await),
        vec![54241]
    );
    assert_eq!(
        counted(&app.get_json("/api/calls/activity?buckets=4").await),
        1
    );
}

/// ...and every read that answers about **one** Call answers as though it is
/// not there: its detail, its audio, its download, a share link minted for it,
/// a Star left on it, and the quiet spans Catch-up asks for. `404`, never
/// `403` — a hand-typed id must not learn that a Call is waiting.
#[tokio::test]
async fn every_single_call_surface_answers_as_if_a_waiting_call_were_not_there() {
    let app = delayed_by(10).await;
    app.upload_ok(CallUpload::new().audio(&common::two_keyups()))
        .await;
    app.settle().await;
    let id = app.the_call().await.id;

    let surfaces = [
        format!("/api/call/{id}"),
        format!("/api/call/{id}/audio"),
        format!("/api/call/{id}/download"),
    ];
    for path in &surfaces {
        assert_eq!(app.get(path).await.status().as_u16(), 404, "{path}");
    }
    let shared = app.post(&format!("/api/call/{id}/share")).await;
    assert_eq!(shared.status().as_u16(), 404, "nor a share link minted");
    let starred = app.post(&format!("/api/call/{id}/star")).await;
    assert_eq!(starred.status().as_u16(), 404, "nor a Star left");
    let quiet = app.get_json(&format!("/api/calls/quiet?ids={id}")).await;
    assert_eq!(quiet, json!({}), "nor its quiet spans answered for");

    app.advance(10 * MINUTE).await;

    for path in &surfaces {
        assert_eq!(
            app.get(path).await.status().as_u16(),
            200,
            "{path}, once out"
        );
    }
    assert_eq!(
        app.get_json(&format!("/api/call/{id}")).await["delayed"],
        true,
        "and its detail says it was delayed"
    );
    let quiet = app.get_json(&format!("/api/calls/quiet?ids={id}")).await;
    assert!(
        quiet.get(id.to_string()).is_some(),
        "and Catch-up can trim it: {quiet}"
    );
}

/// The three reads that summarise Calls rather than list them leave a waiting
/// one out too: the panel's per-channel activity, a radio's history, and an
/// export of the range. Each is a way to learn that a delayed channel just
/// spoke without reading the Call itself.
#[tokio::test]
async fn a_summary_of_the_archive_does_not_count_a_waiting_call() {
    let app = delayed_by(10).await;
    app.upload_ok(
        CallUpload::new()
            .talkgroup(54241)
            .at(NOW - 30_000)
            .set("source", 4242),
    )
    .await;

    let catalog = app.get_json("/api/catalog").await;
    let channel = &catalog["systems"][0]["talkgroups"][0];
    assert_eq!(channel["ref"], 54241, "the channel is there: {catalog}");
    assert_eq!(channel["recentCalls"], serde_json::Value::Null, "{channel}");
    assert_eq!(
        channel["lastCallAtMs"],
        serde_json::Value::Null,
        "{channel}"
    );
    // An Operator's System rosters no radio by itself, so a radio heard only on
    // a waiting Call is nowhere at all yet.
    assert_eq!(
        app.get(&format!("/api/unit/{SYSTEM}/4242"))
            .await
            .status()
            .as_u16(),
        404,
        "a radio heard only on a waiting Call has no history yet"
    );
    assert_eq!(
        app.get("/api/calls/export?format=zip")
            .await
            .status()
            .as_u16(),
        404,
        "and an export of the range holds nothing"
    );

    app.advance(10 * MINUTE).await;

    let catalog = app.get_json("/api/catalog").await;
    assert_eq!(catalog["systems"][0]["talkgroups"][0]["recentCalls"], 1);
    assert_eq!(
        app.get_json(&format!("/api/unit/{SYSTEM}/4242")).await["callCount"],
        1
    );
    assert_eq!(
        app.get("/api/calls/export?format=zip")
            .await
            .status()
            .as_u16(),
        200
    );
}

// ---------------------------------------------------------------------------
// Whose Delay
// ---------------------------------------------------------------------------

/// Curate channel `talkgroup_ref` on [`SYSTEM`] with its own Delay — `None`
/// leaves it inheriting.
async fn channel(app: &TestApp, talkgroup_ref: i64, delay_minutes: Option<u32>) {
    let (status, body) = app
        .admin_post(
            "/api/admin/talkgroups",
            json!({
                "systemId": app.system_id(SYSTEM).await,
                "ref": talkgroup_ref,
                "delayMinutes": delay_minutes,
            }),
        )
        .await;
    assert_eq!(status, 201, "the channel was refused: {body}");
}

/// The Talkgroup Refs of the Calls a Listener can reach right now, sorted —
/// *which* have gone out is the question here, never in what order.
async fn reachable(app: &TestApp) -> Vec<i64> {
    let mut reached = refs(&app.get_json("/api/calls").await);
    reached.sort_unstable();
    reached
}

/// **The most specific Delay wins, in both directions.** A channel saying
/// nothing inherits its System's; `0` undelays one channel on a delayed System
/// — the thing rdio-scanner cannot say, because there `0` *is* inheriting —
/// and a channel may be delayed for longer than its System.
#[tokio::test]
async fn a_talkgroup_overrides_its_system_either_way() {
    let app = delayed_by(10).await;
    channel(&app, 100, Some(0)).await;
    channel(&app, 200, None).await;
    channel(&app, 300, Some(30)).await;
    for talkgroup in [100, 200, 300] {
        app.upload_ok(CallUpload::new().talkgroup(talkgroup).at(NOW - talkgroup))
            .await;
    }
    app.settle().await;

    assert_eq!(
        reachable(&app).await,
        vec![100],
        "an undelayed channel goes out at once"
    );
    let undelayed = app.get_json("/api/calls?talkgroup=100").await;
    assert_eq!(
        undelayed["results"][0]["delayed"],
        serde_json::Value::Null,
        "and is not flagged: nothing kept it back"
    );

    app.advance(10 * MINUTE).await;
    assert_eq!(reachable(&app).await, vec![100, 200], "the System's Delay");

    app.advance(20 * MINUTE).await;
    assert_eq!(
        reachable(&app).await,
        vec![100, 200, 300],
        "the channel's own"
    );
}

/// **A Patch takes the longest Delay it reaches** — so a tactical channel's
/// Delay holds a transmission patched onto it, whichever channel the Call
/// happened to be filed under.
#[tokio::test]
async fn a_patched_call_waits_out_the_longest_delay_it_reaches() {
    let app = an_instance().await;
    let (status, body) = app
        .admin_post("/api/admin/systems", json!({ "ref": SYSTEM }))
        .await;
    assert_eq!(status, 201, "{body}");
    channel(&app, 54241, None).await;
    channel(&app, 300, Some(15)).await;

    app.upload_ok(CallUpload::new().talkgroup(54241).set("patches", "[300]"))
        .await;
    app.settle().await;
    assert_eq!(
        reachable(&app).await,
        Vec::<i64>::new(),
        "kept back by the patch"
    );

    app.advance(15 * MINUTE).await;
    assert_eq!(reachable(&app).await, vec![54241]);
}

/// **Measured from arrival, never from the recorder's timestamp.** A recorder
/// whose clock runs an hour slow says every Call happened an hour ago, and a
/// Delay measured from that would publish at once — so a slow clock would be a
/// way around an officer-safety policy.
#[tokio::test]
async fn a_recorder_s_slow_clock_does_not_shorten_a_delay() {
    let app = delayed_by(10).await;

    app.upload_ok(CallUpload::new().at(NOW - 60 * 60 * 1000))
        .await;
    app.settle().await;
    assert_eq!(
        reachable(&app).await,
        Vec::<i64>::new(),
        "an hour old, and waiting"
    );

    app.advance(9 * MINUTE).await;
    assert_eq!(reachable(&app).await, Vec::<i64>::new(), "a minute to go");

    app.advance(MINUTE).await;
    assert_eq!(reachable(&app).await, vec![54241]);
}

// ---------------------------------------------------------------------------
// Surviving a restart
// ---------------------------------------------------------------------------

/// **A restart mid-Delay keeps the schedule.** The schedule is a column on the
/// Call, so the next boot reads it and changes nothing: neither early, nor
/// forgotten. (rdio-scanner reads its `delayed` table at boot, *deletes all of
/// it*, then puts rows back one at a time — a crash in that gap publishes
/// everything at once.)
#[tokio::test]
async fn a_restart_part_way_through_a_delay_keeps_its_schedule() {
    let mut app = delayed_by(10).await;
    app.upload_ok(CallUpload::new()).await;
    app.settle().await;
    app.advance(5 * MINUTE).await;

    app.restart().await;
    app.settle().await;
    assert_eq!(reachable(&app).await, Vec::<i64>::new(), "not early");

    app.advance(5 * MINUTE).await;
    assert_eq!(reachable(&app).await, vec![54241], "not forgotten");
    assert_eq!(
        app.get_json("/api/calls").await["results"][0]["delayed"],
        true
    );
}

/// **A Call that came due while the Instance was down goes out as it comes
/// back** — late, never lost. Nothing was running to release it, so this is
/// the release Worker's first pass at boot and nothing else.
#[tokio::test]
async fn a_call_that_came_due_while_the_instance_was_down_goes_out_at_boot() {
    let mut app = delayed_by(10).await;
    app.upload_ok(CallUpload::new()).await;
    app.settle().await;

    app.restart_after(15 * MINUTE).await;
    app.settle().await;

    assert_eq!(reachable(&app).await, vec![54241]);
}

// ---------------------------------------------------------------------------
// The policy in force decides
// ---------------------------------------------------------------------------

/// **Raising a Delay covers the Calls already waiting.** An Operator who
/// lengthens a channel's Delay mid-incident means the last few minutes too —
/// a schedule fixed at arrival would let those out under the old, shorter one.
#[tokio::test]
async fn raising_a_delay_holds_the_calls_already_waiting_for_longer() {
    let app = delayed_by(5).await;
    app.upload_ok(CallUpload::new()).await;
    app.settle().await;
    app.advance(MINUTE).await;

    let system = app.system_id(SYSTEM).await;
    let (status, body) = app
        .admin_patch(
            &format!("/api/admin/systems/{system}"),
            json!({ "delayMinutes": 30 }),
        )
        .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["delayMinutes"], 30);

    app.advance(10 * MINUTE).await;
    assert_eq!(
        reachable(&app).await,
        Vec::<i64>::new(),
        "the old five has passed"
    );

    app.advance(19 * MINUTE).await;
    assert_eq!(reachable(&app).await, vec![54241], "thirty from arrival");
}

/// **Lowering one releases on the new schedule** — and lifting it altogether
/// releases at once, still flagged, because it *was* kept back.
#[tokio::test]
async fn lifting_a_delay_releases_what_it_was_keeping_back() {
    let app = delayed_by(600).await;
    let mut ws = app.connect_ws().await;
    subscribe(&mut ws, ALL).await;
    app.upload_ok(CallUpload::new()).await;
    app.settle().await;

    let system = app.system_id(SYSTEM).await;
    let (status, body) = app
        .admin_patch(
            &format!("/api/admin/systems/{system}"),
            json!({ "delayMinutes": null }),
        )
        .await;
    assert_eq!(status, 200, "{body}");
    app.settle().await;

    let frame = next_json(&mut ws).await;
    assert_eq!(frame["call"]["delayed"], true, "{frame}");
    assert_eq!(reachable(&app).await, vec![54241]);
}

// ---------------------------------------------------------------------------
// The sinks wait too
// ---------------------------------------------------------------------------

/// **A Downstream peer and a Webhook hear a Delayed Call when a Listener may,
/// and not before.** A peer would publish it at once, and a webhook is usually
/// a community's Discord channel — so both are queued in the transaction that
/// releases the Call, and post it as it stands then: flagged.
#[tokio::test]
async fn a_peer_and_a_webhook_hear_a_delayed_call_only_once_it_goes_out() {
    let app = TestApp::builder()
        .clock(Clock::frozen(NOW))
        .config(|config| {
            config.server.public_url = Some(String::from("https://scan.example"));
        })
        .spawn()
        .await;
    app.create_api_key("k").await;
    app.login().await;
    let (status, body) = app
        .admin_post(
            "/api/admin/systems",
            json!({ "ref": SYSTEM, "label": "sys11", "delayMinutes": 10 }),
        )
        .await;
    assert_eq!(status, 201, "{body}");
    let everything = json!({ "sel": { "11": { "*": true } } });
    let peer = Peer::start().await;
    app.add_downstream_as(&peer.url(), "peer-key", everything.clone())
        .await;
    let sink = Sink::start().await;
    app.add_webhook(&sink.url(), &["emergency"], everything)
        .await;

    let start = NOW / 1000;
    let (status, body) = app
        .upload_tr(CallUpload::tr(&format!(
            r#"{{"short_name":"sys11","talkgroup":54241,
                "start_time":{start},"call_length_ms":4000,
                "emergency":1,"encrypted":0}}"#
        )))
        .await;
    assert_eq!(status, 200, "{body}");
    app.settle().await;
    assert!(
        peer.received().is_empty(),
        "the peer would have published it"
    );
    assert!(sink.received().is_empty(), "and so would the channel");

    app.advance(10 * MINUTE).await;

    assert_eq!(peer.received().len(), 1, "forwarded once it went out");
    let posted = sink.received();
    assert_eq!(posted.len(), 1, "posted once it went out");
    assert_eq!(posted[0]["call"]["delayed"], true, "{}", posted[0]);
}

/// **A page found while a Call waits is posted when it goes out — once.** The
/// audio is looked at on arrival, so the mark is on the row long before the
/// release; the release reads the row as it stands and posts it, and the
/// tone-out Worker, finding the Call still waiting, leaves it to the release.
/// Either one alone would be a page posted early or twice.
#[tokio::test]
async fn a_page_found_while_a_call_waits_is_posted_when_it_goes_out_and_once() {
    let app = delayed_by(10).await;
    channel(&app, 54241, None).await;
    let (_, listed) = app.admin_get("/api/admin/talkgroups").await;
    let talkgroup = listed["results"][0]["id"].as_i64().expect("a talkgroup id");
    let (status, created) = app
        .admin_post(
            &format!("/api/admin/talkgroups/{talkgroup}/tones"),
            json!({
                "label": "Station 12",
                "steps": [{ "hz": 1122.5, "minMs": 800 }, { "hz": 1465.6, "minMs": 2000 }],
            }),
        )
        .await;
    assert_eq!(status, 201, "{created}");
    let sink = Sink::start().await;
    app.add_webhook(
        &sink.url(),
        &["tone"],
        json!({ "sel": { "11": { "*": true } } }),
    )
    .await;

    app.upload_ok(CallUpload::new().audio(&page_out(1122.5, 1465.6)))
        .await;
    app.settle().await;
    assert!(
        app.the_call().await.tone_matched(),
        "the page was found on arrival"
    );
    assert!(
        sink.received().is_empty(),
        "and not posted while the Call waits"
    );

    app.advance(10 * MINUTE).await;

    let posted = sink.received();
    assert_eq!(
        posted.len(),
        1,
        "posted once it went out, and once: {posted:?}"
    );
    assert_eq!(posted[0]["marks"], json!(["tone"]));
}

// ---------------------------------------------------------------------------
// Backfill and Catch-up
// ---------------------------------------------------------------------------

/// **A Listener who was away when a Delayed Call went out is backfilled it on
/// reconnect, exactly once.** The Call was stored before the undelayed one the
/// Listener *did* hear, so a cursor over storage order would step straight past
/// it; the Backfill is read in emission order (#94), and the release is an
/// emission.
#[tokio::test]
async fn a_listener_away_when_a_delayed_call_went_out_is_backfilled_it_once() {
    let app = delayed_by(10).await;
    channel(&app, 100, Some(0)).await;

    let mut ws = app.connect_ws().await;
    subscribe(&mut ws, ALL).await;
    app.upload_ok(CallUpload::new().talkgroup(54241)).await;
    app.upload_ok(CallUpload::new().talkgroup(100).at(2_000))
        .await;
    let heard = next_json(&mut ws).await;
    assert_eq!(heard["call"]["talkgroupRef"], 100, "only the undelayed one");
    let cursor = heard["seq"].as_i64().expect("an emission");
    drop(ws);

    app.advance(10 * MINUTE).await;

    let mut ws = app.connect_ws().await;
    subscribe(
        &mut ws,
        &format!(r#"{{"t":"sub","all":true,"since":{cursor}}}"#),
    )
    .await;
    let backfilled = next_json(&mut ws).await;
    assert_eq!(backfilled["call"]["talkgroupRef"], 54241, "{backfilled}");
    assert_eq!(backfilled["catchup"], true);
    assert_eq!(backfilled["call"]["delayed"], true);
    no_frame_within(&mut ws, FILTER_BUDGET).await;
}

// ---------------------------------------------------------------------------
// An Instance that delays nothing pays nothing
// ---------------------------------------------------------------------------

/// **Asserted as a cost, both ways round.** The two reads a Delay could make
/// expensive — serving one Call's audio (which must ask whether it is waiting)
/// and storing a patched Call (which must ask what the patch reaches) — cost
/// exactly what they cost before this existed while nothing is delayed, cost
/// more the moment *anything* is, and go back the moment the last Delay is
/// lifted. Measured as a difference, because a pinned count says nothing about
/// what paid for it.
#[tokio::test]
async fn an_instance_that_delays_nothing_pays_nothing() {
    let app = an_instance().await;
    for system in [SYSTEM, 22] {
        let (status, body) = app
            .admin_post("/api/admin/systems", json!({ "ref": system }))
            .await;
        assert_eq!(status, 201, "{body}");
    }
    channel(&app, 300, None).await;
    let patched = |at: i64| CallUpload::new().at(at).set("patches", "[300]");
    app.upload_ok(patched(10_000)).await;
    app.settle().await;
    let audio = format!("/api/call/{}/audio", app.the_call().await.id);
    let other = app.system_id(22).await;

    let cost = |at: i64| {
        let app = &app;
        let audio = audio.clone();
        async move {
            let (_, serving) = app.statements_during(app.get(&audio)).await;
            let (_, storing) = app.statements_during(app.upload_ok(patched(at))).await;
            (serving, storing)
        }
    };
    let undelayed = cost(20_000).await;

    let (status, body) = app
        .admin_patch(
            &format!("/api/admin/systems/{other}"),
            json!({ "delayMinutes": 5 }),
        )
        .await;
    assert_eq!(status, 200, "{body}");
    let armed = cost(30_000).await;
    assert!(
        armed.0 > undelayed.0 && armed.1 > undelayed.1,
        "a Delay anywhere makes both ask: {undelayed:?} then {armed:?}"
    );

    let (status, body) = app
        .admin_patch(
            &format!("/api/admin/systems/{other}"),
            json!({ "delayMinutes": null }),
        )
        .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(
        cost(40_000).await,
        undelayed,
        "and lifting the last one gives the cost back"
    );
}

// ---------------------------------------------------------------------------
// Curation
// ---------------------------------------------------------------------------

/// **Longer than a day is refused, by name, on both forms and both verbs** — a
/// Delay that long is an Archive nobody can listen to, and almost certainly a
/// number typed into the wrong box. What is stored is what the listing says.
#[tokio::test]
async fn a_delay_longer_than_a_day_is_refused_by_name() {
    let app = an_instance().await;
    let too_long = json!({ "ref": SYSTEM, "delayMinutes": 1441 });
    let (status, refused) = app.admin_post("/api/admin/systems", too_long).await;
    assert_eq!(status, 400, "{refused}");
    assert_eq!(refused["error"], "out-of-range");
    assert_eq!(refused["field"], "delayMinutes");

    let (status, created) = app
        .admin_post(
            "/api/admin/systems",
            json!({ "ref": SYSTEM, "delayMinutes": 1440 }),
        )
        .await;
    assert_eq!(status, 201, "a day exactly is fine: {created}");
    assert_eq!(created["delayMinutes"], 1440);
    let system = created["id"].as_i64().expect("an id");
    let (status, refused) = app
        .admin_patch(
            &format!("/api/admin/systems/{system}"),
            json!({ "delayMinutes": 100_000 }),
        )
        .await;
    assert_eq!(status, 400, "{refused}");
    assert_eq!(refused["field"], "delayMinutes");

    let (status, refused) = app
        .admin_post(
            "/api/admin/talkgroups",
            json!({ "systemId": system, "ref": 100, "delayMinutes": 1441 }),
        )
        .await;
    assert_eq!(status, 400, "{refused}");
    assert_eq!(refused["field"], "delayMinutes");
    let (status, created) = app
        .admin_post(
            "/api/admin/talkgroups",
            json!({ "systemId": system, "ref": 100, "delayMinutes": 15 }),
        )
        .await;
    assert_eq!(status, 201, "{created}");
    assert_eq!(created["delayMinutes"], 15);
    let channel = created["id"].as_i64().expect("an id");
    let (status, refused) = app
        .admin_patch(
            &format!("/api/admin/talkgroups/{channel}"),
            json!({ "delayMinutes": 1441 }),
        )
        .await;
    assert_eq!(status, 400, "{refused}");
    assert_eq!(refused["field"], "delayMinutes");

    let (status, cleared) = app
        .admin_patch(
            &format!("/api/admin/talkgroups/{channel}"),
            json!({ "delayMinutes": null }),
        )
        .await;
    assert_eq!(status, 200, "{cleared}");
    assert_eq!(
        cleared["delayMinutes"],
        serde_json::Value::Null,
        "back to inheriting"
    );
}

// ---------------------------------------------------------------------------
// When the database will not answer
// ---------------------------------------------------------------------------

/// **A release that cannot be written keeps the Call back, and says why.** The
/// release *is* the emission, written in one transaction with what the sinks
/// are owed — so a stamp that fails lets nothing out, rather than letting the
/// Call out unrecorded. An officer-safety policy may err late, never early.
#[tokio::test]
async fn a_release_the_database_refuses_keeps_the_call_back_and_says_so() {
    let logs = common::logs::LogCapture::start();
    let app = delayed_by(10).await;
    app.upload_ok(CallUpload::new()).await;
    app.settle().await;

    app.refuse_updates_to("calls");
    app.advance(10 * MINUTE).await;

    assert_eq!(
        reachable(&app).await,
        Vec::<i64>::new(),
        "late, never early"
    );
    // Once per pass that tried, and moving the clock is a pass of its own.
    let lines = logs.lines_containing("could not release");
    assert!(!lines.is_empty(), "the refusal is never silent");
    assert!(
        lines
            .iter()
            .all(|line| line.contains("reason=release-failed")),
        "{lines:#?}"
    );
}

/// **A Call whose live frame cannot be built is not let out half-published.**
/// The frame is built inside the release, so a Call nobody connected could be
/// shown is not marked as having gone out either — it keeps waiting and is
/// tried again, late rather than missing from the live feed.
#[tokio::test]
async fn a_release_whose_live_frame_cannot_be_built_keeps_the_call_back() {
    let logs = common::logs::LogCapture::start();
    let app = delayed_by(10).await;
    app.upload_ok(CallUpload::new()).await;
    app.settle().await;

    app.refuse_reads_of("call_units");
    app.advance(10 * MINUTE).await;

    assert!(!logs.lines_containing("reason=release-failed").is_empty());
    assert_eq!(
        app.the_call().await.emitted_seq,
        None,
        "not out, in any part"
    );
}

/// **A reschedule that cannot be read leaves the old schedule standing** — the
/// edit itself is saved and answered, and the Calls already waiting keep the
/// Delay they had, which is the safe direction to be wrong in.
#[tokio::test]
async fn a_reschedule_that_cannot_be_read_keeps_the_old_schedule() {
    let logs = common::logs::LogCapture::start();
    let app = delayed_by(10).await;
    app.upload_ok(CallUpload::new()).await;
    app.settle().await;

    app.refuse_reads_of("call_patches");
    let system = app.system_id(SYSTEM).await;
    let (status, body) = app
        .admin_patch(
            &format!("/api/admin/systems/{system}"),
            json!({ "delayMinutes": null }),
        )
        .await;
    assert_eq!(status, 200, "{body}");
    app.settle().await;

    let line = logs.only_line_containing("could not move the waiting Calls");
    assert!(line.contains("reason=reschedule-failed"), "{line}");
    assert_eq!(reachable(&app).await, Vec::<i64>::new(), "still waiting");
}

// ---------------------------------------------------------------------------
// Keep-best, while a Call waits
// ---------------------------------------------------------------------------

/// **A better copy of a waiting Call takes its place and keeps its schedule** —
/// and is what goes out. Ordinarily a replacement is forwarded again, because
/// the peer is holding the worse copy; a Call still waiting has been forwarded
/// to nobody, so its release forwards the copy that won by then, once.
#[tokio::test]
async fn a_better_copy_of_a_waiting_call_is_what_goes_out_and_only_once() {
    let app = delayed_by(10).await;
    let peer = Peer::start().await;
    app.add_downstream_as(
        &peer.url(),
        "peer-key",
        json!({ "sel": { "11": { "*": true } } }),
    )
    .await;
    let heard_with = |errors: u32, audio: &'static [u8]| {
        CallUpload::new()
            .set(
                "frequencies",
                format!(r#"[{{"freq":853712500,"pos":0,"errorCount":{errors}}}]"#),
            )
            .audio(audio)
    };

    app.upload_ok(heard_with(9, b"the-worse-copy")).await;
    app.upload_ok(heard_with(1, b"the-better-copy")).await;
    app.settle().await;
    assert_eq!(app.calls().await.len(), 1, "one transmission, one Call");
    assert!(
        peer.received().is_empty(),
        "nothing forwarded while it waits"
    );

    app.advance(10 * MINUTE).await;

    let received = peer.received();
    assert_eq!(received.len(), 1, "forwarded once: {received:?}");
    assert_eq!(received[0].audio, b"the-better-copy");
}

/// **A waiting Call cannot be frozen into an Event.** An Event is the
/// Operator's to curate and anybody's to open by its link, so adding a Call that
/// has not gone out yet would publish it on the Event's page — it is counted as
/// missing, the answer a Call that is not there gets, and can be added once it
/// has gone out.
#[tokio::test]
async fn a_waiting_call_cannot_be_frozen_into_an_event() {
    let app = delayed_by(10).await;
    app.upload_ok(CallUpload::new()).await;
    app.settle().await;
    let id = app.the_call().await.id;

    let (status, body) = app
        .admin_post(
            "/api/admin/events",
            json!({ "name": "Structure fire", "callIds": [id] }),
        )
        .await;
    assert_eq!(status, 201, "{body}");
    assert_eq!(body["added"]["missing"], 1, "{body}");
    assert_eq!(body["added"]["frozen"], 0);

    app.advance(10 * MINUTE).await;
    let event = body["id"].as_i64().expect("an event");
    let (status, body) = app
        .admin_post(
            &format!("/api/admin/events/{event}/calls"),
            json!({ "callIds": [id] }),
        )
        .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["added"]["frozen"], 1, "once it has gone out: {body}");
}

/// **Folding a channel into another moves its waiting Calls onto the owner's
/// Delay** (#45) — the merge re-files them, and the policy in force for the
/// channel they are now on decides when they go out.
#[tokio::test]
async fn folding_a_channel_moves_its_waiting_calls_onto_the_owner_s_delay() {
    let app = delayed_by(5).await;
    channel(&app, 100, Some(30)).await;
    app.upload_ok(CallUpload::new().talkgroup(200)).await;
    app.settle().await;

    let owner = app
        .talkgroup_by_ref(SYSTEM, 100)
        .await
        .expect("the owner")
        .id;
    let (status, body) = app
        .admin_post(
            &format!("/api/admin/talkgroups/{owner}/members"),
            json!({ "fold": [200] }),
        )
        .await;
    assert_eq!(status, 200, "{body}");

    app.advance(5 * MINUTE).await;
    assert_eq!(
        reachable(&app).await,
        Vec::<i64>::new(),
        "the owner's thirty"
    );
    app.advance(25 * MINUTE).await;
    assert_eq!(reachable(&app).await, vec![100]);
}

/// **A reschedule weighs a Patch the way an arrival does** — it is the same
/// decision made again, so a Call waiting on a patch moves when the patched
/// channel's Delay does, and a Call the edit does not touch keeps its schedule.
#[tokio::test]
async fn a_reschedule_weighs_a_patch_the_way_an_arrival_does() {
    let app = an_instance().await;
    let (status, body) = app
        .admin_post("/api/admin/systems", json!({ "ref": SYSTEM }))
        .await;
    assert_eq!(status, 201, "{body}");
    channel(&app, 54241, None).await;
    channel(&app, 100, Some(10)).await;
    channel(&app, 300, Some(15)).await;
    app.upload_ok(CallUpload::new().talkgroup(54241).set("patches", "[300]"))
        .await;
    app.upload_ok(CallUpload::new().talkgroup(100).at(5_000))
        .await;
    app.settle().await;

    let patched = app
        .talkgroup_by_ref(SYSTEM, 300)
        .await
        .expect("the patched channel")
        .id;
    let (status, body) = app
        .admin_patch(
            &format!("/api/admin/talkgroups/{patched}"),
            json!({ "delayMinutes": 30 }),
        )
        .await;
    assert_eq!(status, 200, "{body}");

    app.advance(10 * MINUTE).await;
    assert_eq!(reachable(&app).await, vec![100], "untouched by the edit");
    app.advance(5 * MINUTE).await;
    assert_eq!(reachable(&app).await, vec![100], "the patch's old fifteen");
    app.advance(15 * MINUTE).await;
    assert_eq!(reachable(&app).await, vec![100, 54241], "its new thirty");
}

/// **A backlog bigger than one read goes out whole.** A long outage can leave
/// any number of Calls due at once, and the Worker releases them in bounded
/// reads rather than one unbounded one — every one of them, and each says when
/// it actually reached anybody.
#[tokio::test]
async fn a_backlog_bigger_than_one_read_goes_out_whole_at_boot() {
    use radio_scout::db::entities::call;
    use radio_scout::db::repo::NewCall;
    use sea_orm::{EntityTrait, sea_query::Expr};

    let logs = common::logs::LogCapture::start();
    let mut app = an_instance().await;
    // Half a read again past one: the Worker reads at most `BATCH` at a time.
    let backlog = radio_scout::delay::worker::BATCH as i64 * 3 / 2;
    for at in 0..backlog {
        app.seed_call(
            NewCall::new(SYSTEM, 54241, at * 1_000),
            common::audio_at("a.wav"),
        )
        .await;
    }
    call::Entity::update_many()
        .col_expr(call::Column::DelayedUntilMs, Expr::value(NOW))
        .exec(&app.db)
        .await
        .expect("a backlog, all due");

    app.restart().await;
    app.settle().await;

    let calls = app.calls().await;
    assert!(
        calls.iter().all(|call| call.emitted_seq.is_some()),
        "every Call went out"
    );
    let published = logs.lines_containing("delayed call published");
    assert_eq!(published.len() as i64, backlog);
    assert!(
        published.iter().all(|line| line.contains("waited_ms=")),
        "each says how long it waited: {published:#?}"
    );
}

/// **A better copy that names a longer-delayed patch holds the Call for it.**
/// Keep-best unions what every copy says the transmission reached, so the
/// replaced Call now reaches a channel with a longer Delay — and "the longest
/// it reaches" has to be the same answer whichever copy arrived first.
#[tokio::test]
async fn a_better_copy_that_reveals_a_longer_delayed_patch_holds_the_call_for_it() {
    let app = delayed_by(10).await;
    channel(&app, 54241, None).await;
    channel(&app, 300, Some(30)).await;
    let heard_with = |errors: u32| {
        CallUpload::new().set(
            "frequencies",
            format!(r#"[{{"freq":853712500,"pos":0,"errorCount":{errors}}}]"#),
        )
    };

    app.upload_ok(heard_with(9)).await;
    app.upload_ok(heard_with(1).set("patches", "[300]")).await;
    app.settle().await;
    assert_eq!(app.calls().await.len(), 1, "one transmission, one Call");

    app.advance(10 * MINUTE).await;
    assert_eq!(
        reachable(&app).await,
        Vec::<i64>::new(),
        "the patch's thirty"
    );
    app.advance(20 * MINUTE).await;
    assert_eq!(reachable(&app).await, vec![54241]);
}
