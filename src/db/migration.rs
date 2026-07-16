//! Schema migrations. Tables are generated from the entity definitions via
//! SeaORM's `Schema`, so one migration emits correct DDL for **both** SQLite and
//! Postgres (ADR-0003) with no hand-branched SQL. Composite-unique and search
//! indexes are added explicitly since they aren't expressible on the entities.

use sea_orm::Schema;
use sea_orm_migration::prelude::*;

use crate::db::entities::{
    api_key, call, call_frequency, call_patch, call_unit, group, site, system, tag, talkgroup,
    talkgroup_group, unit,
};

pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(m0001_init::Migration),
            Box::new(m0002_api_keys::Migration),
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
