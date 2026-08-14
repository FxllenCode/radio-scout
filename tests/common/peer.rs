//! A stub **Downstream** peer (#52), on the [`super::push::PushService`]
//! precedent.
//!
//! Forwarding is an outbound HTTP request to somebody else's server, which is
//! the kind of thing a suite normally mocks. It is not mocked here: [`Peer`] is
//! a real Axum server on an ephemeral port speaking the rdio `/api/call-upload`
//! contract, so a test asserts on the multipart that actually left the process —
//! headers, field names, audio bytes and all — rather than on an intention.
//!
//! It can also be **taken down and brought back**, which is what the outage half
//! of the ticket needs. Two kinds of down, deliberately, because they are
//! different arms of the retry policy: [`Peer::answer_with`] is a peer that is
//! up and refusing (a `503` restart, a `401` mistyped key, a `417` Call it will
//! never take), and [`unreachable_url`] is a peer that is not there at all,
//! which fails as a transport error with no status to read.
//!
//! What a test *waits* on is not this peer but the sender's own
//! `TestApp::deliveries_settled`: the peer having answered is a moment earlier
//! than the queue having shrunk, so a depth read against an arrival would race
//! the delete behind it.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU16, Ordering};
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::extract::{Multipart, State};
use axum::http::StatusCode;
use axum::routing::post;

/// One upload a peer received, parsed the way rdio's own handler parses it.
#[derive(Debug, Clone, Default)]
pub struct Received {
    /// Every text field, by name. The audio is not in here — see
    /// [`Received::audio`].
    pub fields: std::collections::HashMap<String, String>,
    /// The `audio` part's bytes, verbatim.
    pub audio: Vec<u8>,
    /// Its filename and `Content-Type`, as the part declared them.
    pub audio_filename: Option<String>,
    pub audio_content_type: Option<String>,
}

impl Received {
    /// A text field, or `None` if the delivery did not carry one.
    pub fn field(&self, name: &str) -> Option<&str> {
        self.fields.get(name).map(String::as_str)
    }

    /// A field parsed as JSON — `units`, `frequencies`, `patches`.
    pub fn json(&self, name: &str) -> serde_json::Value {
        self.field(name)
            .map(|raw| {
                serde_json::from_str(raw)
                    .unwrap_or_else(|error| panic!("{name} is not json ({error}): {raw:?}"))
            })
            .unwrap_or(serde_json::Value::Null)
    }

    /// The Talkgroup Ref this delivery was for — the field every assertion about
    /// *which* Call arrived reads.
    pub fn talkgroup(&self) -> i64 {
        self.field("talkgroup")
            .and_then(|raw| raw.parse().ok())
            .unwrap_or_default()
    }
}

/// A stub peer: it accepts `POST /api/call-upload`, records what arrived, and
/// answers with whatever a test told it to.
#[derive(Clone)]
pub struct Peer {
    addr: SocketAddr,
    state: PeerState,
}

#[derive(Clone, Default)]
struct PeerState {
    received: Arc<Mutex<Vec<Received>>>,
    status: Arc<AtomicU16>,
    /// How long to hold a delivery before answering — see [`Peer::stall_for`].
    stall: Arc<Mutex<Option<std::time::Duration>>>,
    /// The key this peer issued, if a test set one — see [`Peer::expect_key`].
    key: Arc<Mutex<Option<String>>>,
}

impl Peer {
    /// Start one, answering `200` with rdio's own success body.
    pub async fn start() -> Self {
        let state = PeerState::default();
        state.status.store(200, Ordering::Relaxed);
        let app = Router::new()
            .route("/api/call-upload", post(receive))
            .with_state(state.clone());

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve");
        });
        Peer { addr, state }
    }

    /// The base URL an Operator would configure — no `/api/call-upload`, which
    /// the sender appends itself.
    pub fn url(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// Answer later deliveries with this status. `503` is a peer restarting;
    /// `401` a mistyped key; `417` a Call it will never accept.
    ///
    /// A non-2xx is **not recorded**, which is what makes "it queued rather than
    /// arriving" assertable: the peer saw the bytes and refused them, exactly as
    /// a real one would.
    pub fn answer_with(&self, status: u16) {
        self.state.status.store(status, Ordering::Relaxed);
    }

    /// Come back up, taking Calls again.
    pub fn come_back(&self) {
        self.answer_with(200);
    }

    /// Hold every delivery's socket open for this long before answering.
    ///
    /// A peer that is *slow* rather than refusing — the case that proves one
    /// peer's trouble is its own, and the one rdio cannot survive because it
    /// POSTs inline on the ingest goroutine.
    pub fn stall_for(&self, holding: std::time::Duration) {
        *self.state.stall.lock().expect("stall") = Some(holding);
    }

    /// Answer `401` to any delivery not carrying this key, as a real peer does.
    ///
    /// **The only way to test a mistyped key without a race.** Switching the
    /// peer between statuses cannot: an attempt started before the key was
    /// corrected can arrive after the peer came back, so the delivery lands
    /// under the *old* key and the assertion becomes a coin toss — which is
    /// exactly how the first version of that test failed, once in nine runs.
    /// Checking the key here means a delivery carrying the wrong one is never
    /// recorded, whatever the timing.
    pub fn expect_key(&self, key: &str) {
        *self.state.key.lock().expect("key") = Some(key.to_string());
    }

    /// Everything received so far, in arrival order, and forget it.
    pub fn received(&self) -> Vec<Received> {
        std::mem::take(&mut *self.state.received.lock().expect("received"))
    }

    /// How many deliveries have arrived — without taking them.
    pub fn count(&self) -> usize {
        self.state.received.lock().expect("received").len()
    }

    /// The Talkgroup Refs that have arrived, in order — what an ordering
    /// assertion is made of.
    pub fn talkgroups(&self) -> Vec<i64> {
        self.state
            .received
            .lock()
            .expect("received")
            .iter()
            .map(Received::talkgroup)
            .collect()
    }
}

async fn receive(State(state): State<PeerState>, mut multipart: Multipart) -> StatusCode {
    // Read the body before deciding, always: a peer that hung up still consumed
    // what it was sent, and reading it is also what proves the body was
    // well-formed multipart rather than something axum rejected.
    let mut received = Received::default();
    while let Ok(Some(part)) = multipart.next_field().await {
        let name = part.name().unwrap_or_default().to_string();
        if name == "audio" {
            received.audio_filename = part.file_name().map(str::to_string);
            received.audio_content_type = part.content_type().map(str::to_string);
            received.audio = part
                .bytes()
                .await
                .map(|bytes| bytes.to_vec())
                .unwrap_or_default();
            continue;
        }
        if let Ok(value) = part.text().await {
            received.fields.insert(name, value);
        }
    }

    let stall = *state.stall.lock().expect("stall");
    if let Some(holding) = stall {
        tokio::time::sleep(holding).await;
    }

    let expected = state.key.lock().expect("key").clone();
    if let Some(expected) = expected
        && received.field("key") != Some(expected.as_str())
    {
        // rdio's own answer to a key it does not know (`api.go:96`).
        return StatusCode::UNAUTHORIZED;
    }

    let status = StatusCode::from_u16(state.status.load(Ordering::Relaxed))
        .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    if status.is_success() {
        state.received.lock().expect("received").push(received);
    }
    status
}

/// A URL nothing is listening on — a peer that is not merely refusing but
/// *absent*, which is the transport-error arm of the retry policy.
///
/// Port 1 on loopback: privileged, so nothing a developer runs is on it, and
/// a connection to it is refused immediately rather than timing out — which
/// keeps a test that wants an unreachable peer as fast as one that wants a
/// `503`.
pub fn unreachable_url() -> String {
    String::from("http://127.0.0.1:1")
}
