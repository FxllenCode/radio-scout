//! The call-metadata repository seam for the walking skeleton.
//!
//! `InMemoryCallRepository` is a placeholder metadata store behind the
//! `CallRepository` trait; ticket #3 built the real SeaORM data layer
//! (`crate::db`) and ticket #5 wires it into ingest here. Audio blobs live in
//! `crate::blob` (ADR-0002), not in this module.

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::call::{CallId, IngestedCall, StoredCall};

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
