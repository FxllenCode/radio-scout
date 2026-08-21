//! The **Webhook** sender (#54): what one delivery to an Operator's URL actually
//! *is*.
//!
//! The queue around it — the ordering rule, the retry policy, the accounting and
//! the loop — is [`crate::delivery`], shared with [`crate::downstream::sender`].
//! What is here is the errand: read the Call as a Listener sees it, render the
//! body, POST it, and settle the row.
//!
//! # How this differs from forwarding, and why that is all
//!
//! - **No audio.** A webhook carries facts; the bytes stay in the Archive and
//!   the payload links to them. So there is no object read, which is also why an
//!   **Encrypted Call** is perfectly deliverable here where it is not
//!   forwardable — it has no audio object, and this body does not need one.
//! - **The marks ride with the delivery.** The queue row says which Call and
//!   which webhook; what the webhook *asked for* is on its own row, and the
//!   Call's own marks are read back with it — so a mark added to a Call after
//!   it was queued is carried, and one an Operator has since stopped watching
//!   for is not.
//! - **The URL is the credential**, so nothing here may render it: not in a log
//!   line, not in an error, not in a span. A webhook is named by its **Id**.

use std::sync::Arc;

use tracing::{Instrument, Level, span, warn};

use super::{Marks, Webhook, WebhookConfig, payload};
use crate::AppState;
use crate::call::{CallId, StoredCall};
use crate::db::repo;
use crate::delivery::{self, Failed, Failure, Queued, Settled, Verdict};
use crate::worker::Worker;

/// One Call as a Webhook is about to be told about it.
///
/// The Call a **Listener** would see, plus the marks it carries — which is the
/// deliberate difference from [`crate::downstream::dialect::Forwardable`]: a
/// peer stores the Call as if it were the recorder, so it needs what the
/// recorder said; a webhook is read by a person or a script, so it gets what the
/// app shows.
#[derive(Debug, Clone)]
pub struct Deliverable {
    pub call: StoredCall,
    pub marks: Marks,
}

/// The three questions **one delivery** asks of the world — the Call it is made
/// of, the webhook it goes to, and how it ends.
///
/// A port for the reason [`crate::enhance::Archive`] is one: the arms worth
/// testing here are the failures, and a database that answers cannot produce
/// them.
#[async_trait::async_trait]
pub trait Sinks: Send + Sync {
    /// The Call and its marks, or `None` if it is gone.
    async fn deliverable(&self, call_id: CallId) -> Result<Option<Deliverable>, Failure>;

    /// POST one delivery. `Ok(status)` is an answer from the webhook, whatever
    /// it says; `Err(())` is never having got one.
    ///
    /// The transport error is deliberately not carried out, and here it is not a
    /// matter of taste: reqwest's `Display` renders the URL, and a webhook's URL
    /// *is* its credential (ADR-0011 rule 2). What an Operator needs — that this
    /// webhook cannot be reached — is the `Err` itself.
    async fn post(&self, hook: &Webhook, body: payload::Body) -> Result<u16, ()>;

    /// Apply what was decided, to the queue row and to the webhook's health.
    async fn settle(
        &self,
        delivery_id: i64,
        webhook_id: i64,
        now_ms: i64,
        settled: &Settled,
    ) -> Result<(), Failure>;
}

/// Start the sender. `None` means one is already running.
///
/// Started whatever the roster says, for [`crate::downstream::sender::spawn`]'s
/// reason: a Webhook is a **row**, so an Instance with none has an empty roster
/// rather than a disabled feature, and one added from the browser five minutes
/// from now must be delivered to without a restart. With none configured it
/// sleeps on its wake-up and costs nothing.
pub fn spawn(state: AppState) -> Option<Worker> {
    let dispatch = state.webhooks.dispatch();
    // Taken, not checked: a second call finds it gone and starts nothing.
    dispatch.claim()?;
    dispatch.owes_a_first_pass();

    Some(delivery::run(
        super::WORKER,
        Arc::new(Posting(state)),
        dispatch,
    ))
}

/// The Instance, as the draining loop asks questions of it.
///
/// A newtype for the reason [`crate::downstream::sender::Forwarding`] is one:
/// the two queues need the same trait on the same state, and one type cannot
/// implement one trait twice.
pub struct Posting(pub AppState);

#[async_trait::async_trait]
impl delivery::Outbox for Posting {
    type Sink = Webhook;

    async fn roster(&self) -> Result<Vec<Webhook>, Failure> {
        Ok(repo::delivering_webhooks(&self.0.db).await?)
    }

    async fn head(&self, webhook_id: i64) -> Result<Option<Queued>, Failure> {
        Ok(repo::next_webhook_delivery(&self.0.db, webhook_id)
            .await?
            .map(|row| Queued {
                id: row.id,
                call_id: row.call_id,
                attempts: row.attempts,
                next_attempt_ms: row.next_attempt_ms,
            }))
    }

    async fn attempt(&self, hook: &Webhook, head: &Queued) {
        let now_ms = self.0.clock.now_ms();
        attempt(
            &self.0,
            self.0.webhooks.config(),
            self.0.webhooks.public_url(),
            hook,
            head,
            now_ms,
        )
        // The **Id**, never the URL — the span is on every line one attempt
        // writes, so a URL here would be the widest possible leak of the one
        // string this feature has to keep.
        .instrument(span!(
            Level::ERROR,
            "webhook",
            webhook = hook.id,
            call_id = head.call_id
        ))
        .await;
    }

    async fn depth(&self) -> Result<u64, Failure> {
        Ok(repo::webhook_depths(&self.0.db)
            .await?
            .values()
            .sum::<i64>()
            .max(0) as u64)
    }

    fn now_ms(&self) -> i64 {
        self.0.clock.now_ms()
    }
}

/// One delivery attempt, start to finish: read the Call, render the body, POST
/// it, and settle the row.
pub async fn attempt(
    sinks: &dyn Sinks,
    config: &WebhookConfig,
    public_url: Option<&str>,
    hook: &Webhook,
    head: &Queued,
    now_ms: i64,
) {
    let settled = delivery::decide(
        post(sinks, hook, head.call_id, public_url).await.err(),
        head,
        config.retry(),
        now_ms,
    );
    delivery::say(&settled, super::WORKER);
    if let Err(error) = sinks.settle(head.id, hook.id, now_ms, &settled).await {
        // The one failure this module cannot recover from by retrying: if the
        // row cannot be written, the same delivery is attempted again on the
        // next wake-up — which for a *successful* delivery means the webhook
        // receives it twice. Unlike a Downstream there is no dedup at the far
        // end to make that invisible, so saying it is the only way an Operator
        // can recognise a duplicate message rather than wonder about it.
        warn!(
            reason = %"queue-unwritable",
            %error,
            "a webhook delivery could not be settled; it may be sent again"
        );
    }
}

/// Read, render and POST. `Err` is the one word that explains what went wrong.
async fn post(
    sinks: &dyn Sinks,
    hook: &Webhook,
    call_id: CallId,
    public_url: Option<&str>,
) -> Result<(), Failed> {
    let deliverable = sinks
        .deliverable(call_id)
        .await
        .map_err(|error| {
            warn!(%error, "could not read a Call to post");
            Failed::Unreadable
        })?
        // Retention is entitled to prune a Call a webhook has been unreachable
        // longer than.
        .ok_or(Failed::Vanished)?;

    // **What is sent is the overlap, not the Call's whole mark set** — the same
    // predicate `Webhook::admits` fired on, so what a webhook is told can never
    // disagree with why it was woken.
    let marks = deliverable.marks.wanted_by(&hook.marks);
    let body = payload::body(hook.format, &deliverable.call, &marks, public_url);

    match sinks.post(hook, body).await {
        Ok(status) => match Verdict::of_status(status) {
            Verdict::Delivered => Ok(()),
            _ => Err(Failed::Status(status)),
        },
        Err(()) => Err(Failed::Unreachable),
    }
}

/// The world an `AppState` is: its database for the rows, its client for the
/// webhook.
///
/// The only implementation that ships, and deliberately thin — every decision is
/// above, and each of these is one call. This is the half a test cannot
/// substitute, so it is the half that must have nothing in it worth testing.
#[async_trait::async_trait]
impl Sinks for AppState {
    async fn deliverable(&self, call_id: CallId) -> Result<Option<Deliverable>, Failure> {
        Ok(crate::archive::deliverable(&self.db, call_id).await?)
    }

    async fn post(&self, hook: &Webhook, body: payload::Body) -> Result<u16, ()> {
        self.webhooks
            .dispatch()
            .client()
            .post(&hook.url)
            .header(http::header::CONTENT_TYPE, &body.content_type)
            .body(body.bytes)
            .send()
            .await
            .map(|response| response.status().as_u16())
            .map_err(|error| {
                // `without_url`, and here it is load-bearing rather than
                // careful: reqwest renders "error sending request for url (…)",
                // and for a Discord webhook that URL is the token.
                tracing::debug!(error = %error.without_url(), "a webhook delivery did not complete");
            })
    }

    async fn settle(
        &self,
        delivery_id: i64,
        webhook_id: i64,
        now_ms: i64,
        settled: &Settled,
    ) -> Result<(), Failure> {
        match settled {
            Settled::Taken => {
                repo::drop_webhook_delivery(&self.db, delivery_id).await?;
                repo::webhook_succeeded(&self.db, webhook_id, now_ms).await?;
                self.webhooks.dispatch().delivery_settled();
            }
            Settled::Abandoned { why } => {
                repo::webhook_failed(&self.db, webhook_id, now_ms, why).await?;
                repo::drop_webhook_delivery(&self.db, delivery_id).await?;
                self.webhooks.dispatch().delivery_settled();
            }
            Settled::Deferred {
                attempts,
                next_attempt_ms,
                why,
            } => {
                repo::webhook_failed(&self.db, webhook_id, now_ms, why).await?;
                repo::defer_webhook_delivery(
                    &self.db,
                    delivery_id,
                    *attempts,
                    *next_attempt_ms,
                    why,
                )
                .await?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::delivery::Outbox;
    use crate::testing::LogCapture;
    use crate::webhook::{Format, Mark};
    use rstest::rstest;
    use std::sync::Mutex;

    /// **One sender, and only ever one** (#93). Two would each take the head of
    /// the same webhook's queue, so an Operator's channel would receive one
    /// Emergency twice — and unlike a Downstream there is no dedup at the far
    /// end to hide it.
    #[tokio::test]
    async fn a_second_sender_cannot_be_started() {
        let (state, _tmp) = an_app().await;

        let first = spawn(state.clone()).expect("the first sender starts");
        let second = spawn(state.clone());

        assert!(second.is_none(), "a second webhook sender was started");
        first.stop().await;
    }

    /// An Instance's state over a temp database and store — the two senders'
    /// shared preamble. The `TempDir` is handed back because dropping it takes
    /// the database with it.
    async fn an_app() -> (AppState, tempfile::TempDir) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let db = crate::db::connect(&crate::testing::sqlite_url(&tmp))
            .await
            .expect("db");
        let store = std::sync::Arc::new(crate::BlobStore::filesystem(tmp.path()).expect("store"));
        (
            AppState::new(store, db, crate::IngestConfig::default()),
            tmp,
        )
    }

    /// **What a previous process left, counted.**
    ///
    /// The number the boot hands the sender's meter (#93), and the one thing on
    /// this port that is arithmetic rather than one call: a wrong total does not
    /// lose a delivery — every queued row is still sent — it makes `settle()`
    /// claim the Instance has caught up when it has not, which is a race every
    /// later test in the suite inherits and none of them can assert.
    ///
    /// So it is asserted here directly, over real rows, rather than through a
    /// restart whose only observable outcome is the delivery that happens
    /// either way.
    #[tokio::test]
    async fn the_resume_count_is_every_delivery_every_webhook_is_owed() {
        let (state, _tmp) = an_app().await;
        let outbox = Posting(state);

        assert_eq!(
            outbox.depth().await.expect("depth"),
            0,
            "an Instance whose queue is empty owes nothing"
        );

        // Two webhooks, three deliveries between them — so a count that answered
        // with one row, one webhook, or a constant is a different number.
        repo::queue_webhook_deliveries(&outbox.0.db, 1, &[1, 2], 0)
            .await
            .expect("queue");
        repo::queue_webhook_deliveries(&outbox.0.db, 2, &[1], 0)
            .await
            .expect("queue");

        assert_eq!(outbox.depth().await.expect("depth"), 3);
    }

    // -- The world, substituted ----------------------------------------------

    #[derive(Debug)]
    struct Refused(&'static str);

    impl std::fmt::Display for Refused {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "the fake world refused to {}", self.0)
        }
    }

    impl std::error::Error for Refused {}

    #[derive(Default)]
    struct FakeWorld {
        call: Option<Deliverable>,
        /// What the webhook answers, or `None` for one that cannot be reached.
        answer: Option<u16>,
        fails: Option<&'static str>,
        settled: Mutex<Vec<Settled>>,
        posted: Mutex<Vec<payload::Body>>,
    }

    impl FakeWorld {
        fn refuse(&self, question: &'static str) -> Result<(), Refused> {
            match self.fails == Some(question) {
                true => Err(Refused(question)),
                false => Ok(()),
            }
        }

        fn settled(&self) -> Vec<Settled> {
            self.settled.lock().expect("settled").clone()
        }

        fn sent(&self) -> serde_json::Value {
            let posted = self.posted.lock().expect("posted");
            serde_json::from_slice(&posted.first().expect("a body").bytes).expect("json")
        }
    }

    #[async_trait::async_trait]
    impl Sinks for FakeWorld {
        async fn deliverable(&self, _call_id: CallId) -> Result<Option<Deliverable>, Failure> {
            self.refuse("read the Call")?;
            Ok(self.call.clone())
        }

        async fn post(&self, _hook: &Webhook, body: payload::Body) -> Result<u16, ()> {
            self.posted.lock().expect("posted").push(body);
            self.answer.ok_or(())
        }

        async fn settle(
            &self,
            _delivery_id: i64,
            _webhook_id: i64,
            _now_ms: i64,
            settled: &Settled,
        ) -> Result<(), Failure> {
            self.refuse("settle the delivery")?;
            self.settled.lock().expect("settled").push(settled.clone());
            Ok(())
        }
    }

    fn hook() -> Webhook {
        Webhook {
            id: 1,
            url: String::from("https://discord.com/api/webhooks/1/t0ken"),
            format: Format::RadioScout,
            marks: [Mark::Emergency].into_iter().collect(),
            scope: crate::selection::Selection::default(),
        }
    }

    fn queued(id: i64, next_attempt_ms: i64) -> Queued {
        Queued {
            id,
            call_id: 100 + id,
            attempts: 0,
            next_attempt_ms,
        }
    }

    fn a_call() -> StoredCall {
        StoredCall {
            id: 101,
            system_ref: 11,
            system_label: None,
            talkgroup_ref: 54241,
            talkgroup_label: None,
            talkgroup_group: None,
            talkgroup_tag: None,
            led: None,
            patches: vec![],
            frequency: None,
            unit_ref: None,
            unit_label: None,
            timestamp: Some(1_700_000_000_000),
            audio_mime: None,
            duration_ms: None,
            emergency: true,
            encrypted: false,
            tone: false,
            tones: Vec::new(),
            site_ref: None,
            site_label: None,
            object_key: String::from("calls/101.wav"),
            audio_url: Some(String::from("/api/call/101/audio")),
        }
    }

    /// A world where everything works.
    fn working() -> FakeWorld {
        FakeWorld {
            call: Some(Deliverable {
                call: a_call(),
                marks: Marks::on_call(true, false),
            }),
            answer: Some(204),
            ..FakeWorld::default()
        }
    }

    async fn deliver(world: &FakeWorld, hook: &Webhook) {
        attempt(
            world,
            &WebhookConfig::default(),
            Some("https://scan.example"),
            hook,
            &queued(7, 0),
            1_000,
        )
        .await;
    }

    /// Discord answers `204 No Content` on success, which the shared verdict has
    /// to read as taken rather than as an empty body worth retrying.
    #[tokio::test]
    async fn a_webhook_that_takes_a_call_has_its_delivery_dropped() {
        let world = working();

        deliver(&world, &hook()).await;

        assert_eq!(world.settled(), vec![Settled::Taken]);
        assert_eq!(world.sent()["call"]["id"], 101);
    }

    /// **What is sent is the overlap**, not the Call's whole mark set: a webhook
    /// is told about the mark it asked for, so it never has to work out why it
    /// was woken.
    #[tokio::test]
    async fn a_webhook_is_told_about_the_marks_it_asked_for() {
        let world = working();
        let watching_nothing = Webhook {
            marks: Marks::default(),
            ..hook()
        };

        deliver(&world, &watching_nothing).await;

        assert_eq!(world.sent()["marks"], serde_json::json!([]));
    }

    /// Every way an attempt can fail, and what each one does to the queue.
    ///
    /// The `429` is Discord's rate limit and the `404` is a webhook deleted in
    /// Discord — both retried, because both are things an Operator resolves and
    /// the backlog is what makes resolving them worth doing.
    #[rstest]
    #[case::gone(FakeWorld { call: None, ..working() }, "call-gone", true)]
    #[case::call_unreadable(
        FakeWorld { fails: Some("read the Call"), ..working() }, "archive-unreadable", false
    )]
    #[case::unreachable(FakeWorld { answer: None, ..working() }, "sink-unreachable", false)]
    #[case::rate_limited(FakeWorld { answer: Some(429), ..working() }, "sink-refused (429)", false)]
    #[case::deleted(FakeWorld { answer: Some(404), ..working() }, "sink-refused (404)", false)]
    #[case::malformed(FakeWorld { answer: Some(400), ..working() }, "sink-refused (400)", true)]
    #[tokio::test]
    async fn every_failed_attempt_settles_the_way_its_reason_says(
        #[case] world: FakeWorld,
        #[case] why: &str,
        #[case] abandoned: bool,
    ) {
        deliver(&world, &hook()).await;

        match (world.settled().first(), abandoned) {
            (Some(Settled::Abandoned { why: said }), true) => assert_eq!(said, why),
            (Some(Settled::Deferred { why: said, .. }), false) => assert_eq!(said, why),
            (settled, _) => panic!("{settled:?} is not the settlement {why:?} asks for"),
        }
    }

    /// **A queue row that cannot be written is said out loud**, because the
    /// consequence is a duplicate message in somebody's channel and there is no
    /// dedup at the far end to make it invisible.
    #[tokio::test]
    async fn a_delivery_that_cannot_be_settled_says_so() {
        let capture = LogCapture::start();
        let world = FakeWorld {
            fails: Some("settle the delivery"),
            ..working()
        };

        deliver(&world, &hook()).await;

        let logged = capture.text();
        assert!(logged.contains(" WARN "), "{logged}");
        assert!(logged.contains("queue-unwritable"), "{logged}");
    }

    /// **The URL never reaches a log line**, at any level — it is the
    /// credential, and every other line about this delivery names the Id.
    #[tokio::test]
    async fn nothing_an_attempt_writes_carries_the_url() {
        let capture = LogCapture::start();
        let world = FakeWorld {
            fails: Some("settle the delivery"),
            answer: Some(500),
            ..working()
        };

        deliver(&world, &hook()).await;

        assert!(!capture.text().contains("t0ken"), "{}", capture.text());
    }

    /// An **encrypted** Call is perfectly deliverable here, where it is never
    /// forwardable: a webhook carries facts, and the one fact it exists to carry
    /// is exactly the one an encrypted Call still has.
    #[tokio::test]
    async fn an_encrypted_call_is_still_posted() {
        let world = FakeWorld {
            call: Some(Deliverable {
                call: StoredCall {
                    encrypted: true,
                    tone: false,
                    tones: Vec::new(),
                    audio_url: None,
                    ..a_call()
                },
                marks: Marks::on_call(true, false),
            }),
            ..working()
        };

        deliver(&world, &hook()).await;

        assert_eq!(world.settled(), vec![Settled::Taken]);
        assert_eq!(world.sent()["call"]["encrypted"], true);
    }
}
