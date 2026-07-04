# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

Radio-Scout is a full-stack, one-stop-shop application for listening to audio from Trunk Recorder and SDRTrunk. It is a replacement for rdio-scanner (which will remain accessible at `/rdio-scanner`). Every feature from rdio-scanner carries over, but optimized, with a beautiful UI.

The philosophy is a simple setup: a one-program install from the command line that just works. There will be a database (choice TBD via a grilling session) and possibly an object store for the audio. This application is likely to run on hardware as low as a Raspberry Pi, so it must be highly optimized, fast, and performant.

- **Backend:** Rust, entirely.
- **Frontend:** Vite + React (TypeScript) + TailwindCSS + shadcn, located in `client/`.

## Hard constraints

- **All development must use Test-Driven Development.** CI is used heavily, and is also essential for deployment when building the application for different targets (PC, Mac, Raspberry Pi). Dev/testing happens on Mac; the target scanner runs on a Raspberry Pi 5.
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

Frontend: once `client/` is scaffolded with Vite, commands run from inside `client/` (`npm run dev`, `npm run build`, `npm run test`, `npm run lint`). Update this section when the frontend is set up.
