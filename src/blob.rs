//! Blob storage for call audio (ADR-0002): a single S3-compatible interface via
//! the `object_store` crate. The default is the local filesystem under
//! `base_dir`; S3-compatible stores (Garage first-class, MinIO/AWS too) are an
//! opt-in config flag, not an architecture fork. Audio never lives in the DB.
//!
//! Serving (`GET /api/call/:id/audio`) proxies with HTTP range by default; the
//! S3 backend can instead issue a short-lived presigned URL after an
//! access-scope check, so the app isn't an audio proxy at scale.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use futures_util::TryStreamExt;
use object_store::aws::{AmazonS3, AmazonS3Builder};
use object_store::local::LocalFileSystem;
use object_store::path::Path as ObjectPath;
use object_store::signer::Signer;
use object_store::{
    BackoffConfig, Error as ObjectError, ObjectStore, ObjectStoreExt, PutPayload, RetryConfig,
};
use tracing::warn;

/// How long a presigned URL stays valid.
const PRESIGN_TTL: Duration = Duration::from_secs(300);

/// How the S3 backend retries a request that failed in a way worth retrying —
/// a refused connection, a 5xx, a throttle (#39).
///
/// `object_store` ships `max_retries: 10` over a `retry_timeout` of three
/// minutes, with a randomized backoff climbing to 15 s a sleep. That is a policy
/// for a fleet talking to AWS, and it is the wrong one here twice over: on a Pi
/// it lets a single Call hold an enhancement worker slot for minutes while its
/// Garage box is down, and because each sleep is a *random draw* the time to
/// surface a dead store is a variable with a tail past a minute — which is what
/// made the unreachable-store tests intermittent rather than simply slow.
///
/// So: retry a blip, not an outage. Four retries still ride out a store that
/// answers `503` while it sheds load; a store that is actually *down* surfaces
/// as an error in a couple of seconds, and the layer above decides what that
/// means — the enhancement worker settles the Call as `skipped` and takes the
/// next one, ingest answers the recorder with a failure it will retry itself.
/// Neither is improved by waiting three minutes first, and a restart takes
/// longer than any retry schedule worth having would wait for.
///
/// `retry_timeout` bounds only the *scheduling* of a further retry, not a
/// request already in flight; a store that accepts a connection and then stalls
/// is bounded instead by `ClientOptions`' own 30 s request timeout, which is
/// left at its default deliberately — the request body is a Call's audio, and a
/// tighter one would start failing real uploads over a slow link.
fn retry_policy() -> RetryConfig {
    RetryConfig {
        backoff: BackoffConfig {
            init_backoff: Duration::from_millis(100),
            max_backoff: Duration::from_secs(1),
            base: 2.0,
        },
        max_retries: 4,
        retry_timeout: Duration::from_secs(5),
    }
}

/// A fresh object key, sharded by a two-character prefix so no directory grows
/// unbounded.
///
/// Shared by ingest and enhancement rather than written twice: both create
/// objects in this store, and a sharding rule that drifted between them would
/// leave orphan-GC listing one layout while something else wrote another.
pub fn new_object_key(extension: &str) -> String {
    let uuid = uuid::Uuid::new_v4().simple().to_string();
    format!("{}/{}.{}", &uuid[0..2], uuid, extension)
}

/// S3-compatible backend configuration (Garage / MinIO / AWS).
#[derive(Debug, Clone)]
pub struct S3Config {
    pub bucket: String,
    pub region: String,
    /// Custom endpoint for self-hosted stores (Garage/MinIO); `None` for AWS.
    pub endpoint: Option<String>,
    pub access_key_id: String,
    pub secret_access_key: String,
    /// Allow plain HTTP (self-hosted Garage/MinIO on a LAN).
    pub allow_http: bool,
}

/// Which storage backend to use. Built from `[storage]` by
/// [`crate::config::Config::storage`] (#17).
#[derive(Debug, Clone)]
pub enum StorageConfig {
    Filesystem { root: PathBuf },
    S3(S3Config),
}

/// A stored object as orphan-GC sees it: what it is, how much space it holds,
/// and when it was last written (which is how the GC tells a genuine orphan from
/// an object ingest wrote moments ago and hasn't inserted a row for yet).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredObject {
    pub key: String,
    pub size: u64,
    /// Unix milliseconds of the object's last write.
    pub last_modified_ms: i64,
}

/// A backend-agnostic blob store. Cheap to clone (shared handles).
#[derive(Clone)]
pub struct BlobStore {
    store: Arc<dyn ObjectStore>,
    /// Present only for S3 backends — used to issue presigned URLs.
    signer: Option<Arc<AmazonS3>>,
}

impl BlobStore {
    /// Build from a `StorageConfig`.
    pub fn open(config: &StorageConfig) -> Result<Self, ObjectError> {
        match config {
            StorageConfig::Filesystem { root } => Self::filesystem(root),
            StorageConfig::S3(cfg) => Self::s3(cfg),
        }
    }

    /// The zero-config default: local filesystem rooted at `root`.
    pub fn filesystem(root: impl Into<PathBuf>) -> Result<Self, ObjectError> {
        let root = root.into();
        std::fs::create_dir_all(&root).map_err(|source| ObjectError::Generic {
            store: "LocalFileSystem",
            source: Box::new(source),
        })?;
        let store = LocalFileSystem::new_with_prefix(root)?;
        Ok(Self {
            store: Arc::new(store),
            signer: None,
        })
    }

    /// An S3-compatible backend (Garage/MinIO/AWS).
    pub fn s3(cfg: &S3Config) -> Result<Self, ObjectError> {
        let mut builder = AmazonS3Builder::new()
            .with_bucket_name(&cfg.bucket)
            .with_region(&cfg.region)
            .with_access_key_id(&cfg.access_key_id)
            .with_secret_access_key(&cfg.secret_access_key)
            .with_allow_http(cfg.allow_http)
            .with_retry(retry_policy());
        if let Some(endpoint) = &cfg.endpoint {
            builder = builder.with_endpoint(endpoint);
        }
        let s3 = Arc::new(builder.build()?);
        Ok(Self {
            store: s3.clone(),
            signer: Some(s3),
        })
    }

    /// Whether this backend serves via presigned URLs (S3) rather than proxying.
    pub fn is_presigning(&self) -> bool {
        self.signer.is_some()
    }

    /// This store with its backend wrapped by `decorate`, which is handed the
    /// current backend and returns the one to use in its place.
    ///
    /// **The seam a test harness makes I/O fail through (#37).** Every worker
    /// that reads or writes audio has an error arm — the enhancement worker
    /// settling a Call as `skipped`, ingest answering a recorder 500 — and while
    /// the only store in the suite is a filesystem that works, not one of them
    /// is reachable. They shipped untested until a store could be *told* to
    /// fail. Composing rather than constructing is what lets that decoration sit
    /// over a real filesystem store, or a real S3 one, without the harness
    /// reimplementing either.
    ///
    /// The presigning half is deliberately left pointing at the undecorated S3
    /// client: a decorator has no credentials and cannot sign, and silently
    /// dropping the signer would turn an S3-backed store into a proxying one
    /// halfway through a test.
    pub fn decorated(
        self,
        decorate: impl FnOnce(Arc<dyn ObjectStore>) -> Arc<dyn ObjectStore>,
    ) -> Self {
        Self {
            store: decorate(self.store),
            signer: self.signer,
        }
    }

    /// Store `bytes` under `key`.
    pub async fn put(&self, key: &str, bytes: Bytes) -> Result<(), ObjectError> {
        self.store
            .put(&ObjectPath::from(key), PutPayload::from_bytes(bytes))
            .await?;
        Ok(())
    }

    /// The size in bytes of the object at `key`, or `None` if it's absent.
    pub async fn size(&self, key: &str) -> Result<Option<u64>, ObjectError> {
        match self.store.head(&ObjectPath::from(key)).await {
            Ok(meta) => Ok(Some(meta.size)),
            Err(ObjectError::NotFound { .. }) => Ok(None),
            Err(err) => Err(err),
        }
    }

    /// Fetch the whole object, or `None` if absent.
    pub async fn get(&self, key: &str) -> Result<Option<Bytes>, ObjectError> {
        match self.store.get(&ObjectPath::from(key)).await {
            Ok(result) => Ok(Some(result.bytes().await?)),
            Err(ObjectError::NotFound { .. }) => Ok(None),
            Err(err) => Err(err),
        }
    }

    /// Fetch a byte range `[start, end)` of the object.
    pub async fn get_range(&self, key: &str, start: u64, end: u64) -> Result<Bytes, ObjectError> {
        self.store
            .get_range(&ObjectPath::from(key), start..end)
            .await
    }

    /// Delete the object at `key` (idempotent-ish; missing is not an error).
    pub async fn delete(&self, key: &str) -> Result<(), ObjectError> {
        match self.store.delete(&ObjectPath::from(key)).await {
            Ok(()) | Err(ObjectError::NotFound { .. }) => Ok(()),
            Err(err) => Err(err),
        }
    }

    /// List every object key in the store.
    pub async fn list_keys(&self) -> Result<Vec<String>, ObjectError> {
        Ok(self
            .list_objects()
            .await?
            .into_iter()
            .map(|object| object.key)
            .collect())
    }

    /// List every object with the metadata orphan-GC judges it by (#10).
    pub async fn list_objects(&self) -> Result<Vec<StoredObject>, ObjectError> {
        let metas = self.store.list(None).try_collect::<Vec<_>>().await?;
        Ok(metas
            .into_iter()
            .map(|meta| StoredObject {
                key: meta.location.to_string(),
                size: meta.size,
                last_modified_ms: meta.last_modified.timestamp_millis(),
            })
            .collect())
    }

    /// A short-lived presigned GET URL for `key` — `None` on non-S3 backends.
    /// The caller performs the access-scope check *before* calling this
    /// (listening is open in v1, so the check is a no-op).
    pub async fn presigned_get_url(&self, key: &str) -> Option<Result<String, ObjectError>> {
        let signer = self.signer.as_ref()?;
        Some(
            signer
                .signed_url(http::Method::GET, &ObjectPath::from(key), PRESIGN_TTL)
                .await
                .map(|url| url.to_string()),
        )
    }
}

/// What one orphan-GC pass did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct GcOutcome {
    /// Objects reclaimed, in the order they were deleted.
    pub reclaimed: Vec<StoredObject>,
    /// Objects that were orphans but whose delete failed. Not fatal — they stay
    /// orphans and the next pass retries them, so one unhappy object can't stop
    /// the sweep from reclaiming the rest.
    pub errors: u64,
}

impl GcOutcome {
    /// Total bytes reclaimed.
    pub fn bytes(&self) -> u64 {
        self.reclaimed.iter().map(|object| object.size).sum()
    }
}

/// Orphan-GC (ADR-0002): reclaim stored audio no Call row points at — the
/// residue of an ingest that wrote its object and then failed to insert, or of a
/// prune that deleted the row and crashed before the object.
///
/// **Only objects last written before `written_before_ms` are touched.** Ingest
/// writes the object *before* the row, so an unconditional "no row → delete"
/// sweep would race a Call that is mid-ingest and delete its audio out from
/// under it. The caller derives the cutoff from
/// [`RetentionConfig::orphan_grace`](crate::retention::RetentionConfig::orphan_grace).
pub async fn orphan_gc(
    store: &BlobStore,
    referenced_keys: &HashSet<String>,
    written_before_ms: i64,
) -> Result<GcOutcome, ObjectError> {
    let mut outcome = GcOutcome::default();
    for object in store.list_objects().await? {
        if !is_reclaimable(&object, referenced_keys, written_before_ms) {
            continue;
        }
        match store.delete(&object.key).await {
            Ok(()) => outcome.reclaimed.push(object),
            Err(error) => {
                // Say *why* rather than just counting: the object stays an orphan
                // and the next pass retries it, but the operator needs the cause.
                warn!(object_key = %object.key, %error, "orphan-gc could not delete object");
                outcome.errors += 1;
            }
        }
    }
    Ok(outcome)
}

/// Whether orphan-GC may reclaim `object`: nothing references it *and* it is
/// older than the write grace period.
fn is_reclaimable(
    object: &StoredObject,
    referenced_keys: &HashSet<String>,
    written_before_ms: i64,
) -> bool {
    !referenced_keys.contains(&object.key) && object.last_modified_ms < written_before_ms
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    fn object(key: &str, last_modified_ms: i64) -> StoredObject {
        StoredObject {
            key: key.to_string(),
            size: 1,
            last_modified_ms,
        }
    }

    #[rstest]
    // Unreferenced and written well before the cutoff — a genuine orphan.
    #[case("aa/1.wav", 500, true)]
    // Referenced by a live Call row: never reclaimed, however old.
    #[case("keep/1.wav", 0, false)]
    // Unreferenced but written after the cutoff — this is exactly the object
    // ingest just wrote and hasn't inserted a row for yet.
    #[case("aa/1.wav", 1500, false)]
    // Written *at* the cutoff still counts as inside the grace period.
    #[case("aa/1.wav", 1000, false)]
    fn orphan_reclaim_decision(
        #[case] key: &str,
        #[case] last_modified_ms: i64,
        #[case] expected: bool,
    ) {
        let referenced: HashSet<String> = ["keep/1.wav".to_string()].into_iter().collect();
        assert_eq!(
            is_reclaimable(&object(key, last_modified_ms), &referenced, 1000),
            expected
        );
    }

    /// The upper bound of a `RetryConfig`'s backoff schedule — how long it can
    /// spend *asleep* before giving up, in the unluckiest draw.
    ///
    /// An independent model of `object_store`'s `Backoff::next`
    /// (`src/client/backoff.rs`), which is why it lives here rather than being
    /// asked of the crate: each sleep is drawn from `init_backoff..(previous *
    /// base)` and clamped to `max_backoff`, and the value *returned* is the
    /// previous draw. So the first sleep is always `init_backoff`, and the
    /// bound of the k-th is the bound of the (k-1)-th draw.
    fn worst_case_backoff(config: &RetryConfig) -> Duration {
        let backoff = &config.backoff;
        let mut bound = backoff.init_backoff.as_secs_f64();
        let mut total = 0.0;
        for retry in 0..config.max_retries {
            total += bound;
            if retry + 1 < config.max_retries {
                bound = backoff.max_backoff.as_secs_f64().min(bound * backoff.base);
            }
        }
        Duration::from_secs_f64(total)
    }

    /// The model itself, against hand-worked schedules — otherwise the bounds
    /// asserted below are only as trustworthy as an unchecked formula.
    #[rstest]
    // `object_store`'s own default: 0.1 + (0.2 + 0.4 + 0.8 + 1.6 + 3.2 + 6.4 +
    // 12.8 + 15 + 15), the last two clamped by `max_backoff`.
    #[case(100, 15_000, 2.0, 10, 55_500)]
    // Ours: 0.1 + 0.2 + 0.4 + 0.8, nothing reaching the clamp.
    #[case(100, 1_000, 2.0, 4, 1_500)]
    // `max_backoff` below `init * base` clamps from the very first draw:
    // 1 + 2 + 2 + 2.
    #[case(1_000, 2_000, 2.0, 4, 7_000)]
    // One retry sleeps exactly `init_backoff` once — the schedule never
    // advances.
    #[case(100, 15_000, 2.0, 1, 100)]
    // Retries disabled: no sleeping at all.
    #[case(100, 15_000, 2.0, 0, 0)]
    fn worst_case_backoff_of_a_schedule(
        #[case] init_ms: u64,
        #[case] max_ms: u64,
        #[case] base: f64,
        #[case] max_retries: usize,
        #[case] expected_ms: u64,
    ) {
        let config = RetryConfig {
            backoff: BackoffConfig {
                init_backoff: Duration::from_millis(init_ms),
                max_backoff: Duration::from_millis(max_ms),
                base,
            },
            max_retries,
            retry_timeout: Duration::from_secs(180),
        };
        assert_eq!(
            worst_case_backoff(&config),
            Duration::from_millis(expected_ms)
        );
    }

    /// Why we override at all (#39): `object_store`'s shipped default can sleep
    /// for the better part of a minute before it surfaces a dead store, so the
    /// time to give up is a random variable with a tail far past any deadline a
    /// caller would think to set — and past any time a Pi should hold an
    /// enhancement worker slot for one Call.
    #[test]
    fn the_shipped_default_gives_up_in_minutes() {
        assert!(
            worst_case_backoff(&RetryConfig::default()) > Duration::from_secs(50),
            "if upstream has fixed this, our override can be reconsidered"
        );
    }

    /// Ours gives up in seconds. The bound is what makes an unreachable store a
    /// *bounded* failure: the worker settles the Call and moves on instead of
    /// holding the slot through a backoff schedule nobody chose.
    #[test]
    fn our_policy_gives_up_in_seconds() {
        let policy = retry_policy();
        let bound = worst_case_backoff(&policy);
        assert!(bound < Duration::from_secs(2), "backoff bound: {bound:?}");
        assert!(
            policy.retry_timeout <= Duration::from_secs(5),
            "the backoff bound above assumes every attempt is instant; this is \
             what holds when they are not, and it must stay the looser of the two"
        );
        assert!(
            policy.max_retries > 0,
            "a transient blip should still be retried, not surfaced on first \
             sight — `a_store_that_fails_once_is_retried_and_answers` is the proof"
        );
    }
}
