//! Web Push (#16, spec US 32, ADR-0005), over the real HTTP boundary.
//!
//! The listener-facing half — subscribe, re-sync, unsubscribe — and the sending
//! half, against a stub push service the harness stands up
//! (`common::PushService`). Nothing here mocks the sender: a Call is ingested
//! and the request that leaves the process is the thing asserted on, decrypted
//! with the subscription's own private key.

mod common;

use common::{CallUpload, PushService, SUBSCRIBER_AUTH, SUBSCRIBER_PUBLIC, TestApp};
use radio_scout::db::entities::push_subscription;
use radio_scout::push::Push;
use rstest::rstest;
use sea_orm::EntityTrait;
use serde_json::{Value, json};

/// A `PushSubscription.toJSON()` as a browser reports it, for `endpoint`.
fn subscription(endpoint: &str, selection: Value) -> Value {
    json!({
        "endpoint": endpoint,
        "keys": { "p256dh": SUBSCRIBER_PUBLIC, "auth": SUBSCRIBER_AUTH },
        "selection": selection,
    })
}

/// Everything a listener hears.
fn everything() -> Value {
    json!({ "all": true, "sel": {} })
}

/// Register `endpoint` for everything, returning the token the server issued —
/// what the live-feed socket presents to say "this listener is me".
async fn subscribe_to(app: &TestApp, endpoint: &str) -> String {
    let response = app
        .post_json("/api/push/subscribe", subscription(endpoint, everything()))
        .await;
    assert_eq!(response.status(), 200);
    response.json::<Value>().await.expect("json")["token"]
        .as_str()
        .expect("the subscription's token")
        .to_string()
}

#[tokio::test]
async fn a_browser_subscribes_and_the_server_keeps_it() {
    let app = TestApp::spawn().await;

    let response = app
        .post_json(
            "/api/push/subscribe",
            subscription("https://push.example.net/p/abc", everything()),
        )
        .await;

    assert_eq!(response.status(), 200);
    let token = response.json::<Value>().await.expect("json")["token"]
        .as_str()
        .expect("the subscription's token")
        .to_string();
    let stored = push_subscription::Entity::find()
        .one(&app.db)
        .await
        .expect("read")
        .expect("a stored subscription");
    assert_eq!(stored.endpoint, "https://push.example.net/p/abc");
    assert_eq!(stored.p256dh, SUBSCRIBER_PUBLIC);
    assert_eq!(stored.token, token);
    // The handle a client holds is unguessable: a sequential Id would let
    // anyone silence anyone else's notifications by counting.
    assert_ne!(token, stored.id.to_string());
    assert!(token.len() >= 32, "{token}");
}

/// The public half of the server's VAPID identity is what a browser passes as
/// `applicationServerKey`, so it has to be fetchable before subscribing.
#[tokio::test]
async fn the_vapid_public_key_is_served_to_the_client() {
    let app = TestApp::spawn().await;

    let key = app.get_json("/api/push/key").await;

    assert_eq!(key["key"], common::VAPID_PUBLIC_KEY);
}

/// The whole point of the ticket: a Call on a watched Talkgroup wakes the
/// device, encrypted so that only that device can read it and signed so the
/// push service will carry it.
#[tokio::test]
async fn a_matching_call_notifies_the_subscribed_device() {
    let service = PushService::start().await;
    let app = TestApp::with_key("k").await;
    app.post_json(
        "/api/push/subscribe",
        subscription(&service.endpoint(), everything()),
    )
    .await;

    app.upload_ok(CallUpload::new()).await;

    let pushed = service.wait_for(1).await;
    let sent = &pushed[0];
    assert_eq!(sent.header("content-encoding"), Some("aes128gcm"));
    assert_eq!(sent.header("ttl"), Some("3600"));
    assert_eq!(sent.header("topic"), Some("t11-54241"));
    assert!(
        sent.header("authorization")
            .is_some_and(|value| value.starts_with("vapid t=")),
        "{:?}",
        sent.header("authorization")
    );

    let payload = sent.payload();
    assert_eq!(payload["systemRef"], 11);
    assert_eq!(payload["talkgroupRef"], 54241);
    assert_eq!(payload["count"], 1, "one Call, so it stands for one");
    assert_eq!(
        payload["id"],
        app.the_call().await.id,
        "the Call to open on tap"
    );
}

/// A notification is for a listener who *isn't* listening. While the live-feed
/// socket is open the feed already carries the Call, so the phone stays quiet —
/// the rule that keeps this feature from being the reason someone turns
/// notifications off.
#[tokio::test]
async fn a_listener_with_a_live_socket_is_not_notified() {
    let service = PushService::start().await;
    let app = TestApp::with_key("k").await;
    let token = subscribe_to(&app, &service.endpoint()).await;

    let mut ws = app.connect_ws().await;
    common::subscribe(
        &mut ws,
        &format!(r#"{{"t":"sub","all":true,"push":"{token}"}}"#),
    )
    .await;
    app.upload_ok(CallUpload::new()).await;

    // The feed delivered it...
    assert_eq!(common::next_json(&mut ws).await["t"], "call");
    // ...so the phone was left alone.
    service.expect_nothing().await;
}

/// And the moment that socket goes — a closed tab, the heartbeat reaping a phone
/// iOS suspended, or the listener switching the **live feed off** (#80) —
/// notifications take over.
///
/// That last case is why #80 needed no server change: a deliberately closed
/// socket is indistinguishable from a dropped one on the wire, so the suppression
/// rule above starts telling the truth about a listener who stopped listening the
/// moment the client stops holding the connection. This is the assertion for it.
#[tokio::test]
async fn a_dropped_socket_hands_the_listener_back_to_push() {
    let service = PushService::start().await;
    let app = TestApp::with_key("k").await;
    let token = subscribe_to(&app, &service.endpoint()).await;

    let mut ws = app.connect_ws().await;
    common::subscribe(
        &mut ws,
        &format!(r#"{{"t":"sub","all":true,"push":"{token}"}}"#),
    )
    .await;
    drop(ws);
    // Let the server notice the peer is gone before the Call lands.
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    app.upload_ok(CallUpload::new()).await;

    let pushed = service.wait_for(1).await;
    assert_eq!(pushed[0].payload()["talkgroupRef"], 54241);
}

/// The Selection is the whole point: notifications are for *watched*
/// Talkgroups, judged by exactly the rule the live feed applies.
#[tokio::test]
async fn a_call_outside_the_selection_notifies_nobody() {
    let service = PushService::start().await;
    let app = TestApp::with_key("k").await;
    app.post_json(
        "/api/push/subscribe",
        subscription(
            &service.endpoint(),
            json!({ "all": false, "sel": { "11": { "99999": true } } }),
        ),
    )
    .await;

    app.upload_ok(CallUpload::new()).await; // Talkgroup 54241

    service.expect_nothing().await;
}

/// A Call reaches a listener through a Talkgroup it is **patched** to, and the
/// notification names the Talkgroup they actually watch — not the one the
/// transmission happened to be on, which would be a name they never chose.
#[tokio::test]
async fn a_patched_call_notifies_through_the_watched_talkgroup() {
    let service = PushService::start().await;
    let app = TestApp::with_key("k").await;
    app.post_json(
        "/api/push/subscribe",
        subscription(
            &service.endpoint(),
            json!({ "all": false, "sel": { "11": { "300": true } } }),
        ),
    )
    .await;

    // 300 is a Talkgroup the System knows, so the patch names a real member (#81).
    app.seed_talkgroup(11, 300).await;
    app.upload_ok(CallUpload::new().talkgroup(100).set("patches", "[300]"))
        .await;

    let pushed = service.wait_for(1).await;
    assert_eq!(pushed[0].payload()["talkgroupRef"], 300);
    assert_eq!(pushed[0].header("topic"), Some("t11-300"));
}

/// The anti-storm rule, end to end: a Talkgroup carrying traffic notifies once
/// and then holds its peace.
#[tokio::test]
async fn a_burst_on_one_talkgroup_notifies_once() {
    let service = PushService::start().await;
    let app = TestApp::with_key("k").await;
    subscribe_to(&app, &service.endpoint()).await;

    for at in [1000, 2000, 3000] {
        app.upload_ok(CallUpload::new().at(at)).await;
    }

    let pushed = service.wait_for(1).await;
    assert_eq!(pushed.len(), 1, "three Calls, one notification");
    // ...and the two it stands for are not lost: they are counted into the
    // next one, which the unit tests pin without waiting out a window.
    service.expect_nothing().await;
}

/// `410 Gone` is a push service saying the device unsubscribed behind our back
/// — a subscription kept after that is a row we would retry for every Call
/// forever.
#[tokio::test]
async fn a_gone_subscription_is_forgotten() {
    let service = PushService::start().await;
    let app = TestApp::with_key("k").await;
    subscribe_to(&app, &service.endpoint()).await;
    service.answer_with(410);

    app.upload_ok(CallUpload::new()).await;
    service.wait_for(1).await;

    // The row goes with it, so the next Call doesn't try again.
    for _ in 0..50 {
        if app.count::<push_subscription::Entity>().await == 0 {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("a gone subscription was kept");
}

/// Re-subscribing is how a listener syncs a changed Selection, so the same
/// endpoint must update its row rather than leave a second one behind
/// notifying the same phone twice.
#[tokio::test]
async fn re_subscribing_updates_the_same_device() {
    let service = PushService::start().await;
    let app = TestApp::with_key("k").await;
    let first = subscribe_to(&app, &service.endpoint()).await;

    let again = app
        .post_json(
            "/api/push/subscribe",
            subscription(
                &service.endpoint(),
                json!({ "all": false, "sel": { "11": { "99999": true } } }),
            ),
        )
        .await;

    assert_eq!(again.status(), 200);
    assert_eq!(
        again.json::<Value>().await.expect("json")["token"].as_str(),
        Some(first.as_str()),
        "the same device, and the token a socket may already be holding"
    );
    assert_eq!(app.count::<push_subscription::Entity>().await, 1);
    // The new Selection is the one that decides.
    app.upload_ok(CallUpload::new()).await; // Talkgroup 54241, no longer watched
    service.expect_nothing().await;
}

/// Turning notifications off has to actually stop them.
#[tokio::test]
async fn unsubscribing_forgets_the_device() {
    let service = PushService::start().await;
    let app = TestApp::with_key("k").await;
    let token = subscribe_to(&app, &service.endpoint()).await;

    let response = app
        .post_json("/api/push/unsubscribe", json!({ "token": token }))
        .await;

    assert_eq!(response.status(), 204);
    assert_eq!(app.count::<push_subscription::Entity>().await, 0);
    app.upload_ok(CallUpload::new()).await;
    service.expect_nothing().await;
    // Idempotent: a browser that unsubscribes twice, or one whose row a `410`
    // already removed, is not an error.
    assert_eq!(
        app.post_json("/api/push/unsubscribe", json!({ "token": token }))
            .await
            .status(),
        204
    );
}

/// Everything a message needs is validated at the door, so a stored
/// subscription is always one we can deliver to — and the browser is told which
/// part was wrong rather than never hearing anything again.
#[rstest]
#[case(json!({"endpoint": "not-a-url", "keys": {"p256dh": SUBSCRIBER_PUBLIC, "auth": SUBSCRIBER_AUTH}}), "bad-endpoint")]
#[case(json!({"endpoint": "https://push.example.net/p", "keys": {"p256dh": "nope", "auth": SUBSCRIBER_AUTH}}), "bad-key")]
#[case(json!({"endpoint": "https://push.example.net/p", "keys": {"p256dh": SUBSCRIBER_PUBLIC, "auth": "nope"}}), "bad-auth")]
#[tokio::test]
async fn an_unusable_subscription_is_refused_and_says_why(
    #[case] body: Value,
    #[case] reason: &str,
) {
    let app = TestApp::spawn().await;

    let response = app.post_json("/api/push/subscribe", body).await;

    assert_eq!(response.status(), 400);
    assert_eq!(response.text().await.expect("body"), format!("{reason}\n"));
    assert_eq!(app.count::<push_subscription::Entity>().await, 0);
}

/// A server with no VAPID identity says so, rather than accepting
/// subscriptions it could never deliver to.
#[tokio::test]
async fn a_server_without_push_configured_refuses_it() {
    let app = TestApp::builder().push(Push::disabled()).spawn().await;

    assert_eq!(app.get("/api/push/key").await.status(), 404);
    let response = app
        .post_json(
            "/api/push/subscribe",
            subscription("https://push.example.net/p", everything()),
        )
        .await;
    assert_eq!(response.status(), 404);
    assert_eq!(app.count::<push_subscription::Entity>().await, 0);
}

/// A stub service exists to be pointed at; this proves the harness half before
/// the sender leans on it.
#[tokio::test]
async fn the_stub_push_service_records_what_it_is_sent() {
    let service = PushService::start().await;

    let response = reqwest::Client::new()
        .post(service.endpoint())
        .header("TTL", "60")
        .body(vec![1, 2, 3])
        .send()
        .await
        .expect("post");

    assert_eq!(response.status(), 201);
    let received = service.received();
    assert_eq!(received.len(), 1);
    assert_eq!(received[0].header("ttl"), Some("60"));
    assert_eq!(received[0].body, vec![1, 2, 3]);
}

/// A push service that refuses the message is a WARN and nothing else: the row
/// stays, because a 500 from a push service is its problem, not a device that
/// went away.
#[tokio::test]
async fn a_refused_push_is_logged_and_the_device_is_kept() {
    let capture = common::logs::LogCapture::start();
    let service = PushService::start().await;
    let app = TestApp::with_key("k").await;
    subscribe_to(&app, &service.endpoint()).await;
    service.answer_with(500);

    app.upload_ok(CallUpload::new()).await;
    service.wait_for(1).await;

    let line = capture.wait_for("web push refused").await;
    assert!(line.contains(" WARN "), "{line}");
    assert!(line.contains("status=500"), "{line}");
    assert_eq!(
        app.count::<push_subscription::Entity>().await,
        1,
        "a service that failed is not a device that unsubscribed"
    );
}

/// A push service that cannot be reached at all — the same line, with the
/// transport's own reason, and still no endpoint in it.
#[tokio::test]
async fn a_push_that_cannot_be_delivered_says_which_service_failed() {
    let capture = common::logs::LogCapture::start();
    let app = TestApp::with_key("k").await;
    // Port 1 on loopback: nothing listens, and nothing will.
    let endpoint = "http://127.0.0.1:1/push/gone";
    subscribe_to(&app, endpoint).await;

    app.upload_ok(CallUpload::new()).await;

    let line = capture.wait_for("web push failed").await;
    assert!(line.contains(" WARN "), "{line}");
    assert!(line.contains("service=http://127.0.0.1:1"), "{line}");
    capture.assert_never_logged(endpoint);
}

/// The 5xx paths (#29): a database that has gone out from under the handlers
/// answers with a correlation ref and keeps its cause on the server.
#[rstest]
#[case("/api/push/subscribe", "store-push-subscription")]
#[case("/api/push/unsubscribe", "delete-push-subscription")]
#[tokio::test]
async fn a_broken_database_keeps_its_cause_on_the_server(#[case] path: &str, #[case] stage: &str) {
    let capture = common::logs::LogCapture::start();
    let app = TestApp::spawn().await;
    app.break_table("push_subscriptions").await;

    let body = match path.ends_with("unsubscribe") {
        true => json!({ "token": "a-token-nobody-can-look-up" }),
        false => subscription("https://push.example.net/p/abc", everything()),
    };
    let response = app.post_json(path, body).await;

    assert_eq!(response.status(), 500);
    let reference = common::request_id_of(&response);
    let body = response.text().await.expect("body");
    assert_eq!(body, format!("internal error (ref: {reference})\n"));
    let line = capture.wait_for(stage).await;
    assert!(line.contains(" ERROR "), "{line}");
    assert!(
        line.contains(&app.missing_table_cause("push_subscriptions")),
        "why it failed: {line}"
    );
}

/// A database that goes out from under the *sender* costs notifications, not
/// the process: the Call is already stored and already on the live feed by the
/// time this runs.
#[tokio::test]
async fn a_sender_that_cannot_read_its_subscriptions_says_so_and_keeps_running() {
    let capture = common::logs::LogCapture::start();
    let service = PushService::start().await;
    let app = TestApp::with_key("k").await;
    subscribe_to(&app, &service.endpoint()).await;
    app.break_table("push_subscriptions").await;

    app.upload_ok(CallUpload::new()).await;

    let line = capture.wait_for("web push skipped").await;
    assert!(line.contains(" WARN "), "{line}");
    assert!(
        line.contains(&app.missing_table_cause("push_subscriptions")),
        "{line}"
    );
    service.expect_nothing().await;
}

/// The handle a socket presents is unguessable on purpose. With a sequential
/// Id, anyone could count from 1 and silence every listener's notifications by
/// claiming to be them — so a token nobody issued attaches nothing.
#[tokio::test]
async fn a_socket_cannot_silence_a_subscription_it_does_not_hold() {
    let service = PushService::start().await;
    let app = TestApp::with_key("k").await;
    subscribe_to(&app, &service.endpoint()).await;

    let mut ws = app.connect_ws().await;
    // The Id the row certainly has, and a token nobody was ever given.
    common::subscribe(&mut ws, r#"{"t":"sub","all":true,"push":"1"}"#).await;
    app.upload_ok(CallUpload::new()).await;

    let pushed = service.wait_for(1).await;
    assert_eq!(
        pushed.len(),
        1,
        "the real listener was notified despite the impostor"
    );
}

/// A reload re-subscribes to learn its token back, and must not take the
/// listener's Selection with it: a page that sent a default would overwrite
/// what they chose on their last visit, and the default any page can invent is
/// "everything" — a phone woken by Talkgroups nobody picked.
#[tokio::test]
async fn re_subscribing_without_a_selection_keeps_the_stored_one() {
    let service = PushService::start().await;
    let app = TestApp::with_key("k").await;
    app.post_json(
        "/api/push/subscribe",
        subscription(
            &service.endpoint(),
            json!({ "all": false, "sel": { "11": { "99999": true } } }),
        ),
    )
    .await;

    // What a reload sends: the endpoint and its keys, and nothing else.
    let response = app
        .post_json(
            "/api/push/subscribe",
            json!({
                "endpoint": service.endpoint(),
                "keys": { "p256dh": SUBSCRIBER_PUBLIC, "auth": SUBSCRIBER_AUTH },
            }),
        )
        .await;
    assert_eq!(response.status(), 200);

    app.upload_ok(CallUpload::new()).await; // Talkgroup 54241, not watched
    service.expect_nothing().await;
}
