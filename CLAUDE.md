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
- A **ratcheting project floor** (enforced in-repo: `cargo llvm-cov --fail-under-lines`, Vitest `thresholds`) — rises, never falls. Current baselines: **backend ~96% lines → floor 90**; **frontend 100% lines / ~97% branches → floor 99 lines / 94 branches** (`client/vite.config.ts`, raised with #16).
- **No hard 100%-total gate** — it produces coverage theater. Quality is proven by mutation testing, not by chasing 100%.

**Edge cases are required and operationalized.** "Multiple tests covering edge cases" means `proptest` (property-based — parsers, dedup window, range headers, protocol framing), `rstest` parametrized case tables (multiple named cases per behavior), and `cargo-mutants` mutation testing to prove the assertions actually catch regressions. A test that runs a line without asserting behavior does not count.

**The pyramid (where each layer pays off):**
- **Backend** — unit (`#[cfg(test)] mod tests`, incl. edge-branch tables) for pure logic; **integration** (`tests/`, real HTTP/WS via the harness in `tests/common/`) for behavior + contracts. **Dual-dialect Postgres** in CI (#22: a `postgres:17` service, a database per test); real-S3 (MinIO/Garage) is still owed. rdio-scanner wire responses pinned with **insta** snapshots.
  - **The harness is `common::TestApp` (#21).** `TestApp::spawn()` / `TestApp::with_key("k")` brings up the real router on an ephemeral port over a temp SQLite DB + temp filesystem store **whose temp dir it owns** (no `_tmp` binding to keep alive), and carries the drive + assert helpers: `get`/`get_json`/`get_range`/`post_bytes`/`header_of`, `upload(CallUpload::new()…)` / `upload_tr(CallUpload::tr(meta))` for synthetic Calls in either recorder dialect, `count::<E>()`/`calls()`/`the_call()`/`seed_call`/`seed_system` for rows, `stored`/`object_keys`/`put_object` for stored audio, `connect_ws()` + `subscribe`/`next_json`/`no_frame_within` for live-feed pushes, `login()`/`login_as()`/`post_admin_bytes`/`admin_request` for the admin surface (#19) over a **cookie-jar client** so a session behaves as it does in a browser, and — since #16 — `PushService::start()`, a **real stub push service** on an ephemeral port that records what arrives and can `decrypted()`/`payload()` it with the subscriber's own key, so a notification is asserted on as the bytes that actually left the process. Non-default wiring is `TestApp::builder()`: `.ingest()`, `.heartbeat()`, `.store()` (S3), `.database_url()`, `.trusted_proxies()` (with `upload_via_proxy` to send the `X-Forwarded-For` it judges), `.admin()` (a different session/lockout policy, or `AdminAuth::locked()`), `.push()` (a different coalescing window, or `Push::disabled()`). Its own tests are `tests/harness.rs` — read those first; they are the harness by example. **Build the plumbing into the harness, not into a test file:** a file-local shorthand that names the file's own domain (`recorder_app()`, a `form(…) -> CallUpload`) is fine and expected; a second hand-rolled `spawn`/multipart-builder/`reqwest::Client::new()` is the thing #21 deleted thirteen copies of.
  - **Dual-dialect is done (#22).** Set **`TEST_POSTGRES_URL`** and every `TestApp::spawn` in every binary lands on Postgres, each on a `rs_test_<uuid>` database of its own — the isolation a SQLite-file-per-test gets for free, bought. Unset (the everyday loop, any machine without Docker) stays SQLite, so nothing about local TDD changes. CI runs the suite twice and feeds both runs into one coverage profile. What differs between the dialects, and why `break_table` / `missing_table_cause` exist rather than a pinned SQLite string: [`docs/agents/dual-dialect.md`](docs/agents/dual-dialect.md). `.database_url()` remains the per-call-site override.
- **Frontend** — Vitest + RTL **integration is the workhorse** (network mocked with **MSW** at the boundary — never fetch/module mocking); unit for pure logic (`store/`, `lib/`, `utils/` at per-file 100%); **Vitest Browser Mode** (real browser) for audio-player + Media-Session component wiring — *still owed*; **narrow Playwright E2E** (PWA install/offline/service-worker) — **wired up in #15**, `client/e2e/`, `npm run test:e2e`.
- **iOS background audio, lock-screen/Control-Center controls, and Add-to-Home-Screen install are a real-device MANUAL release gate.** Playwright's WebKit is not iOS Safari and cannot validate them ([ADR-0005](docs/adr/0005-client-audio-media-session-background.md)).

**Tooling** — backend: `cargo-nextest` (runner), `cargo-llvm-cov` (coverage), `proptest`, `rstest`, `insta`, `tokio::time::pause`, `assert_cmd`/`trycmd`; `cargo-mutants` in CI (advisory on the diff, sharded nightly). Frontend: `@vitest/coverage-v8`, `msw`, `vitest-axe`, `jsdom`, `@playwright/test` (#15); Vitest Browser Mode when the audio-player component gets its real-browser layer. **Skip:** tarpaulin, quickcheck, loom, Playwright component-testing (Browser Mode supersedes it).

**Coverage exclusions (documented + auditable — never silent gaming):** generated SeaORM entities + migrations, `main()` bootstrap glue, `build.rs`, the `#[cfg(test)]`-only helpers in `src/testing.rs`, shadcn `client/src/components/ui/**`, `client/src/main.tsx`, **`client/src/sw.ts`** (a WebWorker scope jsdom cannot enter — its decisions live in `client/src/lib/pushMessage.ts` at 100%, and the glue is proven by a Playwright spec that delivers a real push through CDP), `.d.ts`, test files. The same backend list is mirrored for mutation testing in [`.cargo/mutants.toml`](.cargo/mutants.toml), so `cargo mutants` reports only real gaps.

**Enforcement** — the tooling is stood up (and high-risk gaps backfilled) by the **"Test hardening + coverage baseline"** ticket; **CI runs it (#22)**, `.github/workflows/ci.yml`. The same ritual still runs locally before a commit lands, because a red gate is cheaper to find here: `cargo fmt --all`, `cargo clippy --all-targets -- -D warnings`, `cargo nextest run` (+ `cargo test --doc`), `cargo llvm-cov` over the floor, and the client `tsc`/`oxlint`/`vitest --coverage` gates.

**What CI gates on** — **hard** (blocks merge): `cargo fmt --check`, `clippy -D warnings`, the full suite on **both dialects**, doctests, the backend floor (`--fail-under-lines`) and the frontend Vitest `thresholds`, and **100% patch coverage** on every changed line, backend and frontend separately. **Advisory** (annotates, never blocks): `cargo mutants --in-diff` on PRs, the Playwright PWA suite until it has a track record here, and the sharded nightly mutation sweep (`.github/workflows/nightly.yml`).

**`master` is protected**: `Client`, `Backend`, `Workflows` and the four `Build …` jobs are **required status checks**, strict (a branch must be up to date to merge), with force-pushes and deletion off. The patch-coverage gate runs *inside* `Client` and `Backend`, so requiring those requires it. `enforce_admins` is off, so the maintainer can still land an emergency fix — the gate is there to stop accidents, not to lock anyone out of their own repository. `v1` is deliberately unprotected: it is the working branch and takes direct pushes.

**The suite runs on the scanner's own architecture** (#38): a `Backend on arm64` job on `ubuntu-24.04-arm` runs `cargo nextest run` natively, because everything else runs on x86_64 and the build matrix only ever *compiles* for aarch64 — and the enhancement pipeline is float-heavy with per-architecture SIMD, so "passes on the runner, fails on the Pi" was a real and undetectable failure. Deliberately not coverage (the `Backend` job owns the one profile the floor and the patch gate are measured from) and not dual-dialect (that difference isn't architectural). It is a hard gate; adding it to `master`'s required checks is a branch-protection change and therefore the maintainer's.

**Still owed by CI**, deliberately, and tracked rather than forgotten: a **real-S3 (MinIO/Garage)** job for ADR-0002's storage half (#35), and the **Vitest Browser Mode** layer (#34).

Patch coverage is enforced **in-repo** by [`diff-cover`](.github/actions/patch-coverage/action.yml) over the lcov the floor is already measured from, not by Codecov as ADR-0010 pencilled in: a required check that depends on a third-party service being reachable *and* on the repository having been linked there blocks every PR when it is neither, rather than failing one. It is a pull-request gate — on a push there is no base to diff against.

The pipeline's own invisible-failure modes are pinned by **`tests/ci.rs`** (a `--lib` run silently skips the golden suite; a Rust job without the `client-dist` artifact asserts against the fallback page; a Postgres service whose URL never reaches the suite is a green run of SQLite twice) — asserting against the workflows **with their comments stripped**, because these files explain themselves at length and a test that greps the prose stays green after the flag it names is deleted. A `workflows` job runs **`actionlint`** over `.github/workflows` and **`shellcheck`** over `.github/scripts` — actionlint cannot see a composite action, so the one piece of hand-written shell lives in a script precisely so something can read it.

## Logging policy

Full rationale + the incident that bought these rules: [ADR-0011](docs/adr/0011-observability-logging-policy.md). The rules that bind day-to-day work — these are **hard rules**, not style preferences:

1. **`println!` / `eprintln!` / `dbg!` are denied by lint** — `[lints.clippy]` in `Cargo.toml`, so `cargo clippy --all-targets -- -D warnings` fails on a reintroduced one. Output goes through `tracing`, always. There are exactly **three** narrow `#[allow]`s, each with a comment saying why, and each a command whose stdout *is* its product: `examples/feed.rs` (a hand-run CLI), the one test that reports being skipped, and `service::show` (#23 — `service … --print` emits the unit file it would write, and a document interleaved with timestamps is not one). `build.rs` needs none (cargo doesn't run clippy over build scripts).
2. **Never log a secret** — API keys, access codes, admin passwords or hashes — at any level, in any form, not even truncated. Identify a key by its database id or label, resolved after lookup. A credential the operator has no other copy of goes to **a file, never a log line**: first run writes its generated ingest key into `.env` — or into `<base_dir>/.env` when there is no env file to have read it from — created `0600`, with only the path logged (`src/startup.rs`).
3. **Every rejected ingest logs at WARN with a machine-readable `reason`** (`invalid-api-key`, `duplicate`, `blacklisted`, `no-talkgroup`, `not-populated`, plus the malformed-body family). A Call that doesn't become a row leaves a line saying why. Every rejection goes through one funnel (`ingest::rejected`, which writes the line and hands back the response the recorder gets) — a convention, not a guarantee, so a new rejection path has to use it. For the rdio 417s the slug **is** the wire detail with dashes for spaces (`no-talkgroup` ⇄ `Incomplete call data: no talkgroup`), one string rather than two that can drift — so renaming a slug rewrites a recorder-facing string, and every such body is pinned verbatim in `tests/instrumentation.rs`.
4. **Every 5xx logs at ERROR with the cause and a correlation ref**; the response body carries only `internal error (ref: …)`. A handler returns `failure::ServerError::new("<stage-slug>", err)`; the request middleware logs `stage=` + `cause=` against the request id (#28's, echoed as `x-request-id`) and replaces the body — for *any* 5xx, so a route that fails some other way is covered by construction. The rdio-compatible strings for *known* outcomes stay byte-identical — they're a wire contract.
5. **Listener IPs never appear above DEBUG** — and neither does a **push endpoint** (#16), which is a stable per-device identifier and therefore worse: a subscription is named in logs by its database **Id**, the push *service* may be named on a failure, and the transport error is logged through `without_url()` because reqwest's own `Display` would otherwise carry the endpoint in through the back door. Recorder IPs may appear at INFO on ingest routes. **Admin *authentication attempts* may name their source at WARN** (#19) — a refused login is unactionable without an address to firewall, and it is not a record of who listened; the exemption covers `admin::login`'s own lines only, never the `/api/admin/*` request line. A public instance must not accumulate a record of who listened and when.
6. **Static messages, structured fields** — `warn!(reason = "blacklisted", %system_ref, "ingest dropped")`, never a formatted sentence.
7. **Levels mean something:** ERROR = an operator must act · WARN = something was rejected or dropped · INFO = notable normal events (startup, ingest outcome, one line per request) · DEBUG = per-asset/per-range requests, listener IPs, protocol detail · TRACE = wire dumps.
8. **Nothing logs unguarded in a hot loop.** Per-Call fine; per-range-request is DEBUG; per-sample never.

Output goes to **stdout only** (journald/Docker/terminal own persistence and rotation — never a file sink), initialised in `src/observability.rs` and coloured only when stdout is a terminal. The filter selects level *and* target and comes from `--log` / `RUST_LOG` / `[log] directives` / the default `info,sqlx::query=warn,sea_orm_migration=warn` (#17, in that order) — sqlx logs a line per statement at INFO, which on a Pi taking a Call a second is protocol detail belonging at DEBUG, and our own migration lines replace sea-orm's sentences. Directives are **validated at boot**: a filter `tracing` can't parse refuses to start and names the layer it came from, because an operator who asked for TRACE and silently got INFO debugs the wrong log (`observability::subscriber` still falls back to the default as a last resort — a subscriber that fails to build would mean silence).

Applying a migration logs its name at INFO (`db::migrate`, one migration at a time so the line lands after the migration it reports); an already-current schema says so at DEBUG.

**Every HTTP request leaves one line** (`src/http_log.rs`, #28): `method`, `path`, `status`, `duration_us`, under a span carrying a 16-hex `request_id` that the response echoes as `x-request-id` — so a handler's own lines correlate, and the 5xx ref (#29) is the same id. The level is the **louder of the route's class and its outcome**: SPA assets, `/api/call/{id}/audio` and `/healthz` rest at DEBUG (chatty — a Pi must not write a line per range request or per probe), everything else at INFO, and a 4xx/5xx escalates to WARN/ERROR whatever the class. The **path only, never the query string** (rule 2 — access codes are a query parameter in ADR-0008's shape). The client address is the **TCP peer's** unless the operator named that peer in `[server] trusted_proxies` (#17) — the header is spoofable, so with the list empty (what ships) it is never read; when the peer is trusted, the address is the **rightmost entry of `X-Forwarded-For` that isn't itself a trusted proxy** — and rule 5 decides whether it rides: ingest lines always, everything else only on a line that is already DEBUG. The live feed logs its upgrade + connect/disconnect, never a frame.

## Configuration

Full rationale: [ADR-0012](docs/adr/0012-configuration-model.md). One `Config` (`src/config.rs`), resolved once at boot, from four layers — **CLI flag > environment variable > `radio-scout.toml` > default**, loudest first. rdio-scanner has this backwards: `flag.Parse()` runs first and the INI is then loaded *over* the flags (`server/config.go:96-137`), so a flag cannot override a configured value.

- **The file** is sectioned — `[server] [database] [storage] [storage.s3] [retention] [ingest] [admin] [push] [log]` — found via `--config`, then `RADIO_SCOUT_CONFIG`, then the working directory. **No file is not an error**: zero-config first run creates `base_dir`, a SQLite database and a filesystem audio store, and serves (spec US 35).
- **`radio-scout --write-config`** writes a commented file with every setting at its default, and refuses to overwrite one. It is the reference an operator reads; two tests hold it to being true — it must parse back to exactly `Config::default()`, and it must show every key the defaults serialize to, so a setting added without a template line fails the suite.
- **Strict validation.** An unknown key, an unparseable value from any layer, missing S3 credentials for `storage.backend = "s3"`, a zero `retention.batch_size`, a non-positive `max_size_gb`, a negative dedup window, a zero in any of `[admin]`'s four windows (each of which would brick the admin surface rather than merely behave oddly), or log directives `tracing` can't parse — each **refuses to boot**, exits `2`, and names the source, the value and what was expected. rdio silently ignores both the unknown key and a file that fails to load. (`1` means the start itself failed, e.g. a bound port.)
- **Three credentials stay out of the TOML entirely** because first run *writes* them: `RADIO_SCOUT_API_KEY`, `RADIO_SCOUT_ADMIN_PASSWORD` (#19) and — since #16 — `RADIO_SCOUT_VAPID_PRIVATE_KEY`, all generated into `.env` (`0600`) with only the path logged. Radio-Scout ships **no default admin password**; rdio ships a known one behind a nag. Everything about the admin surface that is a knob rather than a secret is `[admin]`: `session_idle_secs` (refreshed by use), `session_max_secs` (never), `lockout_attempts`, `lockout_secs`. The Web Push identity differs from the other two in one way that matters: it must be the **same** key next boot, because a browser pins its public half when it subscribes — so a key that can't be saved (or can't be parsed) leaves notifications **off** with an ERROR rather than running on one that won't survive a restart. Its knobs are `[push]`: `coalesce_secs`, `ttl_secs`, `subject` (a `mailto:`/`https:` contact URI — anything else refuses to boot).
- **Secrets never reach a log line** (ADR-0011 rule 2). `[storage.s3]`'s credentials have no flags — `ps` is world-readable — and come from the file or the environment. The startup summary names the database *dialect* and the storage *backend*, never the URL or the key; `config::S3`'s `Debug` redacts the secret; and a TOML **parse error** reports position + message but never the source line, because `toml::de::Error`'s `Display` quotes the offending line verbatim and the line an operator mistypes is the one they were editing.
- **`[server] trusted_proxies`** (#28's deferred setting) is a list of addresses **and CIDR blocks** — Docker's bridge is a subnet. See the [Logging policy](#logging-policy) for which entry of the chain is believed and why.
- **Boot says where its configuration came from** — the file it read, or that there wasn't one — then the settings that resulted, so "why isn't my setting applying?" has an answer in the log.
- **Every setting has both spellings**, a TOML key and a `RADIO_SCOUT_*` variable (`.env.example` lists them); `main.rs` stays thin because it is excluded from coverage — every decision worth testing is in `config.rs`.
- **`[enhancement]`** (#20, spec US 33-34) is `mode` (`off` — what ships — / `normalize` / `denoise`), `output`, `target_lufs` and `queue_depth`. It carries *policy* only; **scope** is `systems.enhancement` / `talkgroups.enhancement`, nullable so `NULL` inherits — the auto-populate precedent (#8), because a file naming Refs goes stale the moment a recorder finds a new one. `output = "opus"` parses and then **refuses to boot** naming #23: an unbuilt option must never quietly write a different format than the operator asked for.

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
cargo run                   # run the binary (zero-config: creates ./radio-scout-data)
cargo run -- --help         # every flag (#17); --write-config writes a commented radio-scout.toml
cargo run -- service install --print   # (#23) the unit/plist/task it would write, and nothing else
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

# The dual-dialect run (#22): set TEST_POSTGRES_URL and the WHOLE suite moves to
# Postgres, a database per test. Unset = SQLite. docs/agents/dual-dialect.md.
docker run -d --name rs-pg -e POSTGRES_PASSWORD=postgres -e POSTGRES_USER=postgres \
  -e POSTGRES_DB=postgres -p 55432:5432 postgres:17-alpine
TEST_POSTGRES_URL='postgres://postgres:postgres@localhost:55432/postgres' cargo nextest run
docker rm -f rs-pg          # the per-test databases are not dropped; the server is
```

`cargo-nextest`, `cargo-llvm-cov`, and `cargo-mutants` are external binaries (`cargo install …`); `proptest`/`rstest`/`insta` are dev-deps. The dual-dialect run needs only a Postgres to point `TEST_POSTGRES_URL` at ([`docs/agents/dual-dialect.md`](docs/agents/dual-dialect.md)); real-S3 (MinIO/Garage) coverage is still owed. See [Testing & coverage policy](#testing--coverage-policy).

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

**Embedded UI:** the Rust binary serves `client/dist/` via `rust-embed` (`src/web.rs`), so **`npm run build` (in `client/`) must run before `cargo build`/`cargo test`** for the real UI to be served; without it the backend serves a minimal fallback page and the frontend-serving tests assert that fallback instead. `client/dist/` is gitignored; `build.rs` creates the (empty) folder so `rust-embed` compiles on a fresh checkout even before the frontend is built. CI does this by building the SPA once in its `client` job and downloading it into every job that runs cargo (#22) — the artifact is a build input, not an output beside them.

**End-to-end (Playwright) — wired up in #15.** `client/e2e/` + `client/playwright.config.ts`, run with `npm run test:e2e`. It is deliberately five specs, Chromium only, over a **production build served by `vite preview`** (the config builds first): jsdom has no service worker, no cache storage and no manifest processing, and the worker only exists in a build — so this is the only layer that can prove "installable + works offline". CI runs it as an advisory job (#22) until it has a track record there. Layering, per [Testing & coverage policy](#testing--coverage-policy):

- **Vitest Browser Mode** is the middle layer — real-browser component tests for the audio player + Media-Session *wiring*. Prefer it over Playwright component-testing. **Still owed** (#15 brought the browser tooling in but left this out: it re-tests #14's component wiring, not #15's features).
- **Narrow Playwright E2E** covers PWA install criteria, service-worker registration/scope, offline app-shell serving, that the worker never answers `/api/*` from cache, and — since #16 — that a delivered push becomes a notification (`e2e/push.spec.ts`, via `ServiceWorker.deliverPushMessage` over CDP).
- **iOS background audio + lock-screen/Control-Center controls are a real-device manual gate** — Playwright's bundled WebKit is not iOS Safari and cannot validate them. There is no CI substitute. The executable checklist is **§14 of [`docs/research/ios-gap-bridging-mechanism.md`](docs/research/ios-gap-bridging-mechanism.md)**.

**PWA app icons** are rasterized from `client/icons/icon.svg` by `client/scripts/build-icons.sh` (macOS `sips`) into `client/public/`. The PNGs are committed, so neither the build nor CI runs it — re-run it by hand only when the mark changes.

## Packaging & release

Full rationale: [ADR-0007](docs/adr/0007-single-binary-embedded-frontend-distribution.md) + its #23 amendment; the operator-facing guide is [`docs/deploy.md`](docs/deploy.md). What binds day-to-day work:

- **One asset name, three consumers.** `radio-scout-<tag>-<target>.tar.gz` (`.zip` on Windows) is built by `.github/workflows/release.yml`, fetched by `install.sh`, and documented in `docs/deploy.md`. Nothing connects them at runtime, so **`tests/packaging.rs` does**: it parses the release matrix, and *runs the installer for real* against a release served over `file://` — fetch, checksum, unpack, install, and a binary that executes afterwards. Add a target to the matrix and the installer has to be able to ask for it; break the checksum and the test that proves nothing gets installed fails.
- **Linux ships static musl, built natively.** No cross-compilation anywhere: musl inside `rust:alpine` (arm64 on `ubuntu-24.04-arm`), macOS on macOS, Windows on Windows. A glibc binary would not start on the Raspberry Pi OS most Pis run.
- **`[profile.release]` exists and is deliberate** — `lto = "fat"`, one codegen unit, `strip = "debuginfo"` (symbols stay, so a panic on someone's Pi arrives as function names), and `panic = "unwind"` spelled out because `"abort"` would let one bad request take the whole server down. `ci.yml` builds every target in **debug**; `release.yml` is the only place `--release` runs.
- **`radio-scout service …`** (`src/service.rs`) is `install`/`uninstall`/`start`/`stop`/`restart`/`status` over systemd, launchd and a Windows scheduled task. **The platform is a value, not a `cfg`**: `Manager::plan(action, params)` returns a `Plan` of `Write`/`Remove`/`Run` steps, so every platform's unit file, plist and task XML renders — and is snapshot-tested — wherever the suite runs. `--print` shows exactly that plan and touches nothing. The flags given to `install` are baked into the definition (absolutised), and `--database-url` is **refused** rather than written into a world-readable file.
- **The image packages, it does not build.** `docker/Dockerfile` is `FROM scratch` around the binaries the release already produced — so the image and the release are the same bytes. Building from source is `cargo build --release`.
- **Cutting a release is a `v*` tag**, and the tag must match `Cargo.toml`'s version or the workflow refuses. `workflow_dispatch` builds everything and publishes nothing, so the pipeline can be exercised without cutting one.

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

**`.env` — the environment layer, and the ingest key's home.** `RADIO_SCOUT_API_KEY` is registered on every boot, so a recorder's key survives restarts *and* a wiped database, and the feeder reads the same file — nothing gets copy-pasted. A real environment variable beats the file; with the key unset, first run **generates one and writes it into `.env`** (creating the file `0600` if it isn't there, and leaving every other setting in it untouched), then logs the path — never the key (ADR-0011 rule 2). So the key is `cat`-able after the scrollback is gone, and the next boot pins the same one. With no `.env` anywhere, it lands in `<base_dir>/.env` instead of the working directory, which under systemd/Docker is often read-only. If that write fails, no key is registered at all, so a retry actually retries. `.env` is gitignored, `.env.example` is the committed template, and a **disabled** key is never revived by re-registering it (ADR-0008). The key lives here rather than in `radio-scout.toml` because first run *writes* it; every other knob is the environment layer of [Configuration](#configuration) and has a TOML key too.

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

### Dual-dialect testing

`TEST_POSTGRES_URL` moves the whole suite onto Postgres, a database per test. See `docs/agents/dual-dialect.md`.

### Triage labels

Canonical label vocabulary (`needs-triage`, `needs-info`, `ready-for-agent`, `ready-for-human`, `wontfix`). See `docs/agents/triage-labels.md`.

### Domain docs

Single-context: one `CONTEXT.md` + `docs/adr/` at the repo root. See `docs/agents/domain.md`.
