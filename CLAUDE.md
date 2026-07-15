# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

Radio-Scout is a full-stack, one-stop-shop application for listening to audio from Trunk Recorder and SDRTrunk. It is a replacement for rdio-scanner (which will remain accessible at `/rdio-scanner`). Every feature from rdio-scanner carries over, but optimized, with a beautiful UI.

The philosophy is a simple setup: a one-program install from the command line that just works. There will be a database (choice TBD via a grilling session) and possibly an object store for the audio. This application is likely to run on hardware as low as a Raspberry Pi, so it must be highly optimized, fast, and performant.

- **Backend:** Rust, entirely.
- **Frontend:** Vite + React (TypeScript) + TailwindCSS + shadcn, located in `client/`.

## Hard constraints

- **All development must use Test-Driven Development.** CI is used heavily, and is also essential for deployment when building the application for different targets (PC, Mac, Raspberry Pi). Dev/testing happens on Mac; the target scanner runs on a Raspberry Pi 5. **Prefer native tests as the red-green-refactor loop:** Rust `cargo test` (backend unit tests + the in-process HTTP/WS integration harness) and Vitest + React Testing Library (frontend). **Playwright is installed and available** for end-to-end browser tests, but reserve it for flows native tests can't reach — Media Session / lock-screen controls, PWA install, and iOS/WebKit background audio. It complements the TDD loop; it is not the default loop.
- **Performance is first-class.** The app must be fast and performant on hardware as low as a Raspberry Pi.
- **Simple install.** A one-command install that just works.
- **rdio-scanner compatibility.** Figure out what features exist in rdio-scanner — all of them need to work in Radio-Scout. Upstream and downstream must exist and should be backwards compatible with rdio-scanner if at all possible.
- **Recorder integrations.** Create an integration or plugin (per their docs) for both SDRTrunk and Trunk Recorder. The maintainer runs Trunk Recorder on their scanner, so have a plugin/integration ready for that testing phase.
- **PWA / mobile support is extremely important.** You must be able to add the website to your phone and have scanner audio actually work correctly within the OS — e.g. functioning pause/next/previous buttons — and work correctly in the background, especially on iOS. This is lacking in rdio-scanner and is a big problem with it.

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
cargo test                  # run all tests
cargo test <name>           # run tests matching a substring
cargo test <mod>::<test> -- --exact --nocapture   # single test, with stdout
cargo fmt                   # format
cargo clippy --all-targets  # lint
```

Frontend (React + TS + Vite + Tailwind v4 + shadcn/ui + Redux Toolkit/RTK Query), in `client/` — run from inside `client/`:

```bash
npm install                 # first-time setup
npm run dev                 # Vite dev server (proxies /api + /healthz + the WS to the backend on :3000)
npm run build               # type-check + production build to client/dist/ (embedded by the binary)
npm run typecheck           # tsc -b
npm run test                # Vitest + React Testing Library (single run)
npm run test:watch          # Vitest watch mode
npm run lint                # oxlint
```

**Embedded UI:** the Rust binary serves `client/dist/` via `rust-embed` (`src/web.rs`), so **`npm run build` (in `client/`) must run before `cargo build`/`cargo test`** for the real UI to be served; without it the backend serves a minimal fallback page and the frontend-serving tests assert that fallback instead. `client/dist/` is gitignored; `build.rs` creates the (empty) folder so `rust-embed` compiles on a fresh checkout even before the frontend is built. CI (#22) runs the client build before the Rust build.

End-to-end (Playwright) — installed and available now:

```bash
playwright --version        # v1.61.1, installed globally
playwright test             # run E2E specs (all browsers incl. WebKit already downloaded)
playwright test --project=webkit   # iOS/Safari — Media Session, background audio, PWA install
```

When `client/` is scaffolded, also add `@playwright/test` as a `client/` devDependency so E2E runs via `npx playwright test` and in CI next to the unit suite. Default to the native tests above; use Playwright only for the browser-level flows they can't cover.

## Agent skills

### Issue tracker

Issues are tracked in this repo's GitHub Issues via the `gh` CLI. External PRs are not a triage surface. See `docs/agents/issue-tracker.md`.

### Triage labels

Canonical label vocabulary (`needs-triage`, `needs-info`, `ready-for-agent`, `ready-for-human`, `wontfix`). See `docs/agents/triage-labels.md`.

### Domain docs

Single-context: one `CONTEXT.md` + `docs/adr/` at the repo root. See `docs/agents/domain.md`.
