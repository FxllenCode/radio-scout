//! **Webhooks**, driven end to end (#54, spec US 21).
//!
//! The sink is a real HTTP server ([`common::Sink`]), so what is asserted here
//! is the JSON that actually left the process, and the queue is the real table,
//! so an outage is an outage. The pure halves — routing, the mark vocabulary,
//! the two payload shapes, the retry verdict — are unit-tested in
//! `src/webhook/`; this file is for the wiring, the durability, and the two
//! promises that can only be made about a running Instance: that an **ordinary
//! Call costs nothing at all**, and that a webhook's **URL never leaves**.
//!
//! Nothing here is Listener-facing. A Webhook is a row an Operator wrote, and
//! ADR-0014 stays true: this Instance still wakes no device it has never met.

mod common;

use common::{CallUpload, Sink, TestApp, unreachable_hook_url};
use rstest::rstest;
use serde_json::json;

/// A transmission instant nothing else in this file shares.
///
/// **Every Call here needs its own**, because two uploads on one channel a few
/// hundred milliseconds apart are one transmission as far as **Ingest** is
/// concerned (#46) — so a fixed `start_time` would make the second upload of
/// any pair a duplicate, answered `200` and stored as nothing. That reads as
/// "the webhook did not fire" and is in fact "there was no second Call".
fn a_fresh_instant() -> i64 {
    static NEXT: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(1_669_740_338);
    NEXT.fetch_add(60, std::sync::atomic::Ordering::Relaxed)
}

/// Trunk Recorder's meta for a transmission with the **emergency** bit set —
/// the only mark that exists until #55, and the one thing the rdio upload
/// dialect has no field for at all.
fn emergency_meta(talkgroup: i64) -> String {
    emergency_meta_at(talkgroup, a_fresh_instant())
}

/// [`emergency_meta`] at a named instant — for the one assertion that pins a
/// rendered timestamp, which a rolling instant could not.
fn emergency_meta_at(talkgroup: i64, start: i64) -> String {
    format!(
        r#"{{"short_name":"sys11","talkgroup":{talkgroup},
           "start_time":{start},"call_length_ms":4000,
           "emergency":1,"encrypted":0,
           "srcList":[{{"src":4424000,"time":{start},"pos":0.0}}]}}"#
    )
}

/// The same transmission with nobody's thumb on the button — the ordinary Call,
/// which is 99.99% of a scanner's traffic.
fn ordinary_meta(talkgroup: i64) -> String {
    let start = a_fresh_instant();
    format!(
        r#"{{"short_name":"sys11","talkgroup":{talkgroup},
           "start_time":{start},"call_length_ms":4000,
           "emergency":0,"encrypted":0}}"#
    )
}

/// Everything on one System.
///
/// The metas above say `short_name: "sys11"`, which is the label
/// [`TestApp::seed_system`] gives System 11 — so an upload lands on a System with
/// a Ref a scope can name, rather than on whichever Ref auto-populate happened to
/// mint.
fn whole_system(system_ref: i64) -> serde_json::Value {
    json!({ "sel": { system_ref.to_string(): { "*": true } } })
}

/// An app whose retries are quick enough to watch, and a sink for it.
///
/// The backoff is a `Duration` in code and only its *serde* spelling is `_secs`,
/// so a test sets milliseconds directly through the same `Config` an Operator
/// edits — the `tests/downstream.rs` arrangement, and for its reason.
async fn app_with_a_sink() -> (TestApp, Sink) {
    app_at(radio_scout::Clock::system()).await
}

/// [`app_with_a_sink`] with the Instance's clock named — for the one assertion
/// that reads a scheduled instant rather than watching something happen.
async fn app_at(clock: radio_scout::Clock) -> (TestApp, Sink) {
    let app = TestApp::builder()
        .clock(clock)
        .config(|config| {
            config.server.public_url = Some(String::from("https://scan.example"));
            config.webhook.retry_initial = std::time::Duration::from_millis(5);
            config.webhook.retry_max = std::time::Duration::from_millis(20);
        })
        .spawn()
        .await;
    app.login().await;
    app.create_api_key("k").await;
    app.seed_system(11, true, None).await;
    let sink = Sink::start().await;
    (app, sink)
}

/// Upload one Call and let everything it set in motion settle.
async fn upload(app: &TestApp, meta: &str) {
    let (status, body) = app.upload_tr(CallUpload::tr(meta)).await;
    assert_eq!(status, 200, "the upload should have been accepted: {body}");
    app.settle().await;
}

// ---------------------------------------------------------------------------
// The payload, over the wire
// ---------------------------------------------------------------------------

/// **The headline test**: a Call carrying an Emergency reaches a webhook sink as
/// JSON, carrying the Call a Listener would see and an absolute link to its
/// audio.
#[tokio::test]
async fn a_call_carrying_an_emergency_reaches_a_webhook_as_json() {
    let (app, sink) = app_with_a_sink().await;
    app.add_webhook(&sink.url(), &["emergency"], whole_system(11))
        .await;

    upload(&app, &emergency_meta(54241)).await;

    let received = sink.received();
    assert_eq!(received.len(), 1, "one Emergency, one message");
    let posted = &received[0];
    assert_eq!(posted["marks"], json!(["emergency"]));
    assert_eq!(posted["call"]["talkgroupRef"], 54241);
    assert_eq!(posted["call"]["emergency"], true);
    assert_eq!(
        posted["call"]["audioUrl"], "https://scan.example/api/call/1/audio",
        "the link is absolute, because a chat room is not on this origin"
    );
}

/// The Discord-shaped variant is what Discord accepts: one embed, a title, a
/// colour and an ISO-8601 timestamp — the fields its API validates and answers
/// `400` for.
#[tokio::test]
async fn the_discord_variant_is_the_shape_discord_renders() {
    let (app, sink) = app_with_a_sink().await;
    app.add_webhook_shaped(&sink.url(), "discord", &["emergency"], whole_system(11))
        .await;

    upload(&app, &emergency_meta_at(54241, 1_669_740_338)).await;

    let received = sink.received();
    let embed = &received[0]["embeds"][0];
    // Auto-populate labels a new Talkgroup with its own Ref, so this is an
    // uncurated channel's title. The `Talkgroup {ref}` fallback is for a Call
    // whose channel carries no label at all, and is unit-tested.
    assert_eq!(embed["title"], "54241");
    assert_eq!(embed["description"], "Emergency");
    assert!(embed["color"].as_u64().is_some_and(|c| c > 0));
    assert_eq!(
        embed["timestamp"], "2022-11-29T16:45:38Z",
        "ISO-8601, which is the only shape Discord's embed timestamp accepts"
    );
    assert_eq!(embed["url"], "https://scan.example/api/call/1/audio");
}

/// **An Instance nobody has told where it lives still delivers**, with every
/// fact and no link — because a *relative* URL in a Discord embed is a `400`,
/// and a `400` abandons the delivery. The Emergency arriving without a link
/// beats it not arriving at all.
#[tokio::test]
async fn an_instance_with_no_public_url_posts_the_facts_and_no_link() {
    let app = TestApp::builder().spawn().await;
    app.login().await;
    app.create_api_key("k").await;
    app.seed_system(11, true, None).await;
    let sink = Sink::start().await;
    app.add_webhook(&sink.url(), &["emergency"], whole_system(11))
        .await;

    upload(&app, &emergency_meta(54241)).await;

    let received = sink.received();
    assert_eq!(received.len(), 1);
    assert_eq!(received[0]["call"]["talkgroupRef"], 54241);
    assert!(
        received[0]["call"]["audioUrl"].is_null(),
        "a relative URL is worse than none: {received:?}"
    );
}

/// An **encrypted** Call is posted, where it is never forwarded: a webhook
/// carries facts, and an encrypted Emergency is exactly the fact an Operator
/// most wants to be told about.
#[tokio::test]
async fn an_encrypted_emergency_is_still_posted() {
    let (app, sink) = app_with_a_sink().await;
    app.add_webhook(&sink.url(), &["emergency"], whole_system(11))
        .await;

    upload(
        &app,
        r#"{"short_name":"sys11","talkgroup":54241,"start_time":1669740338,
            "call_length_ms":4000,"emergency":1,"encrypted":1}"#,
    )
    .await;

    let received = sink.received();
    assert_eq!(received.len(), 1, "an encrypted Emergency still says so");
    assert_eq!(received[0]["call"]["encrypted"], true);
    assert!(received[0]["call"]["audioUrl"].is_null());
}

// ---------------------------------------------------------------------------
// What fires, and what does not
// ---------------------------------------------------------------------------

/// **The trigger is a mark *and* a scope.** Everything else is somebody else's
/// business, and a webhook that fired on ordinary traffic would be a firehose
/// into a chat room rather than an inbox.
#[rstest]
#[case::in_scope_and_marked(true, 11, 1)]
#[case::marked_but_out_of_scope(true, 22, 0)]
#[case::in_scope_but_unmarked(false, 11, 0)]
#[tokio::test]
async fn only_a_marked_call_on_a_scoped_channel_fires(
    #[case] emergency: bool,
    #[case] scoped_system: i64,
    #[case] expected: usize,
) {
    let (app, sink) = app_with_a_sink().await;
    app.add_webhook(&sink.url(), &["emergency"], whole_system(scoped_system))
        .await;

    let meta = match emergency {
        true => emergency_meta(54241),
        false => ordinary_meta(54241),
    };
    upload(&app, &meta).await;

    assert_eq!(sink.count(), expected);
}

/// A webhook an Operator has switched off receives nothing, and nothing is
/// queued for it either — so switching one back on does not replay the week.
#[tokio::test]
async fn a_disabled_webhook_is_neither_posted_to_nor_queued_for() {
    let (app, sink) = app_with_a_sink().await;
    let id = app
        .add_webhook(&sink.url(), &["emergency"], whole_system(11))
        .await;
    let (status, _) = app
        .admin_patch(
            &format!("/api/admin/webhooks/{id}"),
            json!({"disabled": true}),
        )
        .await;
    assert_eq!(status, 200);

    upload(&app, &emergency_meta(54241)).await;

    assert_eq!(sink.count(), 0);
    assert_eq!(app.queued_for_webhook(id).await, 0);
}

/// **An ordinary Call costs this feature nothing at all** — not one statement.
///
/// This is the difference from **Downstream**, which reads its roster on every
/// upload because any Call may reach a peer. A Call with no mark can reach no
/// webhook, so the roster read sits behind the mark check, and on a Pi taking a
/// Call a second with Emergencies a few times a day that is the whole cost of
/// having the feature.
///
/// Asserted as a *difference* rather than as a pinned number, so it survives
/// every other statement ingest gains.
#[tokio::test]
async fn an_ordinary_call_issues_no_webhook_statement_at_all() {
    let (app, sink) = app_with_a_sink().await;
    app.add_webhook(&sink.url(), &["emergency"], whole_system(11))
        .await;
    // One Call first, so the measurements below are of a **steady state**: the
    // very first upload on a channel auto-populates a Talkgroup and costs a
    // handful of statements that have nothing to do with this feature.
    upload(&app, &ordinary_meta(54241)).await;
    app.settle().await;

    let before = app.statements_issued();
    upload(&app, &ordinary_meta(54241)).await;
    let ordinary = app.statements_issued() - before;

    let before = app.statements_issued();
    upload(&app, &emergency_meta(54241)).await;
    let marked = app.statements_issued() - before;

    assert!(
        marked > ordinary,
        "an Emergency should cost the roster read and the queue insert that an \
         ordinary Call does not: {ordinary} vs {marked}"
    );
    assert_eq!(sink.count(), 1);
}

// ---------------------------------------------------------------------------
// Durability
// ---------------------------------------------------------------------------

/// **A sink's outage costs delay, not Calls.** The Emergency is written to the
/// queue inside the transaction that stores the Call, so it survives whatever
/// the endpoint is doing.
#[tokio::test]
async fn an_unreachable_sink_queues_rather_than_losing_the_call() {
    let (app, _sink) = app_with_a_sink().await;
    let id = app
        .add_webhook(&unreachable_hook_url(), &["emergency"], whole_system(11))
        .await;

    upload(&app, &emergency_meta(54241)).await;

    assert_eq!(
        app.queued_for_webhook(id).await,
        1,
        "the Emergency is written down, not dropped"
    );
}

/// ...and it drains **in order** once the endpoint comes back, which is what
/// makes a chat room readable rather than a shuffled pile.
#[tokio::test]
async fn an_outage_queues_calls_and_drains_them_in_order() {
    let (app, sink) = app_with_a_sink().await;
    let id = app
        .add_webhook(&sink.url(), &["emergency"], whole_system(11))
        .await;
    sink.answer_with(503);

    for talkgroup in [100, 200, 300] {
        upload(&app, &emergency_meta(talkgroup)).await;
    }
    assert_eq!(app.queued_for_webhook(id).await, 3, "all three are owed");
    assert_eq!(sink.count(), 0);

    sink.come_back();
    app.webhooks_settled(3).await;

    let arrived: Vec<i64> = sink
        .received()
        .iter()
        .map(|posted| posted["call"]["talkgroupRef"].as_i64().expect("a ref"))
        .collect();
    assert_eq!(arrived, vec![100, 200, 300]);
    assert_eq!(app.queued_for_webhook(id).await, 0);
}

/// A restart posts what the last process could not — the queue is a table, so
/// the Emergency survives the Instance that took it.
///
/// The endpoint is made deliverable by **re-pointing the webhook** rather than
/// by switching a stub's status, and that is not incidental: a status switch
/// leaves a window in which an attempt begun under the old one lands under the
/// new, which `common::peer` documents as a one-in-nine flake. Nothing ever
/// answers on the unreachable URL, so no delivery can be recorded before the
/// `PATCH` — and the sender finding the new address on a retry is itself the
/// property an Operator relies on when they fix a typo.
#[tokio::test]
async fn a_restart_posts_what_the_last_process_could_not() {
    let (mut app, sink) = app_with_a_sink().await;
    let id = app
        .add_webhook(&unreachable_hook_url(), &["emergency"], whole_system(11))
        .await;
    upload(&app, &emergency_meta(54241)).await;
    assert_eq!(app.queued_for_webhook(id).await, 1);

    app.restart().await;
    // A **Session** does not survive the process that issued it (CONTEXT.md), so
    // the Operator signs in again — which is exactly what one would do.
    app.login().await;
    let (status, _) = app
        .admin_patch(
            &format!("/api/admin/webhooks/{id}"),
            json!({ "url": sink.url() }),
        )
        .await;
    assert_eq!(status, 200);
    app.webhooks_settled(1).await;

    assert_eq!(sink.count(), 1, "the restart picked the queue back up");
    assert_eq!(app.queued_for_webhook(id).await, 0);
}

/// **A body the sink will never accept is abandoned, and the queue moves on.**
/// The alternative wedges every later Emergency behind a message Discord has
/// already judged — which is the one failure that would make this feature worse
/// than useless.
#[tokio::test]
async fn a_message_the_sink_will_never_accept_is_abandoned_and_the_queue_moves_on() {
    let (app, sink) = app_with_a_sink().await;
    let id = app
        .add_webhook(&sink.url(), &["emergency"], whole_system(11))
        .await;
    sink.answer_with(400);

    upload(&app, &emergency_meta(100)).await;
    app.webhooks_settled(1).await;
    assert_eq!(app.queued_for_webhook(id).await, 0, "abandoned, not stuck");

    sink.come_back();
    upload(&app, &emergency_meta(200)).await;
    app.webhooks_settled(2).await;

    let arrived: Vec<i64> = sink
        .received()
        .iter()
        .map(|posted| posted["call"]["talkgroupRef"].as_i64().expect("a ref"))
        .collect();
    assert_eq!(arrived, vec![200], "the one behind it still went");
}

/// **A rate limit is not a refusal.** Discord answers `429` when a channel is
/// busy, and abandoning there would drop exactly the Emergencies that arrived
/// in a flurry — which is when an Operator needs them most.
#[tokio::test]
async fn a_rate_limited_message_is_kept_and_retried() {
    let (app, sink) = app_with_a_sink().await;
    let id = app
        .add_webhook(&sink.url(), &["emergency"], whole_system(11))
        .await;
    sink.answer_with(429);

    upload(&app, &emergency_meta(54241)).await;
    assert_eq!(app.queued_for_webhook(id).await, 1);

    sink.come_back();
    app.webhooks_settled(1).await;

    assert_eq!(sink.count(), 1, "kept until the channel had room");
}

/// The backoff an Operator configured is the one applied — visible as a value on
/// the queue row, because a sender reading a default configuration instead of
/// this Instance's would still retry and no assertion about *what* arrives could
/// tell the difference.
#[tokio::test]
async fn the_configured_backoff_is_the_one_scheduled() {
    // **A frozen clock**, because this is the one assertion about a *scheduled*
    // instant rather than about something that happened: against a running clock
    // the answer is "five milliseconds after whenever the attempt was", which is
    // unknowable from outside and reads as a flake.
    const NOW: i64 = 1_700_000_000_000;
    let (app, sink) = app_at(radio_scout::Clock::frozen(NOW)).await;
    let id = app
        .add_webhook(&sink.url(), &["emergency"], whole_system(11))
        .await;
    sink.answer_with(503);

    upload(&app, &emergency_meta(54241)).await;
    // One attempt has been made and deferred; the row says when the next is due.
    let head = loop {
        let head = app
            .head_webhook_delivery(id)
            .await
            .expect("a queued delivery");
        if head.attempts > 0 {
            break head;
        }
        tokio::task::yield_now().await;
    };

    assert_eq!(head.attempts, 1);
    assert_eq!(
        head.next_attempt_ms,
        NOW + 5,
        "the configured 5ms, not the shipped 5s"
    );
    assert_eq!(head.last_failure.as_deref(), Some("sink-refused (503)"));
}

/// One sink's trouble is its own: a webhook holding the socket open delays
/// nothing but its own queue.
#[tokio::test]
async fn a_sink_holding_the_socket_open_does_not_delay_another() {
    let (app, slow) = app_with_a_sink().await;
    let prompt = Sink::start().await;
    slow.stall_for(std::time::Duration::from_secs(30));
    app.add_webhook(&slow.url(), &["emergency"], whole_system(11))
        .await;
    app.add_webhook(&prompt.url(), &["emergency"], whole_system(11))
        .await;

    // Deliberately *not* `settle()`: the slow sink is still holding its attempt,
    // so the sender has not caught up and will not until that attempt ends. The
    // whole claim is that the other sink does not have to wait for it.
    let (status, body) = app.upload_tr(CallUpload::tr(&emergency_meta(54241))).await;
    assert_eq!(status, 200, "{body}");
    app.webhooks_settled(1).await;

    assert_eq!(prompt.count(), 1, "the prompt sink was not held up");
    assert_eq!(slow.count(), 0, "and the slow one is still holding");
}

/// **Each webhook drains its own queue**, keyed on its own row id.
///
/// The shared draining loop looks a sink up by [`radio_scout::delivery::Sink::id`]
/// — which queue to read, and which sink is already attempting — so an id that
/// answered the same number for every row would have every webhook draining the
/// first one's backlog. That is invisible to any test with one webhook in it,
/// and invisible even to two webhooks that are *scoped alike*: both would still
/// receive a Call they were entitled to. It takes two scoped **differently**,
/// each holding a Call the other must never see.
#[tokio::test]
async fn two_webhooks_each_drain_their_own_queue() {
    let (app, fire) = app_with_a_sink().await;
    let police = Sink::start().await;
    app.add_webhook(
        &fire.url(),
        &["emergency"],
        json!({ "sel": { "11": { "100": true } } }),
    )
    .await;
    app.add_webhook(
        &police.url(),
        &["emergency"],
        json!({ "sel": { "11": { "200": true } } }),
    )
    .await;

    upload(&app, &emergency_meta(100)).await;
    upload(&app, &emergency_meta(200)).await;
    app.settle().await;

    let heard = |sink: &Sink| -> Vec<i64> {
        sink.received()
            .iter()
            .map(|posted| posted["call"]["talkgroupRef"].as_i64().expect("a ref"))
            .collect()
    };
    assert_eq!(heard(&fire), vec![100]);
    assert_eq!(heard(&police), vec![200]);
}

/// **The reading an Operator acts on**, since #70's status page does not exist
/// yet: queue depth, when it last worked, and how many attempts have failed
/// since — and the reset a success brings.
#[tokio::test]
async fn the_admin_listing_shows_a_webhooks_health() {
    let (app, sink) = app_with_a_sink().await;
    let id = app
        .add_webhook(&sink.url(), &["emergency"], whole_system(11))
        .await;
    sink.answer_with(503);

    upload(&app, &emergency_meta(54241)).await;

    let (_, body) = app.admin_get("/api/admin/webhooks").await;
    let row = &body["results"][0];
    assert_eq!(row["id"], id);
    assert_eq!(row["queued"], 1, "the durable depth, not the meter's");
    assert!(row["consecutiveFailures"].as_i64().unwrap_or_default() >= 1);
    assert_eq!(row["lastFailure"], "sink-refused (503)");
    assert!(row["lastSuccessMs"].is_null(), "it has never worked");

    sink.come_back();
    app.webhooks_settled(1).await;

    let (_, body) = app.admin_get("/api/admin/webhooks").await;
    let row = &body["results"][0];
    assert_eq!(row["queued"], 0);
    assert_eq!(row["consecutiveFailures"], 0, "reset by a success");
    assert!(row["lastSuccessMs"].as_i64().is_some());
}

/// A new webhook keeps the name it was created with — the only thing on the
/// screen that tells two Discord rows apart once the address is gone.
#[tokio::test]
async fn a_new_webhook_keeps_the_label_it_was_created_with() {
    let (app, sink) = app_with_a_sink().await;

    let (status, created) = app
        .admin_post(
            "/api/admin/webhooks",
            json!({
                "label": "  dispatch channel  ",
                "url": sink.url(),
                "marks": ["emergency"],
            }),
        )
        .await;

    assert_eq!(status, 201, "{created}");
    assert_eq!(
        created["label"], "dispatch channel",
        "trimmed, and kept: {created}"
    );
    let (_, body) = app.admin_get("/api/admin/webhooks").await;
    assert_eq!(body["results"][0]["label"], "dispatch channel");
}

// ---------------------------------------------------------------------------
// The URL is a credential
// ---------------------------------------------------------------------------

/// **A webhook's URL never reaches a log line**, at any level, on any path — it
/// is the credential, and every line about a delivery names the **Id**.
///
/// Driven through a failing delivery on purpose: the success path is quiet, and
/// the lines that carry a URL in a naive implementation are the error ones,
/// because that is where a transport error's own `Display` gets rendered.
#[tokio::test]
async fn a_webhooks_url_never_reaches_a_log_line() {
    let capture = common::logs::LogCapture::start();
    let (app, _sink) = app_with_a_sink().await;
    app.add_webhook(&unreachable_hook_url(), &["emergency"], whole_system(11))
        .await;

    upload(&app, &emergency_meta(54241)).await;
    app.settle().await;

    let logged = capture.text();
    assert!(!logged.contains("/hook/"), "{logged}");
    assert!(!logged.contains(Sink::TOKEN), "{logged}");
}

/// ...and it never comes back out of the admin surface either. The listing
/// carries the **host**, which is what lets an Operator tell two Discord
/// webhooks apart without handing either token to anyone who reaches the page.
#[tokio::test]
async fn the_admin_listing_shows_a_host_and_never_a_url() {
    let (app, sink) = app_with_a_sink().await;
    let id = app
        .add_webhook(&sink.url(), &["emergency"], json!({ "all": true }))
        .await;

    let (status, body) = app.admin_get("/api/admin/webhooks").await;

    assert_eq!(status, 200);
    let row = &body["results"][0];
    assert_eq!(row["id"], id);
    assert!(
        row["host"]
            .as_str()
            .is_some_and(|h| h.starts_with("127.0.0.1"))
    );
    assert_eq!(row["marks"], json!(["emergency"]));
    assert_eq!(row["format"], "radio-scout");
    assert!(
        !body.to_string().contains(Sink::TOKEN),
        "the token came back out of the admin API: {body}"
    );
}

/// A `PATCH` that does not mention the URL keeps the stored one — which is the
/// only thing that makes editing a webhook's scope possible at all, since the
/// screen can never show the credential again.
#[tokio::test]
async fn re_scoping_a_webhook_keeps_the_url_it_already_had() {
    let (app, sink) = app_with_a_sink().await;
    let id = app
        .add_webhook(&sink.url(), &["emergency"], json!({}))
        .await;

    let (status, _) = app
        .admin_patch(
            &format!("/api/admin/webhooks/{id}"),
            json!({ "scope": whole_system(11) }),
        )
        .await;
    assert_eq!(status, 200);

    upload(&app, &emergency_meta(54241)).await;

    assert_eq!(
        sink.count(),
        1,
        "the re-scoped webhook still knows where to post"
    );
}

// ---------------------------------------------------------------------------
// Curation
// ---------------------------------------------------------------------------

/// A URL that could never be posted to is refused **while an Operator can still
/// see what they typed** — the last moment it will ever be on their screen.
#[rstest]
#[case::relative("/hook/abc", "unusable-webhook-url")]
#[case::no_scheme("discord.com/api/webhooks/1/t", "unusable-webhook-url")]
#[case::wrong_scheme("file:///etc/passwd", "unusable-webhook-url")]
#[case::blank("", "field-required")]
#[tokio::test]
async fn a_url_that_could_never_be_posted_to_is_refused(#[case] url: &str, #[case] slug: &str) {
    let app = TestApp::spawn().await;
    app.login().await;

    let (status, body) = app
        .admin_post(
            "/api/admin/webhooks",
            json!({ "url": url, "marks": ["emergency"] }),
        )
        .await;

    assert_eq!(status, 400, "{body}");
    assert_eq!(body["error"], slug, "{body}");
}

/// A mark or a format this release does not know is a typo an Operator can fix,
/// so it is refused by name rather than silently dropped into a webhook that
/// never fires.
#[rstest]
#[case::mark(json!({"marks": ["tone"]}), "unknown-mark", "tone")]
#[case::format(json!({"format": "slack"}), "unknown-format", "slack")]
#[tokio::test]
async fn an_unknown_mark_or_format_is_refused_by_name(
    #[case] extra: serde_json::Value,
    #[case] slug: &str,
    #[case] named: &str,
) {
    let app = TestApp::spawn().await;
    app.login().await;
    let mut body = json!({ "url": "https://hooks.example/x", "marks": ["emergency"] });
    for (key, value) in extra.as_object().expect("an object") {
        body[key] = value.clone();
    }

    let (status, answered) = app.admin_post("/api/admin/webhooks", body).await;

    assert_eq!(status, 400, "{answered}");
    assert_eq!(answered["error"], slug, "{answered}");
    assert!(
        answered["detail"]
            .as_str()
            .is_some_and(|d| d.contains(named)),
        "{answered}"
    );
}

/// **Every field a `PATCH` can name**, in one request — and the one it does not
/// name is the URL, which is the only thing that makes editing possible at all.
///
/// Worth driving through the real surface rather than trusting the form: each
/// field is its own arm on the server, and an arm nothing exercises is an arm
/// that can be dropped without a test noticing.
#[tokio::test]
async fn a_patch_can_change_everything_except_the_address() {
    let (app, sink) = app_with_a_sink().await;
    let id = app
        .add_webhook(&sink.url(), &["emergency"], json!({}))
        .await;

    let (status, body) = app
        .admin_patch(
            &format!("/api/admin/webhooks/{id}"),
            json!({
                "label": "renamed",
                "format": "discord",
                "marks": ["emergency"],
                "scope": whole_system(11),
            }),
        )
        .await;

    assert_eq!(status, 200, "{body}");
    assert_eq!(body["label"], "renamed");
    assert_eq!(body["format"], "discord");
    assert_eq!(body["marks"], json!(["emergency"]));

    // ...and it still knows where to post, which is the whole claim.
    upload(&app, &emergency_meta(54241)).await;
    assert_eq!(sink.count(), 1);
}

/// A `PATCH` that names a URL replaces it — the other half of "blank means
/// keep" — and one that names an unusable one is refused rather than stored.
#[tokio::test]
async fn a_patch_that_names_an_address_replaces_it_and_a_bad_one_is_refused() {
    let (app, sink) = app_with_a_sink().await;
    let moved = Sink::start().await;
    let id = app
        .add_webhook(&unreachable_hook_url(), &["emergency"], whole_system(11))
        .await;

    let (status, body) = app
        .admin_patch(
            &format!("/api/admin/webhooks/{id}"),
            json!({ "url": "not-a-url" }),
        )
        .await;
    assert_eq!(status, 400, "{body}");
    assert_eq!(body["error"], "unusable-webhook-url");

    let (status, _) = app
        .admin_patch(
            &format!("/api/admin/webhooks/{id}"),
            json!({ "url": moved.url() }),
        )
        .await;
    assert_eq!(status, 200);

    upload(&app, &emergency_meta(54241)).await;

    assert_eq!(moved.count(), 1, "the new address is the one used");
    assert_eq!(sink.count(), 0);
}

/// Deleting a webhook takes its queue with it — a delivery row outliving its
/// webhook would be read on every wake-up by a query nobody asks for.
#[tokio::test]
async fn deleting_a_webhook_takes_its_queue_with_it() {
    let (app, _sink) = app_with_a_sink().await;
    let id = app
        .add_webhook(&unreachable_hook_url(), &["emergency"], whole_system(11))
        .await;
    upload(&app, &emergency_meta(54241)).await;
    assert_eq!(app.queued_for_webhook(id).await, 1);

    let (status, _) = app.admin_delete(&format!("/api/admin/webhooks/{id}")).await;

    assert_eq!(status, 204);
    assert_eq!(app.queued_for_webhook(id).await, 0);
}

/// **A Call pruned while its webhook was down is abandoned, not retried
/// forever.** Retention is entitled to take a Call an endpoint has been
/// unreachable longer than, and the queue has to notice rather than wedge behind
/// something that will never exist again.
///
/// Undeliverable by *address* rather than by a stub's status, for the reason
/// `common::peer` documents: a status switch leaves a window in which an attempt
/// begun under the old one lands under the new, and here that window is the
/// whole assertion — an attempt rendered *before* the purge, answered *after*
/// the sink came back, posts a Call this test says no longer exists. Nothing
/// answers on the unreachable URL, so the first delivery that can possibly be
/// recorded is one begun after the `PATCH`, by which time the Call is gone.
#[tokio::test]
async fn a_call_that_is_gone_by_the_time_it_is_posted_is_abandoned() {
    let (app, sink) = app_with_a_sink().await;
    let id = app
        .add_webhook(&unreachable_hook_url(), &["emergency"], whole_system(11))
        .await;
    upload(&app, &emergency_meta(54241)).await;
    assert_eq!(app.queued_for_webhook(id).await, 1);

    // Retention's own pass, borrowed by the admin surface: the System goes and
    // takes its Calls with it.
    let systems = app.admin_get("/api/admin/systems").await.1;
    let system = systems["results"][0]["id"].as_i64().expect("a system id");
    let (status, body) = app
        .admin_delete(&format!("/api/admin/systems/{system}?force=true"))
        .await;
    assert_eq!(status, 204, "{body}");

    let (status, _) = app
        .admin_patch(
            &format!("/api/admin/webhooks/{id}"),
            json!({ "url": sink.url() }),
        )
        .await;
    assert_eq!(status, 200);
    app.webhooks_settled(1).await;

    assert_eq!(sink.count(), 0, "there was nothing left to post");
    assert_eq!(app.queued_for_webhook(id).await, 0, "and it stopped asking");
}

/// **A better copy re-enqueues without queueing a second message** (#46 × #54).
/// A **Replacement** can turn an encrypted Call into a decoded one, so the
/// webhook has to be offered the improved copy — but the usual case is that the
/// first copy has not been posted yet, and one row per (webhook, Call) is what
/// keeps that from becoming two messages in somebody's channel.
#[tokio::test]
async fn a_better_copy_does_not_queue_a_second_message() {
    let (app, sink) = app_with_a_sink().await;
    // Undeliverable by *address* rather than by a stub's status: nothing answers
    // on this port, so no attempt can be recorded before the `PATCH` below, and
    // "which copy went" is a fact rather than a race with an in-flight request
    // (`common::peer`'s one-in-nine flake).
    let id = app
        .add_webhook(&unreachable_hook_url(), &["emergency"], whole_system(11))
        .await;
    let start = 1_669_740_338;

    // The same transmission twice: encrypted first, then a decoded copy, which
    // keep-best takes as the better one.
    upload(
        &app,
        &format!(
            r#"{{"short_name":"sys11","talkgroup":54241,"start_time":{start},
                "call_length_ms":4000,"emergency":1,"encrypted":1}}"#
        ),
    )
    .await;
    upload(&app, &emergency_meta_at(54241, start)).await;

    assert_eq!(
        app.queued_for_webhook(id).await,
        1,
        "one transmission, one message owed"
    );

    let (status, _) = app
        .admin_patch(
            &format!("/api/admin/webhooks/{id}"),
            json!({ "url": sink.url() }),
        )
        .await;
    assert_eq!(status, 200);
    app.webhooks_settled(1).await;
    let received = sink.received();
    assert_eq!(received.len(), 1);
    // `encrypted` is omitted when it is false, so its absence *is* the
    // assertion — and the audio link is the other half of the same fact.
    assert!(
        received[0]["call"]["encrypted"].is_null(),
        "the encrypted copy went instead: {received:?}"
    );
    assert!(
        received[0]["call"]["audioUrl"].is_string(),
        "the better copy has audio to link to: {received:?}"
    );
}

/// A queue that cannot be written **fails the upload** rather than storing a
/// Call and quietly owing nobody: the delivery row is written inside the storing
/// transaction, so the two roll back together (#29's 5xx shape).
#[tokio::test]
async fn a_queue_that_cannot_be_written_refuses_the_upload() {
    let (app, sink) = app_with_a_sink().await;
    app.add_webhook(&sink.url(), &["emergency"], whole_system(11))
        .await;
    app.refuse_statements_on("webhook_deliveries");

    let (status, body) = app.upload_tr(CallUpload::tr(&emergency_meta(54241))).await;

    assert_eq!(status, 500, "{body}");
    assert!(body.contains("request id"), "{body}");
}

/// **No secret leaves in the configuration document** (#51's hard rule), which
/// for a Webhook means the whole row does not: its URL *is* the credential, and
/// a backup an Operator emails and commits must stay one they can email and
/// commit.
#[tokio::test]
async fn the_configuration_document_carries_no_webhooks_at_all() {
    let (app, sink) = app_with_a_sink().await;
    app.add_webhook(&sink.url(), &["emergency"], json!({ "all": true }))
        .await;

    let (status, body) = app.admin_get("/api/admin/config").await;

    assert_eq!(status, 200);
    let document = body.to_string();
    assert!(!document.contains(Sink::TOKEN), "{document}");
    assert!(!document.contains("webhook"), "{document}");
}
