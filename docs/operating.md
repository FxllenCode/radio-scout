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
log_days = 30     # stored log events, on their own window
```

A **sweep** runs at startup and on an interval: age Calls out, then prune oldest-first until
the size cap is met, then prune stored log events past `log_days`, then reclaim audio no Call
points at.

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

Sessions have both an **idle** window (refreshed by use) and an **absolute** lifetime (never
refreshed) — the second is the bound on a cookie somebody walked off with. Failed logins are
rate-limited per source address; the cooldown runs from the *last* attempt, so hammering keeps
it locked and walking away clears it.

> **Behind a reverse proxy terminating TLS, set `[server] trusted_proxies`.** The session cookie
> is marked `Secure` only when a *trusted* proxy reports `X-Forwarded-Proto: https`. With the
> list empty — what ships — that header is never believed, and an HTTPS deployment still hands
> out a cookie a browser will replay over plain `http://` to the same host.

### Running the instance from a browser

**Settings → Admin.** Everything an instance is made of is editable there, so running one never
needs SSH:

| Screen | What it owns |
| --- | --- |
| **Talkgroups** | labels, names, tags, groups, LED colours, blacklists — filtered and paged, with **multi-select bulk assignment** so categorising a county is one action rather than an afternoon |
| **Systems** | label, ref, the per-system auto-populate toggle, the enhancement scope, and the raw blacklist |
| **Units** | naming the radios, filtered to *the ones nobody has named yet* |
| **Groups** / **Tags** | the two category vocabularies, with a count of what is behind each |
| **API keys** | issue (shown once), label, scope to a System, disable, revoke |

Four things are worth knowing before you start:

- **A delete that would take Calls with it is refused**, and says how many. Removing Calls is
  [Retention's](#retention) job — it deletes the row, the audio object and reclaims the orphans
  behind it. If you mean it, the refusal itself offers the button that goes through with it.
- **Blacklisting is a toggle on the talkgroup row.** It writes the Ref onto its System's list,
  which is the same field the System form shows — use that one to refuse a Ref *before* its
  channel exists, which is how you stop a patch-minted TGID from ever cluttering the panel.
- **An API key is shown exactly once.** They are stored hashed, so nothing can show one again;
  copy it when you issue it. **Disable** is the reversible off and **Revoke** deletes the row —
  both stick, including across a restart with `RADIO_SCOUT_API_KEY` still set.
- **Nothing is silently ignored.** A name already taken, a Ref another channel answers to, an
  LED outside the palette, a blank required field — each is refused, named, and shown beside the
  input that caused it. Ports, storage and retention are still `radio-scout.toml`'s: this screen
  owns the entities Calls are addressed to, not the machine.

Bulk CSV import is still there and still the fastest way to name a county at once — see below.

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
  `name`/`description`, `tag`, `group`, `led`, `system`, `memberRefs`), and unknown columns are
  ignored. With no header row, RadioReference's column positions are assumed.
- **Re-importing is safe.** Rows upsert on (System, Ref) rather than appending, so running it
  twice does not duplicate anything.
- **A blank cell means "leave alone"**, never "erase".
- **Every rejected row is reported** with its line number and a machine-readable reason; the
  whole import is one transaction, so it either all applies or none of it does.

`?system=` sets the default System for rows that do not name one; a `system` column overrides it
per row.

### Merging duplicate channels

Some systems mint a fresh talkgroup id for every patch event, and multi-site systems can show the
same channel under more than one number. Auto-populate does what it is told and creates a channel
for each, so a county panel fills up with buttons nobody chose. A **member Ref** fixes that: one
Talkgroup answers to several numbers, and everything else — the panel, search, the live feed,
blacklists — sees one channel.

Add a `memberRefs` column, semicolon-separated:

```csv
ref,label,memberRefs
100,Fire Dispatch,8123;8124
```

- **Naming a Ref that is already its own Talkgroup folds it in.** Its archived Calls move across —
  they are still there, under the channel that now owns them — and the duplicate disappears from
  the panel. Traffic arriving under `8123` keeps arriving; it just lands on `100`.
- **The cell is the whole list**, the way the `group` column is. Dropping `8123` from it unmerges:
  the channel comes back, with the label it had and exactly the Calls that arrived under it. `-`
  means the empty list, which is how the last one is removed.
- **`dryRun=true` tells you how much history a fold would move** before it moves any, as
  `callsRepointed` in the report. Use it — this is the one import that rewrites the archive
  rather than the configuration.
- **A Ref belongs to one channel.** A row claiming one that another Talkgroup already lists is
  rejected (`member-ref-owned-elsewhere`), and so is a row whose own `ref` is currently somebody's
  member Ref (`ref-is-a-member-ref`) — so re-importing last year's county export cannot quietly
  undo your merges. Unmerge from the owner's row instead.
- **Folding a channel that already has members of its own** is rejected (`member-ref-owns-members`)
  until the cell lists those too, and the message says which. Otherwise the file would stop
  describing what it made, and re-importing it would unmerge them again.

Every merge leaves a log line saying what moved (`talkgroup member Refs changed`, with the counts),
so `journalctl -u radio-scout` has the record even if you lost the report.

### Naming the radios

Radios name themselves, with nothing configured. SDRTrunk puts a `talkerAlias` on every upload
and Trunk Recorder sends a `tag_ota` per source — both are the name the radio broadcast about
itself — and either one is enough for a radio to become a named **unit**. rdio-scanner reads
those fields and throws them away, which is why units there are bare numbers forever.

That name shows up wherever a source appears: the scanner display, the recent list, every search
row, and the call detail. Tapping it opens that radio's history — which talkgroups it uses, when
it was first and last heard, and its calls.

A fleet's own numbering is a CSV, the same shape as the talkgroup one:

```csv
ref,label,memberRefs
1200,Engine 1,1201-1299;4471
1300,Ladder 3,
```

```sh
curl -X POST 'http://localhost:3000/api/admin/units/import?system=411&dryRun=true' \
     -H 'Content-Type: text/csv' --data-binary @units.csv
```

- **`memberRefs` takes Ranges as well as single ids** (`1201-1299;4471`), because fleets number
  their radios in blocks. Everything the block covers is one apparatus: its calls, its history,
  and the name on every row.
- The rest behaves exactly like the talkgroup import — `dryRun=true` first, headers matched by
  name (`ref`/`unit`/`radioid`, `label`/`alias`/`name`, `memberRefs`/`ranges`, `system`), a blank
  cell means "leave alone", `-` empties the list, re-importing is a no-op, and every rejected row
  is reported with its line number. Unlike talkgroups there is **no positional layout**: nobody
  exports unit lists, so a file with no header row is refused rather than guessed at.
- **A name you write down is never overwritten** by what the air says. A radio with no name yet
  takes the first one offered — so a row that exists only to own a Range gets named the moment
  one of its radios keys.
- **Ranges may not overlap.** A row claiming a span another apparatus already owns is rejected
  (`range-overlaps`) and the message names the span in the way; a radio inside two blocks would
  otherwise belong to whichever row the database happened to return first.

### Names SDRTrunk was already sending you

If you run SDRTrunk, the radio aliases and site names you set up in it are **already in your
archive** — they have been all along, and nothing has been reading them.

SDRTrunk's upload API has no field for either. But every MP3 it uploads carries an ID3 tag it
wrote on the way past, holding the alias list you configured for the radio that keyed, the name
of the tower that channel is tuned to, and what it was being demodulated as. Radio-Scout reads
that tag as each call arrives, and a background sweep goes back over the calls you already had.

Nothing to configure, and nothing to change on the recorder. What you get:

- radios named the way you named them in SDRTrunk, not just the `talkerAlias` they broadcast;
- **real site names** — "Downtown" instead of "Site 1" — which is the only way an SDRTrunk call
  gets a site at all, since its uploads carry no site field;
- the decoder (`P25 Phase 1`, `DMR`) on each call.

A name you have written down here always wins: this fills in blanks and never overwrites a unit
or site you have named, and never overwrites anything the recorder itself sent on the upload.

The sweep over your existing archive is bounded and picks up where it left off after a restart —
about 12 000 calls an hour by default, so a hundred thousand are done overnight. It reads each
stored call exactly once and then has nothing left to do, and it never rewrites audio.

The one decision worth making is whether to run it at all. If your audio lives on metered object
storage, `[mining] sweep = false` skips the one read per stored call it would otherwise cost —
and new calls are still mined as they arrive, because that happens on the way in and is not a
setting. Its pace is `[mining] interval_secs` and `batch_size`; `--write-config` describes both.

### Hearing each call once

A console patch makes your recorder upload the same transmission once for every talkgroup in the
patch, and a multi-site system can hand you the same call off two towers. rdio-scanner plays each
of those, because the only thing it compares is the talkgroup and the timestamp. Radio-Scout treats
them as one call: same system, within the dedup window, and reaching a talkgroup in common — its
own, or one either copy says it is patched to. You get one call. (A talkgroup id the patch
invented still gets a button if a copy naming it is the *first* to arrive — nothing exists to
match it against yet. Merge it into the real channel, or blacklist it.)

When the same transmission does arrive twice, **the better copy is the one you keep** — fewer
decode errors first, then longer audio. A better copy arriving a moment later takes the stored
call's place without changing its id, so a call already in your queue or on screen simply improves;
nothing jumps, nothing plays twice, and the link to it keeps working. Copies that arrive after the
window has passed are still recognised as duplicates — they just do not upgrade what is stored.

Nothing here needs configuring, and the defaults are the recommendation. Two knobs exist for
systems whose patch data cannot be trusted:

```toml
[ingest]
dedup_window_ms = 500       # how far apart two calls can be *on the air* and still be one
dedup_scope = "patched"     # or "talkgroup": ignore patch membership, match the channel only
dedup_keep = "best"         # or "first": keep whichever copy arrived first, like rdio-scanner
dedup_replace_secs = 30     # how long a stored call stays open to a better copy
```

The two windows measure different things and that is why there are two. `dedup_window_ms` is about
the *air*: how far apart two transmissions can start and still be the same one — every copy reports
the same start time, so half a second is plenty, and widening it starts merging genuinely different
back-to-back calls. `dedup_replace_secs` is about your *network*: how long after storing a call
Radio-Scout will still accept a better copy of it. A recorder posting one file per patched talkgroup
takes a few seconds to get through them, so raise this one — not the other — if better copies are
arriving too late to count.

Every rejected copy leaves a line saying `reason=duplicate` and naming the call that beat it, so
"why is this call not in the archive?" has an answer.

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
- **Every refused request says why**, with a machine-readable `reason=` — `invalid-api-key`,
  `duplicate`, `blacklisted`, `no-talkgroup`, and the same for the admin and notification
  surfaces. A Call that does not become a row leaves a line explaining itself. The message is
  always `request refused`; what it was is the `reason=`, so one grep finds all of them.
- **Every 5xx logs its cause against that request id**, and the response body carries only the
  id. The cause goes to you, never to the client.
- **Secrets are never logged**, at any level, in any form. Nor are listener IP addresses above
  DEBUG, nor push endpoints ever — a public instance must not accumulate a record of who
  listened and when. Recorder addresses may appear on ingest routes, and refused admin logins
  name their source, because that one is unactionable without an address to firewall.

A filter the logger cannot parse **refuses to boot** and names the layer it came from — an
operator who asked for TRACE and silently got INFO debugs the wrong log.

### Reading the log without a shell

Radio-Scout also keeps what it said in the database, and **Settings → Logs** shows it: newest
first, filtered by level and date, behind the admin password. If you run it as a service on a
box you do not have a terminal on, this is how you answer *why did my recorder's Calls stop
arriving?*

```toml
[log]
database_level = "info"   # "off", "error", "warn" or "info"

[retention]
log_days = 30             # 0 keeps them forever
```

Four things worth knowing about it:

- **It is a second sink, not the first.** The console still gets everything; the database gets
  a copy, written by a background task through a queue. A database that is slow, broken or not
  there yet cannot slow down or fail the request that produced the line — if it cannot keep up
  it drops events and says so on the console, and if it cannot write at all it says that once
  and carries on.
- **`database_level` is independent of `directives`.** Turning the console up to chase a
  problem does not change what is stored, and turning it down does not empty the Logs view.
- **There is deliberately no `debug`.** DEBUG is where a listener's IP address can appear, and a
  public instance must not accumulate a database of who listened and when. Asking for it
  **refuses to boot** rather than quietly storing it. (It is also what stops a Pi writing a row
  per audio range request.)
- **Stored logs are pruned by the same sweep that prunes Calls**, on their own window — so
  `days = 0` (keep every Call forever) does not also mean an unbounded logs table.

An event is stored as its **parts** — level, time, target, message, its structured fields, and
the request id — so the Logs view can filter and you can search it. That request id is the same
one in an `internal error (request id: …)` a listener reads out to you, which is what makes it
useful when you have no shell to grep.

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
