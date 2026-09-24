# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

Radio-Scout is a full-stack, one-stop-shop application for listening to audio from Trunk Recorder and SDRTrunk. It is a replacement for rdio-scanner. Every feature from rdio-scanner carries over, but optimized, with a beautiful UI.

**`/rdio-scanner` is not served, and is not v1 scope.** Hosting the legacy Angular app is on the spec's "later" list (#24), so nothing should be built against the assumption that it exists — the compatibility promise lives at the *ingest* boundary (wire formats, response strings), not at a legacy URL. `README.md` says so publicly, under "What is missing".

The philosophy is a simple setup: a one-program install from the command line that just works. There will be a database (choice TBD via a grilling session) and possibly an object store for the audio. This application is likely to run on hardware as low as a Raspberry Pi, so it must be highly optimized, fast, and performant.

- **Backend:** Rust, entirely.
- **Frontend:** Vite + React (TypeScript) + TailwindCSS + shadcn, located in `client/`.

## Where the documentation lives

Three readers, and a document is written for exactly one of them. Keeping a fact in the wrong reader's document is how it goes stale — an Operator never reads `docs/agents/`, so a truth kept only there is not published.

| Reader | Documents |
| --- | --- |
| **Operator** — installs and runs an Instance | [`README.md`](README.md) (front door + happy path), [`docs/deploy.md`](docs/deploy.md) (every install path, service, Docker), [`docs/recorders.md`](docs/recorders.md) (Trunk Recorder + SDRTrunk), [`docs/operating.md`](docs/operating.md) (storage, retention, enhancement, admin, logging), [`docs/migrating-from-rdio-scanner.md`](docs/migrating-from-rdio-scanner.md), [`plugins/trunk-recorder/README.md`](plugins/trunk-recorder/README.md) (ships *inside* the plugin tarball, so it is the one operator document that travels away from the repository — it points at `docs/recorders.md` rather than restating it) |
| **Listener** — uses the app | [`docs/using.md`](docs/using.md) |
| **Contributor** — this file's reader | `CLAUDE.md`, [`CONTEXT.md`](CONTEXT.md), [`docs/adr/`](docs/adr/), [`docs/agents/`](docs/agents/), [`docs/spec/`](docs/spec/) |

**`tests/docs.rs` gates the operator-facing set**: every `RADIO_SCOUT_*` they name must be one `src/` reads, the README's platform table must match the release matrix, the `curl | sh` URL must match `install.sh`'s own repository slug, and every relative link and image must resolve. Prose is not gated — this pins the facts that drift, not the writing. ADRs are deliberately excluded: an ADR is a dated record and is *supposed* to keep saying what it said at the time. It also holds this file to its budget, and this file's and the agent docs' links to resolving.

`--write-config` remains the settings reference (two tests hold it complete), so `docs/operating.md` explains *whether you want* a setting and never restates the list.

**This file holds only rules that bind work in any module.** It loads into every session, so every character here is paid for before any work starts, and `tests/docs.rs` fails it above 40,000 characters. **A ticket's rationale goes in its module's `//!` (or `/** */`) header** — why *this* module is shaped the way it is, what rdio does differently, what review found — **or in an ADR if it overrides the spec or its own ticket.** A lesson that genuinely applies everywhere earns one line under [Lessons that apply everywhere](#lessons-that-apply-everywhere), never a paragraph. Harness and CI detail lives in [`docs/agents/testing.md`](docs/agents/testing.md) and [`docs/agents/ci.md`](docs/agents/ci.md).

## Hard constraints

- **A ticket is a claim, not an instruction — grill before you build.** Before the first test of any ticket, say three things: what the ticket asserts, what you intend to build, and what you are unsure of. **If anything is genuinely open, ask before building it** — and *a claim in the ticket that looks wrong is an open question*, not a detail to work around. If nothing is open, say so explicitly and name the assumptions you are proceeding on. **Never resolve an open question by guessing**, and never discover the answer only in review.

  This is deliberately conditional. Plenty of tickets are mechanical and grilling them wastes everyone's attention, which is how a maintainer is trained to rubber-stamp the ones that matter. Two tools, two situations: `AskUserQuestion` for a choice between options; **`/grill-with-docs`** when the answer is a durable decision, because it is the stateful one and leaves the trail in [`CONTEXT.md`](CONTEXT.md) or an ADR.

- **No transcription. Banned, permanently.** Radio-Scout does not transcribe audio — no speech-to-text in any form: no local model, no cloud API, no plugin hook, no "experimental" flag. Features must never depend on transcripts existing (so no keyword alerts, no transcript search, no transcript-derived geolocation). This is a maintainer decision (2026-07-29), made knowing transcription is the community's top-requested feature — do not re-propose it. Non-speech audio DSP (enhancement, tone detection) is a separate question and not covered by this ban.

- **No notifications. Banned, permanently ([ADR-0014](docs/adr/0014-no-notifications.md)).** Radio-Scout does not wake a device — no Web Push, no device notifications, no permission prompt, no subscription, and no code, credential, configuration or dependency kept against their return. **"Alert" is not a word this project uses**: it was defined as a notification delivered by Web Push, and delivery is what was removed. What a Recorder proves about a transmission stays a **mark on a Call** — the **Emergency** bit (#42), a tone-out match (#55) — shown, filtered and searched on, delivered to nobody. This is a maintainer decision (2026-08-16), taken while grilling #53, on a feature that shipped in 0.1.0 and worked; that it worked is not grounds to reopen it. **Webhooks (#54) are not covered, and shipped** — an Operator wiring a URL to receive their own flagged Calls is arranging their own inbox, and sits beside **Downstream**. What that ticket may never grow is a Listener-facing half: no subscription, no device, no permission prompt. The removal itself is #107, which is also where the reasoning and everything it cost are written down.

- **All development is Test-Driven Development, under a quantified coverage policy** — see [Testing & coverage policy](#testing--coverage-policy) below ([ADR-0009](docs/adr/0009-testing-strategy.md) + [ADR-0010](docs/adr/0010-coverage-policy-and-test-tooling.md)). CI is used heavily and is essential for deployment across targets (PC, Mac, Raspberry Pi); dev/testing happens on Mac, the target scanner runs on a Raspberry Pi 5. Red-green-refactor on **native tests** — Rust `cargo nextest` (unit + the in-process HTTP/WS integration harness) and Vitest + React Testing Library (frontend). Every PR must hold **100% patch/diff coverage** (every new/changed line tested) over a **ratcheting project floor**, with quality enforced by **mutation testing** (`cargo-mutants` + `proptest`) — *not* by a 100%-total gate. Reserve Playwright for browser-only flows; iOS background audio / lock-screen controls are a **real-device manual gate**.
- **Performance is first-class.** The app must be fast and performant on hardware as low as a Raspberry Pi.
- **Simple install.** A one-command install that just works.
- **rdio-scanner compatibility — as a floor, not a ceiling.** Figure out what features exist in rdio-scanner — all of them need to work in Radio-Scout. Upstream and downstream must exist and should be backwards compatible with rdio-scanner if at all possible. **But Radio-Scout must _improve_ on rdio, not clone it.** For every feature, first research how rdio does it, then research how to do it *better* — the goal is a superset that fixes rdio's weaknesses (see [Improve, don't clone](#improve-dont-clone-rdio)). Compatibility is preserved at the wire/contract boundaries (ingest response strings, recorder payloads, and the `/rdio-scanner` legacy surface *if it is ever built*); everything behind those boundaries is free to be better.
- **Recorder integrations.** Create an integration or plugin (per their docs) for both SDRTrunk and Trunk Recorder. The maintainer runs Trunk Recorder on their scanner, so have a plugin/integration ready for that testing phase.

  **Trunk Recorder has two shipped ways in, and a feature goes into both or neither.** `radio-scout-upload.sh` (#43) and `plugins/trunk-recorder/` (#44) post the same contract, and `tests/trplugin.rs::the_upload_script_and_the_plugin_land_the_identical_call` compares the rows they land, whole. A divergence that genuinely belongs to one of them is argued in that test, never worked around it ([`docs/agents/testing.md`](docs/agents/testing.md#recorder-artifacts)).

  **SDRTrunk cannot have a plugin, and does not need one (#48)** — **Mining** is its integration; the design is in `src/mining/mod.rs` and `src/mining/sweep.rs`.

- **Nothing is un-instrumented.** All application output goes through `tracing` — `println!`/`eprintln!`/`dbg!` are **denied by lint** in library and binary code ([ADR-0011](docs/adr/0011-observability-logging-policy.md)). Secrets are never logged at any level in any form; every rejected ingest logs *why*; every 5xx logs its cause against the request id and returns only that id. See [Logging policy](#logging-policy).
- **PWA / mobile support is extremely important.** You must be able to add the website to your phone and have scanner audio actually work correctly within the OS — e.g. functioning pause/next/previous buttons — and work correctly in the background, especially on iOS. This is lacking in rdio-scanner and is a big problem with it.

## Testing & coverage policy

Full rationale: [ADR-0009](docs/adr/0009-testing-strategy.md) (pyramid, integration harness, recorder golden suite) + [ADR-0010](docs/adr/0010-coverage-policy-and-test-tooling.md) (coverage numbers, tool stack). The rules that bind day-to-day work, symmetric for backend and frontend:

**Coverage gates:**
- **100% patch/diff coverage** on every PR — every new or changed line is tested. This is the hard gate; it makes "new code ships with tests" true by construction.
- A **ratcheting project floor** (enforced in-repo: `cargo llvm-cov --fail-under-lines`, Vitest `thresholds`) — rises, never falls. Current baselines: **backend ~96% lines → floor 90**; **frontend 100% lines / ~97% branches → floor 99 lines / 94 branches** (`client/vite.config.ts`, raised with #16).
- **No hard 100%-total gate** — it produces coverage theater. Quality is proven by mutation testing, not by chasing 100%.

**Edge cases are required and operationalized.** "Multiple tests covering edge cases" means `proptest` (property-based — parsers, dedup window, range headers, protocol framing), `rstest` parametrized case tables (multiple named cases per behavior), and `cargo-mutants` mutation testing to prove the assertions actually catch regressions. A test that runs a line without asserting behavior does not count.

**The pyramid (where each layer pays off):**
- **Backend** — unit (`#[cfg(test)] mod tests`, incl. edge-branch tables) for pure logic; **integration** (`tests/`, real HTTP/WS via the harness in `tests/common/`) for behavior + contracts. **Dual-dialect Postgres** in CI (#22: a `postgres:17` service, a database per test) and **real S3** (#35: MinIO in `Backend`, Garage in a job of its own, a bucket per test). rdio-scanner wire responses pinned with **insta** snapshots.
- **Frontend** — Vitest + RTL **integration is the workhorse** (network mocked with **MSW** at the boundary — never fetch/module mocking); unit for pure logic (`store/`, `lib/`, `utils/` at per-file 100%); **Vitest Browser Mode** (real browser) for audio-player + Media-Session component wiring — **wired up in #34**, `client/src/**/*.browser.test.tsx`, `npm run test:browser`; **narrow Playwright E2E** (PWA install/offline/service-worker) — **wired up in #15**, `client/e2e/`, `npm run test:e2e`.
- **iOS background audio, lock-screen/Control-Center controls, and Add-to-Home-Screen install are a real-device MANUAL release gate.** Playwright's WebKit is not iOS Safari and cannot validate them ([ADR-0005](docs/adr/0005-client-audio-media-session-background.md)).

**The machinery** is [`docs/agents/testing.md`](docs/agents/testing.md). The rules it enforces:

- **Every integration test drives `common::TestApp`**, which starts the same Instance the binary boots. Build new plumbing into the harness, never a second hand-rolled `spawn` or multipart builder in a test file; a file-local shorthand naming that file's own domain is fine.
- **The suite waits with `app.settle()`, and nothing else.** A sleep can only say "not yet". If `settle()` cannot see some new asynchrony, the fix is that the new thing is a **Worker**.
- **A fault is injected at the module's own interface** (`blob::AudioStore`, `db::Db`, `enhance::Archive`). Never damage the thing underneath, and never encode a caller's call order in the fault machinery.
- **Both dialects, always.** `TEST_POSTGRES_URL` moves the whole suite to Postgres ([`docs/agents/dual-dialect.md`](docs/agents/dual-dialect.md)). A SQLite-only loop cannot see a Postgres failure, so run it before calling database work done.
- **What runs on a recorder is tested by running it**, never by asserting against a committed fixture of what it is supposed to emit.
- **An equivalent mutant is excluded one at a time, with its proof written beside it** in `.cargo/mutants.toml`. Never exclude a whole function or file.

### Lessons that apply everywhere

Each was paid for once, in a module whose header tells the story.

- **Decide purely, then perform.** A protocol, a policy or a state machine is a function from values to values (`live::Connection::on`, `ingest::admit`, `lib/run.ts`'s `advance`), and the socket, database or screen is an adapter around it.
- **A policy is written once**, whatever number of surfaces ask it (#92). A surface's *shape* may differ; the rule underneath may not.
- **Every child table of `calls` is named in `repo::delete_calls`.** Forget one and the retention sweep fails at the oldest marked Call, forever. Each such feature carries an "a marked Call is still prunable" test.
- **`SUM` goes through `db::sum_bigint`**, and **an aggregate groups by the output column's alias** (`activity::bucket_group`). Both are Postgres failures a SQLite run cannot see.
- **Assert cost as a statement-count difference** (`statements_issued()` sampled either side of the work, at two sizes), not as a ceiling. It is the only way an N+1 is visible from outside.
- **A flattened document owns its keys.** A field beside a `#[serde(flatten)]` is named for what it is, never for a key the flattened type already spends.
- **A cached gate is re-read on the same request that changes it** (`Tones::armed`, `Access::is_gating`). A stale "off" is a silent failure.
- **A credential never reaches a listing row or a log line.** One that must travel in a URL rides the query string, which `http_log` never writes.
- **A control that would be refused is not offered.** The catalog says what this Instance allows, so the client draws from it.
- **Out of scope answers exactly like not there** — a 404, never a 403 that works as an oracle.

## CI, branches and the local ritual

Detail and reasons: [`docs/agents/ci.md`](docs/agents/ci.md).

- **Hard gates** (block merge): `cargo fmt --check`, `clippy -D warnings`, the full suite on **both dialects**, doctests, the backend floor (`--fail-under-lines`) and the frontend Vitest `thresholds`, and **100% patch coverage** on every changed line, backend and frontend separately. The mutation run, the Playwright PWA suite and the nightly mutation sweep are **advisory**.
- **`next` is the working branch.** It takes direct pushes, one ticket at a time. A version lands on `master` as one pull request when it is complete, and `master` is protected with nine required checks.
- **The patch-coverage gate only runs on that pull request**, so between releases **the local ritual is what holds the line**: `cargo fmt --all`, `cargo clippy --all-targets -- -D warnings`, `cargo nextest run` (+ `cargo test --doc`), `cargo llvm-cov` over the floor, and the client `tsc`/`oxlint`/`vitest --coverage` gates.
- **Clean up after it** ([`docs/agents/machine-hygiene.md`](docs/agents/machine-hygiene.md)): `cargo clean -p radio-scout` (never a bare `cargo clean`), and no background shell left polling a job.

## Logging policy

Full rationale + the incident that bought these rules: [ADR-0011](docs/adr/0011-observability-logging-policy.md). The rules that bind day-to-day work — these are **hard rules**, not style preferences:

1. **`println!` / `eprintln!` / `dbg!` are denied by lint** — `[lints.clippy]` in `Cargo.toml`, so `cargo clippy --all-targets -- -D warnings` fails on a reintroduced one. Output goes through `tracing`, always. Every `#[allow]` is narrow and carries a comment saying why, and each is output whose stdout *is* its product: the hand-run examples (`feed.rs`, `enhance_ab.rs`), the tests that report themselves **skipped** to whoever is reading the run (`tests/db.rs` for Postgres, `tests/common/s3.rs` for real S3, `tests/uploadscript.rs` for a machine with no `curl`, `tests/trplugin.rs` for one with no C++ toolchain — none has a subscriber installed), and `service::show` (#23 — `service … --print` emits the unit file it would write, and a document interleaved with timestamps is not one). `build.rs` needs none (cargo doesn't run clippy over build scripts).
2. **Never log a secret** — API keys, access codes, admin passwords or hashes — at any level, in any form, not even truncated. Identify a key by its database id or label, resolved after lookup. A credential the operator has no other copy of goes to **a file, never a log line**: first run writes its generated ingest key into `.env` — or into `<base_dir>/.env` when there is no env file to have read it from — created `0600`, with only the path logged (`src/startup.rs`).
3. **Every refused request logs a machine-readable `reason`** — WARN where an Operator should look (every ingest rejection: `invalid-api-key`, `duplicate`, `blacklisted`, `no-talkgroup`, `not-populated`, the malformed-body family, a failed admin login), DEBUG where the request log's own 4xx line already covers it and an Operator would not act (a Call that isn't there, a bad filter, the admin guard). A Call that doesn't become a row leaves a line saying why. **Structural since #92**, not a convention: one message (`request refused`), one callsite, and a closed `failure::Reason` whose single exhaustive match decides slug + level + status + body together — so a refusal that skips the line is not expressible and an arm that decides none of those doesn't compile. For the rdio 417s the slug **is** the wire detail with dashes for spaces (`no-talkgroup` ⇄ `Incomplete call data: no talkgroup`), one string rather than two that can drift — so renaming a slug rewrites a recorder-facing string, and every such body is pinned verbatim in `tests/instrumentation.rs`.
4. **Every 5xx logs at ERROR with the cause and the request id**; the response body carries only `internal error (request id: …)`. A handler returns `Result<T, failure::Failure>` and names the stage (`.map_err(Stage::Dedup.failed())`); the request middleware logs `stage=` + `cause=` against the request id (#28's, echoed as `x-request-id`) and replaces the body — for *any* 5xx, so a route that fails some other way is covered by construction, which is why the redaction stays in the middleware and not in the response conversion. A `Failure` captures the span it was built in and re-enters it to write the line — **both arms**, because axum renders a handler's return value after every span it ran in has closed, and on a dropped Call that line is the only record there is. The rdio-compatible strings for *known* outcomes stay byte-identical — they're a wire contract.
5. **Listener IPs never appear above DEBUG.** Recorder IPs may appear at INFO on ingest routes. A **Downstream** peer's key never appears at all, at any level — it is stored recoverably because a hash cannot be POSTed, which makes it the one credential in the process that could leak through an ordinary error line. (The sharpest case used to be a Web Push endpoint, a stable per-device identifier; it went with #107, and its lesson generalises: an error type's own `Display` is part of what a log line says, which is why a transport error is rendered through `without_url()`.) **A refused *authentication attempt* may name its source at WARN** — unactionable without an address to firewall, and not a record of who listened. Three credentials, three refusals, one exemption (#19's admin password, #68's **Access code**, #70's metrics token), and it is the **refusal line's** alone, never the request line beside it. A *successful* authentication names nobody, which is the half that keeps it from becoming the record it exists to prevent; the boundary is structural, since `Refusal::attempted_from` is the only way an address reaches a refusal and `failure.rs`'s own table asserts no other arm carries the field. [ADR-0011](docs/adr/0011-observability-logging-policy.md)'s rule 5 carries the table. A public instance must not accumulate a record of who listened and when.
6. **Static messages, structured fields** — `warn!(reason = %slug, "request refused")`, never a formatted sentence. **Structural since #92**: there is one refusal callsite in the process, so "which fields, rendered how" stopped being a per-handler decision.
7. **Levels mean something:** ERROR = an operator must act · WARN = something was rejected or dropped **and an operator would want to know** (a refusal an Operator would not act on is DEBUG, #92) · INFO = notable normal events (startup, ingest outcome, one line per request) · DEBUG = per-asset/per-range requests, listener IPs, protocol detail · TRACE = wire dumps.
8. **Nothing logs unguarded in a hot loop.** Per-Call fine; per-range-request is DEBUG; per-sample never.

Output goes to **stdout only**; journald, Docker or the terminal own persistence. How the console filter, the stored operator log and the per-request line implement these rules is in `src/observability.rs`, `src/logsink.rs` and `src/http_log.rs`.

## Configuration

Full rationale: [ADR-0012](docs/adr/0012-configuration-model.md) + its #87 and #90 amendments. One `Config` (`src/config.rs`), resolved once at boot, from four layers — **CLI flag > environment variable > `radio-scout.toml` > default**, loudest first. rdio-scanner has this backwards: `flag.Parse()` runs first and the INI is then loaded *over* the flags (`server/config.go:96-137`), so a flag cannot override a configured value.

**A new subsystem is a configuration section wired inside `src/instance.rs`, never in `main.rs`**, which is excluded from coverage, so anything wired there is unreachable by a test. It earns a place in `Wiring` only if it genuinely varies between two real runs.

**A background task is a `worker::Worker`** (`src/worker.rs`), and four rules bind a new one: double-spawn is structurally impossible; work is admitted where it is handed over, before the task is spawned; a `Ticket` rides with the work and settles on drop; and the Instance owns the handle. What one *unit* of its work is, is the Worker's own decision.

**Adding a setting is a field, a default, a template line and a row (#87).** Each section **is** its subsystem's own configuration type — `[admin]` *is* `admin::AdminConfig`, `[retention]` *is* `retention::RetentionConfig`, `[storage]` and `[storage.s3]` are `blob`'s, `[enhancement]` is `enhance`'s, `[ingest]` is `ingest`'s, `[downstream]` is `downstream`'s, `[webhook]` is `webhook`'s — deriving `Serialize`/`Deserialize` and carrying its own serde attributes. There is no mirrored struct and no `Config::admin()` to translate; there is one type, so the file's defaults and the code's cannot drift because they are the same values. `[server]` and `[database]` stay in `config.rs`: they have no subsystem to belong to. `[log]` is the one section spanning two, so it is `observability::LogConfig` (`directives`, the console's) holding a `logsink::StoredLevel` (`database_level`, the sink's).

Two things follow that are easy to get wrong:

- **Units live at the serde boundary.** A `Duration` field keeps its `_secs` key through `config::secs`; `retention.max_size_bytes` keeps `max_size_gb` through `retention::gigabytes`. Both **refuse an unusable value in the deserializer**, the `ProxyNet` precedent — so the refusal carries a line and column, and the expectation text is one constant shared with the environment layer.
- **`config::SETTINGS` is the environment layer**, not a description of it: `resolve` walks it, and so do the tests, against the serialized shape of `Config` in both directions — so a setting with no environment spelling fails the suite, and so does an entry naming a key no configuration has. `.env.example` is asserted against the same table in `tests/docs.rs`, both ways, with every value it shows fed through the setting that would read it. One gap is deliberately left open (Rust has no reflection): a *newly added optional* setting, which serializes to nothing at its default, is invisible until something sets it.

**Strict validation**: an unknown key or an unusable value from any layer refuses to boot with exit `2`, naming the source, the value and what was expected. **Every setting has both spellings**, a TOML key and a `RADIO_SCOUT_*` variable. **Two credentials never go in the TOML** (`RADIO_SCOUT_API_KEY`, `RADIO_SCOUT_ADMIN_PASSWORD`), because first run *writes* them. The rest of the file's behaviour is described in `src/config.rs`.

## Improve, don't clone rdio

**Every feature is a chance to be better than rdio — take it.** rdio-scanner is the reference for *what* to build and the compatibility contract, never the ceiling for *how well*. The workflow for any ticket that touches an rdio-equivalent feature is:

1. **Research how rdio does it** — read the actual source in `rdio-scanner/` (and the recorders when relevant), not just the docs. Understand the behavior *and its weaknesses* (rdio's real pain points: DB-stored audio, proprietary JSON-over-WS, no background/lock-screen audio, dated UI, stale/half-open connections, missed calls across reconnects, no heartbeat of its own).
2. **Research how to do it better** — deliberately look for an improvement: robustness (heartbeat, reconnect catch-up, backpressure), performance (Pi-first), UX (mobile/PWA/background), or correctness. Cite the improvement in the ADR/PR/commit so the *why* is durable.
3. **Preserve compatibility only where it's a contract** — recorder-facing wire formats and response strings stay byte-compatible (as would the legacy `/rdio-scanner` surface, which is deferred and unbuilt); internal protocols and storage are ours to improve (e.g. our own live-feed protocol per [ADR-0004](docs/adr/0004-live-feed-raw-websocket.md), object-storage audio per [ADR-0002](docs/adr/0002-audio-object-storage.md)).
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
# `--shm-size` is not optional: Docker's default 64MB of /dev/shm runs out
# part-way through the suite's parallelism, and a random handful of tests then
# fail with "could not resize shared memory segment" while everything behind
# them times out. It reads as a flaky suite and is nothing of the sort.
docker run -d --name rs-pg --shm-size=1g \
  -e POSTGRES_PASSWORD=postgres -e POSTGRES_USER=postgres \
  -e POSTGRES_DB=postgres -p 55432:5432 postgres:17-alpine
TEST_POSTGRES_URL='postgres://postgres:postgres@localhost:55432/postgres' cargo nextest run
docker rm -fv rs-pg         # -v, or the data outlives the server: see machine-hygiene.md
```

`cargo-nextest`, `cargo-llvm-cov`, and `cargo-mutants` are external binaries (`cargo install …`); `proptest`/`rstest`/`insta` are dev-deps. The dual-dialect run needs only a Postgres to point `TEST_POSTGRES_URL` at ([`docs/agents/dual-dialect.md`](docs/agents/dual-dialect.md)), and the real-S3 run only a MinIO/Garage to point `TEST_S3_ENDPOINT` at ([`docs/agents/real-s3.md`](docs/agents/real-s3.md)). See [Testing & coverage policy](#testing--coverage-policy).

Frontend (React + TS + Vite + Tailwind v4 + shadcn/ui + Redux Toolkit/RTK Query), in `client/` — run from inside `client/`:

```bash
npm install                 # first-time setup
npm run dev                 # Vite dev server (proxies /api + /healthz + the WS to the backend on :3000)
npm run build               # type-check + production build to client/dist/ (embedded by the binary)
npm run typecheck           # tsc -b
npm run test                # Vitest + React Testing Library (single run)
npm run test:watch          # Vitest watch mode
npm run test:coverage       # Vitest with @vitest/coverage-v8 + thresholds (MSW at the network boundary)
npm run test:browser        # Vitest Browser Mode: the audio player + Media Session in real Chromium (#34)
npm run test:e2e            # Playwright: the PWA/service-worker/offline layer, over a real build
npm run lint                # oxlint
```

**Embedded UI:** the Rust binary serves `client/dist/` via `rust-embed` (`src/web.rs`), so **`npm run build` (in `client/`) must run before `cargo build`/`cargo test`** for the real UI to be served; without it the backend serves a minimal fallback page and the frontend-serving tests assert that fallback instead. `client/dist/` is gitignored; `build.rs` creates the (empty) folder so `rust-embed` compiles on a fresh checkout even before the frontend is built. CI does this by building the SPA once in its `client` job and downloading it into every job that runs cargo (#22) — the artifact is a build input, not an output beside them.

The browser layers (Browser Mode, Playwright, the iOS gate): [`docs/agents/testing.md`](docs/agents/testing.md#the-browser-layers).

**PWA app icons** are rasterized from `client/icons/icon.svg` by `client/scripts/build-icons.sh` (macOS `sips`) into `client/public/`. The PNGs are committed, so neither the build nor CI runs it — re-run it by hand only when the mark changes.

## Packaging & release

Full rationale: [ADR-0007](docs/adr/0007-single-binary-embedded-frontend-distribution.md) + its #23 amendment; the operator-facing guide is [`docs/deploy.md`](docs/deploy.md). Detail: [`docs/agents/ci.md`](docs/agents/ci.md#packaging--release). What binds day-to-day work:

- **One asset name, three consumers** — `release.yml`, `install.sh` and `docs/deploy.md` — held together by `tests/packaging.rs`, which runs the installer for real.
- **No cross-compilation.** Linux ships static musl built natively; nothing is built for a target on a different architecture.
- **`[profile.release]` is deliberate** (fat LTO, symbols kept, `panic = "unwind"`), and `release.yml` is the only place `--release` runs.
- **The platform is a value, not a `cfg`**: `radio-scout service` returns a `Plan`, so every platform's unit file renders and is snapshot-tested everywhere.
- **One Rust, pinned in `rust-toolchain.toml`** ([ADR-0015](docs/adr/0015-pinned-rust-toolchain.md)); bumps arrive as a Renovate PR to be read, never automerged.
- **A release is a `v*` tag matching `Cargo.toml`'s version.**

## Live testing (real binary, real browser, real recorder)

The suites can't answer "does a Call actually arrive and make a sound". Full procedure, the two instances (`./radio-scout-live-test` hermetic and wiped each run, `./radio-scout-data` durable), `.env` and the Trunk Recorder setup: [`docs/agents/live-testing.md`](docs/agents/live-testing.md); `/live-test` runs it. Live tests hit the **embedded build on `:3000`**, never trigger an `alert`/`confirm`, and read the console before calling anything a pass. **A live test supplements the suites, never replaces them**: fix what it finds test-first.

## Agent skills

### Issue tracker

Issues are tracked in this repo's GitHub Issues via the `gh` CLI. External PRs are not a triage surface. See `docs/agents/issue-tracker.md`.

**The loop is one ticket per session, in a fresh context**: take the frontier ticket (open, `blocked_by == 0`, unassigned) → confirm it hasn't already landed (`git log --oneline --grep '#<n>'` — an open state is not proof it's unbuilt) → **read it as a claim and grill anything open** (the hard constraint above) → `/implement`, which drives `/tdd` for the build and closes with `/code-review`.

**Definition of done: committed, pushed, and closed** — all three, in the session that did the work. Finishing a ticket means `git push`, then `gh issue comment <n>` with what shipped (commit SHA, criteria met, anything left to a later ticket), then `gh issue close <n>`. A built-but-open ticket silently blocks every ticket behind it, because the frontier query treats open blockers as live gates.

### Dual-dialect testing

`TEST_POSTGRES_URL` moves the whole suite onto Postgres, a database per test. See `docs/agents/dual-dialect.md`.

### Real-S3 testing

`TEST_S3_ENDPOINT` (+ credentials) runs `tests/s3.rs` against a MinIO/Garage that answers, a bucket per test; unset, those tests skip. See `docs/agents/real-s3.md`.

### Triage labels

Canonical label vocabulary (`needs-triage`, `needs-info`, `ready-for-agent`, `ready-for-human`, `wontfix`). See `docs/agents/triage-labels.md`.

### Machine hygiene

The ritual's leftovers: background shells that outlive what they watched, and `target/` trees that grow without bound. Check both before calling a ticket finished. See `docs/agents/machine-hygiene.md`.

### Domain docs

Single-context: one `CONTEXT.md` + `docs/adr/` at the repo root. See `docs/agents/domain.md`.
