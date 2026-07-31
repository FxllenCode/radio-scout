//! Schema migrations. Tables are generated from the entity definitions via
//! SeaORM's `Schema`, so one migration emits correct DDL for **both** SQLite and
//! Postgres (ADR-0003) with no hand-branched SQL. Composite-unique and search
//! indexes are added explicitly since they aren't expressible on the entities.

use sea_orm::Schema;
use sea_orm_migration::prelude::*;

use crate::db::entities::{
    api_key, call, call_frequency, call_patch, call_unit, group, log_event, push_subscription,
    site, system, tag, talkgroup, talkgroup_group, talkgroup_ref, unit, unit_ref,
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
            Box::new(m0006_enhancement::Migration),
            Box::new(m0007_logs::Migration),
            Box::new(m0008_recorder_truth::Migration),
            Box::new(m0009_channel_merge::Migration),
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

/// Audio enhancement (#20) needs two different things recorded.
///
/// **State, on the Call** — enhancement happens after the `200`, so at any
/// moment a Call is passthrough, queued, enhanced, or was tried and could not
/// be. Two things read it: audio serving, which must not tell a client to cache
/// a version about to be replaced, and boot, which re-queues what a restart
/// interrupted.
///
/// **Scope, on the System and the Talkgroup** — nullable, because `NULL` is the
/// third state that makes inheritance work: *say nothing and follow the
/// instance*. `auto_populate` (m0004) is a plain boolean for the same job and
/// cannot express it, which is why turning that off for one System is awkward.
mod m0006_enhancement {
    use super::*;

    pub struct Migration;

    impl MigrationName for Migration {
        fn name(&self) -> &str {
            "m0006_enhancement"
        }
    }

    #[async_trait::async_trait]
    impl MigrationTrait for Migration {
        async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
            if !manager.has_column("calls", "enhancement").await? {
                manager
                    .alter_table(
                        Table::alter()
                            .table(call::Entity)
                            .add_column(
                                ColumnDef::new(call::Column::Enhancement)
                                    .string()
                                    .not_null()
                                    // Every Call that already exists was stored
                                    // as the recorder sent it. Marking them
                                    // `none` rather than `pending` is what stops
                                    // enabling enhancement from silently
                                    // rewriting an archive on the next boot.
                                    .default(call::EnhancementState::NONE),
                            )
                            .to_owned(),
                    )
                    .await?;
            }
            for (table, entity) in [("systems", system::Entity)] {
                if !manager.has_column(table, "enhancement").await? {
                    manager
                        .alter_table(
                            Table::alter()
                                .table(entity)
                                .add_column(
                                    ColumnDef::new(system::Column::Enhancement).boolean().null(),
                                )
                                .to_owned(),
                        )
                        .await?;
                }
            }
            if !manager.has_column("talkgroups", "enhancement").await? {
                manager
                    .alter_table(
                        Table::alter()
                            .table(talkgroup::Entity)
                            .add_column(
                                ColumnDef::new(talkgroup::Column::Enhancement)
                                    .boolean()
                                    .null(),
                            )
                            .to_owned(),
                    )
                    .await?;
            }
            Ok(())
        }

        async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
            manager
                .alter_table(
                    Table::alter()
                        .table(call::Entity)
                        .drop_column(call::Column::Enhancement)
                        .to_owned(),
                )
                .await?;
            manager
                .alter_table(
                    Table::alter()
                        .table(system::Entity)
                        .drop_column(system::Column::Enhancement)
                        .to_owned(),
                )
                .await?;
            manager
                .alter_table(
                    Table::alter()
                        .table(talkgroup::Entity)
                        .drop_column(talkgroup::Column::Enhancement)
                        .to_owned(),
                )
                .await
        }
    }
}

/// The operator log surface (#30) needs somewhere to keep what the server said.
/// A new table rather than a column, so the entity-derived-DDL tax m0003 and
/// m0004 pay does not apply — nothing already exists to diverge from.
///
/// Two indexes, because the Logs view only ever asks two questions: the newest
/// events (optionally within a date range), and the same thing narrowed to a
/// severity. Both are also what retention's own `DELETE … WHERE at_ms <` walks.
mod m0007_logs {
    use super::*;

    pub struct Migration;

    impl MigrationName for Migration {
        fn name(&self) -> &str {
            "m0007_logs"
        }
    }

    #[async_trait::async_trait]
    impl MigrationTrait for Migration {
        async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
            let schema = Schema::new(manager.get_database_backend());
            manager
                .create_table(schema.create_table_from_entity(log_event::Entity))
                .await?;
            manager
                .create_index(
                    Index::create()
                        .name("idx_logs_time")
                        .table(log_event::Entity)
                        .col(log_event::Column::AtMs)
                        .to_owned(),
                )
                .await?;
            manager
                .create_index(
                    Index::create()
                        .name("idx_logs_level_time")
                        .table(log_event::Entity)
                        .col(log_event::Column::Level)
                        .col(log_event::Column::AtMs)
                        .to_owned(),
                )
                .await
        }

        async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
            manager
                .drop_table(Table::drop().table(log_event::Entity).to_owned())
                .await
        }
    }
}

/// Ingest enrichment (#42): the recorder truth the parser used to discard.
///
/// Trunk Recorder writes emergency, encrypted, priority, audio type, a stop
/// time and per-source over-the-air aliases into every call `.json`
/// (`call_concluder.cc`'s `create_call_json`); rdio-scanner's parser reads past
/// all of it, and so did ours. Duration is the one every recorder can answer —
/// from its own metadata or, failing that, from the audio's container header at
/// ingest — so the column stops being something only Enhancement ever filled in.
///
/// **`emergency` and `encrypted` are `NOT NULL DEFAULT false`, not nullable.**
/// A recorder that says nothing said "no". Making them tri-state would leave
/// #53's alerting unable to tell "no emergency" from "this recorder never
/// mentions emergencies", which is a distinction no listener can act on and a
/// query nobody can index sensibly.
///
/// `site_id` points at the `sites` table m0001 has created and nothing has ever
/// written to — a plain column rather than a declared foreign key, for the
/// reason spelled out on `call::Model::site_id`. rdio's generic `site` field is
/// a bare number, but #48 backfills the same column from SDRTrunk's ID3 tags
/// where a site is a *name*, and a row is the only shape that holds both.
///
/// Guarded column by column, for the reason m0003 wrote down: m0001 generates
/// its DDL from the *live* entities, so a fresh database already has all of
/// these by the time this runs and an upgraded one has none of them.
mod m0008_recorder_truth {
    use super::*;
    use crate::db::entities::{call_frequency, call_unit};

    pub struct Migration;

    impl MigrationName for Migration {
        fn name(&self) -> &str {
            "m0008_recorder_truth"
        }
    }

    /// Add `column` to `entity`'s table if it isn't there already. One helper
    /// because this migration adds eleven columns across three tables and the
    /// guard is the only interesting part of each.
    ///
    /// The table is named **once**, by its `Iden`, and the column name is read
    /// off the definition. `has_column` takes strings and `alter_table` takes
    /// the entity, so passing both separately would let the guard probe one
    /// table while the `ALTER` hit another — a silently skipped column, which
    /// is exactly the failure this family of migrations exists to prevent.
    async fn add_column(
        manager: &SchemaManager<'_>,
        entity: impl Iden + Clone + 'static,
        column: ColumnDef,
    ) -> Result<(), DbErr> {
        let table = entity.to_string();
        let name = column.get_column_name();
        if manager.has_column(&table, &name).await? {
            return Ok(());
        }
        manager
            .alter_table(Table::alter().table(entity).add_column(column).to_owned())
            .await
    }

    #[async_trait::async_trait]
    impl MigrationTrait for Migration {
        async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
            add_column(
                manager,
                call::Entity,
                ColumnDef::new(call::Column::StopAtMs)
                    .big_integer()
                    .null()
                    .to_owned(),
            )
            .await?;
            add_column(
                manager,
                call::Entity,
                ColumnDef::new(call::Column::Emergency)
                    .boolean()
                    .not_null()
                    .default(false)
                    .to_owned(),
            )
            .await?;
            add_column(
                manager,
                call::Entity,
                ColumnDef::new(call::Column::Encrypted)
                    .boolean()
                    .not_null()
                    .default(false)
                    .to_owned(),
            )
            .await?;
            add_column(
                manager,
                call::Entity,
                ColumnDef::new(call::Column::Priority)
                    .integer()
                    .null()
                    .to_owned(),
            )
            .await?;
            add_column(
                manager,
                call::Entity,
                ColumnDef::new(call::Column::AudioType)
                    .string()
                    .null()
                    .to_owned(),
            )
            .await?;
            // A plain column, not a foreign key — see `call::Model::site_id`.
            // SQLite cannot add a constraint to a table it already made, so a
            // declared relation would give a fresh database and an upgraded one
            // permanently different schemas, which is precisely the divergence
            // m0003 and m0004 exist to close.
            add_column(
                manager,
                call::Entity,
                ColumnDef::new(call::Column::SiteId)
                    .big_integer()
                    .null()
                    .to_owned(),
            )
            .await?;

            add_column(
                manager,
                call_frequency::Entity,
                ColumnDef::new(call_frequency::Column::AtMs)
                    .big_integer()
                    .null()
                    .to_owned(),
            )
            .await?;

            add_column(
                manager,
                call_unit::Entity,
                ColumnDef::new(call_unit::Column::TagOta)
                    .string()
                    .null()
                    .to_owned(),
            )
            .await?;
            add_column(
                manager,
                call_unit::Entity,
                ColumnDef::new(call_unit::Column::Emergency)
                    .boolean()
                    .not_null()
                    .default(false)
                    .to_owned(),
            )
            .await?;
            add_column(
                manager,
                call_unit::Entity,
                ColumnDef::new(call_unit::Column::SignalSystem)
                    .string()
                    .null()
                    .to_owned(),
            )
            .await?;
            add_column(
                manager,
                call_unit::Entity,
                ColumnDef::new(call_unit::Column::AtMs)
                    .big_integer()
                    .null()
                    .to_owned(),
            )
            .await?;

            // Every archive query that offers "hide the kerchunks" (#42's
            // min-duration filter) walks this alongside the time ordering the
            // m0001 indexes already provide.
            if !manager.has_index("calls", "idx_calls_duration").await? {
                manager
                    .create_index(
                        Index::create()
                            .name("idx_calls_duration")
                            .table(call::Entity)
                            .col(call::Column::DurationMs)
                            .to_owned(),
                    )
                    .await?;
            }
            Ok(())
        }

        async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
            for column in [
                call::Column::StopAtMs,
                call::Column::Emergency,
                call::Column::Encrypted,
                call::Column::Priority,
                call::Column::AudioType,
                call::Column::SiteId,
            ] {
                manager
                    .alter_table(
                        Table::alter()
                            .table(call::Entity)
                            .drop_column(column)
                            .to_owned(),
                    )
                    .await?;
            }
            manager
                .alter_table(
                    Table::alter()
                        .table(call_frequency::Entity)
                        .drop_column(call_frequency::Column::AtMs)
                        .to_owned(),
                )
                .await?;
            for column in [
                call_unit::Column::TagOta,
                call_unit::Column::Emergency,
                call_unit::Column::SignalSystem,
                call_unit::Column::AtMs,
            ] {
                manager
                    .alter_table(
                        Table::alter()
                            .table(call_unit::Entity)
                            .drop_column(column)
                            .to_owned(),
                    )
                    .await?;
            }
            Ok(())
        }
    }
}

/// Channel merge (#45, spec US 15–18): the member Refs a Talkgroup answers to,
/// the Refs and Ranges a Unit owns, and the Ref each Call actually arrived
/// under.
///
/// Two new tables and one new column, and the column is the load-bearing part.
/// Resolution rewrites a Call's Talkgroup *before* it is stored, so without a
/// record of what the recorder said, a fold is irreversible: unfolding would
/// have no way to tell which of the owner's Calls used to be the folded
/// channel's. `calls.talkgroup_ref` is that record, written on every Call rather
/// than only merged ones — a Ref becomes a member Ref long after its Calls
/// arrived, so a column filled in only when it already mattered would be empty
/// exactly when it is needed.
///
/// It is deliberately a bare number, not a foreign key: it names what the radio
/// network said, which may be a Ref no row has ever existed for.
///
/// The two tables need no guard (nothing exists to diverge from — the m0005
/// precedent); the column does, for the reason m0003 wrote down.
mod m0009_channel_merge {
    use super::*;

    pub struct Migration;

    impl MigrationName for Migration {
        fn name(&self) -> &str {
            "m0009_channel_merge"
        }
    }

    #[async_trait::async_trait]
    impl MigrationTrait for Migration {
        async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
            let schema = Schema::new(manager.get_database_backend());
            manager
                .create_table(schema.create_table_from_entity(talkgroup_ref::Entity))
                .await?;
            manager
                .create_table(schema.create_table_from_entity(unit_ref::Entity))
                .await?;

            // A member Ref is unique within its System, exactly as a primary Ref
            // is (`idx_talkgroups_system_ref`). Without this a Ref could be owned
            // by two Talkgroups at once and resolution would depend on row order
            // — the one failure mode that would look like a flaky test.
            manager
                .create_index(
                    Index::create()
                        .name("idx_talkgroup_refs_system_ref")
                        .table(talkgroup_ref::Entity)
                        .col(talkgroup_ref::Column::SystemId)
                        .col(talkgroup_ref::Column::Ref)
                        .unique()
                        .to_owned(),
                )
                .await?;
            // Ranges overlap in ways a unique index cannot express, so this one
            // is an access path rather than a constraint: resolution asks "which
            // Range on this System contains this Ref?" on every Call carrying a
            // source, and overlap is refused when a Range is *written*
            // (`repo::add_unit_range`) where the whole set is in hand.
            manager
                .create_index(
                    Index::create()
                        .name("idx_unit_refs_system_span")
                        .table(unit_ref::Entity)
                        .col(unit_ref::Column::SystemId)
                        .col(unit_ref::Column::RefFrom)
                        .col(unit_ref::Column::RefTo)
                        .to_owned(),
                )
                .await?;

            if !manager.has_column("calls", "talkgroup_ref").await? {
                manager
                    .alter_table(
                        Table::alter()
                            .table(call::Entity)
                            .add_column(
                                ColumnDef::new(call::Column::TalkgroupRef)
                                    .big_integer()
                                    .null(),
                            )
                            .to_owned(),
                    )
                    .await?;
            }
            // Unfold's working set: "every Call on this Talkgroup that arrived
            // under that Ref". Without the index that is a table scan of the
            // archive, on the Pi, inside the transaction holding the fold.
            if !manager
                .has_index("calls", "idx_calls_talkgroup_arrived_as")
                .await?
            {
                manager
                    .create_index(
                        Index::create()
                            .name("idx_calls_talkgroup_arrived_as")
                            .table(call::Entity)
                            .col(call::Column::TalkgroupId)
                            .col(call::Column::TalkgroupRef)
                            .to_owned(),
                    )
                    .await?;
            }
            Ok(())
        }

        async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
            manager
                .drop_table(Table::drop().table(talkgroup_ref::Entity).to_owned())
                .await?;
            manager
                .drop_table(Table::drop().table(unit_ref::Entity).to_owned())
                .await?;
            manager
                .alter_table(
                    Table::alter()
                        .table(call::Entity)
                        .drop_column(call::Column::TalkgroupRef)
                        .to_owned(),
                )
                .await
        }
    }
}
