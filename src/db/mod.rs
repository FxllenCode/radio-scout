//! The database layer (ADR-0003): SeaORM entities, migrations, and the
//! repository over SQLite (default) and Postgres (opt-in). Audio lives in object
//! storage (ADR-0002); this layer holds only small metadata.

pub mod entities;
pub mod migration;
pub mod repo;

use sea_orm::{ConnectionTrait, Database, DatabaseConnection, DbErr, Statement};
use sea_orm_migration::MigratorTrait;

pub use sea_orm::DbBackend;

/// Connect to the database at `url` and bring the schema up to date.
///
/// SQLite URLs get `foreign_keys = ON` (off by default in SQLite). Postgres
/// enforces foreign keys unconditionally.
pub async fn connect(url: &str) -> Result<DatabaseConnection, DbErr> {
    let db = Database::connect(url).await?;
    if db.get_database_backend() == DbBackend::Sqlite {
        // WAL persists in the file header (set once); foreign_keys is
        // per-connection (see the note in `authorize_ingest` callers / #5).
        for pragma in ["PRAGMA journal_mode = WAL;", "PRAGMA foreign_keys = ON;"] {
            db.execute(Statement::from_string(DbBackend::Sqlite, pragma))
                .await?;
        }
    }
    migration::Migrator::up(&db, None).await?;
    Ok(db)
}
