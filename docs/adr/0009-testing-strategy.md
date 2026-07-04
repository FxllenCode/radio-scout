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
- Recorder compatibility can never silently regress, which is the single biggest risk to the "drop-in replacement" promise.
