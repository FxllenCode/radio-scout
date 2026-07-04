# SQLite by default, Postgres for networked deployments

## Context

The "one program, just works" philosophy and the Raspberry Pi target favor an embedded, zero-config database. But the database must also be **hostable on a separate machine** (for a more durable/beefier box, or eventually multiple Radio-Scout instances against one DB) — which an embedded file database cannot do. Because audio lives in object storage ([ADR-0002](0002-audio-object-storage.md)), the database only holds small metadata, so the usual "blobs bloat the DB" pressure toward a big server DB is absent.

## Decision

- **SQLite is the zero-config embedded default** — a single file under `base_dir`, WAL mode. Ingest is a serialized pipeline, so SQLite's single-writer model is not a constraint.
- **Postgres is a first-class opt-in backend** for networked / separate-machine / multi-instance deployments, validated in CI.
- Both are supported through **SeaORM**, a dialect-generating async query layer, rather than hand-maintained per-dialect SQL — avoiding the two-dialect maintenance burden visible throughout rdio-scanner's Go code (`GROUP_CONCAT` vs `STRING_AGG`, `LastInsertId` vs `RETURNING`, etc.). SeaORM was chosen over Diesel (sync core; `diesel-async` does not support SQLite, our default) and over SeaQuery+sqlx (more boilerplate, DIY migrations) for its native-async fit with Axum/Tokio and built-in migrations. The accepted trade-off is runtime rather than compile-time query validation.
- Running SQLite over a network filesystem (NFS/SMB) is **explicitly rejected** — it is a corruption/locking hazard. "Remote database" means Postgres, not a shared SQLite file.

## Considered and rejected

- **libSQL (`sqld`)** — attractive because it is SQLite's dialect with a network server mode and embedded replicas, giving remote capability without a second dialect. Rejected for now in favor of Postgres's maturity on this foundational, hard-to-reverse layer. Revisit if the two-dialect burden proves costly.
- **Postgres-only everywhere** — one dialect and simplest code, but it forces every install (including casual Pi users) to run a database server, defeating the zero-config install goal.

## Consequences

- Every migration and query must be exercised against both SQLite and Postgres in CI.
- SeaORM's runtime query validation means query correctness leans on our test suite rather than the compiler — consistent with the project's TDD mandate.
