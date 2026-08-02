//! Blob storage for call audio (ADR-0002): a single S3-compatible interface via
//! the `object_store` crate. The default is the local filesystem under
//! `base_dir`; S3-compatible stores (Garage first-class, MinIO/AWS too) are an
//! opt-in config flag, not an architecture fork. Audio never lives in the DB.
//!
//! Serving (`GET /api/call/:id/audio`) proxies with HTTP range by default; the
//! S3 backend can instead issue a short-lived presigned URL after an
//! access-scope check, so the app isn't an audio proxy at scale.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
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
use serde::{Deserialize, Serialize};
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

/// `[storage]` — where Call audio lives (US 39, ADR-0002), as an operator
/// writes it.
///
/// The section and the store's own configuration are two genuinely different
/// shapes rather than a mirror, which is why [`Storage::resolve`] survived #87's
/// merge while six other translation functions did not: the filesystem root
/// defaults to a directory under `[server] base_dir`, so a section cannot become
/// a [`StorageConfig`] without being told about another section.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Storage {
    pub backend: Backend,
    /// Filesystem root for audio. Unset means `<base_dir>/audio`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
    pub s3: S3Config,
}

/// The audio directory zero-config creates inside `base_dir`.
const DEFAULT_AUDIO_DIR: &str = "audio";

impl Storage {
    /// The store to open (ADR-0002). Zero-config is a directory under
    /// `base_dir`; `path` moves it without moving the database.
    pub fn resolve(&self, base_dir: &Path) -> StorageConfig {
        match self.backend {
            Backend::Filesystem => StorageConfig::Filesystem {
                root: match &self.path {
                    Some(path) => path.clone(),
                    None => base_dir.join(DEFAULT_AUDIO_DIR),
                },
            },
            Backend::S3 => StorageConfig::S3(self.s3.clone()),
        }
    }
}

/// Which audio backend to open.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
#[clap(rename_all = "lowercase")]
pub enum Backend {
    /// The zero-config default: a directory of files.
    #[default]
    Filesystem,
    /// An S3-compatible object store — Garage, MinIO, AWS.
    S3,
}

impl Backend {
    /// The spelling used in the file, the flag and the environment.
    fn as_str(self) -> &'static str {
        match self {
            Backend::Filesystem => "filesystem",
            Backend::S3 => "s3",
        }
    }
}

impl std::fmt::Display for Backend {
    /// Unquoted in a log line, so `storage=s3` greps (ADR-0011 rule 6).
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for Backend {
    type Err = ();

    /// One spelling per backend, shared by the file, the flag and the
    /// environment — so `RADIO_SCOUT_STORAGE_BACKEND=s3` and
    /// `backend = "s3"` cannot drift apart.
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        [Backend::Filesystem, Backend::S3]
            .into_iter()
            .find(|backend| backend.as_str() == text)
            .ok_or(())
    }
}

/// `[storage.s3]` — credentials for the S3-compatible backend (Garage / MinIO /
/// AWS), and what the store is opened from. One type since #87: what an
/// operator writes is what [`BlobStore::open`] reads.
///
/// `Debug` is hand-written, not derived — see the impl below.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct S3Config {
    pub bucket: String,
    pub region: String,
    /// Custom endpoint for self-hosted stores (Garage/MinIO); `None` for AWS.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
    pub access_key_id: String,
    pub secret_access_key: String,
    /// Allow plain HTTP (self-hosted Garage/MinIO on a LAN).
    pub allow_http: bool,
}

impl Default for S3Config {
    fn default() -> Self {
        S3Config {
            bucket: String::new(),
            // What Garage and MinIO answer to when they don't care, and AWS's
            // oldest region — a value that makes an unconfigured `region` a
            // non-event rather than a required field.
            region: "us-east-1".to_string(),
            endpoint: None,
            access_key_id: String::new(),
            secret_access_key: String::new(),
            allow_http: false,
        }
    }
}

/// Debug, minus the secret (ADR-0011 rule 2). [`StorageConfig`] derives `Debug`
/// and so does [`crate::config::Config`], so a single `?storage` in a boot
/// failure or an S3 incident would otherwise put the secret access key in a log
/// line. Nothing prints it today; the point is that the type stops permitting it
/// (#85).
///
/// The access key *id* stays: it identifies which credential is loaded and is
/// not itself a secret.
impl std::fmt::Debug for S3Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("S3Config")
            .field("bucket", &self.bucket)
            .field("region", &self.region)
            .field("endpoint", &self.endpoint)
            .field("access_key_id", &self.access_key_id)
            .field("secret_access_key", &"<redacted>")
            .field("allow_http", &self.allow_http)
            .finish()
    }
}

/// Which storage backend to use. Built from `[storage]` by
/// [`crate::config::Config::storage`] (#17).
///
/// `Debug` is derived deliberately: it delegates to [`S3Config`]'s redacting
/// impl, so the containing type is covered by construction rather than by a
/// second hand-written impl that could drift from it.
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
/// `webpush::Recipient` redacts an endpoint and [`S3Config`] redacts a key.
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

/// What Radio-Scout does with stored audio (#97).
///
/// The **port** an `AppState`, an `Instance` and every worker holds, and the
/// one [`BlobStore`] implements. It is deliberately Radio-Scout's own
/// vocabulary rather than `object_store`'s: bytes and keys, `None` for an
/// object that is not there, and no notion of a multipart upload, a listing
/// stream or a `GetOptions`.
///
/// **Why it exists.** Every arm that handles a store refusing something — the
/// enhancement worker settling a Call as `skipped`, serving telling "gone" from
/// "broken" — is unreachable while the only store is a filesystem that works.
/// Before this the seam was *under* the store: a decorator implementing seven
/// methods of `object_store`'s trait, which had to know that `serve::audio` stats
/// an object before it reads it (a `head` was never failed, only a `get`) — the
/// audio path's internal call order, written into the fault machinery, where a
/// rewrite of the handler would have silently stopped reaching the arm. Naming
/// the dependency here instead means a substitute answers *these* questions and
/// knows nothing about how they are asked.
#[async_trait::async_trait]
pub trait AudioStore: Send + Sync {
    /// Store `bytes` under `key`.
    async fn put(&self, key: &str, bytes: Bytes) -> Result<(), ObjectError>;

    /// The size in bytes of the object at `key`, or `None` if it's absent.
    async fn size(&self, key: &str) -> Result<Option<u64>, ObjectError>;

    /// Fetch the whole object, or `None` if absent.
    async fn get(&self, key: &str) -> Result<Option<Bytes>, ObjectError>;

    /// Fetch a byte range `[start, end)` of the object.
    async fn get_range(&self, key: &str, start: u64, end: u64) -> Result<Bytes, ObjectError>;

    /// Delete the object at `key` (idempotent-ish; missing is not an error).
    async fn delete(&self, key: &str) -> Result<(), ObjectError>;

    /// List every object with the metadata orphan-GC judges it by (#10).
    async fn list_objects(&self) -> Result<Vec<StoredObject>, ObjectError>;

    /// List every object key in the store.
    ///
    /// Provided, not required: it is [`AudioStore::list_objects`] with the
    /// metadata dropped, so an implementer that had to write it out could only
    /// get it wrong.
    async fn list_keys(&self) -> Result<Vec<String>, ObjectError> {
        Ok(self
            .list_objects()
            .await?
            .into_iter()
            .map(|object| object.key)
            .collect())
    }

    /// Whether this backend serves via presigned URLs (S3) rather than proxying.
    fn is_presigning(&self) -> bool;

    /// A short-lived presigned GET URL for `key` — `None` on non-S3 backends.
    async fn presigned_get_url(&self, key: &str) -> Option<Result<PresignedUrl, ObjectError>>;
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

    /// The attributes a stored object carries.
    ///
    /// Empty on the filesystem backend, and not merely as an optimisation:
    /// `object_store` specifies that a backend which cannot honour an attribute
    /// returns an error, and `LocalFileSystem` cannot store one. It needs none
    /// either — nothing fetches a local object directly, so the caching promise
    /// is made by `serve::audio`'s own response header.
    fn object_attributes(&self) -> object_store::Attributes {
        match self.is_presigning() {
            true => object_store::Attributes::from_iter([(
                object_store::Attribute::CacheControl,
                object_store::AttributeValue::from(crate::serve::AUDIO_CACHE_CONTROL),
            )]),
            false => object_store::Attributes::new(),
        }
    }

    /// [`AudioStore::presigned_get_url`] against an explicit clock.
    ///
    /// The clock is a parameter because every decision here is about elapsed
    /// time — reuse, re-mint, prune — and the window is five minutes wide. Given
    /// `now`, this is the whole of that logic and a test can walk it to the
    /// boundary and past it; the trait method supplies the real clock and
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
}

#[async_trait::async_trait]
impl AudioStore for BlobStore {
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
    async fn put(&self, key: &str, bytes: Bytes) -> Result<(), ObjectError> {
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

    fn is_presigning(&self) -> bool {
        self.signer.is_some()
    }

    /// The size in bytes of the object at `key`, or `None` if it's absent.
    async fn size(&self, key: &str) -> Result<Option<u64>, ObjectError> {
        match self.store.head(&ObjectPath::from(key)).await {
            Ok(meta) => Ok(Some(meta.size)),
            Err(ObjectError::NotFound { .. }) => Ok(None),
            Err(err) => Err(err),
        }
    }

    /// Fetch the whole object, or `None` if absent.
    async fn get(&self, key: &str) -> Result<Option<Bytes>, ObjectError> {
        match self.store.get(&ObjectPath::from(key)).await {
            Ok(result) => Ok(Some(result.bytes().await?)),
            Err(ObjectError::NotFound { .. }) => Ok(None),
            Err(err) => Err(err),
        }
    }

    /// Fetch a byte range `[start, end)` of the object.
    async fn get_range(&self, key: &str, start: u64, end: u64) -> Result<Bytes, ObjectError> {
        self.store
            .get_range(&ObjectPath::from(key), start..end)
            .await
    }

    /// Delete the object at `key` (idempotent-ish; missing is not an error).
    async fn delete(&self, key: &str) -> Result<(), ObjectError> {
        match self.store.delete(&ObjectPath::from(key)).await {
            Ok(()) | Err(ObjectError::NotFound { .. }) => Ok(()),
            Err(err) => Err(err),
        }
    }

    /// List every object with the metadata orphan-GC judges it by (#10).
    async fn list_objects(&self) -> Result<Vec<StoredObject>, ObjectError> {
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
    async fn presigned_get_url(&self, key: &str) -> Option<Result<PresignedUrl, ObjectError>> {
        self.presigned_get_url_at(key, std::time::SystemTime::now())
            .await
    }
}

impl BlobStore {
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
    store: &dyn AudioStore,
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

    /// **A filesystem write must ask for nothing**, and that is not an
    /// optimisation: `object_store` specifies that a backend which cannot
    /// honour an attribute returns an error, and `LocalFileSystem` cannot store
    /// one — so a store that asked would fail every write on a Pi.
    ///
    /// The S3 half of this decision is asserted where it can be seen for real:
    /// `tests/blob.rs` writes through the S3 backend to a stub that records the
    /// headers, and `tests/s3.rs` reads it back off a store that answers.
    #[test]
    fn a_filesystem_write_asks_for_nothing_a_filesystem_would_refuse() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let filesystem = BlobStore::filesystem(tmp.path()).expect("fs store");

        assert!(filesystem.object_attributes().is_empty());
    }

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

    /// The other credential this module holds, and the one an incident would
    /// reach for: the S3 secret access key (#85). Asserted through
    /// [`StorageConfig`] rather than [`S3Config`] because that is the type a
    /// boot failure or an `?storage` would actually print — so this proves the
    /// enum's *derived* `Debug` delegates to the redacting one, which is the
    /// whole reason the containing type is left derived.
    #[test]
    fn debug_never_carries_the_s3_secret() {
        let config = StorageConfig::S3(S3Config {
            bucket: "radio-scout".into(),
            region: "us-east-1".into(),
            endpoint: Some("http://127.0.0.1:9000".into()),
            access_key_id: "GK1234".into(),
            secret_access_key: "s3cr3t-do-not-print".into(),
            allow_http: true,
        });

        let shown = format!("{config:?}");

        assert!(
            !shown.contains("s3cr3t-do-not-print"),
            "the secret is redacted: {shown}"
        );
        // The access key *id* stays: it identifies which credential is loaded,
        // is not itself a secret, and is what an operator debugging a 403 needs.
        assert!(
            shown.contains("GK1234"),
            "still says which credential: {shown}"
        );
        // The rest of the configuration is untouched — this redacts one field,
        // it does not blind the type.
        assert!(shown.contains("radio-scout"), "{shown}");
        assert!(shown.contains("127.0.0.1:9000"), "{shown}");
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
    /// the store answers the client directly there, so a header `serve::audio`
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
                crate::serve::AUDIO_CACHE_CONTROL
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
