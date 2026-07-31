# Pointing a recorder at Radio-Scout

Radio-Scout accepts uploads in **rdio-scanner's dialect**, exactly — same endpoint, same field
names, same aliases, same response strings. So there is **nothing to patch and no plugin to
build**: Trunk Recorder and SDRTrunk already know how to talk to it, and all you change is a
URL.

Trunk Recorder can do better than that dialect, though, and the recommended setup below uses a
small shipped script to send everything it knows. There is a first-party plugin that sends the
same thing, for installs that would rather load one. SDRTrunk has one way in, and it is the URL.

Every claim here about a recorder was read out of that recorder's source, not its docs, with
line references so it can be re-checked when those projects move.

## Before you start

You need two things from the instance:

- **Its address.** `http://<host>:3000` by default. The binary binds `0.0.0.0`, so a recorder
  on another machine can reach it — check the host's firewall if it can't.
- **The ingest API key.** First run generates one and writes it to `.env`; it is never logged,
  so `cat .env` is how you read it back. You can also set `RADIO_SCOUT_API_KEY` yourself to
  anything high-entropy (`openssl rand -hex 16`) — it is registered on every boot, so it
  survives restarts and even a wiped database.

You do **not** need to define Systems or Talkgroups first. Unknown ones are created the first
time a Call mentions them, so an empty instance fills itself in as traffic arrives. Tidy the
names afterwards with a [talkgroup CSV import](operating.md#tidying-up-talkgroup-names).

---

## Trunk Recorder

There are three ways in, and they differ in **how much of what your recorder knows survives the
trip** — and in what it costs you to set up.

| | `uploadScript` (recommended) | `radio_scout_uploader` plugin | `rdioscanner_uploader` plugin |
| --- | --- | --- | --- |
| Setup | One line in `config.json` + one shipped script | One block in `config.json` — **and a Trunk Recorder rebuild** | One block in `config.json`, no download |
| Emergency / encrypted flags | ✅ | ✅ | ❌ |
| Exact call duration | ✅ | ✅ | ❌ (measured from the audio instead) |
| Per-frequency decode health | ✅ | ✅ | partial (no timing) |
| Over-the-air radio aliases | ✅ | ✅ | ❌ |
| Priority, audio type, stop time | ✅ | ✅ | ❌ |
| Retries a failed upload | ❌ (never — see below) | ✅ (Trunk Recorder's, ~2 min then ~4) | ✅ |
| Talkgroup allow/deny globs | ❌ | ✅ | ✅ |

The `rdioscanner_uploader` plugin works, and if you are already running it nothing is broken.
But the rdio-scanner dialect it speaks has no field for most of what Trunk Recorder writes
down, so that half is discarded at the door. The other two send the recorder's own `.json`
untouched.

**Start with `uploadScript`.** It is one line and needs no rebuild, and for almost everyone
that is the end of it. The plugin is worth the rebuild for one reason: **it can retry.** A
script cannot — a non-zero exit from `uploadScript` takes down every *other* plugin on the
recorder (see below), so ours deliberately gives up on the first failure and the Call is gone.
A plugin's failure is its own, so Radio-Scout being down for a restart costs you nothing.

### The recommended setup: `uploadScript`

Fetch the script onto the **recorder** — not necessarily the machine running Radio-Scout. It is
published with each release, alongside the `SHA256SUMS` that covers it:

```bash
curl -fsSLO https://github.com/FxllenCode/radio-scout/releases/latest/download/radio-scout-upload.sh
curl -fsSL  https://github.com/FxllenCode/radio-scout/releases/latest/download/SHA256SUMS \
  | grep radio-scout-upload.sh | sha256sum -c -
chmod +x radio-scout-upload.sh
sudo mv radio-scout-upload.sh /opt/
```

Put the address and the key in a file, and give it to the user Trunk Recorder runs as. The key
must not go on a command line, because `ps` shows those to every user on the box:

```bash
sudo tee /etc/radio-scout.env >/dev/null <<'EOF'
RADIO_SCOUT_URL=http://<host>:3000
RADIO_SCOUT_API_KEY=<the key from .env>
EOF
sudo chown "$(id -un)" /etc/radio-scout.env   # ...or root, if TR runs as root
sudo chmod 0600 /etc/radio-scout.env
```

> **Get the ownership right.** The file is *read by the script*, which runs as whoever Trunk
> Recorder does. If TR cannot read it the script treats that as a broken install and exits
> non-zero, which — see below — stops the other plugins too. `sudo -u <tr-user> cat
> /etc/radio-scout.env` is the one-line check.

The file is **sourced by the shell**, not parsed like systemd's `EnvironmentFile=`. In practice
that means the same `KEY=value` lines work, but a value containing spaces needs quotes.

Then one line in Trunk Recorder's `config.json`:

```jsonc
"uploadScript": "/opt/radio-scout-upload.sh --env-file /etc/radio-scout.env"
```

That is the whole of it. Trunk Recorder appends the call's `.wav`, `.json` and `.m4a` paths
itself; the script picks the `.m4a` when `compressWav` made one (much smaller over a home
uplink) and the `.wav` when it didn't.

If you would rather keep the key in your service manager than in a file, drop `--env-file` and
set the two variables in the environment Trunk Recorder runs with —
`Environment=RADIO_SCOUT_API_KEY=…` in a systemd unit, or `-e` on a `docker run`. The script
reads `RADIO_SCOUT_URL` and `RADIO_SCOUT_API_KEY` from wherever they come from, and `--server`
overrides the address for a second recorder pointed somewhere else.

**When something goes wrong it says so in Trunk Recorder's own log**, prefixed so it is
greppable:

```
radio-scout: upload failed (curl 7): curl: (7) Failed to connect to scout.lan port 3000
radio-scout: upload refused (HTTP 401): Invalid API key for system 0 talkgroup 54155.
```

`upload failed` means nobody answered; `upload refused` means Radio-Scout did, and the rest of
the line is its own words. Neither ever contains the API key.

**A failed upload never fails the call.** Trunk Recorder treats a non-zero exit from
`uploadScript` as a fatal error for that call: it stops, and — this is the part that matters —
it skips every *other* plugin too (`call_concluder.cc:981-987`), with no retry. So if
Radio-Scout is down or restarting, this script complains loudly and exits **0**, and your
existing rdio-scanner feed carries on untouched. It exits non-zero only when the *setup* is
wrong — no key, no server, an unreadable file — which is something you want to find out on the
first call rather than a week later.

### The first-party plugin: `radio_scout_uploader`

Sends exactly what the `uploadScript` does — Trunk Recorder's own call metadata, untouched —
but from inside the recorder's process, so there is no shell and no `curl` per Call. What you
get for the rebuild is **retries**: if the upload fails, Trunk Recorder keeps the call's files
and tries this plugin again after roughly two minutes, then four, before giving up. Nothing
else on the recorder is affected either way.

Fetch it onto the **recorder**, into the Trunk Recorder source tree you built from:

```bash
cd /path/to/trunk-recorder
curl -fsSLO https://github.com/FxllenCode/radio-scout/releases/latest/download/radio-scout-tr-plugin.tar.gz
curl -fsSL  https://github.com/FxllenCode/radio-scout/releases/latest/download/SHA256SUMS \
  | grep radio-scout-tr-plugin | sha256sum -c -
mkdir -p user_plugins
tar -xzf radio-scout-tr-plugin.tar.gz -C user_plugins
```

Then rebuild Trunk Recorder the way you built it the first time:

```bash
cmake -B build && cmake --build build -j"$(nproc)" && sudo cmake --install build
```

`user_plugins/*/CMakeLists.txt` is picked up automatically — the configure step prints
`Added user plugin: radio-scout` when it has found it. If you don't see that line, the archive
landed somewhere other than `user_plugins/`.

Then the `plugins` entry in `config.json`:

```jsonc
"plugins": [
  {
    "name": "radio-scout",
    "library": "libradio_scout_uploader.so",
    "server": "http://<host>:3000",
    "apiKey": "<the key from .env>",
    // Optional. Default 60 — how long one upload may take before it is
    // abandoned and left to the retry.
    "timeoutSecs": 60,
    // Optional, and only needed for per-system keys or filters. A system with
    // no entry here uploads with the key above and sends everything.
    "systems": [
      {
        "shortName": "<must match a system in your main config>",
        "talkgroupAllow": ["54241", "5424*"],
        "talkgroupDeny": ["54999"]
      }
    ]
  }
]
```

- **`server` is a bare base URL**, same as the rdio uploader's — the plugin appends
  `/api/trunk-recorder-call-upload` itself.
- **There is no `systemId`.** The native endpoint files a Call under the System it resolves
  from the recorder's own `shortName`, creating it if it has never been seen.
- **`talkgroupAllow` / `talkgroupDeny` take glob patterns** — `*` for any run of characters,
  `?` for exactly one, and every other character means itself, so `5.155` matches a talkgroup
  with a dot in it and nothing else. A non-empty allow list is exhaustive; deny then removes
  from what is left.
- **Failures name themselves in Trunk Recorder's log**, prefixed with the plugin's `name`:

  ```
  [radio-scout]	fulton TG 54155	upload failed: Failed to connect to scout.lan port 3000
  [radio-scout]	fulton TG 54155	upload refused (HTTP 401): Invalid API key for system 0 talkgroup 54155.
  ```

  `upload failed` means nobody answered; `upload refused` means Radio-Scout did, and the rest
  of the line is its own words. Neither ever contains the API key.

> **A Call on an encrypted talkgroup is forwarded, not dropped.** The `rdioscanner_uploader`
> plugin discards those (`rdioscanner_uploader.cc:171-173`), because the rdio dialect has no
> field to say what they are. This one sends them, and Radio-Scout stores a flagged row with no
> audio — so you keep the record that the channel was busy.
>
> Whether you ever see one is Trunk Recorder's decision, and it changed. **On 5.0.2** a plugin
> gets an encrypted Call only when that system has `"monitorEncrypted": true` — otherwise
> nothing is recorded to conclude. **On the current development branch** (post-5.0.2, what
> `git clone` and the `latest` Docker image give you) it never gets one at all: `conclude_call`
> writes the metadata and returns before any plugin or `uploadScript` runs
> (`call_concluder.cc:1244-1252`). Nothing to configure either way — the plugin is simply
> right when it is asked.

### The alternative: the rdio-scanner uploader plugin

Add an entry to the `plugins` array in `config.json`:

```jsonc
"plugins": [
  {
    "name": "radio-scout",
    "library": "librdioscanner_uploader.so",
    "server": "http://<host>:3000",
    "systems": [
      {
        "shortName": "<must match a system in your main config>",
        "apiKey": "<RADIO_SCOUT_API_KEY>",
        "systemId": 411,
        // Optional — a busy system does not have to send you everything.
        "talkgroupAllow": ["54241", "5424*"]
      }
    ]
  }
]
```

- **`server` is a bare base URL.** The plugin appends the path itself —
  `data.server + "/api/call-upload"` (`rdioscanner_uploader.cc:319`). Adding the path yourself
  produces `/api/call-upload/api/call-upload` and nothing works.
- **`systemId` becomes the System's Ref**, the identity Radio-Scout files Calls under.
  `shortName` must match a system in your main configuration.
- **`talkgroupAllow` / `talkgroupDeny` take glob patterns** (`rdioscanner_uploader.cc:603`),
  which is the cheap way to keep a Pi from drinking the whole firehose.

### Running alongside your existing rdio-scanner

**You can upload to both at once, and it is safe.** `initialize_plugins` iterates *every*
element of `plugins` and calls `setup_plugin` per entry
(`plugin_manager.cc:41`), and each instance keeps its own `Rdio_Scanner_Uploader_Data`
(`rdioscanner_uploader.cc:30`) — so two entries have independent servers and keys, and your
existing feed is untouched. This is the recommended way to try Radio-Scout, and the
recommended way to cut over.

Give the entries **distinct `name`s**. Trunk Recorder logs the plugin's name on every failure:

```
<name> Upload Error (HTTP <code>): <body>
```

(`rdioscanner_uploader.cc:546`.) In a two-uploader config that name is the only thing telling
you which server rejected a Call. Success is quiet.

---

## SDRTrunk

In **Streaming**, add a broadcast configuration of type **Rdio Scanner**, then fill in:

| Field | Value |
| --- | --- |
| **Name** | Anything — it is a local label |
| **RdioScanner URL** | `http://<host>:3000` — the **base** URL |
| **API Key** | `RADIO_SCOUT_API_KEY` |
| **System ID** | The number you want this system filed under (its Ref) |
| **Max Recording Age (seconds)** | See the warning below |

**Enter the base URL only.** The editor shows a static `/api/call-upload` beside the field and
appends it when you save — `host.replace(API_PATH, ""); host += API_PATH`
(`RdioScannerEditor.java:100-110`) — then hides it again when you reopen the editor. The
playlist XML therefore stores the full URL while the box shows a base one; both are correct,
and typing the path in yourself is harmless because the editor strips it first.

> **`Max Recording Age` silently drops a backlog.** SDRTrunk discards recordings older than
> this before uploading them. If Radio-Scout is down for longer than that window, those Calls
> are gone — they are not queued and retried. Set it deliberately.

---

## Checking it works

The recorder's own logs are the first place to look, but the response body is the real answer.
Radio-Scout returns rdio-scanner's exact strings, so a recorder written against rdio
understands them unchanged:

| Response | Status | Meaning |
| --- | --- | --- |
| `Call imported successfully.` | 200 | Stored |
| `duplicate call rejected` | 200 | An identical Call arrived inside the dedup window — expected on a re-send |
| `Incomplete call data: no talkgroup` | 417 | The upload carried no talkgroup |
| `Incomplete call data: no audio` | 417 | The upload carried no audio part |
| `Incomplete call data: malformed multipart body` | 417 | The body was not parseable as multipart |
| `Invalid API key for system <n> talkgroup <n>.` | 401 | The key does not match, is disabled, or is not scoped to that System |

A rejection is answered `200` in two cases on purpose — a duplicate, and a Call dropped by
policy — because a recorder that gets an error will retry forever, and neither of those will
ever succeed.

From the instance's side:

```sh
curl 'http://<host>:3000/api/calls?limit=5'      # the newest Calls, as JSON
```

Every rejected upload is also logged with a machine-readable reason
(`invalid-api-key`, `duplicate`, `blacklisted`, `no-talkgroup`, …), so
`journalctl -u radio-scout -f` tells you *why* something isn't arriving rather than merely
that it isn't.

## Common problems

**Nothing arrives, no errors in the recorder's log.** The recorder is probably not reaching the
host at all. Check the firewall on the machine running Radio-Scout, and that you used its LAN
address rather than `localhost`.

**`Invalid API key for system …`.** The recorder's key must equal `RADIO_SCOUT_API_KEY`
exactly. Read the real value with `cat .env` — do not retype it from a log, because it is never
logged. A key can also be scoped to particular Systems, in which case it is valid but not for
the System named in the message.

**404s from Trunk Recorder.** Almost always `/api/call-upload` typed into `server`. It is a
base URL.

**Calls arrive with numeric names instead of labels.** That is auto-populate doing its job:
the recorder sent a Talkgroup it had no name for. Import a talkgroup CSV to fix the names in
one shot — see [operating.md](operating.md#tidying-up-talkgroup-names).

**A patched call doesn't reach everyone you expected.** A patch reaches listeners through its
member Talkgroups, and Radio-Scout counts a member only when the System already has that
Talkgroup. It has to: SDRTrunk lists the radios patched into a group in the same field as the
talkgroups, with nothing separating them, so a number it has never seen could be either — and
guessing wrong would push audio to whoever selected that channel. Two things follow. Radios
patched into a group are ignored, which is what you want. And a Talkgroup that has never
carried a call of its own is not yet known, so it is skipped on the first patch and included
from then on. Importing a talkgroup CSV up front makes every member known immediately — see
[operating.md](operating.md#tidying-up-talkgroup-names).

## A note on the Trunk-Recorder-native endpoint

Radio-Scout also serves `POST /api/trunk-recorder-call-upload`, which takes Trunk Recorder's
own `.wav` + `.json` metadata format rather than the rdio dialect. The `rdioscanner_uploader`
plugin — the one everybody runs today — posts to `/api/call-upload` instead, so nothing needs
configuring for it and nothing breaks if you ignore it.

It is worth knowing about because **the rdio dialect throws away most of what your recorder
knows.** Trunk Recorder writes all of this into every call's `.json`, and none of it fits
through `/api/call-upload`:

| What TR writes | What Radio-Scout does with it |
| --- | --- |
| `emergency` | An ⚠ badge on the Call, live and in the archive |
| `encrypted` | The Call is stored as a flagged, metadata-only row — no audio object at all |
| `call_length_ms` | The Call's length, exact rather than measured off the audio |
| `stop_time` | When the transmission ended |
| `priority`, `audio_type` | Recorded, and served by `GET /api/call/{id}` |
| `freqList` error/spike counts | Per-frequency decode health, for spotting a dying dongle |
| `srcList` `tag_ota` | The name each radio broadcast about *itself*, kept beside your configured alias |

Everything the rdio dialect already carries works exactly as it does there, and the response
strings are byte-identical, so a recorder cannot tell the difference from its side.

Anything in the `.json` that Radio-Scout does not model — `freq_error`, `signal`, `noise`,
`color_code`, and the rest — is ignored rather than treated as an error, so a Trunk Recorder
newer than your Radio-Scout still uploads fine.

**The shipped `uploadScript` and the `radio_scout_uploader` plugin are both paths to it**, and
both are documented above — there is nothing else to configure to get these fields. This is
also the right target if you are writing something yourself.
