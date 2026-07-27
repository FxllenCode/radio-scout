# Coverage policy and test tooling: patch-100% + ratcheting floor + mutation, not a 100%-total gate

## Context

[ADR-0009](0009-testing-strategy.md) mandates the full pyramid, the in-process integration harness, the recorder golden suite, and red-green-refactor. It does **not** quantify coverage or name tools. The stated goal is that *every* code path — backend and frontend — is rigorously tested with edge cases, "as good as possible," and enforced by CI. The naive reading of that is a hard **100% total line-coverage merge gate**.

Deep research (Codecov's own guidance, Google's internal standards, the Rust and React communities — see the sources gathered for this decision) converges against that: a hard 100%-total gate reliably produces **coverage theater** (tests that execute lines but assert nothing) and brittle tests welded to implementation details; the 90%→100% grind surfaces mostly trivial bugs. The higher-signal way to actually get "nothing untested ships, edge cases enforced" is a different combination.

## Decision

Extend ADR-0009 with a concrete, enforceable coverage policy and a named tool stack. **Applies symmetrically to backend and frontend.**

### Coverage policy

- **Hard gate: 100% patch/diff coverage** on every PR — every new or changed line must be covered. This is the mechanism that makes "new code ships with tests" true by construction. Backend and frontend are tracked as separate coverage flags.
- **Hard gate: a ratcheting project floor** — set at (or a few points below, for headroom against a deterministic dip) the measured baseline after the initial hardening pass, allowed to rise, never to fall. Enforced **in-repo** (`cargo llvm-cov --fail-under-lines`, Vitest `thresholds`) so the gate never depends on a third-party service's uptime; the patch gate rides on Codecov (the one thing it does uniquely well).
- **Quality signal, not just quantity: mutation testing.** `cargo-mutants` (`--in-diff` advisory on PRs; full sharded run nightly) proves assertions actually *catch* regressions — the real thing 100%-coverage only weakly proxies. Paired with `proptest` for parsers / dedup-window / range-header / protocol-framing logic.
- **"Edge cases" is operationalized**, not aspirational: `proptest` (generates edge inputs + shrinks counterexamples, seeds committed), `rstest` parametrized case tables (multiple named cases per behavior), and mutation testing together are how "multiple tests covering edge cases" is met and checked.
- **Documented, auditable exclusions** (never silent gaming of the number): generated SeaORM entities + migrations, `main()` bootstrap glue, `build.rs`, shadcn `components/ui/**` primitives, `main.tsx` entrypoint, `.d.ts`. Listed in CLAUDE.md.
- **No hard 100%-total gate.** 100% *total* is not the target; a high ratcheting floor plus 100% patch plus mutation is.

### Tool stack

- **Backend, adopt now (the everyday loop):** `cargo-nextest` (runner — process-per-test isolation matters for our WS/DB/port state), `cargo-llvm-cov` (LLVM source-based coverage — cross-platform incl. macOS/Pi arm64; tarpaulin is Linux-x86_64-only and is rejected), `proptest`, `rstest`, `insta` (snapshot the exact rdio-scanner wire responses — the automated backstop for [ADR-0001](0001-ingest-compatible-own-live-feed-protocol.md)), `tokio::time::pause` for dedup/time logic, `assert_cmd`/`trycmd` for the one-command-install CLI surface.
- **Backend, selective CI jobs:** `testcontainers` (real Postgres for dual-dialect per [ADR-0003](0003-database-sqlite-postgres.md); MinIO/Garage for real S3 per [ADR-0002](0002-audio-object-storage.md)); `cargo-mutants`.
- **Frontend, adopt now:** `@vitest/coverage-v8` (v4 AST-remapping is Istanbul-accurate) + thresholds; **MSW** at the network boundary (never fetch/module mocking — real request path through RTK Query); `vitest-axe`; keep `jsdom` (happy-dom breaks axe). Vitest+RTL **integration** is the workhorse layer (Testing Trophy), not isolated unit tests.
- **Frontend, when the audio/PWA features land:** Vitest **Browser Mode** (real browser) for the audio-player + Media-Session component wiring; **narrow** Playwright E2E (PWA install/offline/service-worker + Media-Session smoke).
- **Skip:** tarpaulin, quickcheck (proptest supersedes), loom (no hand-rolled lock-free code), Playwright component-testing (Browser Mode supersedes it), and any hard 100%-total gate.

### iOS reality (sharpens ADR-0009's manual-device rule)

**Playwright's bundled WebKit is not the Safari/iOS Safari Apple ships.** It smoke-tests Media-Session *wiring* and Safari-family rendering, but cannot reproduce iOS background-audio survival, lock-screen/Control-Center controls, or Add-to-Home-Screen install (iOS has no install event to automate). Those — the exact failures this project exists to fix ([ADR-0005](0005-client-audio-media-session-background.md)) — are a **real-iOS-device manual release gate**, not automatable. This is a required release gate, not optional.

### Enforcement timing

CI does not exist yet (ticket #22). Until it does, coverage + mutation join the **local merge-gate ritual** alongside `cargo fmt` / `clippy -D warnings` / tests. #22 then wires the full gate set (nextest, llvm-cov → Codecov flags, Vitest coverage, the patch gate, the `client/dist` build-artifact → Rust-jobs ordering for `rust-embed`, arm64 release matrix, sharded Playwright, nightly mutation).

## Amendment (#22, 2026-07-26): the patch gate runs in-repo, not on Codecov

Everything above stands except **where the patch gate runs**. `.github/workflows/ci.yml` enforces it with [`diff-cover`](../../.github/actions/patch-coverage/action.yml) over the very lcov the project floor is already measured from.

The reasoning that put the *floor* in-repo — "so the gate never depends on a third-party service's uptime" — turned out to apply to the patch gate too, and harder. Codecov's `patch` status is genuinely the best of its kind, but making it a **required** check means a PR cannot merge unless a third party is reachable *and* the repository has been linked there; a required check that never reports blocks every pull request rather than failing one, and the repository has no Codecov account. `diff-cover` reads the same lcov, resolves the diff with git, and answers on the runner. The `project`-status trend view and the `carryforward` flags are what this gives up; the blocking gate is what it buys.

Codecov remains a reasonable *addition* later, as an advisory upload for the trend UI. It is not a dependency of merging.

The other half of this ticket — **dual-dialect** — is `TEST_POSTGRES_URL` plus a `postgres:17` service, not the `testcontainers` crate named under "Backend, selective CI jobs". Same reason in a different key: `testcontainers` boots a container from inside the test process, and nextest runs process-per-test, so every developer would need a Docker daemon to run `cargo test` at all. The env-var seam keeps the everyday loop on SQLite and moves the whole suite with one variable. Real-S3 (MinIO/Garage) is still owed. See [`docs/agents/dual-dialect.md`](../agents/dual-dialect.md).

## Considered options

- **Hard 100%-total line-coverage gate** (the literal ask) — rejected: coverage theater, brittle tests, and the 90→100 grind's poor bug-yield outweigh the marginal confidence. Achievable in Rust only with heavy exclusions of derives/glue.
- **Going-forward-only (no backfill, floor = current)** — rejected as the baseline stance because it lets a known security gap (disabled-key auth is untested) and rich untested branches linger; a targeted hardening pass runs first (see the "Test hardening + coverage baseline" ticket).
- **Broad Playwright E2E across every flow** — rejected: slower, flakier, and gives false confidence exactly on the iOS-critical behaviors it cannot actually validate.

## Consequences

- New code cannot merge untested (patch gate); the project floor only climbs.
- Coverage % alone is never trusted as proof of quality — mutation testing is the correctness signal, so effort goes into assertions that bite, not lines that merely run.
- Exclusions are explicit and reviewable; a reviewer can audit every carve-out.
- The initial hardening pass (tooling + baseline + backfill of high-risk gaps) is upfront cost before feature velocity resumes — accepted.
- Mutation and full E2E are slow, so they run as advisory/nightly/CI jobs, never in the red-green inner loop.
- The iOS release gate stays human — accepted as intrinsic to the platform, not a tooling deficiency to fix later.
