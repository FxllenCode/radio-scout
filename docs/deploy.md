# Deploying Radio-Scout

Radio-Scout is **one file**. The frontend is compiled into the binary, first run
creates its own database and audio store, and there is nothing else to install —
no runtime, no ffmpeg, no package manager ([ADR-0007](adr/0007-single-binary-embedded-frontend-distribution.md)).

Four ways in, in the order most people want them, and then
[how to put it on the internet](#5-putting-it-on-the-internet). The
[README](../README.md) covers the quickest of them in a paragraph; this is the
whole picture, including the ones it skips.

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

Every release publishes one archive per platform, the Trunk Recorder `radio-scout-upload.sh` ([recorders.md](recorders.md)), plus a `SHA256SUMS`:
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
  `CAP_NET_BIND_SERVICE` rather than the whole thing running as root. That
  includes [built-in TLS](#built-in-tls)'s 443, so **install again after turning
  it on**: the unit only grants what the configuration it was installed with
  asked for, and a boot that cannot bind says so, naming the capability.
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

**Watching a recorder's folder from a container** ([Dirwatch](recorders.md#dirwatch-a-recorder-that-only-writes-files))
needs that folder mounted in and named as a root — and, since the image carries no zone
database, the host's zone mounted too, for the recorders that write local times:

```sh
docker run -d -p 3000:3000 \
  -v radio-scout-data:/data \
  -v /srv/trunk-recorder:/recordings \
  -v /etc/localtime:/etc/localtime:ro \
  -e RADIO_SCOUT_DIRWATCH_ROOTS=/recordings \
  ghcr.io/fxllencode/radio-scout:latest
```

The container's user (below) has to be able to read the folder, and to write it if the watch
deletes as it goes.

**One gotcha, and it is Docker's:** the image runs as uid `65532`, not root. A
*named* volume (`-v radio-scout-data:/data`, above) inherits the right ownership
when Docker creates it. A *bind mount* (`-v ./data:/data`) does not — the host
directory keeps its own ownership, and the scanner cannot write to it. Either
use a named volume, or `chown 65532:65532 ./data` first.

## 5. Putting it on the internet

Out of the box the scanner speaks plain HTTP on port 3000, which is right for a LAN
and wrong for the internet. Three ways to give it HTTPS and a public name, **in the
order to try them**:

| | [Cloudflare Tunnel](#cloudflare-tunnel-recommended) | [A reverse proxy](#a-reverse-proxy) | [Built-in TLS](#built-in-tls) |
| --- | --- | --- | --- |
| Ports to forward on your router | none | 443, and 80 | 443, or 80 |
| Works behind CGNAT / no public IP | yes | no | no |
| A certificate to keep on the Pi | none | the proxy's | the scanner's own |
| Another program to run | `cloudflared` | Caddy, nginx… | none |
| Who else sees the traffic | Cloudflare | nobody | nobody |

**Start with the tunnel.** It is what the maintainer runs, and it needs nothing
opened on your network: `cloudflared` dials *out* to Cloudflare, so there is no
port to forward, no home IP address published, and it works on the CGNAT
connections that make forwarding impossible. The other two are for when you would
rather no third party sat in front of your scanner — and built-in TLS is also
where rdio-scanner's `ssl_auto_cert` and `ssl_cert_file` went.

Whichever you choose, set [`public_url`](operating.md#webhooks) to the address
people will use, so share links, embed snippets and webhook links point there
rather than at your LAN address. (Built-in TLS sets it for you.)

### Cloudflare Tunnel (recommended)

You need a domain whose DNS Cloudflare runs. Then, in the Cloudflare dashboard:

1. **Networking → Tunnels → Create a tunnel**, and name it.
2. Pick your operating system and **run the install command it shows on the
   scanner** — `sudo cloudflared service install <token>`. That installs
   `cloudflared` as a service that comes back at boot, like the scanner does.
3. On the tunnel's **Routes** tab, **Add route → Published application**: your
   subdomain and domain, and **Service URL `http://localhost:3000`**.

Then tell the scanner its name, and restart it:

```toml
[server]
public_url = "https://scanner.example"
```

That is all. **Nothing else needs configuring**: `cloudflared` relays every visitor
from `127.0.0.1`, and loopback is a trusted proxy by default — so the log names
the real visitor, the admin lockout counts each visitor separately (rather than
locking *you* out because a stranger guessed five times), and the admin session
cookie is marked `Secure`. Your recorders on the LAN keep posting to
`http://<host>:3000` exactly as before; the tunnel is only how the internet gets in.

**In Docker**, run `cloudflared` beside the scanner, sharing its network, so it
reaches the scanner on `localhost` too and nothing needs trusting:

```yaml
# docker-compose.yml
services:
  radio-scout:
    image: ghcr.io/fxllencode/radio-scout:latest
    volumes: ["radio-scout-data:/data"]
    environment:
      RADIO_SCOUT_PUBLIC_URL: https://scanner.example
    ports: ["3000:3000"]          # your LAN and your recorders
  cloudflared:
    image: cloudflare/cloudflared:latest
    command: tunnel --no-autoupdate run
    environment:
      TUNNEL_TOKEN: ${TUNNEL_TOKEN}   # the token from step 2
    network_mode: service:radio-scout
volumes:
  radio-scout-data:
```

(If `cloudflared` runs on its own network instead, add that network's subnet to
[`trusted_proxies`](operating.md#behind-a-reverse-proxy) — Docker's networks come
from `172.16.0.0/12` unless you have changed its address pools.)

Three things worth knowing, all Cloudflare's rather than the scanner's:

- **Cloudflare's terms name audio.** Its CDN terms say that serving "a
  disproportionate percentage of pictures, audio files, or other large files"
  without one of its paid products may get your access limited. A scanner serves
  audio. Short calls at hobby scale are the kind of use many people run without
  trouble, but that judgment is yours, not ours. If it worries you, keep audio in
  [S3-compatible storage](operating.md#storage): the scanner then hands each
  listener a short-lived link straight to the bucket, so call audio never passes
  through the tunnel. The [station stream](operating.md#the-station-stream) still
  does, because it is one continuous response from the scanner itself.
- **A recorder posting from outside your LAN goes through Cloudflare too**, and
  Bot Fight Mode can answer a non-browser client with a challenge it cannot solve.
  Uploads then fail with a 403 that never reaches the scanner. Either post over the
  LAN (a recorder on the same machine should use `http://127.0.0.1:3000` anyway),
  or add a WAF skip rule for `/api/call-upload` and `/api/trunk-recorder-call-upload`.
- **To close listening to strangers**, put Cloudflare Access in front of the
  hostname. That is Cloudflare's login, in front of the whole thing; the scanner's
  own [access codes](operating.md#gating-sensitive-channels) gate single channels.

### A reverse proxy

If you already run one, point it at port 3000 and proxy the WebSocket too — the
live feed, the API and the app are one origin. Caddy does certificates, WebSockets
and the forwarded headers by itself:

```sh
caddy reverse-proxy --from scanner.example --to localhost:3000
```

A proxy on the same machine is trusted by default, exactly as a tunnel is. One
anywhere else has to be named in
[`trusted_proxies`](operating.md#behind-a-reverse-proxy), or the scanner will not
believe what it says about who is asking — or that they asked over HTTPS.

### Built-in TLS

The scanner can get and renew its own certificate from Let's Encrypt, with nothing in
front of it. It is the right choice on a VPS, or at home if you would rather forward
ports than route through Cloudflare.

```toml
[tls]
domains = ["scanner.example"]
email = "you@example.com"     # optional
```

The name has to resolve to this machine already, and the internet has to reach it
on **443** — or on **80**, which Let's Encrypt can use instead. On first boot the
scanner asks for a certificate. It tries the challenge Let's Encrypt makes on port
443 first, and falls back to the one on port 80, so either forwarded port is
enough. Naming a domain accepts the CA's subscriber agreement, and the log links
it when the account is registered, on the first issuance. From then on:

- **It renews itself**, when Let's Encrypt suggests (ARI) or two-thirds of the way
  through the certificate's life, with nothing restarted. The certificate is kept
  in `<base_dir>/tls/`, readable by the scanner's account alone, so a restart serves
  it again rather than asking for another.
- **Settings → Admin → Status shows it**: when it expires, when it renews, and the
  last thing that went wrong in the CA's own words. `/metrics` carries the expiry
  for an alert. A renewal that fails is an ERROR in the log every time it is
  tried; the old certificate keeps working meanwhile, and the page gets louder as
  expiry nears.
- **Try it on Let's Encrypt's staging CA first**, if you are unsure of your DNS or
  ports: add `directory = "https://acme-staging-v02.api.letsencrypt.org/directory"`.
  Browsers will not trust that certificate, but a mistake will not use up your real
  rate limit. Remove the line to switch, and a real certificate is issued.

**Port 3000 does not go away** when TLS comes on, and it does not stay open to the
internet either. It answers by who is asking:

| Who | What they get on the plain port |
| --- | --- |
| This machine, your LAN, a Tailscale network (carrier-NAT space, `100.64.0.0/10`) | the whole app, exactly as before |
| Let's Encrypt, checking a challenge | the answer it is owed |
| Anyone else | a redirect to `https://` |

(Carrier-NAT space is where Tailscale numbers a tailnet, and it is also where some ISPs put
customers who can reach one another. If yours is one, do not expose `[server] port`.)

So a recorder on the same Pi keeps posting to `http://127.0.0.1:3000`, which matters
because posting to your own public name from inside your network needs "hairpin
NAT" that many home routers do not do. And on a VPS nothing but the redirect is
reachable over plain HTTP. (rdio-scanner keeps serving the whole app, admin login
included, on its plain port to everyone.)

**Ports.** HTTPS listens on `[tls] port`, 443 by default. On Linux a port below 1024
needs root or a capability: `radio-scout service install` grants it when the
configuration asks for one, as above. Behind a home router, forward 443 to the
scanner's `[tls] port` and, if you want the redirect and the port-80 challenge, 80
to its `[server] port`. The internal numbers can be anything.

**In Docker**, publish 443, and the plain port to your LAN only. Docker lets the
image's account bind 443 inside the container, so nothing else is needed:

```sh
docker run -d -p 443:443 -p 192.168.1.5:3000:3000 \
  -e RADIO_SCOUT_TLS_DOMAINS=scanner.example \
  -v radio-scout-data:/data ghcr.io/fxllencode/radio-scout:latest
```

Publish the plain port to the internet (`-p 80:3000`) only on standard Docker on
Linux. The plain port tells a stranger from a neighbour by the address the
connection comes from. Standard Docker hands the container the real one; rootless
Docker and Docker Desktop show every connection as coming from a private gateway,
which the scanner would take for your LAN.

**Your own certificate** instead — certbot's, `tailscale cert`'s, a wildcard you
already have:

```toml
[tls]
cert_file = "/etc/letsencrypt/live/scanner.example/fullchain.pem"
key_file = "/etc/letsencrypt/live/scanner.example/privkey.pem"
```

The files are **read again every minute**, so when certbot renews them the scanner
serves the new certificate without a restart. (rdio reads them once, at boot.) A file
caught half-written keeps the old certificate in service and says so.

**What refuses to boot**, so it is not found out from a browser later: a wildcard
name (that needs a DNS challenge, which the scanner does not do), an IP address in
place of a name, a name that is not one, an ACME directory that is not `https://`,
a certificate file without its key or the other way round, both `domains` and your
own files at once, `[tls] port` the same as `[server] port`, and — once the rest is
fine — certificate files that cannot be read. Each says which setting, and why.

**Coming from rdio-scanner**: `ssl_auto_cert` is `[tls] domains`, `ssl_cert_file`
and `ssl_key_file` are `cert_file` and `key_file`, and `ssl_listen` is `[tls] port`.

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
