//! The handle every statement goes through (#97).
//!
//! [`Db`] is what an `AppState`, an `Instance` and every worker hold instead of
//! a `DatabaseConnection`. It is composed rather than constructed: whatever is
//! put behind it answers statements, so a decorator can sit between the
//! application and the driver — the move [`crate::blob::AudioStore`] makes for
//! the object store, one level up.
//!
//! **Why the seam is here and not below it.** Reaching a handler's 5xx arm, or
//! a worker's, means making one statement fail. Before this the only way was
//! `DROP TABLE` plus a trigger written twice in two dialects' procedural SQL,
//! which could only fail the *first* statement to touch a table and could only
//! be recognised by matching each driver's own wording for "no such table" —
//! two strings that are not ours and can change under us. Composing here, a
//! test names the statement it wants to fail and gets back a refusal it wrote
//! itself — and it names it by the **quoted table name**, which sea-orm writes
//! identically on both dialects, so one rule matches on either. The repository
//! functions were already generic over [`ConnectionTrait`], so nothing below
//! had to move.
//!
//! **Transactions are inside the seam, not beside it.** Ingest writes a Call's
//! frequency roster inside its insert transaction, and that statement is one an
//! Operator's 5xx stage table names — so [`Db::begin`] hands back a [`Txn`]
//! that is composed the same way, rather than sea-orm's `DatabaseTransaction`.
//! A seam a transaction escaped would be a seam with a hole exactly where the
//! writes are.

use std::sync::Arc;

use sea_orm::{
    ConnectionTrait, DatabaseConnection, DatabaseTransaction, DbBackend, DbErr, ExecResult,
    QueryResult, Statement, TransactionTrait,
};

/// The database, as the application holds it.
///
/// Cheap to clone (the pool handle behind it is), `Send + Sync`, and a
/// [`ConnectionTrait`] — so every `repo::` function takes it unchanged.
#[derive(Clone)]
pub struct Db(Arc<dyn Connection>);

impl Db {
    /// A handle onto anything that answers statements.
    pub fn new(connection: impl Connection + 'static) -> Self {
        Db(Arc::new(connection))
    }

    /// Begin a transaction, as a handle more statements can be issued on.
    pub async fn begin(&self) -> Result<Txn, DbErr> {
        self.0.begin().await
    }

    /// Close the pool behind this handle.
    ///
    /// By reference rather than by value, because a handle is shared: every
    /// worker and every handler holds a clone of the same one, and there is no
    /// single owner to consume. Closing therefore makes *every* clone fail,
    /// which is exactly what it means for the database to have gone away.
    pub async fn close(&self) -> Result<(), DbErr> {
        self.0.close().await
    }
}

impl std::fmt::Debug for Db {
    /// The dialect, and nothing else: a connection's `Debug` carries its URL,
    /// and a URL carries a password (ADR-0011 rule 2).
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Db({:?})", self.get_database_backend())
    }
}

/// A transaction, as the application holds it.
///
/// Owns its statements the way [`Db`] does, so a composed handle's transaction
/// is composed too.
pub struct Txn(Box<dyn Transaction>);

impl Txn {
    /// A handle onto anything that answers statements and can be committed.
    pub fn new(transaction: impl Transaction + 'static) -> Self {
        Txn(Box::new(transaction))
    }

    /// Commit everything issued on this handle.
    pub async fn commit(self) -> Result<(), DbErr> {
        self.0.commit().await
    }

    /// Discard everything issued on this handle.
    pub async fn rollback(self) -> Result<(), DbErr> {
        self.0.rollback().await
    }
}

/// Something a [`Db`] can be made from: statements, and a transaction.
///
/// `Send + Sync` because an `AppState` is cloned into every handler and every
/// worker task.
#[async_trait::async_trait]
pub trait Connection: ConnectionTrait + Send + Sync {
    /// Begin a transaction on this connection.
    async fn begin(&self) -> Result<Txn, DbErr>;

    /// Close the pool, so that every handle onto it starts failing.
    async fn close(&self) -> Result<(), DbErr>;
}

/// Something a [`Txn`] can be made from: statements, and an end.
///
/// `self: Box<Self>` rather than `self`, because this is used through a trait
/// object and a by-value `self` would not be object-safe.
#[async_trait::async_trait]
pub trait Transaction: ConnectionTrait + Send + Sync {
    /// Commit everything issued on this transaction.
    async fn commit(self: Box<Self>) -> Result<(), DbErr>;
    /// Discard everything issued on this transaction.
    async fn rollback(self: Box<Self>) -> Result<(), DbErr>;
}

/// Statements go wherever this handle points. Written out rather than derived
/// because `ConnectionTrait` has no blanket impl for a wrapper — which is the
/// same reason it can be decorated at all.
#[async_trait::async_trait]
impl ConnectionTrait for Db {
    fn get_database_backend(&self) -> DbBackend {
        self.0.get_database_backend()
    }

    async fn execute(&self, statement: Statement) -> Result<ExecResult, DbErr> {
        self.0.execute(statement).await
    }

    async fn execute_unprepared(&self, sql: &str) -> Result<ExecResult, DbErr> {
        self.0.execute_unprepared(sql).await
    }

    async fn query_one(&self, statement: Statement) -> Result<Option<QueryResult>, DbErr> {
        self.0.query_one(statement).await
    }

    async fn query_all(&self, statement: Statement) -> Result<Vec<QueryResult>, DbErr> {
        self.0.query_all(statement).await
    }

    fn support_returning(&self) -> bool {
        self.0.support_returning()
    }
}

#[async_trait::async_trait]
impl ConnectionTrait for Txn {
    fn get_database_backend(&self) -> DbBackend {
        self.0.get_database_backend()
    }

    async fn execute(&self, statement: Statement) -> Result<ExecResult, DbErr> {
        self.0.execute(statement).await
    }

    async fn execute_unprepared(&self, sql: &str) -> Result<ExecResult, DbErr> {
        self.0.execute_unprepared(sql).await
    }

    async fn query_one(&self, statement: Statement) -> Result<Option<QueryResult>, DbErr> {
        self.0.query_one(statement).await
    }

    async fn query_all(&self, statement: Statement) -> Result<Vec<QueryResult>, DbErr> {
        self.0.query_all(statement).await
    }

    fn support_returning(&self) -> bool {
        self.0.support_returning()
    }
}

/// The real thing: a pool talking to SQLite or Postgres.
#[async_trait::async_trait]
impl Connection for DatabaseConnection {
    async fn begin(&self) -> Result<Txn, DbErr> {
        Ok(Txn::new(TransactionTrait::begin(self).await?))
    }

    async fn close(&self) -> Result<(), DbErr> {
        self.close_by_ref().await
    }
}

#[async_trait::async_trait]
impl Transaction for DatabaseTransaction {
    async fn commit(self: Box<Self>) -> Result<(), DbErr> {
        DatabaseTransaction::commit(*self).await
    }

    async fn rollback(self: Box<Self>) -> Result<(), DbErr> {
        DatabaseTransaction::rollback(*self).await
    }
}

#[cfg(test)]
mod tests {
    use crate::testing::sqlite_url;

    /// The handle is a `ConnectionTrait`, so everything already written against
    /// one works through it — including a transaction's own statements, which
    /// is the half a seam is easiest to leave a hole in.
    ///
    /// Prepared and unprepared both, because a composed handle intercepts each
    /// statement method separately: one left delegating straight past would be
    /// a hole exactly where somebody eventually reaches for raw SQL.
    #[tokio::test]
    async fn statements_and_transactions_both_run_through_the_handle() {
        use sea_orm::{ConnectionTrait, Statement};

        let tmp = tempfile::tempdir().expect("tempdir");
        let db = crate::db::connect(&sqlite_url(&tmp))
            .await
            .expect("connect");

        let txn = db.begin().await.expect("begin");
        crate::db::repo::create_api_key(&txn, "k", None, None, 0)
            .await
            .expect("insert inside the transaction");
        txn.execute_unprepared(r#"UPDATE "api_keys" SET "label" = 'renamed'"#)
            .await
            .expect("unprepared sql inside the transaction");
        txn.commit().await.expect("commit");

        assert_eq!(
            crate::db::repo::count_api_keys(&db).await.expect("count"),
            1,
            "a committed transaction's rows are there afterwards"
        );
        let label: String = db
            .query_one(Statement::from_string(
                db.get_database_backend(),
                r#"SELECT "label" FROM "api_keys""#,
            ))
            .await
            .expect("query")
            .expect("a row")
            .try_get("", "label")
            .expect("the label");
        assert_eq!(
            label, "renamed",
            "...including what an unprepared statement did to them"
        );
    }

    /// ...and a rolled-back one leaves nothing, which is the property that
    /// makes ingest's "write the object before the row" recoverable.
    #[tokio::test]
    async fn a_rolled_back_transaction_leaves_nothing_behind() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let db = crate::db::connect(&sqlite_url(&tmp))
            .await
            .expect("connect");

        let txn = db.begin().await.expect("begin");
        crate::db::repo::create_api_key(&txn, "k", None, None, 0)
            .await
            .expect("insert inside the transaction");
        txn.rollback().await.expect("rollback");

        assert_eq!(
            crate::db::repo::count_api_keys(&db).await.expect("count"),
            0
        );
    }

    /// A handle names its dialect and never its URL — a connection string
    /// carries a Postgres password, and `Debug` is exactly how one reaches an
    /// error line an operator pastes into an issue (ADR-0011 rule 2).
    #[tokio::test]
    async fn debugging_the_handle_shows_the_dialect_and_not_the_url() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let url = sqlite_url(&tmp);
        let db = crate::db::connect(&url).await.expect("connect");

        let rendered = format!("{db:?}");

        assert!(rendered.contains("Sqlite"), "{rendered}");
        assert!(
            !rendered.contains(tmp.path().to_str().expect("utf-8")),
            "{rendered}"
        );
    }
}
