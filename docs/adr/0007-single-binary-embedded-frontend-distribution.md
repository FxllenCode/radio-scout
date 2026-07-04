# Single self-contained binary with embedded frontend; prebuilt cross-compiled distribution

## Context

"One program, just works" is a core philosophy, the deployment targets span Raspberry Pi (arm64), macOS, and Windows/Linux x64, and CI is central to the workflow. The frontend is React/Vite; the backend is Rust/Axum.

## Decision

Radio-Scout ships as a **single self-contained binary** with the built React frontend **embedded via `rust-embed`**; the SPA, REST API, and live-feed WebSocket are all served from one origin/port. First run is zero-config: it creates `base_dir` with a SQLite database and a filesystem audio store.

Distribution:
- **Primary: prebuilt cross-compiled binaries** per OS/arch (including `linux-arm64` for the Pi) via GitHub Releases.
- A `curl | sh` **convenience installer** that fetches the right binary.
- A `radio-scout service install` **subcommand** for systemd/launchd/Windows service autostart.
- A **multi-arch Docker image** published as an additional first-class option.

CI (GitHub Actions) uses a target matrix; each job builds the frontend → embeds it → produces one binary, and the test suite runs against **both SQLite and Postgres** ([ADR-0003](0003-database-sqlite-postgres.md)). arm64 builds use `cross` or `cargo-zigbuild`.

## Considered and rejected

- **Docker-first** — requires Docker on the Pi and is a heavier footprint than one file.
- **Source / `cargo install`** — needs the full Rust toolchain on every target and compiles on-device.

## Consequences

- Releases are self-contained with no runtime dependencies, except an optional *system* ffmpeg used only if the AAC-muxing fallback in [ADR-0006](0006-optional-rust-native-audio-enhancement.md) is taken.
- The frontend must be built before the Rust binary in every CI job (build-order dependency).
