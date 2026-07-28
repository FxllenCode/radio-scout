# Pointing a recorder at Radio-Scout

Radio-Scout accepts uploads in **rdio-scanner's dialect**, exactly — same endpoint, same field
names, same aliases, same response strings. So there is **no plugin to install and nothing to
patch**: Trunk Recorder and SDRTrunk already know how to talk to it, and all you change is a
URL.

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

## A note on the Trunk-Recorder-native endpoint

Radio-Scout also serves `POST /api/trunk-recorder-call-upload`, which takes Trunk Recorder's
own `.wav` + `.json` metadata format rather than the rdio dialect. **You almost certainly do
not want it.** The `rdioscanner_uploader` plugin — the one everybody runs — posts to
`/api/call-upload`, and nothing in the wild posts to the native endpoint. It exists because
that format carries fields the rdio dialect flattens, and it fixes a timestamp bug in rdio's
own handling of them. If you are writing something new, it is the better target; if you are
configuring Trunk Recorder, ignore it.
