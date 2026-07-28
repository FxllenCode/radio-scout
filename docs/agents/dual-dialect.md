# Running the suite against Postgres

Radio-Scout ships on SQLite and Postgres ([ADR-0003](../adr/0003-database-sqlite-postgres.md)), so
[ADR-0009](../adr/0009-testing-strategy.md) requires the suite to run on both. CI does that on every
pull request; this is how to do it on your machine when a change touches a query, a migration, or
anything that reads a driver's error text.

## The switch

**`TEST_POSTGRES_URL`.** Set it and every `TestApp::spawn()` in every test binary lands on Postgres.
Unset — the everyday red-green loop, and any machine without Docker — is SQLite, so nothing about
local TDD changes.

```bash
# A throwaway server. Nothing in it is worth keeping.
docker run -d --name rs-pg -e POSTGRES_PASSWORD=postgres -e POSTGRES_USER=postgres \
  -e POSTGRES_DB=postgres -p 55432:5432 postgres:17-alpine

TEST_POSTGRES_URL='postgres://postgres:postgres@localhost:55432/postgres' cargo nextest run

docker rm -f rs-pg
```

## Each test gets a database of its own

The harness creates `rs_test_<uuid>` on that server per spawned app and migrates it
(`tests/common/mod.rs`). Isolation is a property the SQLite default gets for free from having a file
each; Postgres buys it here, because one shared database would put every concurrently running test's
rows in the same tables — and nextest runs tests in parallel, in separate processes.

Those databases are **not** dropped afterwards: `Drop` cannot await, and the server is a throwaway.
That is also why the command above ends in `docker rm -f`. A run against a long-lived Postgres will
accumulate them; `psql -c "\l"` will show you, and dropping the container is the cure.

Postgres' default 100-connection ceiling is ample for a laptop or a 4-core runner. If you run the
suite against a server with a much lower limit, that is what "too many clients already" means.

## What actually differs between the dialects

Everything found so far, so a new failure can be recognised as a new class rather than re-derived:

- **`DROP TABLE`** — Postgres refuses while foreign keys still reference the table; SQLite has no
  `CASCADE` to offer it. The 5xx tests break a table on purpose to reach the error paths, so this is
  `TestApp::break_table`, which speaks the dialect it is on.
- **Missing-table wording** — SQLite says `no such table: calls`, Postgres says
  `relation "calls" does not exist`. The instrumentation tests assert the driver's own explanation
  reaches the operator's log and never the client's body, so they ask
  `TestApp::missing_table_cause("calls")` rather than pinning one dialect's phrasing — a pinned
  phrase keeps asserting on one dialect and quietly asserts nothing on the other.
- **`SUM(bigint)`** — Postgres widens it to `numeric`, SQLite keeps it an integer, so
  `repo::total_audio_bytes` casts. Guarded by `tests/db.rs`.
- **Text collation** — the two order text differently, which is why the selection catalog sorts in
  Rust rather than in the database ([#12](https://github.com/FxllenCode/radio-scout/issues/12)): a
  panel must read the same on a Pi's SQLite as on a hosted Postgres.

## In CI

`.github/workflows/ci.yml`'s `backend` job runs the suite **twice** — once plain, once with
`TEST_POSTGRES_URL` pointed at a `postgres:17-alpine` service — and both runs feed one coverage
profile, so a line reached only on one dialect still counts as reached.

`tests/ci.rs` pins the trap that job could otherwise fall into: a Postgres service stood up and the
URL never handed to the suite is a green run of SQLite twice.

## The storage half

[`real-s3.md`](real-s3.md) is this document's sibling for [ADR-0002](../adr/0002-audio-object-storage.md)'s
object store: `TEST_S3_ENDPOINT` runs `tests/s3.rs` against a MinIO or Garage that answers, a bucket
per test. It deliberately moves only that binary rather than the whole suite — the reason is there.
