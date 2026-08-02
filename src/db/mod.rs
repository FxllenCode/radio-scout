//! The database layer (ADR-0003): SeaORM entities, migrations, and the
//! repository over SQLite (default) and Postgres (opt-in). Audio lives in object
//! storage (ADR-0002); this layer holds only small metadata.

pub mod entities;
mod handle;
pub mod migration;
pub mod repo;

use sea_orm::{ConnectionTrait, Database, DatabaseConnection, DbErr, Statement};
use sea_orm_migration::MigratorTrait;
use tracing::{debug, info};

pub use handle::{Connection, Db, Transaction, Txn};
pub use sea_orm::DbBackend;

/// Connect to the database at `url` and bring the schema up to date.
///
/// SQLite URLs get `foreign_keys = ON` (off by default in SQLite). Postgres
/// enforces foreign keys unconditionally.
///
/// Migration runs against the connection itself rather than the [`Db`] it is
/// wrapped in: sea-orm's migrator takes its own kind of handle, and a schema is
/// brought up to date once at boot rather than being part of what the
/// application does with a database afterwards.
pub async fn connect(url: &str) -> Result<Db, DbErr> {
    let db = Database::connect(url).await?;
    if db.get_database_backend() == DbBackend::Sqlite {
        // WAL persists in the file header (set once); foreign_keys is
        // per-connection (see the note in `authorize_ingest` callers / #5).
        for pragma in ["PRAGMA journal_mode = WAL;", "PRAGMA foreign_keys = ON;"] {
            db.execute(Statement::from_string(DbBackend::Sqlite, pragma))
                .await?;
        }
    }
    migrate(&db).await?;
    Ok(Db::new(db))
}

/// Bring the schema up to date, **saying what it did** (ADR-0011).
///
/// This used to be a bare `Migrator::up`, which is silent: when a database
/// created before #8 was missing `systems.auto_populate`, the migration that
/// fixed it ran with no output at all, and an operator had no way to tell an
/// upgraded schema from an unchanged one. Migrations are applied one at a time
/// so each line lands *after* the migration it reports — a failure names the
/// last one that actually succeeded.
async fn migrate(db: &DatabaseConnection) -> Result<(), DbErr> {
    let pending = migration::Migrator::get_pending_migrations(db).await?;
    if pending.is_empty() {
        debug!("schema already up to date");
        return Ok(());
    }

    info!(pending = pending.len(), "applying schema migrations");
    for migration in &pending {
        migration::Migrator::up(db, Some(1)).await?;
        info!(migration = migration.name(), "migration applied");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{LogCapture, sqlite_url};

    /// A fresh database says which migrations it applied, by name. Before this,
    /// a first run built the entire schema in silence.
    #[tokio::test]
    async fn a_fresh_database_names_every_migration_it_applies() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let capture = LogCapture::start();

        connect(&sqlite_url(&tmp)).await.expect("connect");

        let logged = capture.text();
        for name in [
            "m0001_init",
            "m0002_api_keys",
            "m0003_call_audio_size",
            "m0004_system_auto_populate",
            "m0005_push_subscriptions",
        ] {
            assert!(
                logged.contains(&format!("migration=\"{name}\"")),
                "{name} missing from:\n{logged}"
            );
        }
        assert!(logged.contains("INFO"), "{logged}");
        assert!(logged.contains("applying schema migrations"), "{logged}");
    }

    /// An upgraded database names *only* what it applied — the m0004 case that
    /// cost an hour on 2026-07-25, where every ingest 500'd because a column was
    /// missing and nothing said whether the fix had run.
    ///
    /// The database is left deliberately behind (the first two migrations only)
    /// and then opened normally, which is exactly the shape of that upgrade.
    #[tokio::test]
    async fn an_upgraded_database_names_only_what_it_applied() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let url = sqlite_url(&tmp);
        let old = sea_orm::Database::connect(&url).await.expect("connect");
        migration::Migrator::up(&old, Some(2))
            .await
            .expect("an out-of-date schema");
        old.close().await.expect("close");

        let capture = LogCapture::start();
        connect(&url).await.expect("upgrade");

        // Derived from the migration list rather than written out, so adding a
        // migration does not break a test about *reporting* migrations.
        let names: Vec<String> = migration::Migrator::migrations()
            .iter()
            .map(|m| m.name().to_string())
            .collect();
        let (already_there, applied) = names.split_at(2);

        let logged = capture.text();
        for name in applied {
            assert!(
                logged.contains(&format!("migration=\"{name}\"")),
                "{name} missing from:\n{logged}"
            );
        }
        for name in already_there {
            assert!(
                !logged.contains(&format!("migration=\"{name}\"")),
                "{name} was already applied; re-reporting it is a lie:\n{logged}"
            );
        }
        assert!(
            logged.contains(&format!("pending={}", applied.len())),
            "{logged}"
        );
    }

    /// A database already at the current schema says so — quietly, at DEBUG, so
    /// a restart doesn't narrate four migrations that didn't happen.
    #[tokio::test]
    async fn an_up_to_date_database_says_so_without_claiming_work() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let url = sqlite_url(&tmp);
        connect(&url).await.expect("first connect");

        let capture = LogCapture::start();
        connect(&url).await.expect("second connect");

        let logged = capture.text();
        assert!(
            !logged.contains("migration applied"),
            "an up-to-date schema must not claim to have applied anything:\n{logged}"
        );
        assert!(logged.contains("schema already up to date"), "{logged}");
        // ...and it stays out of an operator's way at the default level.
        assert!(logged.contains("DEBUG"), "{logged}");
    }
}
