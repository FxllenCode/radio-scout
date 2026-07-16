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
use object_store::{Error as ObjectError, ObjectStore, ObjectStoreExt, PutPayload};

/// How long a presigned URL stays valid.
const PRESIGN_TTL: Duration = Duration::from_secs(300);

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

/// Which storage backend to use. Ticket #17 populates this from TOML/CLI.
#[derive(Debug, Clone)]
pub enum StorageConfig {
    Filesystem { root: PathBuf },
    S3(S3Config),
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
            .with_allow_http(cfg.allow_http);
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
        let metas = self.store.list(None).try_collect::<Vec<_>>().await?;
        Ok(metas.into_iter().map(|m| m.location.to_string()).collect())
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

/// Orphan-GC (ADR-0002): delete every stored object whose key isn't in
/// `referenced_keys`. Returns the keys that were deleted. Ticket #10 sources the
/// referenced set from the DB and runs this on a schedule.
pub async fn orphan_gc(
    store: &BlobStore,
    referenced_keys: &HashSet<String>,
) -> Result<Vec<String>, ObjectError> {
    let mut deleted = Vec::new();
    for key in store.list_keys().await? {
        if !referenced_keys.contains(&key) {
            store.delete(&key).await?;
            deleted.push(key);
        }
    }
    Ok(deleted)
}
