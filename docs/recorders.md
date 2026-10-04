# Pointing a recorder at Radio-Scout

Radio-Scout accepts uploads in **rdio-scanner's dialect**, exactly — same endpoint, same field
names, same aliases, same response strings. So there is **nothing to patch and no plugin to
build**: Trunk Recorder and SDRTrunk already know how to talk to it, and all you change is a
URL.

Trunk Recorder can do better than that dialect, though, and the recommended setup below uses a
small shipped script to send everything it knows. There is a first-party plugin that sends the
same thing, for installs that would rather load one. SDRTrunk has one way in, and it is the URL. For a recorder that only writes files — or one
with no network path to the instance — there is [Dirwatch](#dirwatch-a-recorder-that-only-writes-files).

Every claim here about a recorder was read out of that recorder's source, not its docs, with
line references so it can be re-checked when those projects move.

## Before you start

You need two things from the instance:

- **Its address.** `http://<host>:3000` by default. The binary binds `0.0.0.0`, so a recorder
  on another machine can reach it — check the host's firewall if it can't.
- **The ingest API key.** First run generates one and writes it to `.env`; it is never logged,
  so `cat .env` is how you read it back. You can also set `RADIO_SCOUT_API_KEY` yourself to
  anything high-entropy (`openssl rand -hex 16`) — it seeds an instance whose key roster is
  empty, which covers a first run and a wiped database alike. Once there are keys, the roster
  is yours: issue, scope and revoke them in
  [Settings → Admin → API keys](operating.md#running-the-instance-from-a-browser), and a
  revoked key stays revoked even if the variable is still set.

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
| Per-frequency decode health | ✅ | ✅ | error/spike counts yes, timing no |
| Over-the-air radio aliases | ✅ | ✅ | ❌ (configured alias only) |
| Priority, audio type, stop time | ✅ | ✅ | ❌ |
| Retries a failed upload | ❌ (never — see below) | ✅ (Trunk Recorder's, ~2 min then ~4) | ✅ |
| Talkgroup allow/deny globs | ❌ | ✅ | ✅ |
| Needs a Trunk Recorder rebuild | ❌ | ✅ | ❌ |

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

**Several sites of one network?** Trunk Recorder gives each `system` its own `shortName`, and
by default Radio-Scout files a Call under the System whose label matches it — so two sites of
one WACN/system ID arrive as two Systems. Say which Radio-Scout System they all belong to with
`--system`, on each system's own `uploadScript` line (Trunk Recorder runs one per system):

```jsonc
"uploadScript": "/opt/radio-scout-upload.sh --env-file /etc/radio-scout.env --system 411"
```

`411` is the **Ref** shown beside the System in Settings → Admin → Systems (the number an API
key scoped to a System, and every rdio-scanner upload, calls `system`).

- **Every site of the network needs the flag.** A site without it is matched on its own
  `shortName`, and gets a System of its own.
- **Reuse the Ref the site already has**, rather than picking a fresh one. Naming the Ref of an
  existing System files the site's Calls under it, leaves its label alone, and records the
  `shortName` as the **Site** that heard each Call, shown on the Call itself.
- **A fresh Ref starts a second System** instead, created as `System 411` (the default every
  auto-created System gets) and left that way until you rename it in Settings → Admin → Systems.
  What the site's old System carries stays behind on it: its talkgroup names, groups, tags,
  LEDs, blacklist, retention override and — the one that matters most — its **restrictions**.
  A System a recorder creates is unrestricted, so a restricted site moved to a fresh Ref is
  published openly until someone restricts the new System.
- **A Ref nothing has claimed is only created when global auto-populate is on**
  (`[ingest] auto_populate`). With it off, Radio-Scout drops the Call instead, answering `200`
  as though it had taken it, and says why in its own log as `reason=not-populated` — see
  "`Upload Success` means Radio-Scout accepted the request" further down.

A `--system` that is not a whole number from 1 to 999999999999999999 (so at most 18 digits)
stops the script at once, because a typo there would otherwise quietly file a day's Calls
somewhere you did not mean — but it is only a syntax check, not a check against what Refs
exist: `4111` passes it exactly as readily as `411`, so reusing the right number, copied from
Settings → Admin, is what actually protects you.

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
    // Optional, and only needed for per-system keys, filters or a System Ref.
    // A system with no entry here uploads with the key above, sends everything,
    // and is filed under the System whose label matches its shortName.
    "systems": [
      {
        "shortName": "<must match a system in your main config>",
        // Optional. File this system's Calls under Radio-Scout System 411 rather
        // than matching shortName against a label — give every site of one
        // network the same number.
        "systemId": 411,
        "talkgroupAllow": ["54241", "5424*"],
        "talkgroupDeny": ["54999"]
      }
    ]
  }
]
```

- **`server` is a bare base URL**, same as the rdio uploader's — the plugin appends
  `/api/trunk-recorder-call-upload` itself.
- **`systemId` is optional here.** Without it the native endpoint files a Call under the
  System whose label matches the recorder's own `shortName`, creating it if it has never been
  seen — and, like a named Ref, only when global auto-populate is on. With it, the Call goes
  under that Ref whatever the site is called, which is what you want when several sites share
  one network. It is the same setting and the same rules as `--system` on the `uploadScript`,
  above — reuse the Ref the site already has, give every site of the network the setting, and
  a fresh Ref starts a second System labelled `System <ref>`. It has to be a whole number from
  1 to 999999999999999999, the same range the script takes, and anything else is refused the
  way a bad `--system` stops the script: nothing from that system is uploaded, and every Call
  it refuses says so in Trunk Recorder's log, until the setting is fixed.
- **`talkgroupAllow` / `talkgroupDeny` take glob patterns** — `*` for any run of characters,
  `?` for exactly one, and every other character means itself, so `5.155` matches a talkgroup
  with a dot in it and nothing else. A non-empty allow list is exhaustive; deny then removes
  from what is left.
- **Every outcome is logged in Trunk Recorder's own format**, the same header and wording its
  `rdioscanner_uploader` uses, with the plugin's `name` where that one prints its own:

  ```
  [fulton]	10C	TG:      54155 (EMS Dispatch)	Freq: 773.181250 MHz	radio-scout Upload Success - file size: 32597
  [fulton]	10C	TG:      54155 (EMS Dispatch)	Freq: 773.181250 MHz	radio-scout Upload Error: Failed to connect to scout.lan port 3000
  [fulton]	10C	TG:      54155 (EMS Dispatch)	Freq: 773.181250 MHz	radio-scout Upload Error (HTTP 401): Invalid API key for system 0 talkgroup 54155.
  ```

  `Upload Error: …` with no HTTP status means nobody answered; `Upload Error (HTTP …)` means
  Radio-Scout did, and the rest of the line is its own words. Neither ever contains the API key.
  A Call kept back by `talkgroupAllow`/`talkgroupDeny` says `Skipped upload due to talkgroup
  filter`.
- **`Upload Success` means Radio-Scout accepted the request, not that it kept the Call.** A
  Call on a Talkgroup or System it has no record of, with auto-populate off, is answered
  `200` and dropped (rdio-scanner's own behaviour, so recorders never retry it); the reason is
  in Radio-Scout's log as `reason=not-populated`.

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

### The live dashboard: `radio_scout_status`

A **second, optional** plugin, and a completely separate job from uploading. It sends no audio
and no Calls — it pushes what your SDRs are doing right now, so **Settings → Admin →
Recorders** shows active calls, why any of them are *not* being recorded, control-channel
decode rates and every demodulator's state. Nothing about your uploads changes, and skipping
this leaves that screen empty and nothing else affected.

> **Trunk Recorder has a status plugin and does not build it.** `plugins/stat_socket` is in the
> recorder's source tree, and the top-level `CMakeLists.txt` compiles five plugins by name —
> that is not one of them. So a stock recorder cannot dial a status socket at all, whatever
> `statusServer` says. Radio-Scout ships that plugin, built against the same recorder, with a
> `server` key of its own so it can run beside anything already reading `statusServer`, and with
> two upstream defects fixed (a member read before it is written, and a reconnect delay that
> grows without bound).

Fetch it onto the **recorder**, into the Trunk Recorder source tree you built from. It needs one
library a stock recorder build never asks for — the WebSocket client, websocketpp — so install
that first (Debian, Ubuntu and Raspberry Pi OS all package it):

```bash
sudo apt-get install libwebsocketpp-dev
cd /path/to/trunk-recorder
curl -fsSLO https://github.com/FxllenCode/radio-scout/releases/latest/download/radio-scout-tr-status-plugin.tar.gz
curl -fsSL  https://github.com/FxllenCode/radio-scout/releases/latest/download/SHA256SUMS \
  | grep radio-scout-tr-status-plugin | sha256sum -c -
mkdir -p user_plugins
tar -xzf radio-scout-tr-status-plugin.tar.gz -C user_plugins
cmake -B build && cmake --build build -j"$(nproc)" && sudo cmake --install build
```

The configure step prints `Added user plugin: radio-scout-status` when it has found it.

Then the `plugins` entry — beside the uploader's, if you have one:

```jsonc
"plugins": [
  {
    "name": "radio_scout_status",
    "library": "libradio_scout_status.so",
    // The **same API key** your recorder already uploads with. It rides in the
    // query string because that is the only thing a WebSocket URL can carry —
    // and Radio-Scout never writes a query string to its log.
    "server": "ws://<host>:3000/api/recorder-status?key=<the key from .env>"
  }
]
```

- **`ws://`, not `http://`.** This is a WebSocket. Behind a TLS reverse proxy it is `wss://`.
- **Leave `server` out** and it falls back to the recorder's global `statusServer`, which is
  what to do if nothing else is reading that.
- **A wrong key is refused before the socket opens** — an ordinary `401`, logged on the
  instance as `reason=invalid-recorder-key`, and never carrying the key itself.
- **A recorder that goes quiet is reaped** after thirty to forty-five seconds and shown as
  gone, rather than left on the screen as a row that might be fine. Trunk Recorder reconnects
  on its own.

The **health charts** on that same screen come from somewhere else entirely: they are measured
from the per-call metadata your uploads already carry (tuning error, signal, noise, decode and
spike counts, and which SDR took the call), so they work whether or not this plugin is
installed — but only on the Trunk-Recorder-native paths, because the rdio dialect has no field
for any of it. See [the note below](#a-note-on-the-trunk-recorder-native-endpoint).

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

There is nothing else to set up. In particular, **the radio aliases and site names you have
configured in SDRTrunk reach Radio-Scout on their own**, even though its upload API has no field
for either: SDRTrunk writes them into the ID3 tag of every MP3 it uploads, and Radio-Scout reads
them as each Call arrives. It also goes back over Calls you uploaded before. See
[Names SDRTrunk was already sending you](operating.md#names-sdrtrunk-was-already-sending-you).

---

## Dirwatch: a recorder that only writes files

Everything above is an upload. **Dirwatch** is the other way in: Radio-Scout watches a folder
the recorder already writes to, and ingests each Call it finds there through the same pipeline
an upload takes — deduplication, auto-populate, merges, unit names, all of it. Reach for it when
there is no network path from the recorder to the instance, for **DSDPlus Fast Lane** (which
has no upload at all), or for anything else that writes audio files with the call's details in
their names.

### Turning it on

A watch reads every matching file in its folder and can delete them afterwards, so **where a
watch may be is set in the configuration, not in the browser** — the browser only chooses
inside the folders you allow:

```toml
[dirwatch]
roots = ["/srv/trunk-recorder", "/home/pi/SDRTrunk/recordings"]
```

(or `RADIO_SCOUT_DIRWATCH_ROOTS=/srv/trunk-recorder:/home/pi/SDRTrunk/recordings`, separated as
`PATH` is), then restart. With no roots, Dirwatch is off and the screen says so. Then add a watch
under **Settings → Admin → Dirwatch**: the folder, which recorder writes it, and whether to
delete each file once it is ingested. Radio-Scout's service user needs to be able to read the
folder — and to write it, for delete-after.

### Trunk Recorder

Point the watch at TR's `captureDir` — the whole of it; the `shortName/YYYY/M/D` folders under
it are watched as they appear. TR has to **keep** its files for there to be anything to read, so
leave `audioArchive` and `callLog` at their default `true`. Each Call is its `.json` and the
audio beside it: `wav` by default, or set the watch's extension to `m4a` if `compressWav` is on
and you would rather store the smaller copy.

TR writes the `.json` *before* it renders the audio, so for a moment every Call is a `.json`
alone. A watch waits for the audio to arrive rather than dropping the Call, which is what
rdio-scanner does; a `.json` still alone after a minute is refused `no-audio`.

The `.json` is the same document the native upload sends, read by the same parser, so a Call
that arrives this way is identical to one that arrived by `uploadScript`. Its System is the one
whose label matches the `short_name`, exactly as on an upload — or set **System ref** on the
watch to file everything under a Ref of your choosing.

### SDRTrunk

Set SDRTrunk's audio recording format to **MP3** and point the watch at its recordings folder.
Everything comes from the ID3 tag SDRTrunk writes into each file: the talkgroup and its alias,
the radio, the System by name, the tower, and when it was recorded. Set **System ref** on the
watch if you would rather file it under a Ref than by SDRTrunk's System name.

The tag's date is written in the recorder machine's local time with no zone, and Radio-Scout
reads it in **its own** — see [Times with no zone](#times-with-no-zone).

### DSDPlus Fast Lane

Point the watch at the `Record`, `1R-Record` or `VC-Record` folder. DSDPlus writes nothing but
the path — the folder is the date, the file name the time, the network, the talkgroup and the
radio (`20220809/153120_001_DMR(BS)_1-899_DCC2_Slot1_GC_750[Ram_Muni]_52.mp3`) — and the
System Ref is read from the network field where the protocol carries one (DMR and Connect Plus
base stations, P25's SYSID, NXDN's site or RAN). Where it does not, set **System ref** on the
watch, or the file is refused `no-system`.

### Anything else: a filename mask

A mask describes a file name with tokens: `cymx_#TG_#DATE_#TIME_#HZ` reads
`cymx_1457_20201231_083439_119100000.wav` as talkgroup 1457 at 08:34:39 on 119.1 MHz. The tokens
are rdio-scanner's — `#TG`, `#TGAFS`, `#TGHZ`, `#TGKHZ`, `#TGMHZ`, `#TGLBL`, `#SYS`, `#SYSLBL`,
`#SITE`, `#SITELBL`, `#UNIT`, `#UNITLBL`, `#DATE`, `#TIME`, `#ZTIME`, `#HZ`, `#KHZ`, `#MHZ`,
`#GROUP`, `#TAG` — so a mask that worked there works here, matched anywhere in the name as
rdio does. A watch has to know which talkgroup and which System every file is: from a token,
or from the watch's own **Talkgroup ref** and **System ref**.

The differences from rdio are all fixes: the text between tokens is matched literally (in
rdio a `.` matches anything and a bracket crashes the server), a mask is checked when you save
it rather than at the first file, `#UNITLBL` works, `#MHZ` rounds rather than truncating, and
`#DATE` without a time no longer reads the date as a count of seconds since 1970.

### What a watch does with a file

- **It waits until the file has been left alone** for the watch's delay (2 seconds by default,
  half a second at the least), so a file still being written is never read half-done. A Trunk
  Recorder Call is both of its files, so audio still growing after the `.json` was read is read
  again.
- **Every file gets an answer, and the answer is logged.** A Call stored or replaced, a
  duplicate, a blacklisted talkgroup — those are answers, and with delete-after the file is
  removed. A file that cannot be a Call is refused with a line naming why
  (`file refused reason=no-match file=…`), counted on the watch, and **left where it is** even
  with delete-after; it is not refused again until it changes. A file whose ingest *failed* —
  the disk full, the database unreachable — is **never** deleted, and is tried again.
- **A restart loses nothing.** A watch that deletes as it goes picks up whatever is still in its
  folder. A watch that keeps its files remembers how far it has read, so it picks up exactly the
  files that arrived while Radio-Scout was down — and a new watch pointed at a folder full of
  history does not import it.
- **When a file does not say when it was recorded,** its modification time does, rather than the
  moment Radio-Scout noticed it — so a backfill after downtime files every Call at its real time.

### Network shares

A share mounted over NFS or SMB sends no notice of new files, so turn on **Look every few
seconds** for a watch on one. Everything else is the same.

### Times with no zone

DSDPlus names, a mask's `#TIME`, and SDRTrunk's tag carry a wall-clock time with no zone, and
Radio-Scout reads them in its own. That is right when it runs on the recorder's machine or in
the same zone. In the Docker image, which has no zone database, mount the host's:
`-v /etc/localtime:/etc/localtime:ro`. A mask's `#ZTIME` is UTC and needs neither.

---

## Checking it works

The recorder's own logs are the first place to look, but the response body is the real answer.
Radio-Scout returns rdio-scanner's exact strings, so a recorder written against rdio
understands them unchanged:

| Response | Status | Meaning |
| --- | --- | --- |
| `Call imported successfully.` | 200 | Stored — or kept as a better copy of a Call already stored |
| `duplicate call rejected` | 200 | The same transmission arrived again inside the dedup window — expected on a re-send, and on every patched or multi-site upload |
| `Incomplete call data: no talkgroup` | 417 | The upload carried no talkgroup |
| `Incomplete call data: no audio` | 417 | The upload carried no audio part |
| `Incomplete call data: malformed multipart body` | 417 | The body was not parseable as multipart |
| `Invalid API key for system <n> talkgroup <n>.` | 401 | The key does not match, is disabled, or is not scoped to that System |

A rejection is answered `200` in two cases on purpose — a duplicate, and a Call dropped by
policy — because a recorder that gets an error will retry forever, and neither of those will
ever succeed.

`duplicate call rejected` is **normal and expected** on a patched or multi-site system: your
recorder uploads one transmission once per patched talkgroup, and Radio-Scout keeps the best copy
as a single Call rather than playing it N times. See
[Hearing each call once](operating.md#hearing-each-call-once) if you want the narrower,
rdio-scanner-style behaviour back.

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

**The Recorders screen is empty.** Either the status plugin is not loaded — the recorder's
configure step prints `Added user plugin: radio-scout-status` when it is, and its startup log
lists the plugins it loaded — or its `server` is wrong. It must start `ws://` (or `wss://`) and
end with `/api/recorder-status?key=…`; a refused key shows up on the instance as
`reason=invalid-recorder-key`. Uploading is unaffected either way, so Calls arriving is not
evidence that this is working.

**The receive-health charts show errors but no signal or drift.** Signal, noise, tuning drift
and which SDR took the call come from Trunk Recorder's own call metadata, so they need the
`uploadScript` or the first-party plugin above. The rdio-scanner uploader has no field for any
of those four — but it does send each frequency's decode error and spike counts, so Calls
arriving that way still chart errors and spikes, under their frequency with no SDR named.

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
