//! Blob storage for call audio (ADR-0002): a single S3-compatible interface via
//! the `object_store` crate. The default is the local filesystem under
//! `base_dir`; S3-compatible stores (Garage first-class, MinIO/AWS too) are an
//! opt-in config flag, not an architecture fork. Audio never lives in the DB.
//!
//! Serving (`GET /api/call/:id/audio`) proxies with HTTP range by default; the
//! S3 backend can instead issue a short-lived presigned URL after an
//! access-scope check, so the app isn't an audio proxy at scale.

use std::collections::{HashMap, HashSet};
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

/// How much of a signature's life is never given away (#31).
///
/// A signed URL is reused only while more than this remains, and the redirect to
/// it advertises `max-age = remaining - PRESIGN_MARGIN`. Both directions matter:
/// a client is never handed a URL about to die, and a cached redirect falls out
/// of the client's cache before the signature it points at expires. A stale
/// cached redirect is worse than the double download it exists to prevent — it
/// is a 403 in the middle of playback.
///
/// The margin is also the whole of our tolerance for **clock skew**. A
/// signature's life is judged by the *store* against the `X-Amz-Date` we
/// stamped, so a Radio-Scout running more than a minute behind its Garage would
/// hand out URLs the store has already retired. A minute is generous for two
/// machines on a LAN with any time sync at all, and the failure is loud and
/// immediate rather than subtle — but it is an assumption, not an invariant.
const PRESIGN_MARGIN: Duration = Duration::from_secs(60);

/// The most signatures held at once.
///
/// Not the normal bound. Every entry expires within [`PRESIGN_TTL`] and expired
/// ones are pruned on the way past, so in practice the map holds "distinct Calls
/// served in the last five minutes" — a handful on a Pi. This is the backstop
/// against something walking the whole archive fast enough to outrun that, and
/// the cost of hitting it is one extra signing per object, never a wrong answer.
const PRESIGN_CACHE_CAP: usize = 1024;

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

/// A presigned URL, and how long a redirect to it may be cached.
#[derive(Clone, PartialEq, Eq)]
pub struct PresignedUrl {
    pub url: String,
    /// Seconds a client may cache the redirect — the signature's remaining
    /// validity less [`PRESIGN_MARGIN`], so the cached redirect always expires
    /// first.
    pub max_age_secs: u64,
}

/// Redacted, because a presigned URL **is** a credential: its query string
/// carries the signature that authorises the fetch, and anything holding the URL
/// can read the object until it expires. ADR-0011 rule 2 forbids a secret in a
/// log line at any level, and a derived `Debug` would put one there the first
/// time somebody wrote `?signed` in a `tracing` call. Same reason
/// `webpush::Recipient` redacts an endpoint and `config::S3` redacts a key.
impl std::fmt::Debug for PresignedUrl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PresignedUrl")
            .field("url", &redact_query(&self.url))
            .field("max_age_secs", &self.max_age_secs)
            .finish()
    }
}

/// A signed URL with its query string — the signature — replaced.
fn redact_query(url: &str) -> String {
    match url.split_once('?') {
        Some((base, _)) => format!("{base}?<signature redacted>"),
        None => url.to_string(),
    }
}

/// A backend-agnostic blob store. Cheap to clone (shared handles).
#[derive(Clone)]
pub struct BlobStore {
    store: Arc<dyn ObjectStore>,
    /// Present only for S3 backends — used to issue presigned URLs.
    signer: Option<Arc<AmazonS3>>,
    /// Signatures already minted, by object key (#31). Shared across clones,
    /// because every handler holds a clone of the same store and the point is
    /// that the prefetch and the `<audio>` element get the same URL.
    signed: Arc<std::sync::Mutex<HashMap<String, Signed>>>,
}

/// One cached signature. No `Debug`: the URL is a credential — see
/// [`PresignedUrl`]'s own impl.
#[derive(Clone)]
struct Signed {
    url: String,
    /// When the signature stops being valid, on the **wall clock**.
    ///
    /// `SystemTime`, not `Instant`, because that is the clock SigV4 itself is
    /// stamped from: the signature carries an `X-Amz-Date` and an
    /// `X-Amz-Expires`, and its life is measured against wall time whatever this
    /// process thinks. A monotonic `Instant` does not advance while a machine is
    /// suspended, so a laptop closed for an hour would wake believing every
    /// cached signature still had minutes left and serve dead URLs — 403s in the
    /// middle of playback — until it caught up. Wall time cannot drift away from
    /// the thing it is measuring.
    expires_at: std::time::SystemTime,
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
            signed: Default::default(),
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
            signed: Default::default(),
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
            // The decorated store shares the undecorated one's signature cache,
            // for the same reason it shares its signer: decoration changes how
            // bytes are read, not who the object is.
            signed: self.signed,
        }
    }

    /// Store `bytes` under `key`.
    ///
    /// On an S3 backend the object is written carrying the same `Cache-Control`
    /// the proxying path puts on its own responses (#31). It has to: with a
    /// presigned redirect the store answers the client directly, so a header
    /// Radio-Scout sets on *its* response is never seen. Without one the browser
    /// falls back to heuristic freshness, which for an object written moments
    /// ago is nothing — so the element would revalidate every prefetched Call
    /// instead of playing it from cache, and a stable signed URL would have
    /// bought a 304 rather than the silence it is supposed to buy.
    ///
    /// An object key is minted fresh per object and never rewritten (enhancement
    /// writes a *new* key), so `immutable` is exactly true of the bytes at it.
    pub async fn put(&self, key: &str, bytes: Bytes) -> Result<(), ObjectError> {
        let options = object_store::PutOptions {
            attributes: self.object_attributes(),
            ..Default::default()
        };
        self.store
            .put_opts(
                &ObjectPath::from(key),
                PutPayload::from_bytes(bytes),
                options,
            )
            .await?;
        Ok(())
    }

    /// The attributes a stored object carries.
    ///
    /// Empty on the filesystem backend, and not merely as an optimisation:
    /// `object_store` specifies that a backend which cannot honour an attribute
    /// returns an error, and `LocalFileSystem` cannot store one. It needs none
    /// either — nothing fetches a local object directly, so the caching promise
    /// is made by `serve_audio`'s own response header.
    fn object_attributes(&self) -> object_store::Attributes {
        match self.is_presigning() {
            true => object_store::Attributes::from_iter([(
                object_store::Attribute::CacheControl,
                object_store::AttributeValue::from(crate::AUDIO_CACHE_CONTROL),
            )]),
            false => object_store::Attributes::new(),
        }
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
    ///
    /// **The same URL for the same object, for a slice of its lifetime (#31).**
    /// The client prefetches the next Call's audio and the `<audio>` element
    /// then asks for it again; signing afresh each time meant the element was
    /// sent somewhere the prefetch had not warmed, so every prefetched Call was
    /// downloaded twice. A signature is reused while more than [`PRESIGN_MARGIN`]
    /// of it remains, and the caller is told how long the redirect may be cached
    /// — always less than what is left, so a cached redirect cannot outlive the
    /// signature it points at.
    pub async fn presigned_get_url(&self, key: &str) -> Option<Result<PresignedUrl, ObjectError>> {
        self.presigned_get_url_at(key, std::time::SystemTime::now())
            .await
    }

    /// [`BlobStore::presigned_get_url`] against an explicit clock.
    ///
    /// The clock is a parameter because every decision here is about elapsed
    /// time — reuse, re-mint, prune — and the window is five minutes wide. Given
    /// `now`, this is the whole of that logic and a test can walk it to the
    /// boundary and past it; the public method above supplies the real clock and
    /// nothing else.
    async fn presigned_get_url_at(
        &self,
        key: &str,
        now: std::time::SystemTime,
    ) -> Option<Result<PresignedUrl, ObjectError>> {
        let signer = self.signer.as_ref()?;

        if let Some(cached) = self.cached_signature(key, now) {
            return Some(Ok(cached));
        }

        let url = match signer
            .signed_url(http::Method::GET, &ObjectPath::from(key), PRESIGN_TTL)
            .await
        {
            Ok(url) => url.to_string(),
            Err(err) => return Some(Err(err)),
        };
        let expires_at = now + PRESIGN_TTL;
        self.remember_signature(key, &url, expires_at, now);
        Some(Ok(PresignedUrl {
            url,
            max_age_secs: max_age_of(expires_at, now),
        }))
    }

    /// The signature cache.
    ///
    /// A poisoned lock is taken anyway: the map holds no invariant a panic
    /// could have left half-built — every entry is inserted whole — so the
    /// alternative is a cache that silently stays dead for the life of the
    /// process, and every prefetched Call quietly downloading twice again with
    /// nothing anywhere saying why.
    fn held(&self) -> std::sync::MutexGuard<'_, HashMap<String, Signed>> {
        self.signed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The signature held for `key`, if one is there with enough life left in it
    /// to be worth handing out.
    fn cached_signature(&self, key: &str, now: std::time::SystemTime) -> Option<PresignedUrl> {
        let signed = self.held();
        let entry = signed.get(key)?;
        (entry.expires_at > now + PRESIGN_MARGIN).then(|| PresignedUrl {
            url: entry.url.clone(),
            max_age_secs: max_age_of(entry.expires_at, now),
        })
    }

    /// Hold `url` against `key`, dropping what has expired on the way past.
    fn remember_signature(
        &self,
        key: &str,
        url: &str,
        expires_at: std::time::SystemTime,
        now: std::time::SystemTime,
    ) {
        let mut signed = self.held();
        signed.retain(|_, entry| entry.expires_at > now);
        if signed.len() >= PRESIGN_CACHE_CAP {
            // Nothing here has expired yet, so evict whatever is closest to it —
            // the entry with the least left to give. Dropping the whole map
            // instead would re-break every listener's warmed pair at once, which
            // is precisely the bug this cache exists to fix, and it would happen
            // under load. One eviction is enough because one insert follows.
            let soonest = signed
                .iter()
                .min_by_key(|(_, entry)| entry.expires_at)
                .map(|(key, _)| key.clone())
                // `PRESIGN_CACHE_CAP` is a non-zero constant, so a map that has
                // reached it holds at least one entry to pick.
                .expect("a cache at its cap is not empty");
            signed.remove(&soonest);
            tracing::debug!(cap = PRESIGN_CACHE_CAP, "presigned url cache full");
        }
        signed.insert(
            key.to_string(),
            Signed {
                url: url.to_string(),
                expires_at,
            },
        );
    }
}

/// How long a redirect to a signature expiring at `expires_at` may be cached:
/// what is left of it, less [`PRESIGN_MARGIN`].
///
/// Saturating, so the answer is never a `max-age` longer than the signature —
/// the one outcome that would hand a listener a 403 mid-playback.
fn max_age_of(expires_at: std::time::SystemTime, now: std::time::SystemTime) -> u64 {
    expires_at
        .duration_since(now)
        // A signature already past its expiry has nothing left to advertise —
        // and neither does a clock that has stepped backwards under us.
        .unwrap_or_default()
        .saturating_sub(PRESIGN_MARGIN)
        .as_secs()
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
    // -- The presigned-URL cache (#31) ---------------------------------------

    /// An S3 store that signs offline, so the cache is exercised without a
    /// bucket — SigV4 needs no network.
    fn signing_store() -> BlobStore {
        BlobStore::s3(&S3Config {
            bucket: "radio-scout".into(),
            region: "us-east-1".into(),
            endpoint: Some("http://127.0.0.1:9000".into()),
            access_key_id: "test-access".into(),
            secret_access_key: "test-secret".into(),
            allow_http: true,
        })
        .expect("s3 store")
    }

    /// A fixed clock to measure the window from.
    fn epoch() -> std::time::SystemTime {
        std::time::UNIX_EPOCH + Duration::from_secs(1_700_000_000)
    }

    async fn sign_at(store: &BlobStore, key: &str, at: std::time::SystemTime) -> PresignedUrl {
        store
            .presigned_get_url_at(key, at)
            .await
            .expect("an S3 backend signs")
            .expect("signing succeeds offline")
    }

    /// How long the store thinks `key`'s signature has left.
    fn held_expiry(store: &BlobStore, key: &str) -> Option<std::time::SystemTime> {
        Some(store.held().get(key)?.expires_at)
    }

    /// The expiry boundary — the half no HTTP test can reach without waiting
    /// five minutes (#31).
    ///
    /// A signature is reused while more than [`PRESIGN_MARGIN`] of its life
    /// remains, and re-minted the moment it is not, so no client is ever handed
    /// a URL with less than the margin left on it — whatever the `max-age` said
    /// when the redirect was cached.
    #[tokio::test]
    async fn a_signature_is_reused_until_the_margin_and_then_re_minted() {
        let store = signing_store();
        let start = epoch();

        let first = sign_at(&store, "ab/clip.m4a", start).await;
        assert_eq!(first.max_age_secs, 240, "300s signed, 60s held back");
        let first_expiry = held_expiry(&store, "ab/clip.m4a").expect("held");

        // Most of the way through: the same URL, advertising what is left.
        let late = start + Duration::from_secs(239);
        let reused = sign_at(&store, "ab/clip.m4a", late).await;
        assert_eq!(reused.url, first.url, "reused, not re-signed");
        assert_eq!(reused.max_age_secs, 1);
        assert_eq!(
            held_expiry(&store, "ab/clip.m4a"),
            Some(first_expiry),
            "a reuse does not extend the signature it reuses"
        );

        // A second later only the margin remains, so it is re-minted — and the
        // client gets a full budget again rather than a dying URL.
        let at_margin = start + Duration::from_secs(240);
        let reminted = sign_at(&store, "ab/clip.m4a", at_margin).await;
        assert_eq!(reminted.max_age_secs, 240, "re-signed at the margin");
        assert_eq!(
            held_expiry(&store, "ab/clip.m4a"),
            Some(at_margin + PRESIGN_TTL),
            "the held signature is the new one"
        );
    }

    /// Two objects are two signatures — a cache that answered for the wrong key
    /// would send a listener to somebody else's audio.
    #[tokio::test]
    async fn each_object_gets_its_own_signature() {
        let store = signing_store();

        let one = sign_at(&store, "ab/one.m4a", epoch()).await;
        let two = sign_at(&store, "cd/two.m4a", epoch()).await;

        assert_ne!(one.url, two.url);
        assert!(one.url.contains("ab/one.m4a"), "{}", one.url);
        assert!(two.url.contains("cd/two.m4a"), "{}", two.url);
    }

    /// Expired entries go on the way past, so the map is bounded by what has
    /// been served inside one TTL rather than by everything ever served.
    #[tokio::test]
    async fn expired_signatures_are_dropped_rather_than_accumulating() {
        let store = signing_store();
        for n in 0..8 {
            sign_at(&store, &format!("ab/{n}.m4a"), epoch()).await;
        }
        assert_eq!(store.held().len(), 8);

        // At the exact instant they expire — not a moment after — one more
        // request clears every one of them out. A signature with zero seconds
        // left is spent, and holding it would let dead entries count against
        // the cap.
        let later = epoch() + PRESIGN_TTL;
        sign_at(&store, "cd/later.m4a", later).await;

        let held = store.held();
        assert_eq!(
            held.len(),
            1,
            "only the live one is held: {:?}",
            held.keys()
        );
        assert!(held.contains_key("cd/later.m4a"));
    }

    /// The backstop: something walking the archive faster than the TTL ages
    /// entries out cannot grow the map without bound. Hitting it costs one extra
    /// signing per object and never a wrong URL.
    #[tokio::test]
    async fn the_cache_is_capped_even_when_nothing_has_expired() {
        let store = signing_store();
        for n in 0..=PRESIGN_CACHE_CAP {
            sign_at(&store, &format!("ab/{n}.m4a"), epoch()).await;
        }

        let held = store.held().len();
        assert!(held <= PRESIGN_CACHE_CAP, "held {held}");
        // ...and it still answers correctly afterwards.
        let after = sign_at(&store, "cd/after.m4a", epoch()).await;
        assert!(after.url.contains("cd/after.m4a"), "{}", after.url);
        assert_eq!(after.max_age_secs, 240);
    }

    /// A signed URL is a bearer credential, so it must not be able to reach a
    /// log line even by accident (ADR-0011 rule 2). `Debug` is where that would
    /// happen — the first `?signed` somebody writes in a `tracing` call — so it
    /// prints the object it names and nothing that would let a reader fetch it.
    #[tokio::test]
    async fn debug_never_carries_the_signature() {
        let store = signing_store();

        let signed = sign_at(&store, "ab/clip.m4a", epoch()).await;
        let shown = format!("{signed:?}");

        assert!(
            shown.contains("radio-scout/ab/clip.m4a"),
            "still says which object: {shown}"
        );
        assert!(
            !shown.contains("X-Amz-Signature"),
            "the signature is redacted: {shown}"
        );
        assert!(
            !shown.contains("X-Amz-Credential"),
            "and so is the access key id: {shown}"
        );
        // The whole query string goes, not just the parts named above.
        assert!(shown.contains("<signature redacted>"), "{shown}");
    }

    /// A URL with nothing to redact is passed through rather than mangled.
    #[test]
    fn redacting_a_url_with_no_query_leaves_it_alone() {
        assert_eq!(
            redact_query("http://store/radio-scout/ab/clip.m4a"),
            "http://store/radio-scout/ab/clip.m4a"
        );
    }

    /// The bytes are written carrying the caching promise on an S3 backend —
    /// the store answers the client directly there, so a header `serve_audio`
    /// puts on its own response is never seen (#31). The filesystem backend
    /// gets none: `object_store` errors on an attribute a backend cannot store,
    /// and nothing fetches a local object directly anyway.
    ///
    /// That the attribute survives a round trip through a *real* store is
    /// `tests/s3.rs`; this pins which backend asks for it.
    #[test]
    fn only_the_s3_backend_stamps_objects_with_cache_control() {
        let dir = tempfile::tempdir().expect("tempdir");

        let filesystem = BlobStore::filesystem(dir.path()).expect("fs store");
        assert!(filesystem.object_attributes().is_empty());

        let attributes = signing_store().object_attributes();
        assert_eq!(
            attributes.get(&object_store::Attribute::CacheControl),
            Some(&object_store::AttributeValue::from(
                crate::AUDIO_CACHE_CONTROL
            )),
            "the same promise the proxied path makes"
        );
    }

    /// A filesystem backend has no signer, so there is nothing to cache and
    /// nothing to answer — the proxying path stays exactly as it was.
    #[tokio::test]
    async fn a_filesystem_store_never_signs() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = BlobStore::filesystem(dir.path()).expect("fs store");

        assert!(!store.is_presigning());
        assert!(store.presigned_get_url("ab/clip.wav").await.is_none());
    }

    /// `max_age_of` never returns more than the signature has left, whatever it
    /// is handed — including one that already expired, and a clock that stepped
    /// backwards under it.
    #[rstest]
    #[case(300, 240)]
    #[case(61, 1)]
    #[case(60, 0)]
    #[case(1, 0)]
    #[case(0, 0)]
    #[case(-1, 0)]
    #[case(-3600, 0)]
    fn max_age_never_outlives_the_signature(#[case] remaining_secs: i64, #[case] expected: u64) {
        let now = epoch();
        let expires_at = match remaining_secs >= 0 {
            true => now + Duration::from_secs(remaining_secs as u64),
            false => now - Duration::from_secs(remaining_secs.unsigned_abs()),
        };

        assert_eq!(max_age_of(expires_at, now), expected);
    }
}
