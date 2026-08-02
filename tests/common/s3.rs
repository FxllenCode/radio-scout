//! Real-S3 support for the harness (ticket #35) — ADR-0009's storage half,
//! mirroring the seam #22 built for its database half.
//!
//! **`TEST_S3_ENDPOINT` is the whole switch.** Set it (with credentials) and
//! `tests/s3.rs` runs against a Garage/MinIO that answers; unset — the everyday
//! red-green loop, and any machine without a store to hand — those tests skip,
//! saying so on the run's own output. `docs/agents/real-s3.md` is the command
//! that provides one.
//!
//! Unlike `TEST_POSTGRES_URL` it deliberately does **not** move the rest of the
//! suite. A database is one connection per app; a bucket would be a network
//! round-trip behind every `put_object`, `stored` and `object_keys` in the
//! project — and the everyday loop's speed is the thing #22 was careful to
//! protect. The S3 backend is small and self-contained enough that a suite of
//! its own covers it.
//!
//! It is also where the S3 stores that **misbehave on purpose** live (#39) —
//! [`unreachable_store`] and [`FlakyStore`]. Those need no server to be provided
//! and so run everywhere, but they are the same kind of thing as the above (an
//! S3-backed [`BlobStore`] a test is handed) and belong in one place rather than
//! copied into each file that wants one.

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::get;
use bytes::Bytes;
use object_store::aws::{AmazonS3, AmazonS3Builder};
use object_store::path::Path as ObjectPath;
use object_store::signer::Signer;
use radio_scout::{BlobStore, S3Config};

/// The bucket the offline stores below pretend to serve. Nothing checks it —
/// one of them never answers and the other answers everything — but it has to
/// agree with the endpoint's path for the request to be shaped like S3's.
const OFFLINE_BUCKET: &str = "radio-scout";

/// Credentials for a store that will never validate them. Present because
/// `AmazonS3Builder` requires them to sign, and named so a log line or an error
/// carrying one is obviously from a test.
const OFFLINE_KEY_ID: &str = "test-access";
const OFFLINE_SECRET: &str = "test-secret";

/// An S3 store at an endpoint **nothing is listening on**.
///
/// Port 1 is reserved and never bound, so every attempt is refused the instant
/// it is made — which leaves `blob::retry_policy`'s backoff schedule as the only
/// thing between the call and its error (#39). That is what three tests want
/// when they assert on an archive that is down: `tests/blob.rs` for the policy
/// itself, `tests/archive.rs` for the 500 a download becomes, `tests/enhance.rs`
/// for the Call the worker settles as `skipped` rather than retrying forever.
pub fn unreachable_store() -> BlobStore {
    BlobStore::s3(&S3Config {
        bucket: OFFLINE_BUCKET.into(),
        region: DEFAULT_REGION.into(),
        endpoint: Some("http://127.0.0.1:1".into()),
        access_key_id: OFFLINE_KEY_ID.into(),
        secret_access_key: OFFLINE_SECRET.into(),
        allow_http: true,
    })
    .expect("build an unreachable s3 store")
}

/// A stub S3 on an ephemeral port that **fails a fixed number of times and then
/// works** — a store having a blip rather than an outage (#39).
///
/// The other half of the retry policy: [`unreachable_store`] proves it gives up,
/// this proves it does not give up *first*. `503 Service Unavailable` is what
/// object storage answers while it is restarting or shedding load, and it is one
/// of the statuses `object_store` is willing to retry (`is_server_error`), so a
/// policy that rides out a blip returns the object and a policy that does not
/// returns the 503.
#[derive(Clone)]
pub struct FlakyStore {
    addr: SocketAddr,
    state: FlakyState,
}

#[derive(Clone)]
struct FlakyState {
    /// Counts up on every request, so the handler can answer the first
    /// `failures` differently and the test can assert how many it took.
    requests: Arc<AtomicUsize>,
    failures: usize,
    body: Bytes,
}

impl FlakyStore {
    /// Start one: the first `failures` requests get a 503, every one after that
    /// gets `body`.
    pub async fn start(failures: usize, body: Bytes) -> Self {
        let state = FlakyState {
            requests: Arc::new(AtomicUsize::new(0)),
            failures,
            body,
        };
        let app = Router::new()
            .route("/{*key}", get(flaky_get))
            .with_state(state.clone());

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve");
        });
        FlakyStore { addr, state }
    }

    /// A [`BlobStore`] pointed at it — the real S3 backend, real SigV4, over a
    /// real socket. Only the server on the other end is a stub.
    pub fn store(&self) -> BlobStore {
        BlobStore::s3(&S3Config {
            bucket: OFFLINE_BUCKET.into(),
            region: DEFAULT_REGION.into(),
            endpoint: Some(format!("http://{}", self.addr)),
            access_key_id: OFFLINE_KEY_ID.into(),
            secret_access_key: OFFLINE_SECRET.into(),
            allow_http: true,
        })
        .expect("build a flaky s3 store")
    }

    /// How many requests have arrived — one more than the number of failures
    /// means the retry happened and the second attempt was the one that worked.
    pub fn requests(&self) -> usize {
        self.state.requests.load(Ordering::Relaxed)
    }
}

async fn flaky_get(State(state): State<FlakyState>) -> (StatusCode, Bytes) {
    let seen = state.requests.fetch_add(1, Ordering::Relaxed);
    if seen < state.failures {
        return (StatusCode::SERVICE_UNAVAILABLE, Bytes::new());
    }
    (StatusCode::OK, state.body.clone())
}

/// A stub S3 that accepts a write and **remembers the headers it arrived
/// with** (#31, #97).
///
/// What a write asks a store to record about an object is invisible from a
/// filesystem round trip and invisible from the caller's side, so it used to be
/// asserted by intercepting `PutOptions` inside a decorator under the store.
/// That proved what Radio-Scout *asked for* and stopped one step short of what
/// `object_store` then *sent* — and it needed a production decoration seam to
/// exist at all.
///
/// Over a socket there is no gap left: this is the real S3 backend, real SigV4,
/// and the header a browser would eventually be served by. `tests/s3.rs` proves
/// the same thing against a store that answers for real, and skips wherever one
/// is not running; this half runs everywhere.
#[derive(Clone)]
pub struct RecordingStore {
    addr: SocketAddr,
    headers: Recorded,
}

/// The headers every write has arrived with, newest last.
type Recorded = Arc<std::sync::Mutex<Vec<(String, String)>>>;

impl RecordingStore {
    /// Start one on an ephemeral port.
    pub async fn start() -> Self {
        let headers = Arc::new(std::sync::Mutex::new(Vec::new()));
        let app = Router::new()
            .route("/{*key}", axum::routing::put(record_put))
            .with_state(headers.clone());

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve");
        });
        RecordingStore { addr, headers }
    }

    /// A [`BlobStore`] pointed at it.
    pub fn store(&self) -> BlobStore {
        BlobStore::s3(&S3Config {
            bucket: OFFLINE_BUCKET.into(),
            region: DEFAULT_REGION.into(),
            endpoint: Some(format!("http://{}", self.addr)),
            access_key_id: OFFLINE_KEY_ID.into(),
            secret_access_key: OFFLINE_SECRET.into(),
            allow_http: true,
        })
        .expect("build a recording s3 store")
    }

    /// What `name` arrived as on the last write, if it arrived at all.
    pub fn header(&self, name: &str) -> Option<String> {
        self.headers
            .lock()
            .expect("recorded headers")
            .iter()
            .rev()
            .find(|(header, _)| header == name)
            .map(|(_, value)| value.clone())
    }
}

/// Record what arrived, and answer the way S3 does.
///
/// The `ETag` is not decoration: `object_store` reads one off every successful
/// write and treats its absence as a failed put, so a stub that answers a bare
/// `200` is not answering like an object store.
async fn record_put(
    State(headers): State<Recorded>,
    request: axum::extract::Request,
) -> ([(axum::http::HeaderName, &'static str); 1], StatusCode) {
    let mut recorded = headers.lock().expect("recorded headers");
    for (name, value) in request.headers() {
        if let Ok(value) = value.to_str() {
            recorded.push((name.as_str().to_string(), value.to_string()));
        }
    }
    ([(axum::http::header::ETAG, "\"an-etag\"")], StatusCode::OK)
}

/// How long the `CreateBucket` signature is valid. It is signed and sent in the
/// same breath, so this is a timeout, not a lifetime anyone holds.
const CREATE_BUCKET_TTL: Duration = Duration::from_secs(60);

/// The region to sign with when the run did not name one. MinIO accepts any;
/// Garage checks it against its own `s3_region`, which is why it is settable.
const DEFAULT_REGION: &str = "us-east-1";

/// An S3-compatible store this run may use.
#[derive(Debug, Clone)]
pub struct S3Server {
    endpoint: String,
    access_key_id: String,
    secret_access_key: String,
    region: String,
}

/// The store this run was handed, or `None` for "there isn't one".
///
/// Only the endpoint decides that. A credential missing beside a present
/// endpoint **panics** rather than skipping: a half-configured run that quietly
/// skips is the exact failure this ticket exists to remove — a green run that
/// exercised nothing.
pub fn s3_server() -> Option<S3Server> {
    let endpoint = std::env::var("TEST_S3_ENDPOINT").ok()?;
    Some(S3Server {
        endpoint,
        access_key_id: required("TEST_S3_ACCESS_KEY_ID"),
        secret_access_key: required("TEST_S3_SECRET_ACCESS_KEY"),
        region: std::env::var("TEST_S3_REGION").unwrap_or_else(|_| DEFAULT_REGION.to_string()),
    })
}

/// A real-S3 store of this test's own — or `None`, having said on the run's
/// output why the test that follows is about to prove nothing.
pub async fn test_bucket() -> Option<BlobStore> {
    let Some(server) = s3_server() else {
        // Test-runner output, not application output: a skipped test has to say
        // so to whoever is reading the run, and no subscriber is installed here.
        #[allow(clippy::print_stderr)]
        {
            eprintln!(
                "skipping real-S3 test: TEST_S3_ENDPOINT unset \
                 (see docs/agents/real-s3.md)"
            );
        }
        return None;
    };
    Some(server.create_test_bucket().await)
}

impl S3Server {
    /// What `[storage.s3]` would have produced for `bucket` on this server.
    pub fn config(&self, bucket: &str) -> S3Config {
        S3Config {
            bucket: bucket.to_string(),
            region: self.region.clone(),
            endpoint: Some(self.endpoint.clone()),
            access_key_id: self.access_key_id.clone(),
            secret_access_key: self.secret_access_key.clone(),
            // Every store these tests reach is a throwaway on loopback.
            allow_http: true,
        }
    }

    /// Create a bucket of this test's own and open a [`BlobStore`] onto it.
    ///
    /// A bucket each, for the reason each test gets a database each: `list_keys`
    /// and orphan-GC see the *whole* store, and nextest runs tests in parallel,
    /// in separate processes — so one shared bucket would have every concurrent
    /// test's objects in every other test's listing. The name is a v4 UUID, so
    /// it is unique across processes and not just within one.
    ///
    /// Deliberately **not** emptied afterwards, for the same reason
    /// `create_test_database` leaves its database behind: `Drop` cannot await,
    /// and the store these run against is a throwaway that dies with the job.
    pub async fn create_test_bucket(&self) -> BlobStore {
        let config = self.config(&format!("rs-test-{}", uuid::Uuid::new_v4().simple()));
        create_bucket(&config).await;
        BlobStore::s3(&config).expect("open the test bucket")
    }
}

/// `CreateBucket`, which `object_store` offers no method for — it is an S3
/// request against the bucket *root*, and a presigned `PUT` at that path is
/// exactly that request.
///
/// Signing it with the same SigV4 code the production store uses is what keeps
/// an S3 SDK — and its dependency tree — out of every test build. The shape is
/// `create_test_database`'s: the harness opens its own admin handle to do the
/// one thing the application deliberately cannot.
///
/// Takes the very [`S3Config`] the [`BlobStore`] will be opened from, so the
/// bucket that gets created and the bucket that gets used cannot describe
/// themselves differently.
async fn create_bucket(config: &S3Config) {
    let url = signer(config)
        .signed_url(http::Method::PUT, &ObjectPath::from(""), CREATE_BUCKET_TTL)
        .await
        .expect("sign CreateBucket");
    let endpoint = url.origin().ascii_serialization();
    let response = reqwest::Client::new()
        .put(url)
        .send()
        .await
        .unwrap_or_else(|err| panic!("TEST_S3_ENDPOINT `{endpoint}` did not answer: {err}"));
    let status = response.status();
    assert!(
        status.is_success(),
        "create bucket `{}`: {status} {}",
        config.bucket,
        response.text().await.unwrap_or_default()
    );
}

/// The same store the app will use, as a handle that can sign an *arbitrary*
/// request. [`BlobStore`] exposes only a presigned `GET` — which is the right
/// production surface, and not enough to create a bucket with.
fn signer(config: &S3Config) -> AmazonS3 {
    AmazonS3Builder::new()
        .with_bucket_name(&config.bucket)
        .with_region(&config.region)
        .with_access_key_id(&config.access_key_id)
        .with_secret_access_key(&config.secret_access_key)
        .with_allow_http(config.allow_http)
        .with_endpoint(
            config
                .endpoint
                .clone()
                .expect("an endpoint to sign against"),
        )
        .build()
        .expect("build the bucket-creating handle")
}

/// A credential the endpoint cannot be used without.
fn required(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("TEST_S3_ENDPOINT is set but {name} is not"))
}
