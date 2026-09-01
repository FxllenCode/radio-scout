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

> **Upgrading from 0.1.x: delete any `[push]` section.** Notifications were removed in 0.2.0
> ([ADR-0014](adr/0014-no-notifications.md)) and `[push]` is now an unknown key, so a file that
> still sets one **refuses to boot** and names it. Nothing else in your file changes, and any
> push-related variables left behind in `.env` are simply never looked up, so they cost you
> nothing. Listeners who had notifications switched on lose them; there is no replacement.

### The two credentials that are not in the file

The ingest **API key** and the **admin password** live in `.env` (mode `0600`), because first
run *writes* them. They are never logged — only the path is — so `cat .env` is how you read
them back.

Set them yourself and nothing is generated, which is usually what you want in a container:

```sh
RADIO_SCOUT_API_KEY=…            # what recorders authenticate with
RADIO_SCOUT_ADMIN_PASSWORD=…     # opens /api/admin/
```

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
days = 7            # 0 keeps them forever
max_size_gb = 10    # omit entirely for no cap
log_days = 30       # stored log events, on their own window
listener_days = 90  # listener counts, on theirs
```

A **sweep** runs at startup and on an interval: age Calls out, then prune oldest-first until
the size cap is met, then prune stored log events past `log_days`, then listener counts past
`listener_days`, then reclaim audio no Call points at.

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
| **Talkgroups** | labels, names, tags, groups, LED colours, blacklists — filtered and paged, with **multi-select bulk assignment** so categorising a county is one action rather than an afternoon, and **Merges** for folding duplicates together |
| **Systems** | label, ref, the per-system auto-populate toggle, the enhancement scope, and the raw blacklist |
| **Units** | naming the radios, filtered to *the ones nobody has named yet*, and the **Ranges** a fleet's block occupies |
| **Groups** / **Tags** | the two category vocabularies, with a count of what is behind each |
| **API keys** | issue (shown once), label, scope to a System, disable, revoke |
| **Downstreams** | the other instances you forward calls to — see [below](#forwarding-to-other-instances) |
| **Webhooks** | addresses that receive your flagged calls — see [below](#webhooks) |

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

## Forwarding to other instances

**Settings → Admin → Downstreams.** Each peer is an address, the API key *that peer* issued you,
and which systems and talkgroups to send. Receiving needs nothing configured at either end: a
peer forwarding to *you* is just another recorder, so give them an API key and they are done.

**A peer's outage costs you delay, not calls.** Matching calls are written to a durable queue as
they are stored and drained in the order they arrived once the peer comes back — across a
restart of either end. rdio-scanner posts inline and drops the call when a peer is unreachable,
which is silent: nothing anywhere records what went missing.

The row tells you where each peer stands: how many calls are **queued** for it, when it **last
delivered**, and how many attempts have failed since, with the reason. A peer that has been
restored from a backup says **needs its key** — see [Backups](#backups).

Three things behave the way they do on purpose:

- **A call the peer refuses outright is dropped rather than retried forever.** A `400`, `413`,
  `415`, `417` or `422` means those same bytes will never be accepted, and retrying them would
  hold up every call behind it. Everything else — a wrong key, a wrong address, a restart, a
  rate limit, an unreachable host — keeps the backlog, because those are things you fix.
- **Disabling a peer empties its queue.** Otherwise switching one off for a week and back on
  again replays the week, into an instance whose own retention has probably aged past it.
- **The key is write-only.** It is stored so it can be sent, and it is never shown again, never
  returned by the API and never written to a log. Editing a peer's scope does not mean re-typing
  it — leave the field blank and the stored one is kept.

How hard we try is `[downstream]` in `radio-scout.toml`: `timeout_secs`, and the retry wait,
which doubles from `retry_initial_secs` up to `retry_max_secs`. The peers themselves are never in
the TOML — they are entities, and they live in the browser with everything else you curate.

## Webhooks

**Settings → Admin → Webhooks.** An address that receives the calls you flag — one carrying an
**emergency** — as JSON, or as a message Discord renders. Each one is an address, which marks you
want, which systems and talkgroups it covers, and which shape to send.

This is the automation escape hatch, and it is **yours**, not your listeners': Radio-Scout does
not wake anybody's device, has no push notifications and asks nobody for permission
([ADR-0014](adr/0014-no-notifications.md)). Wiring your own channel up to your own flagged calls
is a different thing, and it is what this screen is for.

**A call has to carry the mark *and* be in the scope.** An emergency on a talkgroup you did not
give this webhook is somebody else's emergency. A webhook watching *no* marks never fires at all,
and the row says **watching nothing** so it does not look configured when it is inert.

**An outage costs you delay, not calls** — the same durable queue the downstream sender drains,
so a Discord outage or a script that was down for an hour delivers an hour late rather than not
at all. The row shows how many calls are **queued**, when it **last delivered**, and the failures
since.

Four things behave the way they do on purpose:

- **The address is a password.** A Discord webhook URL ends in a token, so it is treated exactly
  the way an API key is, and then some: it goes in once, is never shown again, never returned by
  the API, never written to a log, and **never exported in a backup**. The listing shows only the
  host — `discord.com` — so you can still tell two of them apart. Editing a webhook's scope does
  not mean going back to Discord for the URL: leave the address field blank and the stored one is
  kept.
- **A backup does not carry your webhooks.** That is the price of the rule above; a configuration
  document is a file you email and commit, and it stays one. Restoring an instance means re-adding
  them, which is a couple of pastes.
- **Disabling one empties its queue**, so switching a webhook off for a week and back on does not
  post a week of emergencies into a chat room at once.
- **A body the far end refuses outright is dropped rather than retried forever** — a `400`, `413`,
  `415`, `417` or `422`. Everything else keeps the backlog, including Discord's `429` rate limit
  and the `404` a deleted webhook answers, because those are things you fix.

**Links need `[server] public_url`.** A payload carries a link to the call's audio, and this
instance cannot know its own public address — it may be behind a proxy, a tunnel, or three. Set
`public_url` to what you type into a browser and the link appears; leave it unset and the payload
still carries every fact about the call and simply has no link. A *guessed* URL in somebody's chat
room would be worse than none, and a malformed one is a `400` Discord would make us drop the call
over — which is why the setting is checked at boot and the instance refuses to start on one that
is not an absolute `http://` or `https://` address.

How hard we try is `[webhook]` in `radio-scout.toml`: `timeout_secs` (shorter than a downstream's
— this is a few hundred bytes of JSON, not a minute of audio) and the retry wait, which doubles
from `retry_initial_secs` up to `retry_max_secs`.

### The payload

Radio-Scout's own shape is the call exactly as `GET /api/calls` describes it, plus the marks that
fired — so anything you have already written against the search API parses this without a second
parser:

```json
{
  "marks": ["emergency"],
  "call": {
    "id": 42,
    "systemRef": 11,
    "systemLabel": "Fulton County",
    "talkgroupRef": 54241,
    "talkgroupLabel": "Fire Dispatch",
    "unitRef": 1234,
    "unitLabel": "Engine 1",
    "timestamp": 1700000000000,
    "durationMs": 7400,
    "emergency": true,
    "audioUrl": "https://scanner.example/api/call/42/audio"
  }
}
```

`audioUrl` is absent when `public_url` is unset, and when the call is **encrypted** — an encrypted
call is stored as metadata with no audio at all, and it is still delivered, because an encrypted
emergency is exactly the thing you want to be told about.

The Discord shape is one embed with the talkgroup as its title, the marks as its description, and
the paged station, system, unit and duration as fields, linked to the audio. Discord renders it;
nothing needs configuring at its end beyond pasting the webhook URL Discord gave you.

## Tone-out detection

A dispatch console pages a station by transmitting a short sequence of pure tones — Motorola
Quick Call II's two, a single long group tone, or a two-tone page followed by a group tone.
Radio-Scout listens for that sequence in the audio and **marks** the call it happened on, so you
can find it, filter for it, and have it posted to a webhook.

**This is signal processing, not speech recognition.** Radio-Scout does not transcribe anything,
ever — no keyword alerts, no transcript search, nothing that depends on a transcript existing. A
page-out is two sinusoids, and looking for them needs none of that.

**Nothing wakes anybody.** A tone-out is shown, filtered and searched on, exactly like the
emergency flag. There are no push notifications in Radio-Scout at all. A **webhook** you
configured can carry a page to an address *you* chose, which is you arranging your own inbox.

### Writing a profile

Settings → Admin → Talkgroups, find the channel a station is paged on, and press **Tones**. A
profile is a name — "Station 12", which is what you will see on the call — and the tones in order:

| Field | What it means |
| --- | --- |
| **Tone (Hz)** | The frequency, from your tone-set chart. Between 200 and 3300 Hz. |
| **Held for (s)** | The *shortest* it may be. Quick Call is nominally 1.0 s then 3.0 s; leave room, because a squelch opening late clips the front of the first tone. |
| **Tolerance (%)** | How far off a tone may be, as a percentage of that tone. 2% is twice the ±1% the tone set is specified to and is what to leave it at. |
| **Max gap (ms)** | How much silence may sit between two tones before it stops being one sequence. |

A percentage rather than a fixed number of hertz because that is how tone sets are specified: 2%
is ±6 Hz at 300 and ±49 Hz at 2468, and a window wide enough for the top of the band would merge
neighbouring tones at the bottom of it.

If you do not know the frequencies, record a page-out and read them off a spectrum analyser —
Audacity's *Analyze → Plot Spectrum* is enough. A profile whose tones are outside the range
detection listens to, or too short to measure, is **refused when you save it** rather than stored
and silently never firing, because a pager that is not being watched looks exactly like a pager
that has not gone off.

**Disable before you delete.** If a profile is catching somebody else's pages, switch it off — it
keeps the frequencies you measured, and you can widen or narrow it later. Deleting one does not
touch the pages it already caught: each call recorded the station's name at the time it fired.

### What it costs, and when it runs

**Nothing at all until you write a profile.** With none configured, an upload does not spend a
single extra query.

Detection runs **behind** ingest, never inside it: the recorder is answered and the call is on the
live feed before any audio is decoded, so a slow decode can never slow an upload. The page appears
on the call a moment later.

**A profile applies to the calls that follow it**, not to the archive that came before. Nothing
goes back over stored audio when you write one — turning on a feature must not silently re-read a
county's worth of objects — so if you want yesterday's pages, you needed the profile yesterday.

`[tone] queue_depth` in `radio-scout.toml` is how many calls may be waiting to be looked at. Past
that they keep the audio they arrived with and are not checked, which is logged. Detection is much
cheaper than enhancement, so this rarely fills.

### Finding the pages

A marked call carries a badge everywhere it is shown — in the live feed, in RECENT and in the
archive — and the badge **names the station**: hover it, or read it with a screen reader, and it
says *Tone-out: Station 12*. The archive filter has a **Mark** control; pick *Tone-out* to see only
the pages. `GET /api/calls` carries the same thing as a `tones` array, with the offset into the
call each sequence started at.

To have them posted somewhere, add a **webhook** (above) watching the `tone` mark. Its payload
names the station:

```json
{
  "marks": ["tone"],
  "call": {
    "...": "...",
    "tone": true,
    "tones": [{ "label": "Station 12", "atMs": 1840 }]
  }
}
```

## Catch-up, and the quiet-span scan

A listener who is a long way behind can press **CATCH UP** in the queue sheet: the queue drains at
1.5×, and the stretches where nobody is talking are skipped. That second half needs something from
the instance — a browser cannot look at audio samples — so Radio-Scout reads each stored call once,
in the background, and writes down where it is quiet.

**This is the one background worker that looks at every call.** Enhancement ships off; tone-out
detection skips any channel with no profile. There is no equivalent shortcut here: whether a call
has a gap in it can only be answered by looking at it. It is also the cheapest of the three — a
decode and a scan, where enhancement decodes, resamples twice, filters, measures loudness and
re-encodes — and, like both of them, it runs **behind** ingest. The recorder is answered and the
call is on the live feed before anything is decoded.

**This is signal processing, not speech recognition**, on tone-out detection's terms: it asks how
much energy is present, never what was said.

It is **on by default**, because catch-up is a listener feature and nobody should have to find a
setting before the queue can be drained. Turn it off in `radio-scout.toml` if this instance will
never have a listener — a headless forwarder — or if the hardware cannot spare the decode:

```toml
[quiet]
enabled = false
```

Catch-up still works with it off. It raises the playback rate and trims nothing, which is also
what happens for any individual call the scan could not read.

**Nothing goes back over the archive.** Calls stored before you upgraded, or while scanning was
off, are never re-read: switching a feature on must not pull a county's worth of objects back off
your disk at the next boot. `[quiet] queue_depth` is how many calls may be waiting to be scanned;
past that they keep whatever they arrived as, which is logged.

## Listener counts

Settings → Admin → **Listeners** charts how many people have been connected, and when. It answers
one question — *peak listeners, with a timestamp* — because that is the only question a record of
counts can answer, and counts are all there is:

```toml
[listeners]
enabled = true
interval_secs = 60
```

Every minute the instance writes down the **highest number of listeners that were on at once**
since the previous sample. A peak rather than a reading at the tick, so somebody who arrived and
left inside one minute is still counted; a chart that quietly under-reported its own peaks would
look exactly like one that did not.

**Nothing identity-shaped is stored, and there is nowhere for it to go.** The table has three
columns — a row id, an instant, and a number. No address, no session, no user agent, and
deliberately no per-talkgroup breakdown: on a quiet channel with one listener, "who was on Fire
Dispatch at 3am" is precisely the record this instance must not keep. It is the same rule that
keeps a listener's IP out of the log above `debug`.

It is **on by default**, because history cannot be recovered afterwards — an operator who has to
find a setting first has already lost whatever happened before they found it. A day is 1,440 rows
of a few bytes each. Turn it off if you would rather keep nothing, or raise `interval_secs` if a
coarser chart will do. How long the rows survive is `[retention] listener_days` above.

The chart is behind the admin password, unlike everything else a browser can read here. Listening
is open; how many people take that up is yours.

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

There are two ways to do it: the **Merges** button on a talkgroup row, and a `memberRefs` column
in the CSV. Both write the same thing.

#### From the browser

**Settings → Admin → Talkgroups → Merges** on the channel you want to keep. It lists the refs that
channel already answers to, takes more to fold in, and unfolds one with a button.

- **Nothing folds without being shown first.** Every action here previews: it names each ref, the
  channel it would absorb, and how many archived Calls would move — then asks again. That is a real
  `dryRun` of the same transaction, rolled back, not an estimate of it, so what it says is what
  happens when you confirm.
- **A ref with no channel behind it says so** (*"no channel yet, just recorded"*). That is normal
  when you are naming a patch id ahead of hearing it — and it is also what you would see if the ref
  belonged to a *different system*, since a ref only means something inside its own. If you expected
  a fold and got a recording, check the system.
- **Fold a whole selection at once.** Tick the churn rows *and* the real channel, pick which one
  survives from **Fold into**, and it is one request. This is the one to reach for when a patch-happy
  system has left you forty near-identical rows. Selections spanning two systems are refused for the
  reason above.
- **Unfolding is the same flow backwards** and gives back the channel with the label it had and
  exactly the Calls that arrived under it.

A **Ranges** button on a unit row does the equivalent for radios — `1201–1299` in one line, rather
than a row per radio. No preview there, and none needed: Calls name radios by number, so a range
only changes what an apparatus is *called*, and removing one puts the bare numbers back.

#### From a CSV

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
  describing what it made, and re-importing it would unmerge them again. The browser has no file to
  round-trip, so it applies that fold instead and tells you what came across with it.

Every merge leaves a log line saying what moved — `talkgroup member Refs changed`, with the counts,
the refs and the talkgroup ids behind them — so `journalctl -u radio-scout` has the record even if
you lost the report. Both paths write the same line, so it reads the same whichever door the merge
came through. A `-` in `talkgroup_ids` is a ref that absorbed nothing; the two lists line up entry
by entry.

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
  `duplicate`, `blacklisted`, `no-talkgroup`, and the same for the admin surface. A Call that
  does not become a row leaves a line explaining itself. The message is always
  `request refused`; what it was is the `reason=`, so one grep finds all of them.
- **Every 5xx logs its cause against that request id**, and the response body carries only the
  id. The cause goes to you, never to the client.
- **Secrets are never logged**, at any level, in any form. Nor are listener IP addresses above
  DEBUG — a public instance must not accumulate a record of who listened and when. Recorder
  addresses may appear on ingest routes, and refused admin logins name their source, because
  that one is unactionable without an address to firewall.

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

### The configuration on its own

Everything you curated — systems, talkgroups with their merges, groups, tags, named units, the
API-key roster and your downstream peers — also exports as **one JSON file**, separately from the
archive. That is the one to keep in git: it is small, it is a plain document, and it is what you
would hate to retype.

**Settings → Admin → Export.** Import the same file on any instance to reproduce the setup.

- **The file never carries a secret.** API keys are stored hashed, and the hash does not leave
  either — so the export carries each key's *label, scope and disabled flag* and nothing else.
  Importing **re-issues** them and shows the new keys once, labelled, so you know which recorder
  each belongs to. Every recorder needs its new key; nothing else about the roster is lost.

  The same goes for a **downstream** peer's key, for the opposite reason: that one has to be
  stored in a sendable form, which is exactly why it must not be in a file you commit. A restored
  peer comes back with its address and its scope, **switched off and marked *needs its key***
  until you paste in the one that peer issued you. rdio-scanner's export includes every
  downstream's key in plaintext.
- **Importing never deletes.** A document adds and updates what it names and says nothing about
  anything else, so a truncated or hand-edited file cannot destroy a county — and a restore is
  safe to run twice, which matters when the first one died half-way.
- **Preview before you commit.** The import screen shows what would change and lists any entry it
  would refuse, each with its **path in the file** (`systems[0].talkgroups[3]`) so you can find it
  in your editor. The rest still applies — one bad LED colour does not cost you the restore.
- **It is diffable.** The document is a pure function of your configuration: no timestamps, no
  database ids, everything in a fixed order. Two exports of an unchanged instance are byte-identical,
  so `git diff` shows what you changed rather than when you last looked. The date is in the
  filename instead.
- **Only units you curated** are in it — the ones with a name or a range. An instance rosters a
  unit for every radio it has ever heard, and those come back on their own from the next call.

Ports, storage, the database URL and retention are **not** in it: those are `radio-scout.toml`,
and an entity document that carried them between machines would be a way to point a second
instance at the first one's bucket. (The *systems you receive* are in it — it is the machine's
own configuration that is not.)
