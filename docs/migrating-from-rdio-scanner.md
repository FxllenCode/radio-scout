# Migrating from rdio-scanner

Radio-Scout is a replacement for rdio-scanner, and the switch is genuinely small: your
recorders keep working, your talkgroup CSV imports unchanged, and you can run both at once
while you decide.

**The one thing to know before you start: your existing archive does not come across.** Read
[What does not migrate](#what-does-not-migrate) first, then decide.

## What migrates

**Your recorders — with a URL change and nothing else.** Radio-Scout speaks rdio-scanner's
upload API exactly: same endpoint, same field names, same aliases, same response strings. There
is no plugin to install and no patch to apply. Trunk Recorder's `rdioscanner_uploader` and
SDRTrunk's Rdio Scanner streaming type both work as-is.

**Your talkgroup CSV.** The same RadioReference export imports here unchanged — with a header
row it reads columns by name in any order, and without one it falls back to the positions
RadioReference and Trunk Recorder export at.

**Your habits.** Hold, avoid, talkgroup selection, archive search and download are all here.

## What does not migrate

**The call archive.** rdio-scanner stores audio as BLOBs inside its database; Radio-Scout
stores audio as objects and keeps only metadata in the database. There is **no importer**, so a
new instance starts with an empty Archive and fills as Calls arrive.

If your history matters, the realistic options are to keep the old instance running read-only
for as long as you need it, or to accept the gap. Running both in parallel (below) means the
gap is only ever "everything before the day you switched".

**Access codes.** rdio-scanner's per-listener PINs with scoped access and expiry are not built.
Listening is open to whoever can reach the instance — put it behind a VPN or an authenticating
reverse proxy if that matters to you.

**The admin web UI.** Configuration is a TOML file, environment variables and flags rather than
a settings interface. The admin surface today is login and talkgroup CSV import.

**`/rdio-scanner`.** The legacy app is not hosted.

**Downstream forwarding, the broadcast delayer, and dirwatch ingest.** Not built.

## Running both at once

**This is the recommended way to switch**, and it costs nothing: recorders will happily upload
to two servers at the same time. Your existing feed carries on untouched while Radio-Scout
fills up beside it, and if you dislike it you delete one config block.

For **Trunk Recorder**, add a second entry to `plugins` — do not replace the first:

```jsonc
"plugins": [
  { "name": "rdio-scanner", "library": "librdioscanner_uploader.so", /* … existing … */ },
  {
    "name": "radio-scout",
    "library": "librdioscanner_uploader.so",
    "server": "http://<host>:3000",
    "systems": [
      { "shortName": "<same as above>", "apiKey": "<RADIO_SCOUT_API_KEY>", "systemId": 411 }
    ]
  }
]
```

Every entry in `plugins` is loaded independently and keeps its own server and key, so the two
uploads cannot interfere. **Give them distinct names** — Trunk Recorder logs the plugin's name
on upload failures, and that name is the only way to tell which server rejected a Call.

For **SDRTrunk**, add a second Rdio Scanner broadcast configuration in Streaming and enable
both.

## The cutover

1. **Install and start Radio-Scout** — see [deploy.md](deploy.md). First run creates everything
   it needs and prints where it put the credentials.
2. **Read the ingest key**: `cat .env`. It is never logged, so this is the only way to get it.
3. **Add a second uploader** to each recorder, as above. Leave the existing one alone.
4. **Confirm Calls are arriving**: `curl 'http://<host>:3000/api/calls?limit=5'`, or just open
   the app. If nothing shows up, [recorders.md](recorders.md#common-problems) covers the usual
   causes — the most common is putting `/api/call-upload` into Trunk Recorder's `server`, which
   wants a bare base URL.
5. **Import your talkgroup CSV** so names replace numbers — with `dryRun=true` first. See
   [operating.md](operating.md#tidying-up-talkgroup-names).
6. **Set retention** before the archive grows into the disk. `[retention] days` and/or
   `max_size_gb`; see [operating.md](operating.md#retention).
7. **Install it on your phone** — this is the part that is actually different. Safari → Share →
   Add to Home Screen on iOS. See [using.md](using.md#on-your-phone).
8. **Run both for a while.** When you stop opening the old one, remove its uploader entry.

## Differences that will surprise you

**Talkgroups appear on their own.** Unknown Systems, Talkgroups and Units are created the first
time a Call mentions them, so you do not define anything up front — but they arrive named after
their numbers until you import a CSV. rdio requires the definition first; this is the opposite
default.

**Audio is not in the database.** Backups are now two things taken together: the database and
the audio directory (or bucket). See [operating.md](operating.md#backups).

**Configuration is a file, and it is strict.** An unknown key or an unparseable value refuses
to boot and says why. rdio silently ignores both, which is friendlier right up to the moment
you misspell something and spend an evening wondering why it has no effect.

**Flags beat the config file.** rdio-scanner parses flags first and then loads its INI over the
top, so a flag cannot override a configured value. Here the order is flag → environment →
file → default, which is what you would expect.

**There is no default admin password.** rdio ships a known one behind a nag. Radio-Scout
generates one on first run and writes it to `.env`.

## If you go back

Nothing here is one-way. Radio-Scout never touches rdio-scanner's database, its files or its
configuration — a second uploader entry is the only change made outside this instance's own
directory. Delete that entry and `radio-scout-data`, and you are exactly where you started.
