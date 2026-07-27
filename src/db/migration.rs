//! Schema migrations. Tables are generated from the entity definitions via
//! SeaORM's `Schema`, so one migration emits correct DDL for **both** SQLite and
//! Postgres (ADR-0003) with no hand-branched SQL. Composite-unique and search
//! indexes are added explicitly since they aren't expressible on the entities.

use sea_orm::Schema;
use sea_orm_migration::prelude::*;

use crate::db::entities::{
    api_key, call, call_frequency, call_patch, call_unit, group, push_subscription, site, system,
    tag, talkgroup, talkgroup_group, unit,
};

pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(m0001_init::Migration),
            Box::new(m0002_api_keys::Migration),
            Box::new(m0003_call_audio_size::Migration),
            Box::new(m0004_system_auto_populate::Migration),
            Box::new(m0005_push_subscriptions::Migration),
        ]
    }
}

mod m0001_init {
    use super::*;

    pub struct Migration;

    impl MigrationName for Migration {
        fn name(&self) -> &str {
            "m0001_init"
        }
    }

    #[async_trait::async_trait]
    impl MigrationTrait for Migration {
        async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
            let backend = manager.get_database_backend();
            let schema = Schema::new(backend);

            // Parents before children so foreign keys resolve.
            manager
                .create_table(schema.create_table_from_entity(system::Entity))
                .await?;
            manager
                .create_table(schema.create_table_from_entity(tag::Entity))
                .await?;
            manager
                .create_table(schema.create_table_from_entity(group::Entity))
                .await?;
            manager
                .create_table(schema.create_table_from_entity(talkgroup::Entity))
                .await?;
            manager
                .create_table(schema.create_table_from_entity(talkgroup_group::Entity))
                .await?;
            manager
                .create_table(schema.create_table_from_entity(unit::Entity))
                .await?;
            manager
                .create_table(schema.create_table_from_entity(site::Entity))
                .await?;
            manager
                .create_table(schema.create_table_from_entity(call::Entity))
                .await?;
            manager
                .create_table(schema.create_table_from_entity(call_frequency::Entity))
                .await?;
            manager
                .create_table(schema.create_table_from_entity(call_unit::Entity))
                .await?;
            manager
                .create_table(schema.create_table_from_entity(call_patch::Entity))
                .await?;

            // A Ref is unique within its System (not globally).
            manager
                .create_index(
                    Index::create()
                        .name("idx_talkgroups_system_ref")
                        .table(talkgroup::Entity)
                        .col(talkgroup::Column::SystemId)
                        .col(talkgroup::Column::Ref)
                        .unique()
                        .to_owned(),
                )
                .await?;
            manager
                .create_index(
                    Index::create()
                        .name("idx_units_system_ref")
                        .table(unit::Entity)
                        .col(unit::Column::SystemId)
                        .col(unit::Column::Ref)
                        .unique()
                        .to_owned(),
                )
                .await?;
            manager
                .create_index(
                    Index::create()
                        .name("idx_sites_system_ref")
                        .table(site::Entity)
                        .col(site::Column::SystemId)
                        .col(site::Column::Ref)
                        .unique()
                        .to_owned(),
                )
                .await?;

            // Archive-search access paths (time-ordered per talkgroup / system).
            manager
                .create_index(
                    Index::create()
                        .name("idx_calls_talkgroup_time")
                        .table(call::Entity)
                        .col(call::Column::TalkgroupId)
                        .col(call::Column::CallAtMs)
                        .to_owned(),
                )
                .await?;
            manager
                .create_index(
                    Index::create()
                        .name("idx_calls_system_time")
                        .table(call::Entity)
                        .col(call::Column::SystemId)
                        .col(call::Column::CallAtMs)
                        .to_owned(),
                )
                .await?;
            manager
                .create_index(
                    Index::create()
                        .name("idx_calls_time")
                        .table(call::Entity)
                        .col(call::Column::CallAtMs)
                        .to_owned(),
                )
                .await?;

            Ok(())
        }

        async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
            // Children before parents.
            for stmt in [
                Table::drop().table(call_patch::Entity).to_owned(),
                Table::drop().table(call_unit::Entity).to_owned(),
                Table::drop().table(call_frequency::Entity).to_owned(),
                Table::drop().table(call::Entity).to_owned(),
                Table::drop().table(site::Entity).to_owned(),
                Table::drop().table(unit::Entity).to_owned(),
                Table::drop().table(talkgroup_group::Entity).to_owned(),
                Table::drop().table(talkgroup::Entity).to_owned(),
                Table::drop().table(group::Entity).to_owned(),
                Table::drop().table(tag::Entity).to_owned(),
                Table::drop().table(system::Entity).to_owned(),
            ] {
                manager.drop_table(stmt).await?;
            }
            Ok(())
        }
    }
}

mod m0002_api_keys {
    use super::*;

    pub struct Migration;

    impl MigrationName for Migration {
        fn name(&self) -> &str {
            "m0002_api_keys"
        }
    }

    #[async_trait::async_trait]
    impl MigrationTrait for Migration {
        async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
            let schema = Schema::new(manager.get_database_backend());
            manager
                .create_table(schema.create_table_from_entity(api_key::Entity))
                .await
        }

        async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
            manager
                .drop_table(Table::drop().table(api_key::Entity).to_owned())
                .await
        }
    }
}

/// Retention's size cap (#10) needs each Call's audio size. Recording it at
/// ingest turns "how many bytes is the archive?" into one `SUM()` instead of a
/// stat per object — which on an S3/Garage backend would be a network round-trip
/// each, every sweep. Nullable so existing rows migrate without a rewrite; a
/// `NULL` counts as zero toward the cap.
///
/// **Idempotent by necessity.** `m0001_init` generates its DDL from the *live*
/// entity definitions, so adding a field to `call::Model` retroactively puts the
/// column in `m0001` too — a fresh database already has it by the time this runs,
/// while a database migrated before the field existed does not. Both must
/// converge on the same schema, so this checks before it alters. Every future
/// `ALTER`-shaped migration on an entity-derived table needs the same guard.
mod m0003_call_audio_size {
    use super::*;

    /// The physical names the guard probes. `has_column` takes strings, so these
    /// can't come from the entity's `Iden`s.
    const TABLE: &str = "calls";
    const COLUMN: &str = "audio_size";

    pub struct Migration;

    impl MigrationName for Migration {
        fn name(&self) -> &str {
            "m0003_call_audio_size"
        }
    }

    #[async_trait::async_trait]
    impl MigrationTrait for Migration {
        async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
            if manager.has_column(TABLE, COLUMN).await? {
                return Ok(());
            }
            manager
                .alter_table(
                    Table::alter()
                        .table(call::Entity)
                        .add_column(ColumnDef::new(call::Column::AudioSize).big_integer().null())
                        .to_owned(),
                )
                .await
        }

        async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
            if !manager.has_column(TABLE, COLUMN).await? {
                return Ok(());
            }
            manager
                .alter_table(
                    Table::alter()
                        .table(call::Entity)
                        .drop_column(call::Column::AudioSize)
                        .to_owned(),
                )
                .await
        }
    }
}

/// #8 added `auto_populate` and `blacklist` to the System *entity*, which gave
/// every **new** database the columns (m0001 generates its DDL from the live
/// entities) and every **existing** one nothing at all. On a real upgrade that
/// surfaced as `no such column: systems.auto_populate` — an HTTP 500 on every
/// ingest, with the recorder logging an upload error and dropping the Call.
///
/// The guard is the same one m0003 needed: probe before altering, because a
/// fresh database already has the columns from m0001. This is the standing tax
/// on entity-derived DDL, and any future column on an entity-derived table owes
/// the same migration.
mod m0004_system_auto_populate {
    use super::*;

    const TABLE: &str = "systems";

    pub struct Migration;

    impl MigrationName for Migration {
        fn name(&self) -> &str {
            "m0004_system_auto_populate"
        }
    }

    #[async_trait::async_trait]
    impl MigrationTrait for Migration {
        async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
            if !manager.has_column(TABLE, "auto_populate").await? {
                manager
                    .alter_table(
                        Table::alter()
                            .table(system::Entity)
                            .add_column(
                                ColumnDef::new(system::Column::AutoPopulate)
                                    .boolean()
                                    .not_null()
                                    // Per-system opt-in is off by default; the
                                    // global toggle is what a zero-config
                                    // install runs on (#8).
                                    .default(false),
                            )
                            .to_owned(),
                    )
                    .await?;
            }
            if !manager.has_column(TABLE, "blacklist").await? {
                manager
                    .alter_table(
                        Table::alter()
                            .table(system::Entity)
                            .add_column(ColumnDef::new(system::Column::Blacklist).string().null())
                            .to_owned(),
                    )
                    .await?;
            }
            Ok(())
        }

        async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
            for column in [system::Column::AutoPopulate, system::Column::Blacklist] {
                manager
                    .alter_table(
                        Table::alter()
                            .table(system::Entity)
                            .drop_column(column)
                            .to_owned(),
                    )
                    .await?;
            }
            Ok(())
        }
    }
}

/// Web Push (#16) needs a device to survive the restart between the Call that
/// interested it and the one that arrives at 3am — so a subscription is a row,
/// not memory. A new table rather than a column, so the entity-derived-DDL tax
/// m0003 and m0004 pay does not apply: nothing already exists to diverge from.
mod m0005_push_subscriptions {
    use super::*;

    pub struct Migration;

    impl MigrationName for Migration {
        fn name(&self) -> &str {
            "m0005_push_subscriptions"
        }
    }

    #[async_trait::async_trait]
    impl MigrationTrait for Migration {
        async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
            let schema = Schema::new(manager.get_database_backend());
            manager
                .create_table(schema.create_table_from_entity(push_subscription::Entity))
                .await
        }

        async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
            manager
                .drop_table(Table::drop().table(push_subscription::Entity).to_owned())
                .await
        }
    }
}
