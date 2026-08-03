//! Live-feed WebSocket loop (`src/live.rs` `handle_socket`) behaviors that the
//! matcher unit tests can't reach (ticket #26 hardening): malformed-message
//! tolerance, subscription replacement, global `all`, and per-connection
//! filtering across concurrent clients.

mod common;
use common::{
    CallUpload, FILTER_BUDGET, TestApp, Ws, expect_server_closed, frame_within, subscribe,
};
use radio_scout::db::repo::NewCall;

use futures_util::SinkExt;
use tokio_tungstenite::tungstenite::Message as WsMessage;

/// Is a frame delivered within the window? `None` = correctly filtered.
async fn received(ws: &mut Ws) -> Option<serde_json::Value> {
    frame_within(ws, FILTER_BUDGET).await
}

/// An app with the live-feed test key registered.
async fn feed_app() -> TestApp {
    TestApp::with_key("test-key").await
}

/// Post a Call for `system`/`talkgroup`, timestamped so each talkgroup is its
/// own Call rather than a duplicate of the last.
async fn post_call(app: &TestApp, system: i64, talkgroup: i64) {
    app.upload_ok(live_call(system, talkgroup)).await;
}

/// Post a Call carrying a `patches` array (rdio `patches[]`).
async fn post_call_with_patches(app: &TestApp, system: i64, talkgroup: i64, patches: &str) {
    app.upload_ok(live_call(system, talkgroup).set("patches", patches))
        .await;
}

/// The shape every Call above shares: the live-feed key, a small WAV, and a
/// timestamp derived from the Talkgroup so consecutive posts are distinct Calls
/// rather than duplicates of each other (#5's dedup window).
fn live_call(system: i64, talkgroup: i64) -> CallUpload {
    CallUpload::new()
        .key("test-key")
        .system(system)
        .talkgroup(talkgroup)
        .at(1000 + talkgroup)
        .audio(b"audio")
}

/// A malformed frame (and a valid-JSON-but-unknown-shape frame) must be silently
/// ignored — never acked, never fatal — and the loop keeps serving afterward.
#[tokio::test]
async fn malformed_messages_are_ignored_and_the_loop_survives() {
    let app = feed_app().await;
    let mut ws = app.connect_ws().await;

    // Garbage + a well-formed-but-unknown message: neither should ack.
    ws.send(WsMessage::Text("not json {".into())).await.unwrap();
    ws.send(WsMessage::Text(r#"{"t":"bogus"}"#.into()))
        .await
        .unwrap();

    // The connection is still alive: a real subscribe still acks (and it is the
    // FIRST frame we see — the junk produced no frames).
    subscribe(&mut ws, r#"{"t":"sub","sel":{"11":{"54241":true}}}"#).await;
    post_call(&app, 11, 54241).await;

    let call = received(&mut ws).await.expect("subscribed call delivered");
    assert_eq!(call["t"], "call");
    assert_eq!(call["call"]["talkgroupRef"], 54241);
}

/// Re-subscribing REPLACES the matrix: the old talkgroup stops matching, the new
/// one starts.
#[tokio::test]
async fn resubscribing_replaces_the_previous_selection() {
    let app = feed_app().await;
    let mut ws = app.connect_ws().await;

    subscribe(&mut ws, r#"{"t":"sub","sel":{"11":{"100":true}}}"#).await;
    subscribe(&mut ws, r#"{"t":"sub","sel":{"11":{"200":true}}}"#).await; // replaces

    // Old talkgroup 100 is no longer subscribed; new talkgroup 200 is. Posting
    // both, the only frame that arrives is 200 (100 would arrive first if the
    // old selection had leaked through).
    post_call(&app, 11, 100).await;
    post_call(&app, 11, 200).await;

    let call = received(&mut ws).await.expect("new selection delivered");
    assert_eq!(call["call"]["talkgroupRef"], 200, "replaced, not merged");
}

/// `all:true` is the global monitor-everything subscription (spec story 21).
#[tokio::test]
async fn all_true_receives_any_call() {
    let app = feed_app().await;
    let mut ws = app.connect_ws().await;

    subscribe(&mut ws, r#"{"t":"sub","all":true}"#).await;
    post_call(&app, 77, 4242).await; // never explicitly selected

    let call = received(&mut ws)
        .await
        .expect("all:true delivers everything");
    assert_eq!(call["call"]["systemRef"], 77);
    assert_eq!(call["call"]["talkgroupRef"], 4242);
}

/// **Hold System** over the wire (#11, spec US 11): the client can't enumerate a
/// System's Talkgroups, so it holds one with a `"*"` key and the server keeps
/// filtering out everything else — the point of server-side filtering is that a
/// phone stops *receiving* what it isn't listening to.
#[tokio::test]
async fn a_system_wildcard_holds_the_whole_system() {
    let app = feed_app().await;
    let mut ws = app.connect_ws().await;

    subscribe(&mut ws, r#"{"t":"sub","sel":{"11":{"*":true}}}"#).await;

    post_call(&app, 22, 4242).await; // another system: filtered
    post_call(&app, 11, 909).await; // never named individually: delivered
    let call = received(&mut ws).await.expect("held system delivers");
    assert_eq!(call["call"]["systemRef"], 11);
    assert_eq!(call["call"]["talkgroupRef"], 909);
    assert!(received(&mut ws).await.is_none(), "nothing else follows");
}

/// **Avoid** over the wire (#11, spec US 14): a listener starts all-on, so an
/// avoided Talkgroup is an explicit exception to `all` — and the Calls it would
/// have carried never reach the device at all.
#[tokio::test]
async fn an_explicit_exception_avoids_one_talkgroup_of_an_all_on_selection() {
    let app = feed_app().await;
    let mut ws = app.connect_ws().await;

    subscribe(
        &mut ws,
        r#"{"t":"sub","all":true,"sel":{"11":{"54241":false}}}"#,
    )
    .await;

    post_call(&app, 11, 54241).await; // avoided
    post_call(&app, 11, 999).await; // everything else still plays
    let call = received(&mut ws).await.expect("unavoided call delivers");
    assert_eq!(call["call"]["talkgroupRef"], 999);
    assert!(
        received(&mut ws).await.is_none(),
        "the avoided one never came"
    );
}

/// Two clients with different filters are served independently: a call reaches
/// only the client that subscribed to it.
#[tokio::test]
async fn concurrent_clients_are_filtered_independently() {
    let app = feed_app().await;
    let mut a = app.connect_ws().await;
    let mut b = app.connect_ws().await;

    subscribe(&mut a, r#"{"t":"sub","sel":{"11":{"100":true}}}"#).await;
    subscribe(&mut b, r#"{"t":"sub","sel":{"11":{"200":true}}}"#).await;

    post_call(&app, 11, 100).await;

    let to_a = received(&mut a).await.expect("client A subscribed to 100");
    assert_eq!(to_a["call"]["talkgroupRef"], 100);
    assert!(
        received(&mut b).await.is_none(),
        "client B (talkgroup 200) must not receive talkgroup 100"
    );
}

// ---------------------------------------------------------------------------
// #9 improvements over rdio: hello greeting, heartbeat, reconnect catch-up,
// patch fanout.
// ---------------------------------------------------------------------------

/// A client that sends a clean Close is dropped by the server, which
/// reciprocates the close handshake rather than leaking the connection.
#[tokio::test]
async fn client_close_ends_the_connection() {
    let app = feed_app().await;
    let mut ws = app.connect_ws().await;

    ws.send(WsMessage::Close(None)).await.expect("send close");

    expect_server_closed(&mut ws).await;
}

/// On connect the server greets the client with its protocol version and
/// heartbeat cadence (#9) — rdio has no such handshake.
#[tokio::test]
async fn hello_greeting_announces_protocol_and_heartbeat() {
    let app = feed_app().await;

    let (_ws, hello) = app.connect_ws_with_hello().await;
    assert_eq!(hello["t"], "hello");
    assert_eq!(hello["protocol"], 2, "#94's emission cursor is protocol 2");
    assert_eq!(hello["heartbeatMs"], 30_000, "the shipped heartbeat period");
}

// The heartbeat, the ping-on-connect delay and the reaping that follows an
// unanswered ping are `src/live.rs`'s own tests since #94: they are rows in the
// connection's table and a run of its loop under a paused clock, which is why
// the harness no longer has a knob for shortening the shipped period. What lived
// here instead was three tests that shortened it and then slept through it —
// ~750 ms of wall clock to observe two decisions, and the only assertion a sleep
// can make about something that should *not* happen is "not yet".

/// Reconnect catch-up (#9): a client that reconnects with a `since` cursor is
/// backfilled the matching Calls it missed, oldest-first and flagged `catchup`,
/// filtered to its selection — where rdio would have dropped them entirely.
#[tokio::test]
async fn reconnect_catchup_backfills_missed_calls() {
    let app = feed_app().await;

    // Three calls land while the client is (pretend) disconnected.
    post_call(&app, 11, 100).await; // id 1
    post_call(&app, 11, 200).await; // id 2
    post_call(&app, 11, 300).await; // id 3

    let mut ws = app.connect_ws().await;
    // Reconnect: select 100 & 300 with a catch-up cursor from the beginning.
    subscribe(
        &mut ws,
        r#"{"t":"sub","sel":{"11":{"100":true,"300":true}},"since":0}"#,
    )
    .await;

    let first = received(&mut ws).await.expect("catch-up for 100");
    assert_eq!(first["call"]["talkgroupRef"], 100);
    assert_eq!(first["catchup"], true);

    let second = received(&mut ws).await.expect("catch-up for 300");
    assert_eq!(second["call"]["talkgroupRef"], 300);
    assert_eq!(second["catchup"], true);

    // Talkgroup 200 was not selected, so it is not replayed.
    assert!(
        received(&mut ws).await.is_none(),
        "only selected talkgroups are backfilled"
    );
}

/// A fresh subscription with no `since` cursor does not replay history — only
/// live Calls arriving after it flow (the common first-connect case).
#[tokio::test]
async fn fresh_subscription_without_cursor_does_not_replay() {
    let app = feed_app().await;
    post_call(&app, 11, 100).await; // exists before the client subscribes

    let mut ws = app.connect_ws().await;
    subscribe(&mut ws, r#"{"t":"sub","sel":{"11":{"100":true}}}"#).await;

    // No cursor -> the pre-existing call is not backfilled.
    assert!(
        received(&mut ws).await.is_none(),
        "no `since` cursor means no catch-up"
    );
}

/// A **Backfill** costs a fixed handful of database round-trips however many
/// Calls it carries (#86) — the property an N+1 hides, because the frames it
/// sends are correct either way and only the cost behind them is wrong.
///
/// It used to denormalize one Call at a time, so a Listener whose phone woke up
/// on a network blip charged a Pi seven round-trips per Call and up to seven
/// hundred per reconnect. rdio-scanner is worse still: it returns bare ids and
/// makes the client re-request each Call over its own WebSocket.
///
/// Two sizes and not a pinned number, deliberately: what must hold is that the
/// cost does not grow per Call, and a pinned constant would make a legitimately
/// added query look like this regression.
#[tokio::test]
async fn backfill_costs_the_same_queries_however_many_calls_it_carries() {
    let few = backfill_statements(2).await;
    let many = backfill_statements(20).await;

    // A Backfill costs *something*, or the two below are equal because nothing
    // is being counted rather than because nothing grows.
    assert!(few > 0, "a Backfill reads the archive");
    assert_eq!(
        few, many,
        "a Backfill of 20 Calls must cost what a Backfill of 2 does"
    );
}

/// Backfill `calls` Calls to a reconnecting Listener, and answer with the number
/// of statements the Instance issued doing it.
async fn backfill_statements(calls: i64) -> u64 {
    let app = feed_app().await;
    // Distinct Talkgroups, so each is its own Call rather than a duplicate — and
    // so the batched lookups have more than one row to resolve.
    for talkgroup in 0..calls {
        post_call(&app, 11, 100 + talkgroup).await;
    }
    // Every Worker owes nothing (#93), so the statements counted below are the
    // Backfill's own and not an ingest still being finished behind it.
    app.settle().await;

    let mut ws = app.connect_ws().await;
    let before = app.statements_issued();
    subscribe(&mut ws, r#"{"t":"sub","all":true,"since":0}"#).await;
    for _ in 0..calls {
        received(&mut ws).await.expect("a backfilled Call");
    }
    // Every statement the Backfill issues precedes the first frame it sends, so
    // by here they are all counted.
    app.statements_issued() - before
}

/// Every Call goes out carrying the **emission** it was sent as (#94), and the
/// archive records the same number — so the cursor a Listener hands back as
/// `since` is one the server can order a Backfill by.
///
/// Not the Call's id, though on this path they coincide: ingest stores and emits
/// in one breath, so the first Call of an empty archive is row 1 and emission 1.
/// They stop coinciding the moment a **Delay** (#73) holds one back.
#[tokio::test]
async fn a_live_frame_carries_the_emission_it_went_out_as() {
    let app = feed_app().await;
    let mut ws = app.connect_ws().await;
    subscribe(&mut ws, r#"{"t":"sub","all":true}"#).await;

    post_call(&app, 11, 54241).await;

    let frame = received(&mut ws).await.expect("the Call reaches the feed");
    assert_eq!(frame["seq"], 1, "the first emission of an empty archive");
    assert_eq!(
        app.the_call().await.emitted_seq,
        Some(1),
        "and the archive records the emission it went out as"
    );
}

// ---------------------------------------------------------------------------
// #94: the Backfill cursor is an emission sequence, distinct from the Call id.
// ---------------------------------------------------------------------------

/// **The hole an emission cursor closes.** A Call stored *before* another but
/// emitted *after* it must still reach a Listener who reconnects in between —
/// and a cursor over storage order steps straight past it, because the late Call
/// carries the lower id.
///
/// This is #73's **Delay** in miniature: a safety policy stores a Call on arrival
/// and releases it seconds later. Settled here rather than inside #73, because a
/// protocol change arriving as the side effect of a policy feature is how a
/// Listener ends up never hearing the one Call somebody decided to hold.
#[tokio::test]
async fn a_call_stored_early_and_emitted_late_is_backfilled() {
    let app = feed_app().await;
    // Stored first, held back: no emission yet, and the lowest id in the archive.
    let held = app
        .seed_call(NewCall::new(11, 100, 1_000), common::audio_at("held.wav"))
        .await;
    // Stored second and emitted immediately, the ordinary path.
    post_call(&app, 11, 200).await;
    // And now the policy releases the first one.
    app.emit(held).await;

    let mut ws = app.connect_ws().await;
    subscribe(&mut ws, r#"{"t":"sub","all":true,"since":0}"#).await;

    let first = received(&mut ws).await.expect("the Call emitted first");
    let second = received(&mut ws).await.expect("the Call emitted second");
    assert_eq!(first["call"]["talkgroupRef"], 200);
    assert_eq!(
        second["call"]["talkgroupRef"], 100,
        "the held Call is delivered, and after the Call that went out before it"
    );
    assert!(
        second["call"]["id"].as_i64() < first["call"]["id"].as_i64(),
        "it really does carry the lower id: a cursor over ids would have skipped it"
    );
    assert_eq!(second["seq"], 2, "emitted second, so it is emission 2");
}

/// A **Backfill** that could not reach back as far as the Listener asked says so,
/// because a silent truncation is indistinguishable from having missed nothing —
/// and the Listener needs to know to go and search the archive (#13).
#[tokio::test]
async fn a_truncated_backfill_tells_the_listener_their_history_has_a_gap() {
    let app = feed_app().await;
    // One past the bound, so the oldest Call cannot be carried. Seeded and
    // emitted rather than uploaded: this is about the size of the page, and a
    // hundred and one multipart posts would say nothing more about it.
    for talkgroup in 0..101 {
        let id = app
            .seed_call(
                NewCall::new(11, 100 + talkgroup, 1_000 + talkgroup),
                common::audio_at(format!("k{talkgroup}.wav")),
            )
            .await;
        app.emit(id).await;
    }

    let mut ws = app.connect_ws().await;
    subscribe(&mut ws, r#"{"t":"sub","all":true,"since":0}"#).await;

    let gap = received(&mut ws).await.expect("the gap notice");
    assert_eq!(
        gap["t"], "gap",
        "and it arrives before the page it is about"
    );
    assert_eq!(gap["since"], 0);
    let first = received(&mut ws).await.expect("the oldest Call that fit");
    assert_eq!(
        first["seq"], 2,
        "emission 1 is the Call that did not fit, which is what the gap is"
    );
}

/// A Backfill that reached far enough says nothing about gaps: a `gap` on every
/// reconnect is a notice a client learns to ignore.
#[tokio::test]
async fn an_untruncated_backfill_says_nothing_about_gaps() {
    let app = feed_app().await;
    post_call(&app, 11, 100).await;

    let mut ws = app.connect_ws().await;
    subscribe(&mut ws, r#"{"t":"sub","all":true,"since":0}"#).await;

    let frame = received(&mut ws).await.expect("the backfilled Call");
    assert_eq!(frame["t"], "call", "no gap preceded it");
}

/// The emission sequence **survives a restart**. Starting over at 1 would hand
/// new Calls numbers every reconnecting Listener's cursor is already past, so
/// their Backfills would come back empty until the counter had climbed back over
/// the archive — a hole that opens on restart and closes on its own.
#[tokio::test]
async fn the_emission_sequence_resumes_where_the_last_process_left_it() {
    let mut app = feed_app().await;
    post_call(&app, 11, 100).await;
    post_call(&app, 11, 200).await;

    // A restart, not a new app: the archive — and the ingest key registered on
    // it — are the ones the last run left behind.
    app.restart().await;
    post_call(&app, 11, 300).await;

    let after = app.calls().await;
    assert_eq!(
        after.last().expect("the newest Call").emitted_seq,
        Some(3),
        "the third emission of this archive, not the first of this process"
    );
}

/// A boot that cannot read the archive's high-water mark **still boots** — the
/// same posture a Retention sweep that cannot run takes, because a scanner an
/// operator can still reach the Logs view of is worth more than one that refuses
/// to start.
///
/// ERROR rather than the WARN its per-Call sibling uses (ADR-0011 rule 7): a
/// Call that could not record its emission costs one Listener one replay, where
/// a sequence that could not be resumed would mis-number *every* Call this
/// process emits.
#[tokio::test]
async fn a_boot_that_cannot_read_the_emission_sequence_still_serves() {
    let mut app = feed_app().await;
    let logged = app.store_logs();
    app.refuse_statements_on("calls");

    app.restart().await;

    assert_eq!(
        app.get("/healthz").await.status(),
        200,
        "an unreadable archive took the scanner down with it"
    );
    assert!(
        logged
            .wait_for("could not read the emission sequence")
            .await
            .contains("ERROR")
    );
}

/// An emission that cannot be written down costs the Listener a replay, never
/// the Call: everyone connected still hears it, the row keeps no emission (so
/// nothing backfills a Call that may never have gone out), and the operator is
/// told — because a Call missing from one Listener's Backfill and present in
/// everyone else's is only ever reported as "some calls go missing sometimes".
#[tokio::test]
async fn an_emission_that_cannot_be_recorded_still_reaches_the_feed() {
    let app = feed_app().await;
    let logged = app.store_logs();
    let mut ws = app.connect_ws().await;
    subscribe(&mut ws, r#"{"t":"sub","all":true}"#).await;
    // Only the update: the insert and the read-backs an ingest needs still work,
    // so the Call is genuinely stored and genuinely answered.
    app.refuse_updates_to("calls");

    post_call(&app, 11, 54241).await;

    let frame = received(&mut ws).await.expect("the Call still goes out");
    assert_eq!(frame["call"]["talkgroupRef"], 54241);
    assert_eq!(
        app.the_call().await.emitted_seq,
        None,
        "and the row does not claim an emission it never recorded"
    );
    assert!(
        logged
            .wait_for("emission could not be recorded")
            .await
            .contains("WARN")
    );
}

/// A Backfill whose read fails costs the Listener their history, never their
/// connection — and never silently.
///
/// Two failure points, one promise, and since #98 one message: the page and the
/// views behind it are read by one module, so `error=` is what says which
/// statement went down. This case is the page itself.
#[tokio::test]
async fn a_backfill_that_cannot_be_read_leaves_the_connection_serving() {
    let app = feed_app().await;
    let logged = app.store_logs();
    let mut ws = app.connect_ws().await;
    app.refuse_statements_on("calls");

    subscribe(&mut ws, r#"{"t":"sub","all":true,"since":0}"#).await;

    assert!(
        received(&mut ws).await.is_none(),
        "an unreadable page carries no Calls"
    );
    // The connection is still there: it acks a second subscribe.
    subscribe(&mut ws, r#"{"t":"sub","all":true}"#).await;
    assert!(
        logged
            .wait_for("live-feed Backfill failed")
            .await
            .contains("WARN")
    );
}

/// ...and this one is the denormalizing behind it: the page was read, but the
/// Systems and Talkgroups a Call's view is built from are not answering. A
/// half-built page must never reach a Listener either way.
#[tokio::test]
async fn a_backfill_whose_view_cannot_be_built_leaves_the_connection_serving() {
    let app = feed_app().await;
    post_call(&app, 11, 100).await;
    let logged = app.store_logs();
    let mut ws = app.connect_ws().await;
    // `calls` still answers, so the page is read; `systems` does not, so the view
    // cannot be built.
    app.refuse_statements_on("systems");

    subscribe(&mut ws, r#"{"t":"sub","all":true,"since":0}"#).await;

    assert!(received(&mut ws).await.is_none(), "no half-built page");
    assert!(
        logged
            .wait_for("live-feed Backfill failed")
            .await
            .contains("WARN")
    );
}

/// Patch fanout (#9, spec story 18): a Call on a Talkgroup the listener didn't
/// select still reaches them when they subscribe to one it's patched to, and the
/// patch list rides the wire for display.
#[tokio::test]
async fn patched_call_reaches_a_subscriber_of_the_patched_talkgroup() {
    let app = feed_app().await;
    // A patch names a Talkgroup only if the System has one (#81) — a listener
    // can select 300 precisely because it is a channel this System knows.
    app.seed_talkgroup(11, 300).await;
    let mut ws = app.connect_ws().await;

    // Subscribed to 300 only — NOT the call's own talkgroup 100.
    subscribe(&mut ws, r#"{"t":"sub","sel":{"11":{"300":true}}}"#).await;

    post_call_with_patches(&app, 11, 100, "[300]").await;

    let call = received(&mut ws).await.expect("patched call delivered");
    assert_eq!(call["call"]["talkgroupRef"], 100);
    assert_eq!(call["call"]["patches"], serde_json::json!([300]));
}

/// The other half of patch fanout (#81): a number that is *not* a Talkgroup on
/// this System never reaches the listener who selected it.
///
/// SDRTrunk appends a patch group's radio IDs after its talkgroups in the same
/// `patches` array with nothing between them
/// (`RdioScannerBroadcaster.java:546-574`). Treating those trailing entries as
/// Talkgroup Refs pushed the Call to anyone subscribed to a *radio* id that
/// collided with a channel number they had selected — audio arriving on a
/// talkgroup that never carried it.
#[tokio::test]
async fn a_radio_id_trailing_the_patched_talkgroups_does_not_fan_out() {
    let app = feed_app().await;
    // 300 is a Talkgroup; 1610092 is a radio, and this System has no Talkgroup
    // for it — which is the only thing that tells the two apart on the wire.
    app.seed_talkgroup(11, 300).await;
    let mut ws = app.connect_ws().await;

    subscribe(&mut ws, r#"{"t":"sub","sel":{"11":{"1610092":true}}}"#).await;

    post_call_with_patches(&app, 11, 100, "[300, 1610092]").await;

    assert!(
        received(&mut ws).await.is_none(),
        "a trailing radio id is not a patch member, so it fans out to nobody"
    );
}

/// An encrypted Call is still a Call: a listener watching a mostly-encrypted
/// talkgroup sees that it is busy rather than a dead feed (#42, spec US 9).
///
/// It arrives flagged and with **no `audioUrl`**, so the thing the player would
/// need in order to try is simply not there — the queue cannot be poisoned by a
/// client that forgets to check a flag.
#[tokio::test]
async fn an_encrypted_call_reaches_a_listener_flagged_and_unplayable() {
    let app = feed_app().await;
    let mut ws = app.connect_ws().await;
    subscribe(&mut ws, r#"{"t":"sub","all":true}"#).await;

    let meta = r#"{"short_name":"butco","talkgroup":54241,
                   "start_time":1669740338,"call_length_ms":4000,"encrypted":1}"#;
    let (status, body) = app.upload_tr(CallUpload::tr(meta).key("test-key")).await;
    assert_eq!(status, 200, "{body:?}");

    let frame = received(&mut ws).await.expect("the Call reaches the feed");
    assert_eq!(frame["t"], "call");
    assert_eq!(frame["call"]["encrypted"], true);
    assert_eq!(frame["call"]["durationMs"], 4000);
    assert!(
        frame["call"].get("audioUrl").is_none(),
        "nothing to play: {frame}"
    );
}

/// The flags a listener draws badges from ride the same frame — an emergency
/// button press is on screen the moment it happens, not once someone searches
/// the archive for it (#42, and what #53 alerts on).
#[tokio::test]
async fn the_emergency_flag_rides_the_live_frame() {
    let app = feed_app().await;
    let mut ws = app.connect_ws().await;
    subscribe(&mut ws, r#"{"t":"sub","all":true}"#).await;

    let meta = r#"{"short_name":"butco","talkgroup":54241,
                   "start_time":1669740338,"emergency":1}"#;
    app.upload_tr(CallUpload::tr(meta).key("test-key")).await;

    let frame = received(&mut ws).await.expect("the Call reaches the feed");
    assert_eq!(frame["call"]["emergency"], true);
    assert!(
        frame["call"].get("encrypted").is_none(),
        "a quiet flag is omitted, keeping the frame small: {frame}"
    );
}
