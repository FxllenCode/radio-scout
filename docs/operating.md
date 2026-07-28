# Operating Radio-Scout

Getting it installed is [deploy.md](deploy.md); pointing recorders at it is
[recorders.md](recorders.md). This is everything after that — the decisions you make once the
Calls are arriving.

**This is not a settings reference.** `radio-scout --write-config` writes a commented file with
every setting at its default and a note on each, and two tests hold it to being complete, so it
cannot go stale. Read that for *what* a setting is. Read this for *whether you want it*.

```sh
radio-scout --write-config          # radio-scout.toml, every setting, every default
radio-scout --help                  # every flag
```

## How configuration resolves

Four layers, loudest first: **command-line flag → environment variable → `radio-scout.toml` →
built-in default**. Every setting has both a TOML key and a `RADIO_SCOUT_*` variable.

The file is found via `--config`, then `RADIO_SCOUT_CONFIG`, then `radio-scout.toml` in the
working directory. **Having no file is not an error** — that is the zero-config first run.

Two things worth knowing because they differ from what you may be used to:

- **A bad setting refuses to boot.** An unknown key, an unparseable value, S3 selected with no
  credentials, a zero where a duration is required — the process exits and names the source,
  the value and what it expected. It does not start on a default and leave you to notice.
- **Boot tells you where its configuration came from** — which file it read, or that there
  wasn't one — and then the settings that resulted. "Why isn't my setting applying?" is
  answerable from the log.

### The three credentials that are not in the file

The ingest **API key**, the **admin password** and the **Web Push identity** live in `.env`
(mode `0600`), because first run *writes* them. They are never logged — only the path is — so
`cat .env` is how you read them back.

Set them yourself and nothing is generated, which is usually what you want in a container:

```sh
RADIO_SCOUT_API_KEY=…            # what recorders authenticate with
RADIO_SCOUT_ADMIN_PASSWORD=…     # opens /api/admin/
RADIO_SCOUT_VAPID_PRIVATE_KEY=…  # signs push notifications
```

> **The push identity must stay the same across restarts.** A browser pins its public half when
> it subscribes, so a new identity silently stops every existing subscription from ever being
> notified again. If the key cannot be saved, notifications are left **off** with an error
> rather than running on one that will not survive a reboot.

---

## Storage

Audio never goes in the database — only metadata does. That is what keeps SQLite viable and
backups simple.

**Filesystem (the default).** A sharded directory under `base_dir`. Back it up by copying a
folder. Point `[storage] path` at another disk if you want the archive off the SD card:

```toml
[storage]
path = "/mnt/usb/radio-scout-audio"
```

**S3-compatible (Garage, MinIO, AWS).** Worth it when the archive should outlive the Pi's
storage, or live on a NAS. Set `[storage] backend = "s3"` and fill in `[storage.s3]`.

With S3, audio is served differently: instead of proxying bytes through Radio-Scout, it issues
a **short-lived presigned URL** and redirects the browser to fetch directly from the store — so
a busy instance is not also an audio proxy. Nothing about the client changes.

> Put S3 credentials in the environment rather than the TOML if you can. They have no
> command-line flags on purpose: `ps` is world-readable.

A store that hiccups is retried — a few hundred milliseconds of it — so a busy or briefly
restarting Garage does not cost you a call. A store that is genuinely *down* is given up on in
about a second rather than minutes: an upload fails with an error the recorder retries on its own
schedule, and a call waiting to be enhanced is marked skipped instead of holding a worker slot
until your storage comes back. So an outage costs you the calls during it, not a stalled instance
afterwards.

## Database

**SQLite by default**, in `base_dir`, created on first run. It is genuinely the right choice
for a single scanner — the database only holds metadata, so it stays small.

**Postgres** when you want it elsewhere, or expect an archive large enough to want a real
server. Set `[database] url`. Migrations run automatically at boot on either.

```toml
[database]
url = "postgres://user:password@host/radio_scout"
```

Both dialects are tested on every change; neither is a second-class path.

## Retention

An archive that grows forever will eventually fill the disk, and on a Pi that means the
recorder stops too. Two independent bounds, and you can use either or both:

```toml
[retention]
days = 7          # 0 keeps them forever
max_size_gb = 10  # omit entirely for no cap
```

A **sweep** runs at startup and on an interval: age Calls out, then prune oldest-first until
the size cap is met, then reclaim audio no Call points at.

Two details that matter in practice:

- **`batch_size` exists for the Pi.** Deleting in small batches keeps each write lock short, so
  a sweep does not stall ingest. Leave it alone unless you have a reason.
- **Orphan reclamation has a grace period.** Audio is written *before* its database row, so for
  a moment an object legitimately has no row. Anything written inside `orphan_grace_secs`
  (an hour by default) is left alone — otherwise a sweep could delete a Call that is mid-upload.

Total stored size is tracked per Call at ingest, so enforcing the cap is one query rather than
a stat call per object — which on a remote S3 store would be a network round trip each, every
sweep.

## Audio enhancement

**Off by default.** It reprocesses each Call's stored audio so Talkgroups sit at a consistent
loudness rather than swinging between painful and inaudible.

```toml
[enhancement]
mode = "normalize"     # "off" | "normalize" | "denoise"
target_lufs = -16.0
```

- **`normalize`** is the proven win: voice band-pass plus EBU R128 loudness normalization. If
  you turn anything on, turn this on. `-16 LUFS` is a speech target that sounds right on a
  phone speaker; broadcast `-23` is noticeably quieter.
- **`denoise`** adds RNNoise on top. It is **unproven on already-decoded digital audio** —
  P25 and DMR have been through a vocoder, which is not the kind of noise RNNoise was trained
  on. Try it on your own systems and listen before trusting it.

Three things worth understanding before enabling it:

1. **It never runs on the ingest path.** A recorder's upload is stored, inserted and answered
   `200` before any of it starts, so enabling enhancement cannot slow ingest or lose a Call.
2. **The live feed is published at ingest**, not after enhancement — so a backlog never delays
   a listener. `queue_depth` only decides how long a burst can outrun the worker; past that, a
   Call simply keeps the audio the recorder sent.
3. **Scope it per System or Talkgroup** rather than instance-wide if one System is chatty
   enough to eat the CPU. `[enhancement] mode` is the fallback; a System or Talkgroup row that
   says nothing inherits it.

`output = "opus"` parses and then **refuses to boot** — it is not built yet, and quietly
writing a different format than you asked for would be worse.

## The admin surface

Everything under `/api/admin/` is gated by the admin password. There is **no default password**
— first run generates one into `.env`, and if it cannot write it, the admin surface stays shut
rather than opening with something guessable.

There is no admin web UI yet. Today the surface is login/logout and talkgroup CSV import.

Sessions have both an **idle** window (refreshed by use) and an **absolute** lifetime (never
refreshed) — the second is the bound on a cookie somebody walked off with. Failed logins are
rate-limited per source address; the cooldown runs from the *last* attempt, so hammering keeps
it locked and walking away clears it.

> **Behind a reverse proxy terminating TLS, set `[server] trusted_proxies`.** The session cookie
> is marked `Secure` only when a *trusted* proxy reports `X-Forwarded-Proto: https`. With the
> list empty — what ships — that header is never believed, and an HTTPS deployment still hands
> out a cookie a browser will replay over plain `http://` to the same host.

### Tidying up talkgroup names

Auto-populate means an archive is usable immediately, but Talkgroups arrive named after their
numbers. Fix them all at once with a CSV — the same RadioReference export that imports into
rdio-scanner works here unchanged:

```sh
# Log in first; the session cookie and its CSRF token are required.
curl -X POST 'http://localhost:3000/api/admin/talkgroups/import?system=411&dryRun=true' \
     -H 'Content-Type: text/csv' --data-binary @talkgroups.csv
```

- **`dryRun=true` walks the identical path and rolls back**, reporting exactly what would
  change. Use it first, always.
- **Headers are matched by name in any order** (`ref`/`tgid`/`decimal`, `label`/`alphatag`,
  `name`/`description`, `tag`, `group`, `led`, `system`), and unknown columns are ignored. With
  no header row, RadioReference's column positions are assumed.
- **Re-importing is safe.** Rows upsert on (System, Ref) rather than appending, so running it
  twice does not duplicate anything.
- **A blank cell means "leave alone"**, never "erase".
- **Every rejected row is reported** with its line number and a machine-readable reason; the
  whole import is one transaction, so it either all applies or none of it does.

`?system=` sets the default System for rows that do not name one; a `system` column overrides it
per row.

## Logging

Everything goes to **stdout** — journald, Docker or your terminal owns persistence and
rotation. There is no file sink on purpose.

```sh
radio-scout --log debug
RUST_LOG=warn,radio_scout::ingest=trace radio-scout
```

```toml
[log]
directives = "info,sqlx::query=warn,sea_orm_migration=warn"
```

What you can rely on:

- **Every HTTP request leaves one line** — method, path, status, duration — under a request id
  echoed back as `x-request-id`. Chatty routes (audio range requests, health probes, SPA
  assets) sit at DEBUG so a Pi is not writing a line per range request; a 4xx or 5xx escalates
  whatever the route.
- **Every rejected upload says why**, with a machine-readable `reason=` — `invalid-api-key`,
  `duplicate`, `blacklisted`, `no-talkgroup`. A Call that does not become a row leaves a line
  explaining itself.
- **Every 5xx logs its cause against that request id**, and the response body carries only the
  id. The cause goes to you, never to the client.
- **Secrets are never logged**, at any level, in any form. Nor are listener IP addresses above
  DEBUG, nor push endpoints ever — a public instance must not accumulate a record of who
  listened and when. Recorder addresses may appear on ingest routes, and refused admin logins
  name their source, because that one is unactionable without an address to firewall.

A filter the logger cannot parse **refuses to boot** and names the layer it came from — an
operator who asked for TRACE and silently got INFO debugs the wrong log.

## Behind a reverse proxy

Set `[server] trusted_proxies` to the proxy's address or CIDR block — Docker's bridge is a
subnet, so `172.17.0.0/16` is a normal entry:

```toml
[server]
trusted_proxies = ["127.0.0.1", "172.17.0.0/16"]
```

Empty (the default) means `X-Forwarded-For` is **never** read and logs name the TCP peer. That
is deliberate: the header is attacker-controlled, so believing it from anyone lets a stranger
forge a recorder's address into your log. When the peer *is* trusted, the address taken is the
rightmost entry that is not itself a trusted proxy.

This setting also decides whether the admin session cookie gets marked `Secure`. If you
terminate TLS at a proxy, you want it set.

Proxy the WebSocket too — the live feed, the API and the app are all one origin on one port.

## Backups

Two things, and they must be consistent with each other:

1. **The database** — `<base_dir>/radio-scout.db` (plus its WAL), or your Postgres.
2. **The audio** — `<base_dir>/audio`, your `[storage] path`, or your S3 bucket.

A row without its audio object is a Call that 404s on play; an object without its row is an
orphan the next sweep reclaims. Neither is fatal, but taking both at the same moment avoids
both.
