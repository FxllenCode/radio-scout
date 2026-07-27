//! The Web Push side of the harness (#16): a stub push service, and the fixed
//! identities a test encrypts between.
//!
//! Sending a notification is an outbound HTTP request to somebody else's
//! server, which is exactly the kind of thing a suite normally mocks away. It
//! isn't mocked here: [`PushService`] is a real Axum server on an ephemeral
//! port that records what arrives, so a test asserts on the bytes that actually
//! left the process — headers, VAPID token and all — and decrypts the body with
//! the subscriber's own private key.
//!
//! Both keypairs are RFC 8291 §5's, because a fixed identity makes a failure
//! reproducible and the RFC's is the one already proven correct in
//! `src/webpush.rs`.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU16, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::post;
use base64::Engine;

/// base64url, unpadded — everything Web Push encodes.
const B64: base64::engine::general_purpose::GeneralPurpose =
    base64::engine::general_purpose::URL_SAFE_NO_PAD;

/// The server's VAPID private key in tests: RFC 8291's application-server key,
/// so the public half below is a constant a test can assert against.
pub const VAPID_PRIVATE_KEY: &str = "yfWPiYE-n46HLnH0KqZOF1fJJU3MYrct3AELtAQ-oRw";

/// The public half of [`VAPID_PRIVATE_KEY`] — what `GET /api/push/key` serves
/// and a browser passes as `applicationServerKey`.
pub const VAPID_PUBLIC_KEY: &str =
    "BP4z9KsN6nGRTbVYI_c7VJSPQTBtkgcy27mlmlMoZIIgDll6e3vCYLocInmYWAmS6TlzAC8wEqKK6PBru3jl7A8";

/// The subscribing device's public key (RFC 8291's user agent).
pub const SUBSCRIBER_PUBLIC: &str =
    "BCVxsr7N_eNgVRqvHtD0zTZsEc6-VV-JvLexhqUzORcxaOzi6-AYWXvTBHm4bjyPjs7Vd8pZGH6SRpkNtoIAiw4";

/// Its private key — the harness plays the device, so it can read what it is
/// sent.
pub const SUBSCRIBER_PRIVATE: &str = "q1dXpw3UpT5VOmu_cf_v6ih07Aems3njxI-JWgLcM94";

/// Its auth secret.
pub const SUBSCRIBER_AUTH: &str = "BTBZMqHH6r4Tts7J_aSIgg";

/// One request a push service received.
pub struct Pushed {
    pub path: String,
    pub headers: HeaderMap,
    pub body: Vec<u8>,
}

impl Pushed {
    /// A header as text, `None` if absent. Names are matched case-insensitively,
    /// as HTTP does.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).and_then(|value| value.to_str().ok())
    }

    /// The message inside, decrypted with [`SUBSCRIBER_PRIVATE`] — what the
    /// service worker on the device would be handed.
    ///
    /// This is RFC 8291 in reverse, written here rather than in the library
    /// because nothing that ships ever decrypts a push: the server is only ever
    /// the sender.
    pub fn decrypted(&self) -> Vec<u8> {
        use aes_gcm::KeyInit;
        use aes_gcm::aead::Aead;
        use p256::elliptic_curve::sec1::ToEncodedPoint;

        let body = &self.body;
        assert!(body.len() > 86, "a body too short to be a push message");
        let salt = &body[..16];
        let key_len = body[20] as usize;
        assert_eq!(key_len, 65, "an uncompressed P-256 point");
        let sender_public_bytes = &body[21..21 + key_len];
        let ciphertext = &body[21 + key_len..];

        let secret =
            p256::SecretKey::from_slice(&B64.decode(SUBSCRIBER_PRIVATE).expect("base64url"))
                .expect("the subscriber's private key");
        let sender = p256::PublicKey::from_sec1_bytes(sender_public_bytes)
            .expect("the sender's ephemeral key");
        let shared = p256::ecdh::diffie_hellman(secret.to_nonzero_scalar(), sender.as_affine());

        let mut key_info = Vec::new();
        key_info.extend_from_slice(b"WebPush: info\0");
        key_info.extend_from_slice(secret.public_key().to_encoded_point(false).as_bytes());
        key_info.extend_from_slice(sender_public_bytes);
        let auth = B64.decode(SUBSCRIBER_AUTH).expect("base64url");
        let ikm: [u8; 32] = expand(&auth, shared.raw_secret_bytes(), &key_info);
        let cek: [u8; 16] = expand(salt, &ikm, b"Content-Encoding: aes128gcm\0");
        let nonce: [u8; 12] = expand(salt, &ikm, b"Content-Encoding: nonce\0");

        let mut plaintext = aes_gcm::Aes128Gcm::new(&cek.into())
            .decrypt(&nonce.into(), ciphertext)
            .expect("the message decrypts with the subscriber's key");
        // The last-record padding delimiter (RFC 8188 §2).
        assert_eq!(plaintext.pop(), Some(0x02), "last-record delimiter");
        plaintext
    }

    /// [`Pushed::decrypted`], parsed — every payload we send is JSON.
    pub fn payload(&self) -> serde_json::Value {
        let plaintext = self.decrypted();
        serde_json::from_slice(&plaintext).unwrap_or_else(|e| {
            panic!(
                "payload is not json ({e}): {:?}",
                String::from_utf8_lossy(&plaintext)
            )
        })
    }
}

fn expand<const N: usize>(salt: &[u8], ikm: &[u8], info: &[u8]) -> [u8; N] {
    let mut out = [0u8; N];
    hkdf::Hkdf::<sha2::Sha256>::new(Some(salt), ikm)
        .expand(info, &mut out)
        .expect("hkdf");
    out
}

/// A push service on an ephemeral port: it accepts POSTs, records them, and
/// answers with whatever status the test asked for.
#[derive(Clone)]
pub struct PushService {
    addr: SocketAddr,
    state: ServiceState,
}

#[derive(Clone, Default)]
struct ServiceState {
    received: Arc<Mutex<Vec<Pushed>>>,
    status: Arc<AtomicU16>,
}

impl PushService {
    /// Start one, answering `201 Created` as a push service does.
    pub async fn start() -> Self {
        let state = ServiceState::default();
        state.status.store(201, Ordering::Relaxed);
        let app = Router::new()
            .route("/push/{id}", post(receive))
            .with_state(state.clone());

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve");
        });
        PushService { addr, state }
    }

    /// The endpoint a subscription registers — one device.
    pub fn endpoint(&self) -> String {
        self.endpoint_for("abc")
    }

    /// A second device's endpoint on the same service.
    pub fn endpoint_for(&self, id: &str) -> String {
        format!("http://{}/push/{id}", self.addr)
    }

    /// Answer later requests with this status — `410 Gone` is how a push
    /// service says the device unsubscribed.
    pub fn answer_with(&self, status: u16) {
        self.state.status.store(status, Ordering::Relaxed);
    }

    /// Everything received so far, in arrival order.
    pub fn received(&self) -> Vec<Pushed> {
        std::mem::take(&mut *self.state.received.lock().expect("received"))
    }

    /// Wait until `count` requests have arrived, then take them.
    ///
    /// Sending is asynchronous by design — it happens on the fanout, not on the
    /// ingest the test awaited — so "it arrived" is a wait, not a read.
    pub async fn wait_for(&self, count: usize) -> Vec<Pushed> {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            if self.state.received.lock().expect("received").len() >= count {
                return self.received();
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "only {} of {count} pushes arrived within the budget",
                self.state.received.lock().expect("received").len()
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    /// Wait out the window in which a push *would* have arrived, and assert
    /// none did. Short, because this wait is paid in full every run.
    pub async fn expect_nothing(&self) {
        tokio::time::sleep(Duration::from_millis(400)).await;
        let received = self.received();
        assert!(
            received.is_empty(),
            "expected no push, got {} to {:?}",
            received.len(),
            received.iter().map(|p| &p.path).collect::<Vec<_>>()
        );
    }
}

async fn receive(
    State(state): State<ServiceState>,
    axum::extract::Path(id): axum::extract::Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> StatusCode {
    state.received.lock().expect("received").push(Pushed {
        path: format!("/push/{id}"),
        headers,
        body: body.to_vec(),
    });
    StatusCode::from_u16(state.status.load(Ordering::Relaxed)).expect("a status")
}
