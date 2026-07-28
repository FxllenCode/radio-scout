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

## Amendment (#23): static musl, native builds, and a scheduled task on Windows

Building it settled four things this ADR had left open. `.github/workflows/release.yml` and [`docs/deploy.md`](../deploy.md) are the detail; the decisions are:

- **The released Linux binaries are `*-unknown-linux-musl`, statically linked.** A dynamically linked build only runs where glibc is at least as new as the builder's, so a release built on `ubuntu-latest` fails to start on the Raspberry Pi OS release most Pis run — a portability regression against the Go binaries rdio-scanner ships, in the one place ("it runs great on a Pi") where the project cannot afford one. Static musl also makes the container image `FROM scratch` possible with no separate build.
- **Nothing is cross-compiled — `cross`/`cargo-zigbuild` are not used after all.** The tree carries C dependencies (`aws-lc-sys` via `object_store`/`rustls`, built with cmake; `libsqlite3-sys`), and a cross toolchain is one more thing that can be subtly wrong in a way only a user discovers. Every target is built on its own architecture instead: the musl binaries inside `rust:alpine` — on GitHub's arm64 runners for arm64 — macOS on macOS, Windows on Windows. The one exception is `x86_64-apple-darwin`, cross-built from an arm64 Mac runner because Apple's own toolchain targets both and GitHub's Intel macOS runners are on their way out.
- **`radio-scout service install` on Windows registers a boot-triggered scheduled task, not a Windows service.** A real service has to speak the service-control protocol from inside the process, which would mean Windows-only code that no test in this repository could ever execute — CI compiles for Windows, it never runs there. A scheduled task runs an ordinary executable unmodified, so every platform-specific decision is *text*, rendered by `src/service.rs` and asserted on wherever the suite happens to run. The cost is that it appears in Task Scheduler rather than `services.msc`.
- **The profile was measured, not copied.** `opt-level = 3`, fat LTO, one codegen unit, `strip = "debuginfo"` (symbols kept, so a panic on someone else's Pi arrives as function names) and an explicit `panic = "unwind"` — because `"abort"` is the tempting size win and would let one panicking request handler take every listener and recorder down with it. On `aarch64-apple-darwin`, with the real SPA embedded: **17.7 MB**, against 71.1 MB for the same tree in debug, and **~34 ms** from exec to listening on a warm cache. The single embedded binary is the deliverable, so its size is a user-facing number — but these are a developer Mac's; **the Pi 5's own size and cold-start numbers are still owed**, and are the ones that decide whether any of this needs revisiting.
- **`docker/Dockerfile` packages, it does not build.** It assembles the image around the binaries the release already produced, so the image and the release are the same bytes rather than two builds with the same version number. Building an arm64 image from source under QEMU takes the better part of an hour; building from source is `cargo build --release`.

## Considered and rejected

- **Docker-first** — requires Docker on the Pi and is a heavier footprint than one file.
- **Source / `cargo install`** — needs the full Rust toolchain on every target and compiles on-device.

## Consequences

- Releases are self-contained with **no runtime dependencies at all**. The ffmpeg escape hatch [ADR-0006](0006-optional-rust-native-audio-enhancement.md) once reserved for AAC muxing is moot as of #20's amendment: no AAC encoder ships, so nothing in the binary needs muxing help.
- The frontend must be built before the Rust binary in every CI job (build-order dependency).
