//! Making one I/O call fail, by naming it (#37, reshaped by #97).
//!
//! Radio-Scout's workers and handlers are written to survive I/O that fails:
//! the enhancement worker settles a Call as `skipped` and takes the next one,
//! serving tells "gone" from "broken", a 5xx keeps its cause on the server. None
//! of those arms is reachable while the only store is a filesystem that works
//! and the only database is one that answers.
//!
//! Both seams here are **substitutes at an interface Radio-Scout owns**, put in
//! place through `instance::Wiring` exactly as an S3 store or a frozen clock is:
//!
//! - [`Refusing`] answers statements for a [`Db`], refusing any that names a
//!   table it has been told to refuse. It is what replaced `DROP TABLE` plus a
//!   trigger written twice in two dialects' procedural SQL — which could only
//!   fail the *first* statement touching a table, and could only be recognised
//!   by matching each driver's own wording.
//! - [`FaultyStore`] answers for an [`AudioStore`], delegating to a real one
//!   until told to fail. It replaced a decorator over `object_store`'s own
//!   trait, which had to know that `serve::audio` stats an object before it
//!   reads it — the audio path's internal call order, written into the fault
//!   machinery, where a rewrite of the handler would have silently stopped
//!   reaching the arm.
//!
//! Nothing here parks a call to stage "a failure between two statements". It
//! does not need to: an interface can simply *say* that the stat found an object
//! and the read did not ([`Faults::hide_reads`]), where a decorator under the
//! store could only race one real answer against another.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use bytes::Bytes;
use object_store::Error as ObjectError;
use radio_scout::blob::{AudioStore, PresignedUrl, StoredObject};
use radio_scout::db::{Connection, Db, Transaction, Txn};
use radio_scout::instance::Decorate;
use sea_orm::{ConnectionTrait, DbBackend, DbErr, ExecResult, QueryResult, Statement};

/// What a refused statement says, on either dialect.
///
/// Ours rather than a driver's, which is the point: a test asserting that the
/// cause reached the operator's log used to have to know two phrasings for
/// "no such table" and got no warning when it was asserting on the one this run
/// was not using.
pub const REFUSED: &str = "statement refused by the test harness";

/// The `store` field of every error [`FaultyStore`] raises, so a log line or an
/// assertion can tell an injected store failure from a real one.
pub const INJECTED_IO: &str = "injected";

// ---------------------------------------------------------------------------
// The statement seam
// ---------------------------------------------------------------------------

/// Which statements are being refused. Shared by a [`Db`] and every transaction
/// begun on it, so a rule armed after the app started reaches both.
#[derive(Default)]
struct Refusals(std::sync::Mutex<Vec<Rule>>);

/// One table, and how much of what touches it is refused.
struct Rule {
    /// As it appears in the SQL: quoted, which sea-orm writes identically on
    /// both dialects — so a rule is written once and matches on either. The
    /// rest of a statement is *not* identical (Postgres binds `$1` where SQLite
    /// binds `?`), which is why what a rule matches is an identifier and never
    /// a whole statement.
    table: String,
    updates_only: bool,
}

impl Refusals {
    /// The refusal `sql` earns, or `Ok(())` to let it through.
    fn judge(&self, sql: &str) -> Result<(), DbErr> {
        let refused = self
            .0
            .lock()
            .expect("the refusal list")
            .iter()
            .any(|rule| sql.contains(&rule.table) && (!rule.updates_only || is_an_update(sql)));
        match refused {
            true => Err(DbErr::Custom(REFUSED.to_string())),
            false => Ok(()),
        }
    }
}

/// Whether this statement changes rows that are already there.
///
/// **Updates and not inserts**, deliberately: the arms worth reaching on the
/// write side all sit *after* both a read and an insert of the same table have
/// succeeded — the enhancement worker updates a Call row it has just read,
/// ingest marks a Call pending after inserting it — so a test has to be able to
/// arrange rows through the app's own front door and only then take the update
/// away. It is the one thing a dropped table could never stage, and it is why
/// #20 shipped those arms uncovered.
fn is_an_update(sql: &str) -> bool {
    sql.trim_start().starts_with("UPDATE")
}

/// A handle onto the statements an app issues. Cheap to clone; every clone
/// governs the same handle.
#[derive(Clone, Default)]
pub struct Statements(Arc<Refusals>);

impl Statements {
    /// Refuse every statement naming `table`, from now on.
    pub fn refuse(&self, table: &str) {
        self.add(table, false);
    }

    /// Refuse every statement naming `table` that updates a row, leaving reads
    /// and inserts — the arrangement an update arm needs — alone.
    pub fn refuse_updates(&self, table: &str) {
        self.add(table, true);
    }

    fn add(&self, table: &str, updates_only: bool) {
        self.0.0.lock().expect("the refusal list").push(Rule {
            // Quoted the way sea-orm writes an identifier, so `calls` cannot
            // match a column called `calls_id` or a table called `call_patches`.
            table: format!("\"{table}\""),
            updates_only,
        });
    }
}

/// A database handle that answers through `inner` unless [`Statements`] says
/// otherwise.
struct Refusing<C> {
    inner: C,
    refusals: Arc<Refusals>,
}

/// Something to compose around an Instance's database handle, and the handle
/// onto what it will refuse — the one line a test that fails a statement needs.
///
/// A decorator rather than a finished `Db`, so the Instance still opens and
/// migrates its own database exactly as a boot does; this only wraps the
/// result, and wraps it again after a restart.
pub fn refusals() -> (Decorate, Statements) {
    let refusals = Arc::new(Refusals::default());
    let composed = refusals.clone();
    (
        Arc::new(move |db| {
            Db::new(Refusing {
                inner: db,
                refusals: composed.clone(),
            })
        }),
        Statements(refusals),
    )
}

#[async_trait::async_trait]
impl<C: ConnectionTrait + Send + Sync> ConnectionTrait for Refusing<C> {
    fn get_database_backend(&self) -> DbBackend {
        self.inner.get_database_backend()
    }

    async fn execute(&self, statement: Statement) -> Result<ExecResult, DbErr> {
        self.refusals.judge(&statement.sql)?;
        self.inner.execute(statement).await
    }

    async fn execute_unprepared(&self, sql: &str) -> Result<ExecResult, DbErr> {
        self.refusals.judge(sql)?;
        self.inner.execute_unprepared(sql).await
    }

    async fn query_one(&self, statement: Statement) -> Result<Option<QueryResult>, DbErr> {
        self.refusals.judge(&statement.sql)?;
        self.inner.query_one(statement).await
    }

    async fn query_all(&self, statement: Statement) -> Result<Vec<QueryResult>, DbErr> {
        self.refusals.judge(&statement.sql)?;
        self.inner.query_all(statement).await
    }

    fn support_returning(&self) -> bool {
        self.inner.support_returning()
    }
}

#[async_trait::async_trait]
impl Connection for Refusing<Db> {
    /// A transaction begun here is composed too, sharing the same rules — so a
    /// statement inside ingest's insert transaction is as refusable as one
    /// outside it.
    async fn begin(&self) -> Result<Txn, DbErr> {
        Ok(Txn::new(Refusing {
            inner: self.inner.begin().await?,
            refusals: self.refusals.clone(),
        }))
    }

    async fn close(&self) -> Result<(), DbErr> {
        self.inner.close().await
    }
}

#[async_trait::async_trait]
impl Transaction for Refusing<Txn> {
    async fn commit(self: Box<Self>) -> Result<(), DbErr> {
        self.inner.commit().await
    }

    async fn rollback(self: Box<Self>) -> Result<(), DbErr> {
        self.inner.rollback().await
    }
}

// ---------------------------------------------------------------------------
// The store seam
// ---------------------------------------------------------------------------

/// What the store has been told to do.
#[derive(Default)]
struct Script {
    fail_puts: AtomicBool,
    fail_presigning: AtomicBool,
    /// What a read of an object's bytes does. One value rather than a flag
    /// each, because "broken" and "gone" are alternatives: a store cannot both
    /// refuse a read and answer it with nothing, and two flags would make the
    /// answer depend on which was armed first.
    reads: std::sync::Mutex<Reads>,
}

/// How a read of an object's bytes answers.
#[derive(Default, Clone, Copy, PartialEq, Eq)]
enum Reads {
    /// The store hands over what it has.
    #[default]
    Working,
    /// The store refuses — a node shedding load, a disk throwing errors. A 500.
    Failing,
    /// The store says there is no such object — pruned since it was stat'd. A
    /// 404.
    Hidden,
}

/// A handle onto a store that can be made to fail. Cheap to clone; every clone
/// drives the same store.
#[derive(Clone, Default)]
pub struct Faults(Arc<Script>);

impl Faults {
    /// Refuse every write from now on — a disk that filled up, a Garage node
    /// refusing writes.
    pub fn fail_puts(&self) {
        self.0.fail_puts.store(true, Ordering::SeqCst);
    }

    /// Refuse to sign a URL from now on — a clock too far out for SigV4, or
    /// credentials revoked under a running process.
    ///
    /// Only an S3-shaped store ever signs, so this is the one fault that needs
    /// [`faults_over_store`] with an S3 store under it rather than the
    /// filesystem one [`faulty_store`] provides.
    pub fn fail_presigning(&self) {
        self.0.fail_presigning.store(true, Ordering::SeqCst);
    }

    /// Refuse every read of an object's bytes from now on, while still
    /// answering for its size — a store that says it has the object and then
    /// will not hand it over.
    pub fn fail_reads(&self) {
        self.set_reads(Reads::Failing);
    }

    /// Answer every read with "no such object", while still answering for its
    /// size.
    ///
    /// The object pruned between the stat that sized it and the read that would
    /// have served it. Retention and orphan-GC both run while listeners are
    /// listening, so the window is ordinary — and stating it at the interface
    /// is the whole gain: under the store it could only be staged by parking a
    /// real read and pruning a real object while it was held.
    pub fn hide_reads(&self) {
        self.set_reads(Reads::Hidden);
    }

    fn set_reads(&self, reads: Reads) {
        *self.0.reads.lock().expect("the read mode") = reads;
    }

    fn reads(&self) -> Reads {
        *self.0.reads.lock().expect("the read mode")
    }

    fn presigning_fails(&self) -> bool {
        self.0.fail_presigning.load(Ordering::SeqCst)
    }

    fn check_puts(&self) -> Result<(), ObjectError> {
        match self.0.fail_puts.load(Ordering::SeqCst) {
            true => Err(refused("put")),
            false => Ok(()),
        }
    }
}

/// The error an injected store failure raises, tagged so a log line or an
/// assertion can tell it from a real one.
fn refused(operation: &'static str) -> ObjectError {
    ObjectError::Generic {
        store: INJECTED_IO,
        source: format!("injected {operation} failure").into(),
    }
}

/// A real store, answering as it is told to.
///
/// Delegation rather than reimplementation: what a test wants is a store that
/// behaves exactly like the real one except in the one respect under test, and
/// a hand-written fake would be a second implementation of object storage to
/// keep true.
pub struct FaultyStore {
    inner: Box<dyn AudioStore>,
    faults: Faults,
}

#[async_trait::async_trait]
impl AudioStore for FaultyStore {
    async fn put(&self, key: &str, bytes: Bytes) -> Result<(), ObjectError> {
        self.faults.check_puts()?;
        self.inner.put(key, bytes).await
    }

    /// **Not switchable, and for a different reason than it used to be.** The
    /// decorator this replaced could not fail a stat because `head` and `get`
    /// were one backend call and failing both would have made the read arms
    /// unreachable — the handler's call order, encoded here. Nothing forces
    /// that now: `size` is simply an operation no test has needed to fail,
    /// because `serve::audio`'s `stat-audio` arm is reached by a store that is
    /// genuinely broken (`tests/instrumentation.rs` puts a file where the audio
    /// directory should be). Adding a switch is the four lines `put` spends.
    async fn size(&self, key: &str) -> Result<Option<u64>, ObjectError> {
        self.inner.size(key).await
    }

    async fn get(&self, key: &str) -> Result<Option<Bytes>, ObjectError> {
        match self.faults.reads() {
            Reads::Working => self.inner.get(key).await,
            Reads::Failing => Err(refused("read")),
            Reads::Hidden => Ok(None),
        }
    }

    /// A ranged read has no "gone" to report — there is no `Option` in its
    /// answer, because it is only ever issued for an object something has
    /// already sized. A hidden object therefore reads as a refusal here, which
    /// is what the store itself would say.
    async fn get_range(&self, key: &str, start: u64, end: u64) -> Result<Bytes, ObjectError> {
        match self.faults.reads() {
            Reads::Working => self.inner.get_range(key, start, end).await,
            _ => Err(refused("read")),
        }
    }

    async fn delete(&self, key: &str) -> Result<(), ObjectError> {
        self.inner.delete(key).await
    }

    async fn list_objects(&self) -> Result<Vec<StoredObject>, ObjectError> {
        self.inner.list_objects().await
    }

    /// Not switchable: whether a store presigns at all is what backend it *is*
    /// (ADR-0002), not something that fails. Failing the signing itself is
    /// [`Faults::fail_presigning`], below.
    fn is_presigning(&self) -> bool {
        self.inner.is_presigning()
    }

    async fn presigned_get_url(&self, key: &str) -> Option<Result<PresignedUrl, ObjectError>> {
        match self.faults.presigning_fails() {
            true => Some(Err(refused("presign"))),
            false => self.inner.presigned_get_url(key).await,
        }
    }
}

/// Any store, with a [`Faults`] handle onto it — for the faults only some
/// backends can have, [`Faults::fail_presigning`] being the one.
pub fn faults_over_store(store: impl AudioStore + 'static) -> (FaultyStore, Faults) {
    let faults = Faults::default();
    (
        FaultyStore {
            inner: Box::new(store),
            faults: faults.clone(),
        },
        faults,
    )
}

/// A real filesystem store under `dir`, with a [`Faults`] handle onto it — the
/// one line most fault-injecting tests need.
pub fn faulty_store(dir: &std::path::Path) -> (FaultyStore, Faults) {
    faults_over_store(
        radio_scout::BlobStore::filesystem(dir.join("audio")).expect("a filesystem store"),
    )
}
