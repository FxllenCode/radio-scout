# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

Radio-Scout is a full-stack, one-stop-shop application for listening to audio from Trunk Recorder and SDRTrunk. It is a replacement for rdio-scanner (which will remain accessible at `/rdio-scanner`). Every feature from rdio-scanner carries over, but optimized, with a beautiful UI.

The philosophy is a simple setup: a one-program install from the command line that just works. There will be a database (choice TBD via a grilling session) and possibly an object store for the audio. This application is likely to run on hardware as low as a Raspberry Pi, so it must be highly optimized, fast, and performant.

- **Backend:** Rust, entirely.
- **Frontend:** Vite + React (TypeScript) + TailwindCSS + shadcn, located in `client/`.

## Hard constraints

- **All development is Test-Driven Development, under a quantified coverage policy** — see [Testing & coverage policy](#testing--coverage-policy) below ([ADR-0009](docs/adr/0009-testing-strategy.md) + [ADR-0010](docs/adr/0010-coverage-policy-and-test-tooling.md)). CI is used heavily and is essential for deployment across targets (PC, Mac, Raspberry Pi); dev/testing happens on Mac, the target scanner runs on a Raspberry Pi 5. Red-green-refactor on **native tests** — Rust `cargo nextest` (unit + the in-process HTTP/WS integration harness) and Vitest + React Testing Library (frontend). Every PR must hold **100% patch/diff coverage** (every new/changed line tested) over a **ratcheting project floor**, with quality enforced by **mutation testing** (`cargo-mutants` + `proptest`) — *not* by a 100%-total gate. Reserve Playwright for browser-only flows; iOS background audio / lock-screen controls are a **real-device manual gate**.
- **Performance is first-class.** The app must be fast and performant on hardware as low as a Raspberry Pi.
- **Simple install.** A one-command install that just works.
- **rdio-scanner compatibility.** Figure out what features exist in rdio-scanner — all of them need to work in Radio-Scout. Upstream and downstream must exist and should be backwards compatible with rdio-scanner if at all possible.
- **Recorder integrations.** Create an integration or plugin (per their docs) for both SDRTrunk and Trunk Recorder. The maintainer runs Trunk Recorder on their scanner, so have a plugin/integration ready for that testing phase.
- **PWA / mobile support is extremely important.** You must be able to add the website to your phone and have scanner audio actually work correctly within the OS — e.g. functioning pause/next/previous buttons — and work correctly in the background, especially on iOS. This is lacking in rdio-scanner and is a big problem with it.

## Testing & coverage policy

Full rationale: [ADR-0009](docs/adr/0009-testing-strategy.md) (pyramid, integration harness, recorder golden suite) + [ADR-0010](docs/adr/0010-coverage-policy-and-test-tooling.md) (coverage numbers, tool stack). The rules that bind day-to-day work, symmetric for backend and frontend:

**Coverage gates:**
- **100% patch/diff coverage** on every PR — every new or changed line is tested. This is the hard gate; it makes "new code ships with tests" true by construction.
- A **ratcheting project floor** (enforced in-repo: `cargo llvm-cov --fail-under-lines`, Vitest `thresholds`) — rises, never falls. Current baselines: **backend ~92% lines → floor 90**; **frontend ~94% → floor 85 lines / 80 branches** (`client/vite.config.ts`).
- **No hard 100%-total gate** — it produces coverage theater. Quality is proven by mutation testing, not by chasing 100%.

**Edge cases are required and operationalized.** "Multiple tests covering edge cases" means `proptest` (property-based — parsers, dedup window, range headers, protocol framing), `rstest` parametrized case tables (multiple named cases per behavior), and `cargo-mutants` mutation testing to prove the assertions actually catch regressions. A test that runs a line without asserting behavior does not count.

**The pyramid (where each layer pays off):**
- **Backend** — unit (`#[cfg(test)] mod tests`, incl. edge-branch tables) for pure logic; **integration** (`tests/`, real HTTP/WS via the harness in `tests/common/mod.rs`) for behavior + contracts. Dual-dialect Postgres + real-S3 (MinIO/Garage) via **testcontainers** in CI. rdio-scanner wire responses pinned with **insta** snapshots.
- **Frontend** — Vitest + RTL **integration is the workhorse** (network mocked with **MSW** at the boundary — never fetch/module mocking); unit for pure logic (`store/`, `lib/`, `utils/` at per-file 100%); **Vitest Browser Mode** (real browser) for audio-player + Media-Session component wiring; **narrow Playwright E2E** (PWA install/offline/service-worker), added when those features land.
- **iOS background audio, lock-screen/Control-Center controls, and Add-to-Home-Screen install are a real-device MANUAL release gate.** Playwright's WebKit is not iOS Safari and cannot validate them ([ADR-0005](docs/adr/0005-client-audio-media-session-background.md)).

**Tooling** — backend: `cargo-nextest` (runner), `cargo-llvm-cov` (coverage), `proptest`, `rstest`, `insta`, `tokio::time::pause`, `assert_cmd`/`trycmd`; `cargo-mutants` + `testcontainers` in CI. Frontend: `@vitest/coverage-v8`, `msw`, `vitest-axe`, `jsdom`; Vitest Browser Mode + Playwright when audio/PWA lands. **Skip:** tarpaulin, quickcheck, loom, Playwright component-testing (Browser Mode supersedes it).

**Coverage exclusions (documented + auditable — never silent gaming):** generated SeaORM entities + migrations, `main()` bootstrap glue, `build.rs`, shadcn `client/src/components/ui/**`, `client/src/main.tsx`, `.d.ts`, test files.

**Enforcement** — the tooling above is stood up (and high-risk gaps in already-shipped code backfilled) by the **"Test hardening + coverage baseline"** ticket; CI (#22) is not built yet. Until #22, coverage + mutation join the local merge-gate ritual: `cargo fmt --all`, `cargo clippy --all-targets -- -D warnings`, `cargo nextest run` (+ `cargo test --doc`), `cargo llvm-cov` over the floor, and the client `tsc`/`oxlint`/`vitest --coverage` gates must pass before a commit lands. #22 wires it all into CI with a **100% patch-coverage** Codecov gate (separate backend/frontend flags).

## Approach

- Start by doing deep research into rdio-scanner to figure out how it works. Agent-browser access is available, and a live instance of rdio-scanner runs at fultonscanner.com.
- Do a grilling session at the start to design the project, and use Claude design to create mockups before beginning.
- For Rust, likely libraries include Socket.IO (oxide), Axum, and Tokio, among others; the exact set is settled during the grilling phase. Additional libraries may be added as needed.
- For TypeScript, Vite, TailwindCSS, and anything else helpful may be used; additional libraries may be added as needed.

## Reference projects (on disk, not part of this repo)

Three upstream projects are checked out at the repo root and gitignored: `rdio-scanner/`, `sdrtrunk/`, and `trunk-recorder/`. They are read-only reference material — do not build or edit them. Use them to reverse-engineer feature parity and integration contracts:

- **`rdio-scanner/`** — the app being replaced. Go server (`rdio-scanner/server/`) + Angular client (`rdio-scanner/client/`). Source of truth for feature parity, the ingest API (`rdio-scanner/docs/api.md`, the `/api/call-upload` contract), and the live-feed protocol. A live instance runs at fultonscanner.com.
- **`trunk-recorder/`** — C++ recorder the maintainer runs. The plugin to mirror is `trunk-recorder/plugins/rdioscanner_uploader/`.
- **`sdrtrunk/`** — Java SDR app. Its rdio-scanner output lives under `sdrtrunk/src/main/java/io/github/dsheirer/audio/broadcast/rdioscanner/`.

## Commands

Backend (Rust):

```bash
cargo build                 # build
cargo run                   # run the binary
cargo nextest run           # run all tests (preferred runner; `cargo test` still works)
cargo test --doc            # doctests (nextest does not run these)
cargo test <name>           # run tests matching a substring
cargo test <mod>::<test> -- --exact --nocapture   # single test, with stdout
cargo llvm-cov nextest --html                 # coverage report -> target/llvm-cov/html
# enforce the ratcheting project floor (exclude generated/glue code)
cargo llvm-cov nextest --fail-under-lines 90 \
  --ignore-filename-regex '(db/entities/|db/migration\.rs|src/main\.rs|build\.rs)'
cargo mutants --in-diff <(git diff origin/master...)   # mutation-test only changed code
cargo fmt                   # format
cargo clippy --all-targets  # lint
```

`cargo-nextest`, `cargo-llvm-cov`, and `cargo-mutants` are external binaries (`cargo install …`); `proptest`/`rstest`/`insta` are dev-deps. `testcontainers` (real Postgres/S3) lands with CI (#22). See [Testing & coverage policy](#testing--coverage-policy).

Frontend (React + TS + Vite + Tailwind v4 + shadcn/ui + Redux Toolkit/RTK Query), in `client/` — run from inside `client/`:

```bash
npm install                 # first-time setup
npm run dev                 # Vite dev server (proxies /api + /healthz + the WS to the backend on :3000)
npm run build               # type-check + production build to client/dist/ (embedded by the binary)
npm run typecheck           # tsc -b
npm run test                # Vitest + React Testing Library (single run)
npm run test:watch          # Vitest watch mode
npm run test:coverage       # Vitest with @vitest/coverage-v8 + thresholds (MSW at the network boundary)
npm run lint                # oxlint
```

**Embedded UI:** the Rust binary serves `client/dist/` via `rust-embed` (`src/web.rs`), so **`npm run build` (in `client/`) must run before `cargo build`/`cargo test`** for the real UI to be served; without it the backend serves a minimal fallback page and the frontend-serving tests assert that fallback instead. `client/dist/` is gitignored; `build.rs` creates the (empty) folder so `rust-embed` compiles on a fresh checkout even before the frontend is built. CI (#22) runs the client build before the Rust build.

**End-to-end (Playwright) — NOT wired up yet** (contrary to earlier notes, it is not installed). It is added when the browser-only flows exist (#11/#14/#15), as a `@playwright/test` `client/` devDependency run via `npx playwright test` next to the unit suite and in CI (sharded). Layering, per [Testing & coverage policy](#testing--coverage-policy):

- **Vitest Browser Mode** is the middle layer — real-browser component tests for the audio player + Media-Session *wiring*. Prefer it over Playwright component-testing.
- **Narrow Playwright E2E** covers PWA install/offline/service-worker + a Media-Session smoke test.
- **iOS background audio + lock-screen/Control-Center controls are a real-device manual gate** — Playwright's bundled WebKit is not iOS Safari and cannot validate them. There is no CI substitute.

## Agent skills

### Issue tracker

Issues are tracked in this repo's GitHub Issues via the `gh` CLI. External PRs are not a triage surface. See `docs/agents/issue-tracker.md`.

### Triage labels

Canonical label vocabulary (`needs-triage`, `needs-info`, `ready-for-agent`, `ready-for-human`, `wontfix`). See `docs/agents/triage-labels.md`.

### Domain docs

Single-context: one `CONTEXT.md` + `docs/adr/` at the repo root. See `docs/agents/domain.md`.
