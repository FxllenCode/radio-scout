# Deploying Radio-Scout

Radio-Scout is **one file**. The frontend is compiled into the binary, first run
creates its own database and audio store, and there is nothing else to install —
no runtime, no ffmpeg, no package manager ([ADR-0007](adr/0007-single-binary-embedded-frontend-distribution.md)).

Four ways in, in the order most people want them. The [README](../README.md)
covers the quickest of them in a paragraph; this is the whole picture, including
the ones it skips.

Once it is running: [recorders.md](recorders.md) to point a recorder at it,
[operating.md](operating.md) for storage, retention, enhancement and logging, and
[using.md](using.md) for the app itself.

---

## 1. The installer

```sh
curl -fsSL https://raw.githubusercontent.com/FxllenCode/radio-scout/master/install.sh | sh
```

It works out which binary this machine wants, downloads it from the latest
GitHub release, **checks it against the release's published SHA-256**, and puts
it in `/usr/local/bin` — or `~/.local/bin` when that isn't writable, so it works
without root.

If piping a script into your shell makes you uncomfortable — it should — read it
first, and use the dry run:

```sh
curl -fsSL https://raw.githubusercontent.com/FxllenCode/radio-scout/master/install.sh -o install.sh
sh install.sh --dry-run          # says exactly what it would fetch and where it would go
sh install.sh --dir ~/bin        # or --version v1.2.3, or --target <triple>
```

A checksum that does not match stops the install with nothing written.

## 2. A prebuilt binary, by hand

Every release publishes one archive per platform plus a `SHA256SUMS`:
[github.com/FxllenCode/radio-scout/releases](https://github.com/FxllenCode/radio-scout/releases).

| Platform | Asset |
| --- | --- |
| Raspberry Pi / any 64-bit ARM Linux | `radio-scout-<version>-aarch64-unknown-linux-musl.tar.gz` |
| 64-bit x86 Linux | `radio-scout-<version>-x86_64-unknown-linux-musl.tar.gz` |
| Apple Silicon Mac | `radio-scout-<version>-aarch64-apple-darwin.tar.gz` |
| Intel Mac | `radio-scout-<version>-x86_64-apple-darwin.tar.gz` |
| Windows | `radio-scout-<version>-x86_64-pc-windows-msvc.zip` |

**The Linux binaries are statically linked against musl**, which is the whole
point: they have no libc dependency at all, so the same file runs on Raspberry
Pi OS (any release), Debian, Ubuntu, Alpine and a container built `FROM
scratch`. A dynamically linked build would only run where glibc is at least as
new as the machine it was built on — which is exactly how a Pi ends up with
`GLIBC_2.38 not found` from a binary that works everywhere else.

Verify before running it:

```sh
sha256sum -c SHA256SUMS --ignore-missing     # or: shasum -a 256 -c
tar -xzf radio-scout-*.tar.gz
./radio-scout
```

Then open `http://localhost:3000`. First run creates `./radio-scout-data`, an
ingest key in `.env`, and an admin password — all of it printed as *paths*,
never as secrets ([ADR-0011](adr/0011-observability-logging-policy.md) rule 2),
so `cat .env` after the scrollback is gone.

Next step is a recorder: [recorders.md](recorders.md).

## 3. Run it at boot

```sh
sudo radio-scout service install
```

That is the whole thing. It writes the right definition for this operating
system, registers it, and starts it:

| Platform | What it writes | Registered with |
| --- | --- | --- |
| Linux | `/etc/systemd/system/radio-scout.service` | `systemctl enable --now` |
| macOS | `/Library/LaunchDaemons/io.github.fxllencode.radio-scout.plist` | `launchctl bootstrap system` |
| Windows | `<base-dir>\radio-scout-task.xml` | `schtasks /Create` (a boot-triggered task) |

**Whatever settings you give the install command are baked into the
definition**, resolved to absolute paths — so the service comes up on the same
configuration you just tested, not on the defaults:

```sh
sudo useradd --system --no-create-home radio-scout      # only if using --user
sudo radio-scout service install --port 8080 --base-dir /srv/scanner --user radio-scout
```

Install also **creates the base directory** and, when `--user` names an account,
**gives it to that account** — a service is usually installed before the scanner
has ever been run, and a data directory that a root-run first launch created is
one the service cannot write to. The account itself has to exist first; if it
doesn't, the install stops on the `chown` and says which name it could not find,
rather than registering a service that fails at the next boot.

Other verbs: `uninstall`, `start`, `stop`, `restart`, `status`. And before any
of it,

```sh
radio-scout service install --print
```

prints the exact file it would write and the exact commands it would run, and
changes nothing. (rdio-scanner's `-service install` has no equivalent — it
registers a service with no way to see what it registered.)

Notes worth knowing:

- **The log goes to journald** on Linux, because the process only ever writes to
  stdout. `journalctl -u radio-scout -f`. On macOS launchd discards a daemon's
  stdout unless it is given a path, so the plist points it at
  `<base-dir>/radio-scout.log`.
- **The systemd unit is confined.** `ProtectSystem=strict` with a single
  `ReadWritePaths=<base-dir>`, no new privileges, a `@system-service` syscall
  filter, and an empty capability set — except that a port below 1024 gets
  `CAP_NET_BIND_SERVICE` rather than the whole thing running as root.
- **`--user` is refused on Windows**, where the task runs as the system account:
  a named account would need its password stored alongside the task.
- **`--database-url` is refused** by `service install` entirely. It routinely
  carries a password and a service definition is world-readable — put it in
  `radio-scout.toml` or `RADIO_SCOUT_DATABASE_URL` instead.

## 4. Docker

```sh
docker run -d --name radio-scout \
  -p 3000:3000 \
  -v radio-scout-data:/data \
  ghcr.io/fxllencode/radio-scout:latest
```

Multi-arch (`linux/amd64`, `linux/arm64`), built `FROM scratch` around the same
static binary the release ships — no shell, no package manager, nothing in the
image but the scanner, its CA certificates and `/data`.

Everything is configurable through the environment
([ADR-0012](adr/0012-configuration-model.md) — every setting has a
`RADIO_SCOUT_*` spelling):

```sh
docker run -d -p 8080:8080 \
  -e RADIO_SCOUT_PORT=8080 \
  -e RADIO_SCOUT_API_KEY=the-key-your-recorder-uses \
  -v radio-scout-data:/data \
  ghcr.io/fxllencode/radio-scout:latest
```

Flags work too — they append to the entrypoint:
`docker run … ghcr.io/fxllencode/radio-scout --log debug`.

First run generates an ingest key and an admin password into `/data/.env` —
inside the volume, so they survive a restart and an image upgrade. They are
never in the log ([ADR-0011](adr/0011-observability-logging-policy.md) rule 2),
and the image has no shell to `docker exec` into, so read them through a
container that does:

```sh
docker run --rm -v radio-scout-data:/data alpine cat /data/.env
```

Simpler for a container: set them yourself, and nothing is generated.

```sh
-e RADIO_SCOUT_API_KEY=… -e RADIO_SCOUT_ADMIN_PASSWORD=…
```

**One gotcha, and it is Docker's:** the image runs as uid `65532`, not root. A
*named* volume (`-v radio-scout-data:/data`, above) inherits the right ownership
when Docker creates it. A *bind mount* (`-v ./data:/data`) does not — the host
directory keeps its own ownership, and the scanner cannot write to it. Either
use a named volume, or `chown 65532:65532 ./data` first.

---

## Building from source

Needs a Rust toolchain and Node. The frontend has to be built first, because
`rust-embed` reads `client/dist` at compile time:

```sh
cd client && npm ci && npm run build && cd ..
cargo build --release
```

The result is `target/release/radio-scout`. There is no Docker image to build
from source — `docker/Dockerfile` is a *packaging* file that assembles an image
around binaries the release workflow has already produced, which is why the
image and the release are provably the same bytes rather than two builds that
happen to have the same version number.

Contributing, and the test policy every change is held to: [CLAUDE.md](../CLAUDE.md).

## How a release is made

`.github/workflows/release.yml`, on a `v*` tag:

1. The SPA is built once and handed to every build job.
2. The tag is checked against `Cargo.toml` — a release whose binary reports a
   different version than the tag is a bug nobody notices until a bug report.
3. Each target is built **natively on its own architecture** with `--release`:
   the musl binaries inside `rust:alpine` (on an arm64 runner for arm64), macOS
   on macOS, Windows on Windows. Nothing about the shipped artifacts depends on
   a cross-toolchain being configured correctly.
4. The archives are collected, `SHA256SUMS` is computed over all of them at
   once, and `gh release create` publishes the lot with generated notes. A tag
   with a pre-release part (`v1.0.0-rc.1`) is published as a pre-release, so it
   stays out of the `releases/latest` the installer resolves.
5. The two Linux binaries become the `linux/amd64` + `linux/arm64` image.
   `:latest` moves only for a final release, so `docker run …:latest` and
   `curl | sh` always land on the same version.

`workflow_dispatch` runs steps 1 and 3 and uploads the archives as build
artifacts, publishing nothing — so the pipeline can be exercised without cutting
a release.
