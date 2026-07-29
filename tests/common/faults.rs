//! Fault injection for the blob store (ticket #37).
//!
//! Radio-Scout's workers are written to survive I/O that fails: the enhancement
//! worker settles a Call as `skipped` and takes the next one, serving tells
//! "gone" from "broken". Those arms shipped **untested**, because the only store
//! the suite had was a temp filesystem that works, and the only way to break one
//! — pointing at an S3 endpoint nothing listens on — fails the *first* operation
//! and so can never reach the second.
//!
//! [`Faults`] is a handle onto a store that does what it is told:
//!
//! ```ignore
//! let (store, faults) = common::faulty_store(tmp.path());
//! let app = TestApp::builder().store(store).enhancement(config).spawn().await;
//!
//! faults.fail_puts();          // the worker's `store-audio` arm
//! ```
//!
//! ## Stalling is the other half, and the more important one
//!
//! Several arms worth reaching are not "an operation failed" but "a failure
//! landed *between* two statements" — a table dropped after the Call behind it
//! was queued, an object pruned after the stat that sized it. Provoking those by
//! racing a sleep against a background worker trades uncovered lines for flaky
//! ones, which is a worse deal and is why [#20] left them alone.
//!
//! So a fault can also **park** an operation: [`Faults::stall_reads`] holds the
//! caller inside its `get`, [`Faults::stalled`] resolves once it is provably
//! parked there, and [`Faults::release`] lets it go. Between the second and the
//! third the test owns the world, and can drop a table or prune an object
//! knowing exactly which statement has run and which has not. Nothing sleeps and
//! nothing races.
//!
//! Its companion for failures the *database* has to stage is
//! [`crate::common::TestApp::fail_writes_to`].
//!
//! [#20]: https://github.com/FxllenCode/radio-scout/issues/20

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use futures_util::stream::BoxStream;
use object_store::path::Path as ObjectPath;
use object_store::{
    CopyOptions, Error as ObjectError, GetOptions, GetResult, ListResult, MultipartUpload,
    ObjectMeta, ObjectStore, PutMultipartOptions, PutOptions, PutPayload, PutResult, Result,
};
use radio_scout::BlobStore;
use tokio::sync::watch;

/// The `store` field of every error this raises, so a log line or an assertion
/// can tell an injected store failure from a real one.
///
/// Its opposite number for the database seam is
/// [`crate::common::INJECTED_WRITE`], which is a whole message rather than a
/// tag because a trigger has nowhere else to put one.
pub const INJECTED_IO: &str = "injected";

/// What the store has been told to do to each kind of operation.
///
/// The two `watch` channels rather than a `Notify` apiece are deliberate: a
/// `Notify` registers interest when its future is first *polled*, not when it is
/// created, so a wake-up that lands in the window between deciding to park and
/// being polled is lost and the parked read never returns. A test harness that
/// hangs one run in fifty is worse than the uncovered lines it was built to
/// reach. `watch::Sender::subscribe` takes effect immediately, and
/// `borrow_and_update` marks the point the waiter is asking *from*, so both
/// handshakes below are settled before either side can miss the other.
struct Script {
    fail_puts: AtomicBool,
    fail_reads: AtomicBool,
    stall_reads: AtomicBool,
    /// How many reads have parked since stalling was last armed — what
    /// [`Faults::stalled`] waits on. Only ever counts up within a round, so a
    /// test that asks after the fact still gets a straight answer;
    /// [`Faults::stall_reads`] is what starts a new round.
    parked: watch::Sender<usize>,
    /// Bumped by [`Faults::release`]; every parked read is waiting on a change.
    released: watch::Sender<u64>,
    /// The [`PutOptions`] of every write that reached the decorator, newest last
    /// — recorded *before* any injected failure, so what a caller asked for is
    /// observable even when the write is made to fail.
    ///
    /// This is the only way to see what `BlobStore::put` asks a store to record
    /// against an object without a store that answers (#31): the `Cache-Control`
    /// an S3-backed store stamps is invisible to a filesystem round trip, and
    /// `tests/s3.rs` — which reads it back off a real store — skips wherever one
    /// is not running, which is the everyday loop.
    puts: std::sync::Mutex<Vec<PutOptions>>,
}

impl Default for Script {
    fn default() -> Self {
        Self {
            fail_puts: AtomicBool::new(false),
            fail_reads: AtomicBool::new(false),
            stall_reads: AtomicBool::new(false),
            parked: watch::Sender::new(0),
            released: watch::Sender::new(0),
            puts: std::sync::Mutex::new(Vec::new()),
        }
    }
}

impl Script {
    /// Fail here if `switch` is thrown, naming `operation` so the test failure
    /// (or the operator-facing log line under test) says which one it was.
    fn check(&self, switch: &AtomicBool, operation: &'static str) -> Result<()> {
        if switch.load(Ordering::SeqCst) {
            return Err(ObjectError::Generic {
                store: INJECTED_IO,
                source: format!("injected {operation} failure").into(),
            });
        }
        Ok(())
    }

    /// Park until released, if reads are being stalled.
    async fn maybe_stall(&self) {
        if !self.stall_reads.load(Ordering::SeqCst) {
            return;
        }
        let mut released = self.released.subscribe();
        released.mark_unchanged();
        // Re-read *after* subscribing. `release` lowers this flag before it
        // bumps the channel, so seeing it still raised here proves the bump has
        // not happened yet — and the subscription above is already in place to
        // catch it when it does. Without this the read could park immediately
        // after a release it was too late to see, and stay parked.
        if !self.stall_reads.load(Ordering::SeqCst) {
            return;
        }
        self.parked.send_modify(|parked| *parked += 1);
        let _ = released.changed().await;
    }
}

/// A handle onto a store that can be made to fail, or to hold still. Cheap to
/// clone; every clone drives the same store.
#[derive(Clone)]
pub struct Faults(Arc<Script>);

impl Faults {
    /// Wrap `store` so this handle governs it. Until told otherwise it behaves
    /// exactly like the store it wraps. Reached through [`faulty_store`].
    fn wrap(store: BlobStore) -> (BlobStore, Faults) {
        let script = Arc::new(Script::default());
        let faults = Faults(script.clone());
        (
            store.decorated(|inner| Arc::new(FaultyStore { inner, script })),
            faults,
        )
    }

    /// Refuse every write from now on.
    pub fn fail_puts(&self) {
        self.0.fail_puts.store(true, Ordering::SeqCst);
    }

    /// Wrap an arbitrary store, for the case where what is under test is not a
    /// filesystem one — an **S3-backed** store, whose `put` stamps attributes a
    /// filesystem store never would (#31).
    pub fn wrapping(store: BlobStore) -> (BlobStore, Faults) {
        Self::wrap(store)
    }

    /// The [`PutOptions`] every write has carried, newest last.
    pub fn puts(&self) -> Vec<PutOptions> {
        self.0.puts.lock().expect("recorded puts").clone()
    }

    /// Refuse every read of an object's *bytes* from now on.
    pub fn fail_reads(&self) {
        self.0.fail_reads.store(true, Ordering::SeqCst);
    }

    /// Park every read of an object's bytes until [`Faults::release`]. The
    /// caller that issued it stays inside its `get`, which is what makes
    /// "between two statements" a place a test can stand.
    ///
    /// Arming resets the parked count, so a test that stalls, releases and
    /// stalls again is asking about *this* round. Without that,
    /// [`Faults::stalled`] would be satisfied by the previous round's reads and
    /// return with nothing actually parked — a silent race rather than a hang,
    /// which is the worse of the two.
    pub fn stall_reads(&self) {
        self.0.parked.send_modify(|parked| *parked = 0);
        self.0.stall_reads.store(true, Ordering::SeqCst);
    }

    /// Resolve once at least `count` reads have parked. Returns immediately if
    /// that many already have, so a test cannot lose the race by arriving late.
    pub async fn stalled(&self, count: usize) {
        let mut parked = self.0.parked.subscribe();
        while *parked.borrow_and_update() < count {
            parked
                .changed()
                .await
                .expect("the store outlives every test that stalls it");
        }
    }

    /// Let every parked read through, and stop parking new ones.
    ///
    /// The flag comes down *before* the channel goes up, which is the half of
    /// the handshake [`Script::maybe_stall`] leans on.
    pub fn release(&self) {
        self.0.stall_reads.store(false, Ordering::SeqCst);
        self.0.released.send_modify(|released| *released += 1);
    }
}

/// The decorator itself: delegates to `inner` unless [`Script`] says otherwise.
#[derive(Debug)]
struct FaultyStore {
    inner: Arc<dyn ObjectStore>,
    script: Arc<Script>,
}

impl std::fmt::Display for FaultyStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Faulty({})", self.inner)
    }
}

impl std::fmt::Debug for Script {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Script")
    }
}

#[async_trait::async_trait]
impl ObjectStore for FaultyStore {
    async fn put_opts(
        &self,
        location: &ObjectPath,
        payload: PutPayload,
        opts: PutOptions,
    ) -> Result<PutResult> {
        // Recorded before the failure check, so a test can arm `fail_puts` to
        // stop the write reaching a backend it cannot talk to and still assert
        // on what the write asked for.
        self.script
            .puts
            .lock()
            .expect("recorded puts")
            .push(opts.clone());
        self.script.check(&self.script.fail_puts, "put")?;
        self.inner.put_opts(location, payload, opts).await
    }

    async fn put_multipart_opts(
        &self,
        location: &ObjectPath,
        opts: PutMultipartOptions,
    ) -> Result<Box<dyn MultipartUpload>> {
        self.script.check(&self.script.fail_puts, "put")?;
        self.inner.put_multipart_opts(location, opts).await
    }

    /// **A `head` is never failed or parked, only a read of the bytes.**
    ///
    /// Not fussiness: `serve_audio` stats an object before it reads it, so a
    /// store that refuses everything fails at the stat and the read arms behind
    /// it stay exactly as unreachable as they were — which is the same "the
    /// first operation is the one that fails" problem an unreachable endpoint
    /// has, and the reason this type exists. `head` and `get` share one backend
    /// call, so the split has to be made here.
    async fn get_opts(&self, location: &ObjectPath, options: GetOptions) -> Result<GetResult> {
        if !options.head {
            self.script.maybe_stall().await;
            self.script.check(&self.script.fail_reads, "read")?;
        }
        self.inner.get_opts(location, options).await
    }

    // Below here: plain passthroughs. Deliberately not switchable *yet* — no
    // test needs a delete or a listing to fail (orphan-GC's delete arm is
    // already covered by a read-only directory in `src/retention.rs`), and a
    // fault nothing arms is a fault nothing proves. Adding one is the four
    // lines `put_opts` spends.
    fn delete_stream(
        &self,
        locations: BoxStream<'static, Result<ObjectPath>>,
    ) -> BoxStream<'static, Result<ObjectPath>> {
        self.inner.delete_stream(locations)
    }

    fn list(&self, prefix: Option<&ObjectPath>) -> BoxStream<'static, Result<ObjectMeta>> {
        self.inner.list(prefix)
    }

    async fn list_with_delimiter(&self, prefix: Option<&ObjectPath>) -> Result<ListResult> {
        self.inner.list_with_delimiter(prefix).await
    }

    async fn copy_opts(
        &self,
        from: &ObjectPath,
        to: &ObjectPath,
        options: CopyOptions,
    ) -> Result<()> {
        self.inner.copy_opts(from, to, options).await
    }
}

/// A real filesystem store under `dir`, with a [`Faults`] handle onto it — the
/// one line most fault-injecting tests need.
pub fn faulty_store(dir: &std::path::Path) -> (BlobStore, Faults) {
    Faults::wrap(BlobStore::filesystem(dir.join("audio")).expect("a filesystem store"))
}
