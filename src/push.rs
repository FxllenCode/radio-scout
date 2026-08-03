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
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use tracing::{debug, info, warn};

use crate::AppState;
use crate::call::StoredCall;
use crate::db::entities::push_subscription;
use crate::db::repo;
use crate::failure::{Failure, Reason, Stage};
use crate::selection::Selection;
use crate::webpush::VapidKey;
use crate::worker::{Handoff, Meter, Worker};

/// What this Worker is called on a status surface (#93).
pub const WORKER: &str = "push";

/// How Web Push behaves — and the `[push]` section itself (#17, ADR-0012, #87).
///
/// The VAPID identity is deliberately not here, for the same reason the admin
/// password isn't: first run **writes** it, so it lives in the environment and
/// `.env` as `RADIO_SCOUT_VAPID_PRIVATE_KEY`. Everything here is a knob rather
/// than a secret.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PushConfig {
    /// At most one notification per Talkgroup per device in this window. `0`
    /// notifies about every Call.
    #[serde(rename = "coalesce_secs", with = "crate::config::secs")]
    pub coalesce: Duration,
    /// How long a push service should hold a notification for a device that is
    /// offline before giving up.
    #[serde(rename = "ttl_secs", with = "crate::config::secs")]
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
    /// What the sender owes: Calls published to the fanout it has not yet
    /// considered, plus notifications still in flight (#93).
    meter: Arc<Meter>,
    /// The coalescer, until [`spawn`] takes it — and with it the right to be
    /// the sender. Two senders each with a coalescer of their own would each
    /// admit the first Call of a window, so a phone would be notified twice
    /// about the same Talkgroup; taking it here is what makes that impossible
    /// rather than merely unlikely.
    start: Handoff<Coalescer>,
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
        let coalescer = Coalescer::new(config.coalesce);
        Push(Some(Arc::new(Inner {
            key,
            config,
            attached: Mutex::new(HashSet::new()),
            meter: Meter::new(),
            start: Handoff::new(coalescer),
        })))
    }

    /// Note that a Call has been published to the fanout, so the sender owes a
    /// decision about it.
    ///
    /// Called from [`crate::AppState::publish`], which is the last place a Call
    /// has one owner: past it the fanout has handed every follower a clone, and
    /// a debt taken on there could never be settled exactly once. A server with
    /// no VAPID identity owes nothing, so this is where that ends.
    pub fn owes_a_call(&self) {
        if let Some(inner) = &self.0 {
            inner.meter.admit_untracked();
        }
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

/// The server's VAPID public key, as a browser's `applicationServerKey`.
#[derive(Debug, Serialize)]
pub struct PublicKey {
    key: String,
}

/// `GET /api/push/key` — the server's VAPID public key, or 404 when push is not
/// configured, which is how the client knows not to offer it.
pub async fn key(State(state): State<AppState>) -> Result<PublicKey, Failure> {
    state
        .push
        .public_key()
        .map(|key| PublicKey { key })
        .ok_or(Reason::PushNotConfigured.into())
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
pub async fn subscribe(
    State(state): State<AppState>,
    Json(body): Json<SubscribeBody>,
) -> Result<SubscriptionToken, Failure> {
    if state.push.public_key().is_none() {
        return Err(Reason::PushNotConfigured.into());
    }
    // Validated before it is stored, so every stored subscription is one we can
    // actually deliver to. The endpoint is deliberately not named anywhere in
    // the refusal: it is a stable per-device identifier, which is exactly what
    // ADR-0011 rule 5 keeps out of an operator's log.
    crate::webpush::Subscription::parse(&body.endpoint, &body.keys.p256dh, &body.keys.auth)
        .map_err(Reason::BadSubscription)?;

    let selection = body
        .selection
        .as_ref()
        .map(|selection| serde_json::to_string(selection).unwrap_or_else(|_| "{}".to_string()));
    // The token, never the Id: it is what the socket and the unsubscribe prove
    // themselves with, and a sequential Id would let anyone silence anyone
    // else's notifications by counting (CONTEXT.md — an Id is never shown to a
    // client).
    let token = repo::upsert_push_subscription(
        &state.db,
        &body.endpoint,
        &body.keys.p256dh,
        &body.keys.auth,
        selection.as_deref(),
        state.clock.now_ms(),
    )
    .await
    .map_err(Stage::StorePushSubscription.failed())?;

    Ok(SubscriptionToken { token })
}

/// What a device proves itself with when it re-subscribes or unsubscribes.
#[derive(Debug, Serialize)]
pub struct SubscriptionToken {
    token: String,
}

// Both of the listener-facing push answers are JSON objects of one field.
crate::answers_json!(PublicKey, SubscriptionToken);

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
) -> Result<Unsubscribed, Failure> {
    repo::delete_push_subscription(&state.db, &body.token)
        .await
        .map(|()| Unsubscribed)
        .map_err(Stage::DeletePushSubscription.failed())
}

/// This device is forgotten — nothing to say, and no body to say it in.
pub struct Unsubscribed;

impl IntoResponse for Unsubscribed {
    fn into_response(self) -> Response {
        StatusCode::NO_CONTENT.into_response()
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
pub fn spawn(state: AppState) -> Option<Worker> {
    let inner = state.push.0.clone()?;
    // The coalescer *is* the right to be the sender (#93): a second call finds
    // it gone and starts nothing.
    let mut coalescer = inner.start.take()?;
    let client = reqwest::Client::builder()
        .timeout(SEND_TIMEOUT)
        .build()
        // A builder carrying nothing but a timeout has no failure mode of its
        // own — `build` reports a TLS backend that will not initialise or a
        // malformed proxy setting, and this configures neither.
        .expect("an HTTP client with only a timeout configured");
    let mut calls = state.live.subscribe();
    let meter = inner.meter.clone();

    Some(Worker::start(
        WORKER,
        meter.clone(),
        move |mut stop| async move {
            // Deliveries still in flight. A set rather than detached spawns so they
            // are this Worker's to reap and, when it stops, to abort — dropping the
            // set aborts them, and each one's Ticket settles with it. Nothing is
            // ever *awaited* here, which is the point: an unreachable push service
            // must hold up neither the device beside it nor the next Call.
            let mut deliveries = tokio::task::JoinSet::new();
            loop {
                tokio::select! {
                    biased;
                    _ = stop.cancelled() => break,
                    // Reap what has finished, so the set does not grow for the life
                    // of the process.
                    Some(_) = deliveries.join_next(), if !deliveries.is_empty() => {}
                    received = calls.recv() => {
                        let fanout = on_broadcast(received);
                        if let Fanout::Notify(call) = &fanout {
                            // Before the Call is discharged, never after: the
                            // deliveries are admitted inside `notify`, so the
                            // depth never dips to zero in the gap between owing
                            // a decision and owing its consequences.
                            notify(&state, &inner, &client, &mut coalescer, call, &mut deliveries)
                                .await;
                        }
                        meter.settle_n(fanout.settles());
                        // **Unreachable, and left in deliberately.** The fanout
                        // closes when its last sender drops, and the sender
                        // lives in the `AppState` this very task owns — so the
                        // channel cannot close while the loop that would notice
                        // is running. It stays because the alternative to
                        // breaking on a closed channel is spinning on it
                        // forever, and because the day something else holds the
                        // fanout this is what stops a busy loop from shipping.
                        // The decision itself is covered:
                        // `every_published_call_is_discharged_exactly_once`.
                        if matches!(fanout, Fanout::Stop) {
                            break;
                        }
                    }
                }
            }
        },
    ))
}

/// What the sender does with one fanout result — a pure decision, kept out of
/// the loop so the two paths that are hard to provoke in an integration test
/// (a lagging sender, a closed channel) are still reachable by a unit test.
/// The same shape `live::on_broadcast` uses, for the same reason.
#[derive(Debug)]
enum Fanout {
    /// Consider this Call for every subscription.
    Notify(Arc<StoredCall>),
    /// This many Calls went past unseen: nothing to do about them, but they are
    /// no longer owed either.
    Skipped(u64),
    /// The fanout is gone — the process is shutting down.
    Stop,
}

impl Fanout {
    /// How many published Calls this result discharges (#93).
    ///
    /// A function rather than a line inside the loop because the arm that
    /// matters is the one that cannot be provoked from a test: a lag needs the
    /// sender to fall a whole broadcast buffer behind. Get it wrong and the
    /// count never catches up, so **every** later `settle()` waits forever —
    /// a failure that would present as the entire suite hanging, from a line
    /// nothing covered.
    fn settles(&self) -> u64 {
        match self {
            // Considered, whatever was decided about it.
            Fanout::Notify(_) => 1,
            // Never seen and never will be. Each was admitted where it was
            // published, and a Call the sender will not see is not one it owes.
            Fanout::Skipped(missed) => *missed,
            // Nothing was taken off the fanout, so nothing is discharged.
            Fanout::Stop => 0,
        }
    }
}

fn on_broadcast(result: Result<crate::live::Emitted, broadcast::error::RecvError>) -> Fanout {
    match result {
        // The emission a Call went out as is the live feed's cursor (#94), and
        // means nothing to a notification: a phone that was asleep is told what
        // is happening now, not where it is in a sequence.
        Ok(emitted) => Fanout::Notify(emitted.call),
        // Falling behind the fanout costs notifications, not correctness — the
        // next Call notifies. Said out loud because a sender that cannot keep
        // up is the operator's to know about.
        Err(broadcast::error::RecvError::Lagged(skipped)) => {
            warn!(skipped, "web push sender lagged behind the fanout");
            Fanout::Skipped(skipped)
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
    deliveries: &mut tokio::task::JoinSet<()>,
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

    let now_ms = state.clock.now_ms();
    for row in subscriptions {
        let Some(delivery) = plan(inner, coalescer, &row, call, now_ms) else {
            continue;
        };
        // Each delivery runs on its own, so an unreachable push service holds
        // up neither the device beside it nor the *next Call*: awaiting them
        // here would put `SEND_TIMEOUT` between a hung service and every
        // notification behind it. Bounded by construction — the coalescer
        // admits at most one per Talkgroup per device per window.
        //
        // Its Ticket rides with it, so the sender is not idle until the last
        // notification has actually left the process — which is what lets a
        // test assert "and nothing was sent" without a sleep.
        let ticket = inner.meter.admit();
        let db = state.db.clone();
        let client = client.clone();
        deliveries.spawn(async move {
            send(db, client, delivery).await;
            drop(ticket);
        });
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
async fn send(db: crate::db::Db, client: reqwest::Client, delivery: Delivery) {
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

    /// **One sender, and only ever one** (#93). Two would each hold a coalescer
    /// of their own, so each would admit the first Call of every window and a
    /// phone would buzz twice for the same Talkgroup — the exact failure
    /// [`Coalescer`] exists to prevent, reintroduced by the wiring rather than
    /// by the logic.
    ///
    /// The coalescer *is* the right to be the sender, so a second start has
    /// nothing to send with.
    #[tokio::test]
    async fn a_second_push_sender_cannot_be_started() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let db = crate::db::connect(&crate::testing::sqlite_url(&tmp))
            .await
            .expect("db");
        let store = Arc::new(crate::BlobStore::filesystem(tmp.path()).expect("store"));
        let mut state = AppState::new(store, db, crate::IngestConfig::default());
        state.push = Push::new(VapidKey::generate(), PushConfig::default());

        let first = spawn(state.clone()).expect("the first sender starts");
        let second = spawn(state.clone());

        assert!(second.is_none(), "a second push sender was started");
        first.stop().await;
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

    /// The sweep's boundary has to be the *same* boundary [`Coalescer::admit`]
    /// uses, or the two disagree about what a spent window is. A bucket exactly
    /// one window old would notify on its next Call anyway, so dropping it
    /// changes nothing — while keeping one a millisecond younger is the
    /// difference between coalescing and a second notification.
    #[test]
    fn the_sweep_drops_exactly_the_buckets_whose_window_has_passed() {
        let mut coalescer = coalescer();
        let window = WINDOW.as_millis() as i64;
        coalescer.admit((1, 11, 1), 0); // a full window old when swept
        coalescer.admit((1, 11, 2), 1); // one millisecond short of it

        coalescer.prune(window);

        assert!(
            !coalescer.seen.contains_key(&(1, 11, 1)),
            "a bucket a full window old is spent"
        );
        assert!(
            coalescer.seen.contains_key(&(1, 11, 2)),
            "a bucket still inside its window would suppress its next Call"
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
            timestamp: None,
            audio_mime: None,
            duration_ms: None,
            emergency: false,
            encrypted: false,
            site_ref: None,
            object_key: String::new(),
            audio_url: Some(String::new()),
        })
    }

    /// The emission a Call went out as is the live feed's cursor and nothing to
    /// a notification (#94) — what matters here is the Call under it.
    #[test]
    fn a_broadcast_call_is_considered_for_notification() {
        let emitted = crate::live::Emitted {
            seq: 7,
            call: a_call(),
        };
        assert!(matches!(on_broadcast(Ok(emitted)), Fanout::Notify(_)));
    }

    /// A sender that cannot keep up loses notifications, not Calls — but the
    /// operator is told, because it is a symptom they own.
    #[test]
    fn a_lagging_sender_says_so_and_keeps_going() {
        let capture = LogCapture::start();

        let action = on_broadcast(Err(broadcast::error::RecvError::Lagged(7)));

        assert!(matches!(action, Fanout::Skipped(7)));
        let logged = capture.text();
        assert!(logged.contains(" WARN "), "{logged}");
        assert!(logged.contains("skipped=7"), "{logged}");
    }

    /// **Every published Call is discharged exactly once** (#93), including the
    /// ones the sender never saw.
    ///
    /// A Call is owed from where it is published, so a lag that swallows seven
    /// of them has to discharge seven — count them as one and the sender's
    /// depth never returns to zero, which means every later `settle()` in the
    /// suite waits forever. That is a whole-suite hang originating in an arm no
    /// integration test can provoke: a lag needs the sender to fall an entire
    /// broadcast buffer behind.
    #[test]
    fn every_published_call_is_discharged_exactly_once() {
        assert_eq!(Fanout::Notify(a_call()).settles(), 1);
        assert_eq!(Fanout::Skipped(7).settles(), 7);
        // Nothing was taken off the fanout, so nothing is discharged — the
        // process is ending and the Calls still owed go with it.
        assert_eq!(Fanout::Stop.settles(), 0);
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

        on_broadcast(Ok(crate::live::Emitted {
            seq: 1,
            call: a_call(),
        }));

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
