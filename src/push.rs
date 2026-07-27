//! Web Push (#16, spec US 32, ADR-0005): the domain half — who is subscribed,
//! which Calls are worth waking a phone for, and how often.
//!
//! The protocol half (encryption, VAPID) is [`crate::webpush`], kept pure and
//! pinned to the RFCs' worked examples. What lives here is everything with an
//! opinion:
//!
//! - **A subscription is a row** ([`crate::db::entities::push_subscription`]),
//!   because the Call that matters arrives at 3am, long after the process that
//!   took the subscription restarted.
//! - **A Call reaches a subscription by exactly the rule the live feed uses**
//!   ([`crate::selection`]) — a listener notified about traffic the feed would
//!   not have played them is a bug that only ever shows up on someone's phone.
//! - **Coalescing** ([`Coalescer`]): at most one notification per Talkgroup per
//!   interval, per device, carrying a count of what it stands for. A busy
//!   System must never storm a phone, and a lull that ends with twelve Calls
//!   should say twelve.
//! - **Nothing is sent to a listener who is demonstrably listening.** A live
//!   socket registers its subscription as *attached*; the heartbeat (#9) reaps
//!   it within a period of an iOS suspension, and notifications take over
//!   exactly then. rdio-scanner has no equivalent — it has no push at all, and
//!   ships separate native apps instead.
//!
//! Sending happens on the fanout ([`spawn`]), not on the ingest path: a
//! recorder's upload must never wait on a push service on the other side of the
//! internet.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use tokio::sync::broadcast;
use tracing::{debug, info, warn};

use crate::AppState;
use crate::call::StoredCall;
use crate::db::entities::push_subscription;
use crate::db::repo;
use crate::failure::ServerError;
use crate::selection::Selection;
use crate::webpush::VapidKey;

/// How Web Push behaves, from `[push]` (#17, ADR-0012).
#[derive(Debug, Clone)]
pub struct PushConfig {
    /// At most one notification per Talkgroup per device in this window.
    pub coalesce: Duration,
    /// How long a push service should hold a notification for a device that is
    /// offline before giving up.
    pub ttl: Duration,
    /// The VAPID `sub` claim: how a push service's operator reaches ours.
    pub subject: String,
}

impl Default for PushConfig {
    fn default() -> Self {
        PushConfig {
            // Long enough that a working System cannot buzz a pocket
            // continuously; short enough that "something is happening" is still
            // news. The count carried in each notification is what keeps the
            // window from hiding activity.
            coalesce: Duration::from_secs(300),
            // A notification about radio traffic is stale within the hour: past
            // that, the archive is the better answer.
            ttl: Duration::from_secs(3600),
            subject: DEFAULT_SUBJECT.to_string(),
        }
    }
}

/// The contact a push service is given when the operator has named none. RFC
/// 8292 requires `sub` to be a `mailto:` or `https:` URI; some services reject
/// a token without one, so there is a default rather than an omission.
pub const DEFAULT_SUBJECT: &str = "mailto:admin@localhost";

/// The Web Push surface, cloned into every handler.
///
/// `None` is a server with no VAPID identity — push is off, the routes say so,
/// and nothing else in the app behaves differently.
#[derive(Clone, Default)]
pub struct Push(Option<Arc<Inner>>);

struct Inner {
    key: VapidKey,
    config: PushConfig,
    /// Subscriptions whose listener has a live-feed socket open right now.
    attached: Mutex<HashSet<i64>>,
}

impl Inner {
    fn is_attached(&self, subscription: i64) -> bool {
        self.attached
            .lock()
            .expect("attached")
            .contains(&subscription)
    }
}

/// A live-feed connection's claim on a push subscription. Dropping it — however
/// the connection ended — hands the listener back to the push sender.
pub struct Attached {
    push: Push,
    subscription: i64,
}

impl Drop for Attached {
    fn drop(&mut self) {
        self.push.detach(self.subscription);
    }
}

impl Push {
    /// A server that can send notifications.
    pub fn new(key: VapidKey, config: PushConfig) -> Self {
        Push(Some(Arc::new(Inner {
            key,
            config,
            attached: Mutex::new(HashSet::new()),
        })))
    }

    /// Note this listener as connected: while their live-feed socket is open
    /// they are demonstrably listening, so nothing is pushed to them. Returns a
    /// guard that detaches on drop, so a connection that ends any way at all —
    /// closed, reaped, errored — cannot leave a listener silently un-notified.
    pub fn attach(&self, subscription: i64) -> Attached {
        if let Some(inner) = &self.0 {
            inner
                .attached
                .lock()
                .expect("attached")
                .insert(subscription);
        }
        Attached {
            push: self.clone(),
            subscription,
        }
    }

    fn detach(&self, subscription: i64) {
        if let Some(inner) = &self.0 {
            inner
                .attached
                .lock()
                .expect("attached")
                .remove(&subscription);
        }
    }

    /// A server with no VAPID identity: subscribing is refused and nothing is
    /// ever sent.
    pub fn disabled() -> Self {
        Push(None)
    }

    /// The public key a browser needs as `applicationServerKey`.
    pub fn public_key(&self) -> Option<String> {
        self.0.as_ref().map(|inner| inner.key.public_base64url())
    }
}

/// `GET /api/push/key` — the server's VAPID public key, or 404 when push is not
/// configured, which is how the client knows not to offer it.
pub async fn key(State(state): State<AppState>) -> Response {
    match state.push.public_key() {
        Some(key) => Json(serde_json::json!({ "key": key })).into_response(),
        None => (StatusCode::NOT_FOUND, "push is not configured\n").into_response(),
    }
}

/// A browser's `PushSubscription.toJSON()`, plus the Selection it wants to be
/// notified about.
#[derive(Debug, Deserialize)]
pub struct SubscribeBody {
    endpoint: String,
    keys: SubscriptionKeys,
    /// What this device wants to be woken for. **Absent means "leave what you
    /// have"** — a reload re-subscribes only to learn its token back, and
    /// overwriting the listener's Selection with a default on every page load
    /// is how a phone ends up notified about Talkgroups nobody chose.
    selection: Option<Selection>,
}

#[derive(Debug, Deserialize)]
struct SubscriptionKeys {
    p256dh: String,
    auth: String,
}

/// `POST /api/push/subscribe` — register (or re-register) this device.
///
/// The same route is the *sync* path: a listener who changes their Selection
/// posts the subscription again, and the endpoint's row is updated rather than
/// duplicated.
pub async fn subscribe(State(state): State<AppState>, Json(body): Json<SubscribeBody>) -> Response {
    if state.push.public_key().is_none() {
        return rejected("not-configured", StatusCode::NOT_FOUND);
    }
    // Validated before it is stored, so every stored subscription is one we can
    // actually deliver to.
    if let Err(invalid) =
        crate::webpush::Subscription::parse(&body.endpoint, &body.keys.p256dh, &body.keys.auth)
    {
        return rejected(invalid.reason(), StatusCode::BAD_REQUEST);
    }

    let selection = body
        .selection
        .as_ref()
        .map(|selection| serde_json::to_string(selection).unwrap_or_else(|_| "{}".to_string()));
    match repo::upsert_push_subscription(
        &state.db,
        &body.endpoint,
        &body.keys.p256dh,
        &body.keys.auth,
        selection.as_deref(),
        crate::now_ms(),
    )
    .await
    {
        // The token, never the Id: it is what the socket and the unsubscribe
        // prove themselves with, and a sequential Id would let anyone silence
        // anyone else's notifications by counting (CONTEXT.md — an Id is never
        // shown to a client).
        Ok(token) => Json(serde_json::json!({ "token": token })).into_response(),
        Err(err) => ServerError::new("store-push-subscription", err).into_response(),
    }
}

/// What a listener turning notifications off sends: the token subscribing gave
/// them, so one device cannot unsubscribe another.
#[derive(Debug, Deserialize)]
pub struct UnsubscribeBody {
    token: String,
}

/// `POST /api/push/unsubscribe` — forget this device.
///
/// Idempotent: a browser that unsubscribes twice, or whose subscription the
/// server already dropped after a `410 Gone`, gets the same answer as one that
/// was there.
pub async fn unsubscribe(
    State(state): State<AppState>,
    Json(body): Json<UnsubscribeBody>,
) -> Response {
    match repo::delete_push_subscription(&state.db, &body.token).await {
        Ok(_) => StatusCode::NO_CONTENT.into_response(),
        Err(err) => ServerError::new("delete-push-subscription", err).into_response(),
    }
}

/// One coalescing bucket: a device, and a Talkgroup within a System.
type Key = (i64, i64, i64);

/// **Coalescing** (CONTEXT.md): at most one notification per Talkgroup per
/// device per window — and never a Call silently dropped for it.
///
/// Leading-edge: the first Call goes out immediately (a scanner is only useful
/// if it is prompt), and everything inside the window is *counted* rather than
/// discarded, so the notification that ends a lull says how much was missed
/// instead of pretending it was one Call. No timers: a suppressed Call needs no
/// wake-up of its own, which matters on a Pi.
#[derive(Debug)]
struct Coalescer {
    window: Duration,
    seen: std::collections::HashMap<Key, Bucket>,
}

#[derive(Debug)]
struct Bucket {
    last_sent_ms: i64,
    /// Calls folded into the next notification.
    pending: u32,
}

/// How many buckets may accumulate before stale ones are swept. A bucket is
/// tiny, and the sweep is O(n) — this is well above any real System's
/// Talkgroups × devices, and exists so a long-lived process cannot grow without
/// bound.
const PRUNE_AT: usize = 4096;

impl Coalescer {
    fn new(window: Duration) -> Self {
        Coalescer {
            window,
            seen: std::collections::HashMap::new(),
        }
    }

    /// Does this Call notify now? `Some(count)` says send, standing for that
    /// many Calls; `None` folds it into the next one.
    fn admit(&mut self, key: Key, now_ms: i64) -> Option<u32> {
        if self.seen.len() >= PRUNE_AT {
            self.prune(now_ms);
        }
        let window_ms = self.window.as_millis() as i64;
        match self.seen.get_mut(&key) {
            None => {
                self.seen.insert(
                    key,
                    Bucket {
                        last_sent_ms: now_ms,
                        pending: 0,
                    },
                );
                Some(1)
            }
            Some(bucket) if now_ms.saturating_sub(bucket.last_sent_ms) >= window_ms => {
                let count = bucket.pending + 1;
                bucket.last_sent_ms = now_ms;
                bucket.pending = 0;
                Some(count)
            }
            Some(bucket) => {
                bucket.pending = bucket.pending.saturating_add(1);
                None
            }
        }
    }

    /// Forget buckets that have gone quiet for long enough that their next Call
    /// would notify anyway — dropping one changes no decision.
    fn prune(&mut self, now_ms: i64) {
        let window_ms = self.window.as_millis() as i64;
        self.seen
            .retain(|_, bucket| now_ms.saturating_sub(bucket.last_sent_ms) < window_ms);
    }
}

// -- Sending ----------------------------------------------------------------

/// How long a push service has to answer before we give up on it. A device
/// nobody can reach must not hold up the notification of the device next to it.
const SEND_TIMEOUT: Duration = Duration::from_secs(10);

/// Start delivering notifications for Calls as they are published.
///
/// This rides the **live-feed fanout** rather than the ingest path: a
/// recorder's upload gets its `200` back without waiting on a push service on
/// the other side of the internet, and the Call reaches a notification by
/// exactly the same broadcast that reaches a socket. A server with no VAPID
/// identity spawns nothing at all.
pub fn spawn(state: AppState) {
    let Some(inner) = state.push.0.clone() else {
        return;
    };
    let client = reqwest::Client::builder()
        .timeout(SEND_TIMEOUT)
        .build()
        // A builder carrying nothing but a timeout has no failure mode of its
        // own — `build` reports a TLS backend that will not initialise or a
        // malformed proxy setting, and this configures neither.
        .expect("an HTTP client with only a timeout configured");
    let mut calls = state.live.subscribe();
    let mut coalescer = Coalescer::new(inner.config.coalesce);

    tokio::spawn(async move {
        loop {
            match on_broadcast(calls.recv().await) {
                Fanout::Notify(call) => {
                    notify(&state, &inner, &client, &mut coalescer, &call).await
                }
                Fanout::Skip => {}
                Fanout::Stop => break,
            }
        }
    });
}

/// What the sender does with one fanout result — a pure decision, kept out of
/// the loop so the two paths that are hard to provoke in an integration test
/// (a lagging sender, a closed channel) are still reachable by a unit test.
/// The same shape `live::on_broadcast` uses, for the same reason.
#[derive(Debug)]
enum Fanout {
    /// Consider this Call for every subscription.
    Notify(Arc<StoredCall>),
    /// Nothing to do, but keep listening.
    Skip,
    /// The fanout is gone — the process is shutting down.
    Stop,
}

fn on_broadcast(result: Result<Arc<StoredCall>, broadcast::error::RecvError>) -> Fanout {
    match result {
        Ok(call) => Fanout::Notify(call),
        // Falling behind the fanout costs notifications, not correctness — the
        // next Call notifies. Said out loud because a sender that cannot keep
        // up is the operator's to know about.
        Err(broadcast::error::RecvError::Lagged(skipped)) => {
            warn!(skipped, "web push sender lagged behind the fanout");
            Fanout::Skip
        }
        Err(broadcast::error::RecvError::Closed) => Fanout::Stop,
    }
}

/// Notify every device a Call reaches, once the coalescer allows it.
async fn notify(
    state: &AppState,
    inner: &Inner,
    client: &reqwest::Client,
    coalescer: &mut Coalescer,
    call: &StoredCall,
) {
    // Read per Call rather than cached: a listener who subscribed a second ago
    // must be notified, and this is a handful of rows on a table nothing else
    // contends for.
    let subscriptions = match repo::push_subscriptions(&state.db).await {
        Ok(subscriptions) => subscriptions,
        Err(error) => {
            warn!(%error, "web push skipped: could not read subscriptions");
            return;
        }
    };

    let now_ms = crate::now_ms();
    for row in subscriptions {
        let Some(delivery) = plan(inner, coalescer, &row, call, now_ms) else {
            continue;
        };
        // Each delivery runs on its own, so an unreachable push service holds
        // up neither the device beside it nor the *next Call*: awaiting them
        // here would put `SEND_TIMEOUT` between a hung service and every
        // notification behind it. Bounded by construction — the coalescer
        // admits at most one per Talkgroup per device per window.
        tokio::spawn(send(state.db.clone(), client.clone(), delivery));
    }
}

/// One notification, decided but not yet sent.
struct Delivery {
    subscription_id: i64,
    request: crate::webpush::PushRequest,
}

/// Decide whether `row` hears about `call` right now, and build the request if
/// so. Pure but for the coalescer it advances — every reason *not* to send is
/// here, in one place, rather than scattered through the sending code.
fn plan(
    inner: &Inner,
    coalescer: &mut Coalescer,
    row: &push_subscription::Model,
    call: &StoredCall,
    now_ms: i64,
) -> Option<Delivery> {
    // A listener with a live socket is listening; the feed already has this
    // Call. The heartbeat (#9) reaps a suspended phone within a period, and
    // notifications resume exactly then.
    if inner.is_attached(row.id) {
        return None;
    }
    let selection: Selection = serde_json::from_str(&row.selection).unwrap_or_default();
    if !selection.admits(call) {
        return None;
    }
    // Notify about the Talkgroup the listener actually selected — for a patched
    // Call that is not necessarily the one it was transmitted on.
    let talkgroup_ref = call
        .talkgroups()
        .find(|&talkgroup_ref| selection.selects(call.system_ref, talkgroup_ref))?;
    let count = coalescer.admit((row.id, call.system_ref, talkgroup_ref), now_ms)?;

    let subscription =
        crate::webpush::Subscription::parse(&row.endpoint, &row.p256dh, &row.auth).ok()?;
    let payload = payload(call, talkgroup_ref, count).to_string();
    let topic = topic_for(call.system_ref, talkgroup_ref);
    Some(Delivery {
        subscription_id: row.id,
        request: inner.key.request(
            &inner.config.subject,
            &subscription,
            crate::webpush::Message {
                payload: payload.as_bytes(),
                ttl: inner.config.ttl,
                topic: topic.as_deref(),
            },
            now_ms,
        ),
    })
}

/// What the service worker is handed. Deliberately small — a push service will
/// refuse more than about 4 KB, and everything else about the Call is one
/// `GET /api/calls` away once the app is open.
fn payload(call: &StoredCall, talkgroup_ref: i64, count: u32) -> serde_json::Value {
    serde_json::json!({
        "id": call.id,
        "systemRef": call.system_ref,
        "talkgroupRef": talkgroup_ref,
        "system": call.system_label,
        "talkgroup": call.talkgroup_label,
        "count": count,
    })
}

/// The RFC 8030 `Topic` for a Talkgroup, or `None` when the Refs are too long
/// to fit the 32-character limit — in which case the message is simply not
/// collapsible, rather than rejected by the push service for a bad header.
fn topic_for(system_ref: i64, talkgroup_ref: i64) -> Option<String> {
    let topic = format!("t{system_ref}-{talkgroup_ref}");
    (topic.len() <= 32).then_some(topic)
}

/// Deliver one notification and act on what the push service says.
async fn send(db: sea_orm::DatabaseConnection, client: reqwest::Client, delivery: Delivery) {
    let subscription = delivery.subscription_id;
    let mut request = client.post(&delivery.request.url);
    for (name, value) in &delivery.request.headers {
        request = request.header(*name, value);
    }

    // The service, never the endpoint: which push service is failing is an
    // operator's business, but the endpoint is a stable per-device identifier
    // and ADR-0011 rule 5 keeps those out of the log.
    let service = crate::webpush::audience_of(&delivery.request.url).unwrap_or_default();
    match request.body(delivery.request.body).send().await {
        Ok(response) if response.status().is_success() => {
            debug!(subscription, "web push sent");
        }
        // The device unsubscribed, or the endpoint expired. The row is now
        // rubbish that would be retried for every Call forever.
        Ok(response) if matches!(response.status().as_u16(), 404 | 410) => {
            let status = response.status().as_u16();
            match repo::delete_push_subscription_by_id(&db, subscription).await {
                Ok(()) => info!(subscription, status, "push subscription gone; forgotten"),
                Err(error) => {
                    warn!(subscription, status, %error, "could not forget a gone push subscription")
                }
            }
        }
        Ok(response) => {
            warn!(
                subscription,
                status = response.status().as_u16(),
                %service,
                "web push refused"
            );
        }
        // `without_url`, because reqwest's own `Display` renders as "error
        // sending request for url (https://fcm.googleapis.com/…/<device>)" —
        // which would put the endpoint in the log through the back door, and
        // rule 5 does not care which door it came through.
        Err(error) => warn!(
            subscription,
            error = %error.without_url(),
            %service,
            "web push failed"
        ),
    }
}

/// Refuse a subscription, saying why in one place (ADR-0011 rule 3's shape: a
/// machine-readable `reason`, and the same slug in the body).
///
/// The endpoint is deliberately not logged: it is a stable per-device
/// identifier, which is exactly what rule 5 keeps out of an operator's log.
fn rejected(reason: &'static str, status: StatusCode) -> Response {
    warn!(reason, "push subscription rejected");
    (status, format!("{reason}\n")).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::LogCapture;
    use rstest::rstest;

    /// One device, one Talkgroup.
    const KEY: Key = (1, 11, 54241);
    const WINDOW: Duration = Duration::from_secs(300);

    fn coalescer() -> Coalescer {
        Coalescer::new(WINDOW)
    }

    /// A scanner is only useful if it is prompt: the first Call of a quiet
    /// Talkgroup goes out at once, standing for itself.
    #[test]
    fn the_first_call_notifies_immediately() {
        assert_eq!(coalescer().admit(KEY, 1_000), Some(1));
    }

    /// The anti-storm rule. A working System can carry a Call every few
    /// seconds; a phone must not buzz for each.
    #[test]
    fn a_busy_talkgroup_notifies_once_per_window() {
        let mut coalescer = coalescer();
        assert_eq!(coalescer.admit(KEY, 0), Some(1));

        for at in [1, 1_000, 299_999] {
            assert_eq!(coalescer.admit(KEY, at), None, "at {at}ms");
        }
    }

    /// ...and nothing is silently dropped for it: the notification that ends
    /// the window says how many Calls it stands for, which is what makes a lull
    /// ending with twelve Calls readable at a glance.
    #[test]
    fn the_next_notification_counts_what_the_window_swallowed() {
        let mut coalescer = coalescer();
        coalescer.admit(KEY, 0);
        for at in [1, 2, 3] {
            coalescer.admit(KEY, at);
        }

        assert_eq!(coalescer.admit(KEY, 300_000), Some(4), "3 held + this one");
        // ...and the count resets, rather than accumulating forever.
        assert_eq!(coalescer.admit(KEY, 600_000), Some(1));
    }

    /// Every bucket is its own: a chatty Talkgroup must not silence a quiet one,
    /// and one device's window says nothing about another's.
    #[rstest]
    #[case((1, 11, 99999), "another talkgroup")]
    #[case((1, 22, 54241), "another system")]
    #[case((2, 11, 54241), "another device")]
    fn buckets_do_not_interfere(#[case] other: Key, #[case] what: &str) {
        let mut coalescer = coalescer();
        coalescer.admit(KEY, 0);

        assert_eq!(coalescer.admit(other, 1), Some(1), "{what}");
        assert_eq!(
            coalescer.admit(KEY, 2),
            None,
            "{what} spent the first's window"
        );
    }

    /// A process that runs for months must not grow a bucket per Talkgroup it
    /// ever saw. Sweeping only ever drops buckets whose next Call would notify
    /// anyway, so it cannot change a decision.
    #[test]
    fn stale_buckets_are_swept_without_changing_a_decision() {
        let mut coalescer = coalescer();
        for device in 0..PRUNE_AT as i64 {
            coalescer.admit((device, 11, 54241), 0);
        }
        assert_eq!(coalescer.seen.len(), PRUNE_AT);

        // Long after every one of them fell quiet.
        let later = 300_000 * 10;
        assert_eq!(coalescer.admit(KEY, later), Some(1));

        assert_eq!(
            coalescer.seen.len(),
            1,
            "the sweep kept only the live bucket"
        );
    }

    /// A window of zero is "notify me about everything", which is a legitimate
    /// thing to configure and must not divide by anything.
    #[test]
    fn a_zero_window_notifies_every_call() {
        let mut coalescer = Coalescer::new(Duration::ZERO);

        assert_eq!(coalescer.admit(KEY, 0), Some(1));
        assert_eq!(coalescer.admit(KEY, 0), Some(1));
    }

    // -- The fanout decision -------------------------------------------------

    fn a_call() -> Arc<StoredCall> {
        Arc::new(StoredCall {
            id: 1,
            system_ref: 11,
            system_label: None,
            talkgroup_ref: 54241,
            talkgroup_label: None,
            talkgroup_group: None,
            talkgroup_tag: None,
            led: None,
            patches: vec![],
            frequency: None,
            source: None,
            date_time: None,
            timestamp: None,
            audio_mime: None,
            object_key: String::new(),
            audio_url: String::new(),
        })
    }

    #[test]
    fn a_broadcast_call_is_considered_for_notification() {
        assert!(matches!(on_broadcast(Ok(a_call())), Fanout::Notify(_)));
    }

    /// A sender that cannot keep up loses notifications, not Calls — but the
    /// operator is told, because it is a symptom they own.
    #[test]
    fn a_lagging_sender_says_so_and_keeps_going() {
        let capture = LogCapture::start();

        let action = on_broadcast(Err(broadcast::error::RecvError::Lagged(7)));

        assert!(matches!(action, Fanout::Skip));
        let logged = capture.text();
        assert!(logged.contains(" WARN "), "{logged}");
        assert!(logged.contains("skipped=7"), "{logged}");
    }

    #[test]
    fn a_closed_fanout_stops_the_sender() {
        assert!(matches!(
            on_broadcast(Err(broadcast::error::RecvError::Closed)),
            Fanout::Stop
        ));
    }

    /// The ordinary case runs per Call and must stay silent (ADR-0011 rule 8).
    #[test]
    fn an_ordinary_broadcast_logs_nothing() {
        let capture = LogCapture::start();

        on_broadcast(Ok(a_call()));

        assert_eq!(capture.text(), "");
    }

    #[test]
    fn a_server_without_a_vapid_identity_has_no_public_key() {
        assert_eq!(Push::disabled().public_key(), None);
        assert_eq!(Push::default().public_key(), None);
    }

    #[test]
    fn a_configured_server_advertises_its_public_key() {
        let key = VapidKey::generate();
        let expected = key.public_base64url();

        let push = Push::new(key, PushConfig::default());

        assert_eq!(push.public_key(), Some(expected));
    }
}
