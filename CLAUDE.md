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
- **rdio-scanner compatibility — as a floor, not a ceiling.** Figure out what features exist in rdio-scanner — all of them need to work in Radio-Scout. Upstream and downstream must exist and should be backwards compatible with rdio-scanner if at all possible. **But Radio-Scout must _improve_ on rdio, not clone it.** For every feature, first research how rdio does it, then research how to do it *better* — the goal is a superset that fixes rdio's weaknesses (see [Improve, don't clone](#improve-dont-clone-rdio)). Compatibility is preserved at the wire/contract boundaries (ingest response strings, recorder payloads, `/rdio-scanner` legacy surface); everything behind those boundaries is free to be better.
- **Recorder integrations.** Create an integration or plugin (per their docs) for both SDRTrunk and Trunk Recorder. The maintainer runs Trunk Recorder on their scanner, so have a plugin/integration ready for that testing phase.
- **Nothing is un-instrumented.** All application output goes through `tracing` — `println!`/`eprintln!`/`dbg!` are **denied by lint** in library and binary code ([ADR-0011](docs/adr/0011-observability-logging-policy.md)). Secrets are never logged at any level in any form; every rejected ingest logs *why*; every 5xx logs its cause against a correlation ref and returns only that ref. See [Logging policy](#logging-policy).
- **PWA / mobile support is extremely important.** You must be able to add the website to your phone and have scanner audio actually work correctly within the OS — e.g. functioning pause/next/previous buttons — and work correctly in the background, especially on iOS. This is lacking in rdio-scanner and is a big problem with it.

## Testing & coverage policy

Full rationale: [ADR-0009](docs/adr/0009-testing-strategy.md) (pyramid, integration harness, recorder golden suite) + [ADR-0010](docs/adr/0010-coverage-policy-and-test-tooling.md) (coverage numbers, tool stack). The rules that bind day-to-day work, symmetric for backend and frontend:

**Coverage gates:**
- **100% patch/diff coverage** on every PR — every new or changed line is tested. This is the hard gate; it makes "new code ships with tests" true by construction.
- A **ratcheting project floor** (enforced in-repo: `cargo llvm-cov --fail-under-lines`, Vitest `thresholds`) — rises, never falls. Current baselines: **backend ~96% lines → floor 90**; **frontend 100% lines / ~97% branches → floor 97 lines / 92 branches** (`client/vite.config.ts`, raised with #15).
- **No hard 100%-total gate** — it produces coverage theater. Quality is proven by mutation testing, not by chasing 100%.

**Edge cases are required and operationalized.** "Multiple tests covering edge cases" means `proptest` (property-based — parsers, dedup window, range headers, protocol framing), `rstest` parametrized case tables (multiple named cases per behavior), and `cargo-mutants` mutation testing to prove the assertions actually catch regressions. A test that runs a line without asserting behavior does not count.

**The pyramid (where each layer pays off):**
- **Backend** — unit (`#[cfg(test)] mod tests`, incl. edge-branch tables) for pure logic; **integration** (`tests/`, real HTTP/WS via the harness in `tests/common/`) for behavior + contracts. Dual-dialect Postgres + real-S3 (MinIO/Garage) via **testcontainers** in CI. rdio-scanner wire responses pinned with **insta** snapshots.
  - **The harness is `common::TestApp` (#21).** `TestApp::spawn()` / `TestApp::with_key("k")` brings up the real router on an ephemeral port over a temp SQLite DB + temp filesystem store **whose temp dir it owns** (no `_tmp` binding to keep alive), and carries the drive + assert helpers: `get`/`get_json`/`get_range`/`post_bytes`/`header_of`, `upload(CallUpload::new()…)` / `upload_tr(CallUpload::tr(meta))` for synthetic Calls in either recorder dialect, `count::<E>()`/`calls()`/`the_call()`/`seed_call`/`seed_system` for rows, `stored`/`object_keys`/`put_object` for stored audio, and `connect_ws()` + `subscribe`/`next_json`/`no_frame_within` for live-feed pushes. Non-default wiring is `TestApp::builder()`: `.ingest()`, `.heartbeat()`, `.store()` (S3), `.database_url()`. Its own tests are `tests/harness.rs` — read those first; they are the harness by example. **Build the plumbing into the harness, not into a test file:** a file-local shorthand that names the file's own domain (`recorder_app()`, a `form(…) -> CallUpload`) is fine and expected; a second hand-rolled `spawn`/multipart-builder/`reqwest::Client::new()` is the thing #21 deleted thirteen copies of.
  - **Dual-dialect is not done.** ADR-0009 wants the suite run against Postgres too; `.database_url()` is only the per-call-site seam. #22 still has to supply the container URL on the *default* path and give each test its own database or schema — the isolation `tests/harness.rs` asserts is something a SQLite-file-per-test gets for free.
- **Frontend** — Vitest + RTL **integration is the workhorse** (network mocked with **MSW** at the boundary — never fetch/module mocking); unit for pure logic (`store/`, `lib/`, `utils/` at per-file 100%); **Vitest Browser Mode** (real browser) for audio-player + Media-Session component wiring — *still owed*; **narrow Playwright E2E** (PWA install/offline/service-worker) — **wired up in #15**, `client/e2e/`, `npm run test:e2e`.
- **iOS background audio, lock-screen/Control-Center controls, and Add-to-Home-Screen install are a real-device MANUAL release gate.** Playwright's WebKit is not iOS Safari and cannot validate them ([ADR-0005](docs/adr/0005-client-audio-media-session-background.md)).

**Tooling** — backend: `cargo-nextest` (runner), `cargo-llvm-cov` (coverage), `proptest`, `rstest`, `insta`, `tokio::time::pause`, `assert_cmd`/`trycmd`; `cargo-mutants` + `testcontainers` in CI. Frontend: `@vitest/coverage-v8`, `msw`, `vitest-axe`, `jsdom`, `@playwright/test` (#15); Vitest Browser Mode when the audio-player component gets its real-browser layer. **Skip:** tarpaulin, quickcheck, loom, Playwright component-testing (Browser Mode supersedes it).

**Coverage exclusions (documented + auditable — never silent gaming):** generated SeaORM entities + migrations, `main()` bootstrap glue, `build.rs`, the `#[cfg(test)]`-only helpers in `src/testing.rs`, shadcn `client/src/components/ui/**`, `client/src/main.tsx`, `.d.ts`, test files. The same backend list is mirrored for mutation testing in [`.cargo/mutants.toml`](.cargo/mutants.toml), so `cargo mutants` reports only real gaps.

**Enforcement** — the tooling above is stood up (and high-risk gaps in already-shipped code backfilled) by the **"Test hardening + coverage baseline"** ticket; CI (#22) is not built yet. Until #22, coverage + mutation join the local merge-gate ritual: `cargo fmt --all`, `cargo clippy --all-targets -- -D warnings`, `cargo nextest run` (+ `cargo test --doc`), `cargo llvm-cov` over the floor, and the client `tsc`/`oxlint`/`vitest --coverage` gates must pass before a commit lands. #22 wires it all into CI with a **100% patch-coverage** Codecov gate (separate backend/frontend flags).

## Logging policy

Full rationale + the incident that bought these rules: [ADR-0011](docs/adr/0011-observability-logging-policy.md). The rules that bind day-to-day work — these are **hard rules**, not style preferences:

1. **`println!` / `eprintln!` / `dbg!` are denied by lint** — `[lints.clippy]` in `Cargo.toml`, so `cargo clippy --all-targets -- -D warnings` fails on a reintroduced one. Output goes through `tracing`, always. `examples/feed.rs` (a hand-run CLI whose stdout is its product) and the one test that reports being skipped carry a narrow `#![allow]` with a comment; `build.rs` needs none (cargo doesn't run clippy over build scripts).
2. **Never log a secret** — API keys, access codes, admin passwords or hashes — at any level, in any form, not even truncated. Identify a key by its database id or label, resolved after lookup. A credential the operator has no other copy of goes to **a file, never a log line**: first run writes its generated ingest key into `.env` — or into `<base_dir>/.env` when there is no env file to have read it from — created `0600`, with only the path logged (`src/startup.rs`).
3. **Every rejected ingest logs at WARN with a machine-readable `reason`** (`invalid-api-key`, `duplicate`, `blacklisted`, `no-talkgroup`, `not-populated`, plus the malformed-body family). A Call that doesn't become a row leaves a line saying why. Every rejection goes through one funnel (`ingest::rejected`, which writes the line and hands back the response the recorder gets) — a convention, not a guarantee, so a new rejection path has to use it. For the rdio 417s the slug **is** the wire detail with dashes for spaces (`no-talkgroup` ⇄ `Incomplete call data: no talkgroup`), one string rather than two that can drift — so renaming a slug rewrites a recorder-facing string, and every such body is pinned verbatim in `tests/instrumentation.rs`.
4. **Every 5xx logs at ERROR with the cause and a correlation ref**; the response body carries only `internal error (ref: …)`. A handler returns `failure::ServerError::new("<stage-slug>", err)`; the request middleware logs `stage=` + `cause=` against the request id (#28's, echoed as `x-request-id`) and replaces the body — for *any* 5xx, so a route that fails some other way is covered by construction. The rdio-compatible strings for *known* outcomes stay byte-identical — they're a wire contract.
5. **Listener IPs never appear above DEBUG.** Recorder IPs may appear at INFO on ingest routes. A public instance must not accumulate a record of who listened and when.
6. **Static messages, structured fields** — `warn!(reason = "blacklisted", %system_ref, "ingest dropped")`, never a formatted sentence.
7. **Levels mean something:** ERROR = an operator must act · WARN = something was rejected or dropped · INFO = notable normal events (startup, ingest outcome, one line per request) · DEBUG = per-asset/per-range requests, listener IPs, protocol detail · TRACE = wire dumps.
8. **Nothing logs unguarded in a hot loop.** Per-Call fine; per-range-request is DEBUG; per-sample never.

Output goes to **stdout only** (journald/Docker/terminal own persistence and rotation — never a file sink), initialised in `src/observability.rs` and coloured only when stdout is a terminal. `RUST_LOG` selects level *and* target; unset (or blank) means `info,sqlx::query=warn,sea_orm_migration=warn` — sqlx logs a line per statement at INFO, which on a Pi taking a Call a second is protocol detail belonging at DEBUG, and our own migration lines replace sea-orm's sentences. An unparseable `RUST_LOG` falls back to that default rather than refusing to boot. #17 adds a `[log]` section that sets the same string from config.

Applying a migration logs its name at INFO (`db::migrate`, one migration at a time so the line lands after the migration it reports); an already-current schema says so at DEBUG.

**Every HTTP request leaves one line** (`src/http_log.rs`, #28): `method`, `path`, `status`, `duration_us`, under a span carrying a 16-hex `request_id` that the response echoes as `x-request-id` — so a handler's own lines correlate, and the 5xx ref (#29) is the same id. The level is the **louder of the route's class and its outcome**: SPA assets, `/api/call/{id}/audio` and `/healthz` rest at DEBUG (chatty — a Pi must not write a line per range request or per probe), everything else at INFO, and a 4xx/5xx escalates to WARN/ERROR whatever the class. The **path only, never the query string** (rule 2 — access codes are a query parameter in ADR-0008's shape). The client address is the **TCP peer's, never `X-Forwarded-For`** (spoofable; #17's config owns trusted proxies), and rule 5 decides whether it rides: ingest lines always, everything else only on a line that is already DEBUG. The live feed logs its upgrade + connect/disconnect, never a frame.

## Improve, don't clone rdio

**Every feature is a chance to be better than rdio — take it.** rdio-scanner is the reference for *what* to build and the compatibility contract, never the ceiling for *how well*. The workflow for any ticket that touches an rdio-equivalent feature is:

1. **Research how rdio does it** — read the actual source in `rdio-scanner/` (and the recorders when relevant), not just the docs. Understand the behavior *and its weaknesses* (rdio's real pain points: DB-stored audio, proprietary JSON-over-WS, no background/lock-screen audio, dated UI, stale/half-open connections, missed calls across reconnects, no heartbeat of its own).
2. **Research how to do it better** — deliberately look for an improvement: robustness (heartbeat, reconnect catch-up, backpressure), performance (Pi-first), UX (mobile/PWA/background), or correctness. Cite the improvement in the ADR/PR/commit so the *why* is durable.
3. **Preserve compatibility only where it's a contract** — recorder-facing wire formats, response strings, and the legacy `/rdio-scanner` surface stay byte-compatible; internal protocols and storage are ours to improve (e.g. our own live-feed protocol per [ADR-0004](docs/adr/0004-live-feed-raw-websocket.md), object-storage audio per [ADR-0002](docs/adr/0002-audio-object-storage.md)).
4. **When an improvement is non-trivial or crosses an ADR boundary, surface the trade-off and get a decision** before building it — don't silently gold-plate, and don't silently settle for parity.

The bar for every feature is "measurably better than rdio for our users (Pi operators + mobile listeners)," not "matches rdio."

## Approach

- Start by doing deep research into rdio-scanner to figure out how it works — then, per [Improve, don't clone](#improve-dont-clone-rdio), research how to do it *better*. Agent-browser access is available, and a live instance of rdio-scanner runs at fultonscanner.com.
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
  --ignore-filename-regex '(db/entities/|db/migration\.rs|src/main\.rs|src/testing\.rs|build\.rs)'
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
npm run test:e2e            # Playwright: the PWA/service-worker/offline layer, over a real build
npm run lint                # oxlint
```

**Embedded UI:** the Rust binary serves `client/dist/` via `rust-embed` (`src/web.rs`), so **`npm run build` (in `client/`) must run before `cargo build`/`cargo test`** for the real UI to be served; without it the backend serves a minimal fallback page and the frontend-serving tests assert that fallback instead. `client/dist/` is gitignored; `build.rs` creates the (empty) folder so `rust-embed` compiles on a fresh checkout even before the frontend is built. CI (#22) runs the client build before the Rust build.

**End-to-end (Playwright) — wired up in #15.** `client/e2e/` + `client/playwright.config.ts`, run with `npm run test:e2e`. It is deliberately four specs, Chromium only, over a **production build served by `vite preview`** (the config builds first): jsdom has no service worker, no cache storage and no manifest processing, and the worker only exists in a build — so this is the only layer that can prove "installable + works offline". CI (#22) will shard it. Layering, per [Testing & coverage policy](#testing--coverage-policy):

- **Vitest Browser Mode** is the middle layer — real-browser component tests for the audio player + Media-Session *wiring*. Prefer it over Playwright component-testing. **Still owed** (#15 brought the browser tooling in but left this out: it re-tests #14's component wiring, not #15's features).
- **Narrow Playwright E2E** covers PWA install criteria, service-worker registration/scope, offline app-shell serving, and that the worker never answers `/api/*` from cache.
- **iOS background audio + lock-screen/Control-Center controls are a real-device manual gate** — Playwright's bundled WebKit is not iOS Safari and cannot validate them. There is no CI substitute. The executable checklist is **§14 of [`docs/research/ios-gap-bridging-mechanism.md`](docs/research/ios-gap-bridging-mechanism.md)**.

**PWA app icons** are rasterized from `client/icons/icon.svg` by `client/scripts/build-icons.sh` (macOS `sips`) into `client/public/`. The PNGs are committed, so neither the build nor CI runs it — re-run it by hand only when the mark changes.

## Live testing (real binary, real browser, real recorder)

The suites can't answer "does a Call actually arrive and make a sound". Full procedure: [`docs/agents/live-testing.md`](docs/agents/live-testing.md); `/live-test` runs it.

**Two instances, never mixed:** `./radio-scout-live-test` is hermetic and **wiped at the start of every scripted run** (empty archive, empty queue, fresh key — so what a test observes is a fact, not leftover history); `./radio-scout-data` is the durable one a real recorder uploads to. Both gitignored.

```bash
cp .env.example .env                           # once: set RADIO_SCOUT_API_KEY to anything random
cd client && npm run build && cd ..            # rust-embed reads client/dist AT COMPILE TIME
rm -rf ./radio-scout-live-test
RADIO_SCOUT_BASE_DIR=./radio-scout-live-test cargo run    # registers the key from .env
cargo run --example feed -- --interval 4s                 # synthetic Calls, real WAV tones
# browse http://localhost:3000  (phone: http://<MAC-LAN-IP>:3000)
```

**`.env` (stopgap until #17).** `RADIO_SCOUT_API_KEY` is registered on every boot, so a recorder's key survives restarts *and* a wiped database, and the feeder reads the same file — nothing gets copy-pasted. A real environment variable beats the file; with the key unset, first run **generates one and writes it into `.env`** (creating the file `0600` if it isn't there, and leaving every other setting in it untouched), then logs the path — never the key (ADR-0011 rule 2). So the key is `cat`-able after the scrollback is gone, and the next boot pins the same one. With no `.env` anywhere, it lands in `<base_dir>/.env` instead of the working directory, which under systemd/Docker is often read-only. If that write fails, no key is registered at all, so a retry actually retries. `.env` is gitignored, `.env.example` is the committed template, and a **disabled** key is never revived by re-registering it (ADR-0008). Every other knob there (`RADIO_SCOUT_BASE_DIR`, `_PORT`, `_RETENTION_*`) is the same pre-#17 env-var stopgap — #17 replaces the lot with TOML + CLI flags.

`examples/feed.rs` (key from `.env`) posts rdio-format multipart with **real audio** — a mono 16-bit WAV pitched by Talkgroup, so a wrong-Call bug is audible before it's visible. `--burst N` fills the listening queue, `--patches A:B` exercises patch fanout, `--seconds 8` gives the waveform something to walk.

**Browser:** live tests hit the **embedded build on `:3000`** (one origin, exactly what ships and what a phone hits), not the Vite dev server. Driving it needs the Claude browser extension installed, signed into the same account, and granted permission for `localhost:3000`. Never trigger an `alert`/`confirm` — a dialog freezes the extension until a human clears it — and read the console before calling anything a pass.

**Trunk Recorder:** Radio-Scout runs on the Mac; the Pi's TR gets a **second** `rdioscanner_uploader` entry beside the existing rdio-scanner one, so the real feed is untouched (verified in TR source: `plugin_manager.cc:41` loads every `plugins` entry, and each keeps its own config). `server` is a bare base URL — the plugin appends `/api/call-upload` itself, so it lands on the generic rdio endpoint (#5), not the TR-native one (#6). Config snippet, `talkgroupAllow` globs, and how to read TR's upload-error logs: see the doc.

**A live test supplements the suites, never replaces them** — fix what it finds test-first. And a desktop Chrome pass says nothing about iOS background audio, lock-screen controls, or Add-to-Home-Screen: those stay a real-device manual gate (ADR-0005).

## Agent skills

### Live testing

`/live-test` — build, launch hermetically, feed synthetic Calls, drive Chrome, report, tear down. See `docs/agents/live-testing.md`.

### Issue tracker

Issues are tracked in this repo's GitHub Issues via the `gh` CLI. External PRs are not a triage surface. See `docs/agents/issue-tracker.md`.

**Definition of done: committed, pushed, and closed** — all three, in the session that did the work. Finishing a ticket means `git push`, then `gh issue comment <n>` with what shipped (commit SHA, criteria met, anything left to a later ticket), then `gh issue close <n>`. A built-but-open ticket silently blocks every ticket behind it, because the frontier query treats open blockers as live gates. Conversely, **before starting a ticket, check whether it already landed** (`git log --oneline --grep '#<n>'`) — its open state is not proof it's unbuilt.

### Triage labels

Canonical label vocabulary (`needs-triage`, `needs-info`, `ready-for-agent`, `ready-for-human`, `wontfix`). See `docs/agents/triage-labels.md`.

### Domain docs

Single-context: one `CONTEXT.md` + `docs/adr/` at the repo root. See `docs/agents/domain.md`.
