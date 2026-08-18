//! The **Downstream** sender (#52): what one delivery to an rdio-compatible
//! peer actually *is*.
//!
//! The queue around it — the ordering rule, the retry policy, the accounting and
//! the loop — is [`crate::delivery`], shared with [`crate::webhook`] (#54). What
//! is here is the errand: read the Call in the rdio dialect, read its audio
//! bytes, POST the multipart body, and settle the row.
//!
//! # The seam
//!
//! [`Deliveries`] is the four questions *one attempt* asks of the world (#37,
//! #97, the [`crate::enhance::Archive`] precedent). Every interesting arm here
//! is a failure of one of them — a Call pruned while its peer was down, an
//! object store that will not answer, a queue row that cannot be written — and
//! none is reachable while the database answers and the store works. Naming the
//! dependency at the interface is what makes them values a test constructs,
//! rather than something to be provoked by damaging what is underneath.
//!
//! The roster and queue reads that used to sit beside them are now
//! [`crate::delivery::Outbox`]'s, because they are the loop's questions rather
//! than an attempt's: [`Forwarding`] answers those, and is one line each.

use std::sync::Arc;

use tracing::{Instrument, Level, span, warn};

use super::{Downstream, DownstreamConfig, dialect};
use crate::AppState;
use crate::call::CallId;
use crate::db::repo;
use crate::delivery::{self, Failed, Failure, Queued, Settled, Verdict};
use crate::worker::Worker;

/// The four questions **one delivery** asks of the world — the Call and audio it
/// is made of, the peer it goes to, and how it ends.
///
/// A port for the reason [`crate::enhance::Archive`] is one: the arms worth
/// testing here are the failures, and a filesystem that works and a database
/// that answers cannot produce them.
#[async_trait::async_trait]
pub trait Deliveries: Send + Sync {
    /// Everything the dialect can say about a Call, or `None` if it is gone.
    async fn forwardable(&self, call_id: CallId) -> Result<Option<dialect::Forwardable>, Failure>;

    /// The bytes behind an object key, or `None` if the object is gone.
    async fn audio(&self, key: &str) -> Result<Option<bytes::Bytes>, Failure>;

    /// POST one delivery. `Ok(status)` is an answer from the peer, whatever it
    /// says; `Err(())` is never having got one.
    ///
    /// The transport error is deliberately not carried out: reqwest's `Display`
    /// renders the URL, and a peer's URL is an Operator-supplied string that may
    /// carry a query parameter (ADR-0011 rule 2's instinct). What an Operator
    /// needs — that this peer cannot be reached — is the `Err` itself.
    async fn deliver(&self, peer: &Downstream, body: dialect::Body) -> Result<u16, ()>;

    /// Apply what was decided, to the queue row and to the peer's health.
    async fn settle(
        &self,
        delivery_id: i64,
        downstream_id: i64,
        now_ms: i64,
        settled: &Settled,
    ) -> Result<(), Failure>;
}

/// Start the sender. `None` means one is already running.
///
/// Started whatever the roster says, unlike the push and enhancement workers: a
/// Downstream is a **row**, so an Instance with no peers has an empty roster
/// rather than a disabled feature, and a peer created from the browser five
/// minutes from now must be forwarded to without a restart. With none
/// configured it sleeps on its wake-up and costs nothing.
///
/// The `Option` is therefore the **double-spawn guard** (#93) rather than a
/// feature switch: two senders would each attempt the same head, so a peer would
/// receive one Call twice and "drains in order" would stop being true.
pub fn spawn(state: AppState) -> Option<Worker> {
    let dispatch = state.downstreams.dispatch();
    // Taken, not checked: a second call finds it gone and starts nothing.
    dispatch.claim()?;
    // The first pass is owed before the task exists, so an Instance is never
    // observed idle in the window before the sender has looked — see
    // `Dispatcher::owes_a_first_pass`.
    dispatch.owes_a_first_pass();

    Some(delivery::run(
        super::WORKER,
        Arc::new(Forwarding(state)),
        dispatch,
    ))
}

/// The Instance, as the draining loop asks questions of it.
///
/// A newtype rather than an `impl` on [`AppState`] directly, because
/// [`crate::webhook`] needs the same trait on the same state for a different
/// queue — one type cannot implement one trait twice.
pub struct Forwarding(pub AppState);

#[async_trait::async_trait]
impl delivery::Outbox for Forwarding {
    type Sink = Downstream;

    async fn roster(&self) -> Result<Vec<Downstream>, Failure> {
        Ok(repo::forwarding_downstreams(&self.0.db).await?)
    }

    async fn head(&self, downstream_id: i64) -> Result<Option<Queued>, Failure> {
        Ok(repo::next_delivery(&self.0.db, downstream_id)
            .await?
            .map(|row| Queued {
                id: row.id,
                call_id: row.call_id,
                attempts: row.attempts,
                next_attempt_ms: row.next_attempt_ms,
            }))
    }

    async fn attempt(&self, peer: &Downstream, head: &Queued) {
        // The Instance's clock, not the machine's (#90): a test that arranges
        // "this delivery is due" must measure from the same instant the sender
        // stamps with.
        let now_ms = self.0.clock.now_ms();
        attempt(&self.0, self.0.downstreams.config(), peer, head, now_ms)
            // Everything one attempt says names the peer and the Call, so a failure
            // and the statements around it read as one story. The span is built here
            // rather than in the shared loop because only a sink knows what to call
            // itself.
            .instrument(span!(
                Level::ERROR,
                "forward",
                downstream = peer.id,
                call_id = head.call_id
            ))
            .await;
    }

    async fn depth(&self) -> Result<u64, Failure> {
        Ok(repo::delivery_depths(&self.0.db)
            .await?
            .values()
            .sum::<i64>()
            .max(0) as u64)
    }

    fn now_ms(&self) -> i64 {
        self.0.clock.now_ms()
    }
}

/// One delivery attempt, start to finish: read the Call, read its audio, POST
/// it, and settle the row.
pub async fn attempt(
    peers: &dyn Deliveries,
    config: &DownstreamConfig,
    peer: &Downstream,
    head: &Queued,
    now_ms: i64,
) {
    let settled = delivery::decide(
        forward(peers, peer, head.call_id).await.err(),
        head,
        config.retry(),
        now_ms,
    );
    delivery::say(&settled, super::WORKER);
    if let Err(error) = peers.settle(head.id, peer.id, now_ms, &settled).await {
        // The one failure this module cannot recover from by retrying: if the
        // row cannot be written, the same delivery is attempted again on the
        // next wake-up — which for a *successful* delivery means the peer
        // receives it twice. Its own dedup is what makes that survivable, and
        // saying so is what lets an Operator recognise it rather than wonder.
        warn!(
            reason = %"queue-unwritable",
            %error,
            "a Downstream delivery could not be settled; it may be sent again"
        );
    }
}

/// Read, build and POST. `Err` is the one word that explains what went wrong.
async fn forward(peers: &dyn Deliveries, peer: &Downstream, call_id: CallId) -> Result<(), Failed> {
    let call = peers
        .forwardable(call_id)
        .await
        .map_err(|error| {
            warn!(%error, "could not read a Call to forward");
            Failed::Unreadable
        })?
        // Retention is entitled to prune a Call a peer has been unreachable
        // longer than.
        .ok_or(Failed::Vanished)?;

    let audio = match peers.audio(&call.object_key).await {
        Ok(Some(audio)) => audio,
        Ok(None) => return Err(Failed::Vanished),
        Err(error) => {
            warn!(%error, "could not read the audio of a Call to forward");
            return Err(Failed::Unreadable);
        }
    };

    let body = dialect::body(
        &call,
        &peer.api_key,
        &audio,
        // A fresh boundary per request. It has to be absent from the body, and
        // a v4 UUID's 122 bits make that a certainty rather than a scan of the
        // audio on every delivery.
        &format!("radioscout{}", uuid::Uuid::new_v4().simple()),
    );

    match peers.deliver(peer, body).await {
        Ok(status) => match Verdict::of_status(status) {
            Verdict::Delivered => Ok(()),
            _ => Err(Failed::Status(status)),
        },
        Err(()) => Err(Failed::Unreachable),
    }
}

/// The world an `AppState` is: its database for the rows, its store for the
/// objects, its client for the peer.
///
/// The only implementation that ships, and deliberately thin — every decision is
/// above, and each of these is one call. This is the half a test cannot
/// substitute, so it is the half that must have nothing in it worth testing.
#[async_trait::async_trait]
impl Deliveries for AppState {
    async fn forwardable(&self, call_id: CallId) -> Result<Option<dialect::Forwardable>, Failure> {
        Ok(crate::archive::forwardable(&self.db, call_id).await?)
    }

    async fn audio(&self, key: &str) -> Result<Option<bytes::Bytes>, Failure> {
        // An **Encrypted Call** is never queued (it has no object), so an empty
        // key here would be a Call whose audio has gone — the same answer the
        // store gives for one that is missing, and asked of nothing.
        if key.is_empty() {
            return Ok(None);
        }
        Ok(self.audio.get(key).await?)
    }

    async fn deliver(&self, peer: &Downstream, body: dialect::Body) -> Result<u16, ()> {
        self.downstreams
            .dispatch()
            .client()
            .post(peer.upload_url())
            .header(http::header::CONTENT_TYPE, &body.content_type)
            .body(body.bytes)
            .send()
            .await
            .map(|response| response.status().as_u16())
            .map_err(|error| {
                // `without_url`, the rule ADR-0011 rule 5 sets: reqwest
                // renders as "error sending request for url (…)", and a peer's
                // URL is an Operator-supplied string that may carry a query
                // parameter.
                tracing::debug!(error = %error.without_url(), "a Downstream delivery did not complete");
            })
    }

    async fn settle(
        &self,
        delivery_id: i64,
        downstream_id: i64,
        now_ms: i64,
        settled: &Settled,
    ) -> Result<(), Failure> {
        match settled {
            Settled::Taken => {
                repo::drop_delivery(&self.db, delivery_id).await?;
                repo::downstream_succeeded(&self.db, downstream_id, now_ms).await?;
                self.downstreams.dispatch().delivery_settled();
            }
            Settled::Abandoned { why } => {
                repo::downstream_failed(&self.db, downstream_id, now_ms, why).await?;
                repo::drop_delivery(&self.db, delivery_id).await?;
                self.downstreams.dispatch().delivery_settled();
            }
            Settled::Deferred {
                attempts,
                next_attempt_ms,
                why,
            } => {
                repo::downstream_failed(&self.db, downstream_id, now_ms, why).await?;
                repo::defer_delivery(&self.db, delivery_id, *attempts, *next_attempt_ms, why)
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
    use rstest::rstest;
    use std::sync::Mutex;

    /// **One sender, and only ever one** (#93). Two would each take the head of
    /// the same peer's queue, so that peer would receive one Call twice and
    /// "drains in order" would stop being true — and `notify_one` would hand a
    /// wake-up to one of them rather than to both.
    ///
    /// The right to drain *is* the guard, so a second start has nothing to drain
    /// with.
    #[tokio::test]
    async fn a_second_sender_cannot_be_started() {
        let (state, _tmp) = an_app().await;

        let first = spawn(state.clone()).expect("the first sender starts");
        let second = spawn(state.clone());

        assert!(second.is_none(), "a second Downstream sender was started");
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

    /// **What a previous process left, counted** — the webhook sender's own note
    /// applies here word for word: a wrong total costs `settle()`'s guarantee
    /// rather than a delivery, so it is asserted over real rows rather than
    /// through a restart that would forward the Calls either way.
    #[tokio::test]
    async fn the_resume_count_is_every_delivery_every_peer_is_owed() {
        let (state, _tmp) = an_app().await;
        let outbox = Forwarding(state);

        assert_eq!(outbox.depth().await.expect("depth"), 0);

        repo::queue_deliveries(&outbox.0.db, 1, &[1, 2], 0)
            .await
            .expect("queue");
        repo::queue_deliveries(&outbox.0.db, 2, &[1], 0)
            .await
            .expect("queue");

        assert_eq!(outbox.depth().await.expect("depth"), 3);
    }

    // -- The world, substituted ----------------------------------------------

    /// What a refused question says. One string, so an assertion about the cause
    /// reaching a log line is about *this* test's failure and not a driver's
    /// phrasing.
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
        call: Option<dialect::Forwardable>,
        audio: Option<bytes::Bytes>,
        /// What the peer answers, or `None` for a peer that cannot be reached.
        answer: Option<u16>,
        /// The one question this world refuses.
        fails: Option<&'static str>,
        settled: Mutex<Vec<Settled>>,
        delivered: Mutex<Vec<dialect::Body>>,
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
    }

    #[async_trait::async_trait]
    impl Deliveries for FakeWorld {
        async fn forwardable(
            &self,
            _call_id: CallId,
        ) -> Result<Option<dialect::Forwardable>, Failure> {
            self.refuse("read the Call")?;
            Ok(self.call.clone())
        }

        async fn audio(&self, _key: &str) -> Result<Option<bytes::Bytes>, Failure> {
            self.refuse("read the audio")?;
            Ok(self.audio.clone())
        }

        async fn deliver(&self, _peer: &Downstream, body: dialect::Body) -> Result<u16, ()> {
            self.delivered.lock().expect("delivered").push(body);
            self.answer.ok_or(())
        }

        async fn settle(
            &self,
            _delivery_id: i64,
            _downstream_id: i64,
            _now_ms: i64,
            settled: &Settled,
        ) -> Result<(), Failure> {
            self.refuse("settle the delivery")?;
            self.settled.lock().expect("settled").push(settled.clone());
            Ok(())
        }
    }

    fn peer(id: i64) -> Downstream {
        Downstream {
            id,
            url: format!("https://peer{id}.example"),
            api_key: String::from("peer-key"),
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

    fn a_call() -> dialect::Forwardable {
        dialect::Forwardable {
            system_ref: 11,
            system_label: None,
            talkgroup_ref: 54241,
            talkgroup_label: None,
            talkgroup_name: None,
            talkgroup_tag: None,
            talkgroup_groups: vec![],
            call_at_ms: 1_000,
            frequency: None,
            site_ref: None,
            audio_name: None,
            audio_mime: None,
            patches: vec![],
            units: vec![],
            frequencies: vec![],
            object_key: String::from("calls/1.wav"),
        }
    }

    /// A world where everything works: a Call with audio, and a peer that takes
    /// it.
    fn working() -> FakeWorld {
        FakeWorld {
            call: Some(a_call()),
            audio: Some(bytes::Bytes::from_static(b"RIFF....")),
            answer: Some(200),
            ..FakeWorld::default()
        }
    }

    #[tokio::test]
    async fn a_peer_that_takes_a_call_has_its_delivery_dropped() {
        let world = working();

        attempt(
            &world,
            &DownstreamConfig::default(),
            &peer(1),
            &queued(7, 0),
            1_000,
        )
        .await;

        assert_eq!(world.settled(), vec![Settled::Taken]);
        let sent = world.delivered.lock().expect("delivered");
        assert_eq!(sent.len(), 1);
        assert!(sent[0].content_type.starts_with("multipart/form-data;"));
    }

    /// Every way an attempt can fail, and what each one does to the queue.
    ///
    /// The three that are unreachable while the database answers and the store
    /// works are the whole reason [`Deliveries`] is a port: a Call pruned while
    /// its peer was down, an object store that will not answer, and a Call read
    /// that fails outright.
    #[rstest]
    #[case::gone(FakeWorld { call: None, ..working() }, "call-gone", true)]
    #[case::audio_gone(FakeWorld { audio: None, ..working() }, "call-gone", true)]
    #[case::call_unreadable(
        FakeWorld { fails: Some("read the Call"), ..working() }, "archive-unreadable", false
    )]
    #[case::audio_unreadable(
        FakeWorld { fails: Some("read the audio"), ..working() }, "archive-unreadable", false
    )]
    #[case::unreachable(FakeWorld { answer: None, ..working() }, "sink-unreachable", false)]
    #[case::refused(FakeWorld { answer: Some(503), ..working() }, "sink-refused (503)", false)]
    #[case::never(FakeWorld { answer: Some(417), ..working() }, "sink-refused (417)", true)]
    #[tokio::test]
    async fn every_failed_attempt_settles_the_way_its_reason_says(
        #[case] world: FakeWorld,
        #[case] why: &str,
        #[case] abandoned: bool,
    ) {
        attempt(
            &world,
            &DownstreamConfig::default(),
            &peer(1),
            &queued(7, 0),
            1_000,
        )
        .await;

        match (world.settled().first(), abandoned) {
            (Some(Settled::Abandoned { why: said }), true) => assert_eq!(said, why),
            (Some(Settled::Deferred { why: said, .. }), false) => assert_eq!(said, why),
            (settled, _) => panic!("{settled:?} is not the settlement {why:?} asks for"),
        }
    }

    /// **A queue row that cannot be written is said out loud**, because the
    /// consequence is a Call the peer receives twice — survivable only because
    /// the peer dedups, and impossible to recognise without the line.
    #[tokio::test]
    async fn a_delivery_that_cannot_be_settled_says_so() {
        let capture = LogCapture::start();
        let world = FakeWorld {
            fails: Some("settle the delivery"),
            ..working()
        };

        attempt(
            &world,
            &DownstreamConfig::default(),
            &peer(1),
            &queued(7, 0),
            1_000,
        )
        .await;

        let logged = capture.text();
        assert!(logged.contains(" WARN "), "{logged}");
        assert!(logged.contains("queue-unwritable"), "{logged}");
    }
}
