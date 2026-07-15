//! Storage seams for the walking skeleton: an audio object store and a call
//! metadata repository, each behind a trait so the real implementations slot in
//! without touching the ingest pipeline.
//!
//! - `FilesystemAudioStore` is the default blob store (ADR-0002). Ticket #4
//!   replaces it with the `object_store` abstraction (fs default / S3-Garage)
//!   plus HTTP range serving and presigned URLs.
//! - `InMemoryCallRepository` is a placeholder metadata store. Ticket #3
//!   replaces it with SeaORM entities + migrations over SQLite/Postgres.

use std::collections::HashMap;
use std::io;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::call::{CallId, IngestedCall, StoredCall};

/// A content-addressed-ish blob store for call audio. Keys are opaque strings
/// generated at ingest time (audio is written before the metadata row exists,
/// per ADR-0001), so the key must not depend on the internal Id.
pub trait AudioStore: Send + Sync {
    /// Store `bytes` under `key`, creating any needed structure.
    fn put(&self, key: &str, bytes: &[u8]) -> io::Result<()>;
    /// Fetch the bytes stored under `key`, or `None` if absent.
    fn get(&self, key: &str) -> io::Result<Option<Vec<u8>>>;
}

/// Filesystem-backed audio store, sharded by the first two characters of the
/// key to keep directories from growing unbounded (`<root>/<ab>/<key>`).
pub struct FilesystemAudioStore {
    root: PathBuf,
}

impl FilesystemAudioStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        FilesystemAudioStore { root: root.into() }
    }

    fn path_for(&self, key: &str) -> PathBuf {
        let shard = if key.len() >= 2 { &key[0..2] } else { "__" };
        self.root.join(shard).join(key)
    }
}

impl AudioStore for FilesystemAudioStore {
    fn put(&self, key: &str, bytes: &[u8]) -> io::Result<()> {
        let path = self.path_for(key);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, bytes)
    }

    fn get(&self, key: &str) -> io::Result<Option<Vec<u8>>> {
        match std::fs::read(self.path_for(key)) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }
}

/// Persists call metadata and assigns each call its internal Id.
pub trait CallRepository: Send + Sync {
    /// Insert a call (whose audio already lives at `object_key`), assigning it a
    /// fresh Id and returning the stored row.
    fn insert(&self, ingested: &IngestedCall, object_key: String) -> StoredCall;
    /// Fetch a stored call by its internal Id.
    fn get(&self, id: CallId) -> Option<StoredCall>;
    /// Number of calls stored (used in tests/diagnostics).
    fn count(&self) -> usize;
}

/// In-memory metadata store. Ids start at 1 and increase monotonically.
pub struct InMemoryCallRepository {
    next_id: AtomicU64,
    calls: Mutex<HashMap<CallId, StoredCall>>,
}

impl InMemoryCallRepository {
    pub fn new() -> Self {
        InMemoryCallRepository {
            next_id: AtomicU64::new(1),
            calls: Mutex::new(HashMap::new()),
        }
    }
}

impl Default for InMemoryCallRepository {
    fn default() -> Self {
        Self::new()
    }
}

impl CallRepository for InMemoryCallRepository {
    fn insert(&self, ingested: &IngestedCall, object_key: String) -> StoredCall {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let call = StoredCall::from_ingested(id, ingested, object_key);
        self.calls
            .lock()
            .expect("call repo poisoned")
            .insert(id, call.clone());
        call
    }

    fn get(&self, id: CallId) -> Option<StoredCall> {
        self.calls
            .lock()
            .expect("call repo poisoned")
            .get(&id)
            .cloned()
    }

    fn count(&self) -> usize {
        self.calls.lock().expect("call repo poisoned").len()
    }
}
