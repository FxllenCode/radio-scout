//! A stub **Webhook** sink (#54).
//!
//! The [`super::peer::Peer`] arrangement, for the other outbound sink and for
//! its reason: posting to a webhook is an outbound HTTP request to somebody
//! else's server, and this is a real Axum server on an ephemeral port rather
//! than a mock — so a test asserts on the JSON that actually left the process,
//! headers and all.
//!
//! Two differences from the peer, both because a webhook is not an rdio
//! instance:
//!
//! - **It accepts a POST at whatever path it was given**, because a webhook URL
//!   is a whole URL an Operator pasted rather than a base with a known path
//!   appended. [`Sink::url`] hands out one with a token-shaped tail, which is
//!   also what makes "the URL never reaches a log line" assertable.
//! - **It answers `204`**, which is what Discord answers, rather than rdio's
//!   `200` with a body. That is a real difference and it exercises the shared
//!   [`radio_scout::delivery::Verdict`] over a status the Downstream path never
//!   sees.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU16, Ordering};
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::post;

/// A stub webhook endpoint: it accepts a POST, records the JSON that arrived,
/// and answers with whatever a test told it to.
#[derive(Clone)]
pub struct Sink {
    addr: SocketAddr,
    state: SinkState,
}

#[derive(Clone, Default)]
struct SinkState {
    received: Arc<Mutex<Vec<serde_json::Value>>>,
    status: Arc<AtomicU16>,
    stall: Arc<Mutex<Option<std::time::Duration>>>,
}

impl Sink {
    /// Start one, answering `204 No Content` as Discord does.
    pub async fn start() -> Self {
        let state = SinkState::default();
        state.status.store(204, Ordering::Relaxed);
        let app = Router::new()
            .route("/hook/{token}", post(receive))
            .with_state(state.clone());

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve");
        });
        Sink { addr, state }
    }

    /// The whole URL an Operator would paste, token and all.
    ///
    /// The tail is deliberately secret-shaped: [`Sink::TOKEN`] is what a test
    /// greps a log capture for to prove the credential never left.
    pub fn url(&self) -> String {
        format!("http://{}/hook/{}", self.addr, Self::TOKEN)
    }

    /// The token in [`Sink::url`] — the string that must never appear anywhere
    /// but in the request itself.
    pub const TOKEN: &'static str = "s3cret-hook-token";

    /// Answer later deliveries with this status. `429` is Discord rate-limiting;
    /// `404` a webhook deleted in Discord; `400` a body it will never render.
    ///
    /// A non-2xx is **not recorded**, which is what makes "it queued rather than
    /// arriving" assertable.
    pub fn answer_with(&self, status: u16) {
        self.state.status.store(status, Ordering::Relaxed);
    }

    /// Come back up, taking Calls again.
    pub fn come_back(&self) {
        self.answer_with(204);
    }

    /// Hold every delivery's socket open for this long before answering — a sink
    /// that is *slow* rather than refusing.
    pub fn stall_for(&self, holding: std::time::Duration) {
        *self.state.stall.lock().expect("stall") = Some(holding);
    }

    /// Everything received so far, in arrival order, and forget it.
    pub fn received(&self) -> Vec<serde_json::Value> {
        std::mem::take(&mut *self.state.received.lock().expect("received"))
    }

    /// How many deliveries have arrived — without taking them.
    pub fn count(&self) -> usize {
        self.state.received.lock().expect("received").len()
    }
}

async fn receive(State(state): State<SinkState>, body: String) -> StatusCode {
    let stall = *state.stall.lock().expect("stall");
    if let Some(holding) = stall {
        tokio::time::sleep(holding).await;
    }

    let status = StatusCode::from_u16(state.status.load(Ordering::Relaxed))
        .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    if status.is_success() {
        state.received.lock().expect("received").push(
            serde_json::from_str(&body).unwrap_or_else(|error| {
                panic!("a webhook body that is not json ({error}): {body}")
            }),
        );
    }
    status
}

/// A URL nothing is listening on — a sink that is not merely refusing but
/// *absent*, which is the transport-error arm of the retry policy.
///
/// Port 1 on loopback, [`super::peer::unreachable_url`]'s reason: privileged, so
/// nothing a developer runs is on it, and a connection is refused immediately
/// rather than timing out.
pub fn unreachable_hook_url() -> String {
    String::from("http://127.0.0.1:1/hook/nobody")
}
