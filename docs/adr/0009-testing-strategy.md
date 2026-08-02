# Testing strategy: full pyramid, integration harness, recorder golden suite

## Context

TDD is mandated and CI is central to the workflow. Two things especially must be guaranteed and kept from regressing: the dual-dialect database layer ([ADR-0003](0003-database-sqlite-postgres.md)) and byte-level recorder compatibility ([ADR-0001](0001-ingest-compatible-own-live-feed-protocol.md)).

## Decision

Stand up the full test pyramid from v1:

- **Backend unit tests** for domain logic: multipart/JSON parsers, the filename-mask mini-language, duplicate detection, access-scope matching, retention, and DSP/enhancement parameters.
- **Integration harness** — the TDD backbone: bring up the Axum app in-process against a temp SQLite DB + temp filesystem object store, POST synthetic calls, and assert on DB rows, stored audio objects, and WebSocket pushes. Runs against **both SQLite and Postgres** (Postgres via testcontainers) in CI.
- **Recorder-compatibility golden suite:** real Trunk Recorder and SDRTrunk multipart payloads as fixtures, asserting our endpoint parses them **and** returns the exact load-bearing response strings (`Call imported successfully.`, `duplicate call rejected`, `incomplete call data: no talkgroup`). This is the automated guarantee behind ADR-0001.
- **Frontend:** Vitest + React Testing Library for store/component units; Playwright for critical-flow e2e.
- **iOS background audio** is validated **manually on a real device** — there is no CI substitute for the iOS mechanics ([ADR-0005](0005-client-audio-media-session-background.md)).
- **Merge gates:** `cargo fmt`, `clippy -D warnings`, all backend tests (both DBs), and frontend tests must pass; work follows red-green-refactor.

## Consequences

- Higher upfront investment (the integration harness, golden fixtures, and Playwright setup) before feature velocity — accepted for the correctness guarantees.
- Postgres testing requires Docker available in CI/dev (testcontainers).

> **Amended by #22 (2026-07-26).** The dual-dialect run is `TEST_POSTGRES_URL` + a `postgres:17` service in CI, not the `testcontainers` crate — see [ADR-0010's amendment](0010-coverage-policy-and-test-tooling.md#amendment-22-2026-07-26-the-patch-gate-runs-in-repo-not-on-codecov). Docker is therefore required in **CI only**: with the variable unset the suite runs on SQLite, so no developer needs a daemon to run `cargo test`.

> **Amended by #97 (2026-08-01): a fault seam goes at the module's own interface, never below it.**
>
> #37 made I/O failures reachable, which was right, but it put both seams underneath the thing being tested and both went wrong in the same way.
>
> The store seam was a decorator implementing seven methods of `object_store`'s trait, and it had to know that `serve_audio` stats an object before it reads one — "a `head` is never failed, only a `get`" was a fact about a *handler* written down in the fault machinery, where a rewrite of the handler would have silently stopped reaching the arm. Reaching "the object was pruned between the stat and the read" meant parking a real read inside the store while a real object was deleted. The database had no seam at all, so failures came from `DROP TABLE` plus a trigger written twice in two dialects' procedural SQL, recognised by matching each driver's own wording for "no such table" — two strings that are not ours.
>
> The rule now: **name the dependency at the interface, and substitute there.**
>
> - **`blob::AudioStore`** is the port an `AppState` holds and `BlobStore` implements. A substitute answers in Radio-Scout's vocabulary — bytes, keys, `None` for absent — and knows nothing about the order anything asks its questions in. "The stat found an object and the read did not" is *stated*, not staged.
> - **`db::Db`** is the handle every statement goes through, composed rather than constructed, so a decorator refusing statements that name a table sits between the application and the driver. Transactions are inside the seam: `Db::begin` returns a composed `Txn`, because ingest's frequency-roster insert is a statement an Operator's 5xx stage table names. A rule names the **quoted table name**, which sea-orm writes identically on both dialects, and the refusal it raises is our own string on both — so nothing about a fault test knows which dialect it is running on.
> - **`enhance::Archive`** is the six questions enhancement asks of the world, and `step` returns a `Settled` rather than only a log line. Its failure arms are unit tests over a substitute; the integration tests keep what they are for — that the worker is wired into a running Instance.
>
> What retired with them: the store fault decorator, the parking handshake, the write-failure triggers, the missing-table wording helper, and `BlobStore::decorated`, the production seam that existed only for the decorator. A test no longer damages a schema to reach an error arm.
>
> The limit is worth stating: a substitute proves the *caller's* behaviour, not the store's or the driver's. Those keep being proven by the real thing — `tests/blob.rs` and `tests/s3.rs` against a store that answers, and the whole suite twice against both dialects.
- Recorder compatibility can never silently regress, which is the single biggest risk to the "drop-in replacement" promise.
