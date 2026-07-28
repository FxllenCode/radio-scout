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

use std::time::Duration;

use object_store::aws::{AmazonS3, AmazonS3Builder};
use object_store::path::Path as ObjectPath;
use object_store::signer::Signer;
use radio_scout::{BlobStore, S3Config};

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
