# Live testing

How to run Radio-Scout for real — against synthetic traffic in a browser, and
against the maintainer's Trunk Recorder over the LAN. Every claim below about
Trunk Recorder was read out of `trunk-recorder/` source, not its docs; line
references are given so they can be re-checked when that checkout moves.

## Two instances, on purpose

| Base dir | Who drives it | Lifetime |
|---|---|---|
| `./radio-scout-live-test` | the `/live-test` skill, synthetic traffic | **wiped at the start of every run** |
| `./radio-scout-data` | plain `cargo run`; where a real recorder uploads | durable — never touched by a scripted test |

Both are gitignored. Keeping them apart is what makes a scripted run mean
something: an empty archive, an empty queue and a known set of Talkgroups, so
"the queue shows 3" is a fact rather than a coincidence of history.

## The loop, without RF

```bash
# 0. Once: copy the template and pick a key (any high-entropy string).
cp .env.example .env          # set RADIO_SCOUT_API_KEY; `openssl rand -hex 16`

# 1. Build the SPA the binary embeds (rust-embed reads client/dist at compile
#    time — skip this and you get the fallback page, not the app).
cd client && npm run build && cd ..

# 2. Start a hermetic instance. It registers the key from .env on every boot.
rm -rf ./radio-scout-live-test
RADIO_SCOUT_BASE_DIR=./radio-scout-live-test cargo run

# 3. In another shell, feed it Calls — same .env, so no key to copy.
cargo run --example feed -- --interval 4s

# 4. Browse http://localhost:3000  (or http://<MAC-LAN-IP>:3000 from a phone)
```

`.env` is a **stopgap until #17** brings real config, and it is gitignored;
`.env.example` is the committed template. `RADIO_SCOUT_API_KEY` is registered on
every boot rather than only on first run, which is what makes a wiped
`./radio-scout-live-test` cost nothing: the key the recorder (or the feeder) is
configured with keeps working. A key an operator *disabled* stays disabled —
re-registering never undoes a revocation (ADR-0008). With no key configured,
first run generates one and **writes it into `.env`** — it is never printed or
logged (ADR-0011 rule 2), so read it back with `cat .env` and point the feeder
or the recorder at it.

The feeder posts to `POST /api/call-upload` with rdio's field names and a real
mono 16-bit WAV per Call, pitched by Talkgroup so two Talkgroups are told apart
**by ear** — a wrong-Call bug is audible before it is visible. Useful flags:

```bash
--burst 5                  # five Calls back to back: fills the listening queue
--patches 54241:54242      # cross-patched Calls (ADR-0004 patch fanout)
--talkgroups 54241,54242   # narrow the mix
--seconds 8                # longer audio, for watching the waveform advance
--count 3                  # send three and exit
```

## Driving the browser

Prerequisites, one time:

1. Install the Claude browser extension (<https://claude.ai/chrome>), signed
   into the same account as Claude Code, and restart Chrome.
2. Grant the extension permission for **`localhost:3000`** — that single origin
   is all a live test needs, which is why the embedded build (not the Vite dev
   server on `:5173`) is what live tests run against.

Then an agent can drive the app: create a tab, navigate to `localhost:3000`,
screenshot, click. Two standing rules:

- **Never trigger a JS dialog** (`alert`/`confirm`) — it blocks the extension
  until a human dismisses it.
- **Read the console** (`read_console_messages`) before declaring a pass. A
  silent WebSocket failure or a rejected `play()` shows up there and nowhere
  else.

### What to actually check

The things no jsdom test can answer:

- A Call arrives with no interaction and **plays audible audio** — the feed,
  the socket and the `<audio>` element in one.
- The **waveform advances** with the audio, and the LED is the Talkgroup's.
- **Hold / Avoid** change what arrives: after Hold TG, the feeder's other
  Talkgroups stop appearing at all (server-side filtering, not client-side
  discarding — confirm in the network panel or by the queue staying flat).
- The **queue count** matches a `--burst`, and skipping walks it.
- **Media Session**: the OS media panel shows the Talkgroup, System and
  artwork, and its buttons drive the app.
- **The archive** (`/search`) finds the Calls that just played, and playback
  mode walks them.

iOS background audio, lock-screen controls and Add-to-Home-Screen remain a
**real-device manual gate** (ADR-0005). A desktop Chrome pass is not evidence
about them.

## Pointing Trunk Recorder at it

Radio-Scout runs on the Mac; the Pi's Trunk Recorder gets a **second**
`rdioscanner_uploader` entry beside the existing rdio-scanner one. This is
safe: `initialize_plugins` iterates every element of the `plugins` array and
calls `setup_plugin` per entry
(`trunk-recorder/trunk-recorder/plugin_manager/plugin_manager.cc:41`), and each
instance keeps its own `Rdio_Scanner_Uploader_Data data` member
(`plugins/rdioscanner_uploader/rdioscanner_uploader.cc:30`) — so the two
uploads have independent servers and keys, and the existing rdio-scanner feed
is untouched.

```jsonc
// Pi's Trunk Recorder config.json — ADD to "plugins", don't replace.
"plugins": [
  { "name": "rdio-scanner", "library": "librdioscanner_uploader.so", /* … existing … */ },
  {
    "name": "Radio-Scout (dev)",
    "library": "librdioscanner_uploader.so",
    "server": "http://<MAC-LAN-IP>:3000",
    "systems": [
      {
        "shortName": "<the shortName from your systems config>",
        "apiKey": "<RADIO_SCOUT_API_KEY from the Mac's .env>",
        "systemId": 411,
        // Optional: keep a dev box from drinking the whole firehose.
        "talkgroupAllow": ["54241", "5424*"]
      }
    ]
  }
]
```

- `server` is a **bare base URL** — the plugin appends `/api/call-upload`
  itself (`rdioscanner_uploader.cc:319`), so it lands on the generic rdio
  endpoint (#5), *not* our Trunk-Recorder-native one (#6, which nothing in the
  wild posts to).
- `systemId` becomes our **System Ref**; `shortName` must match a system in the
  main config. Unknown Systems and Talkgroups are auto-populated (#8), so
  nothing needs configuring on our side first.
- `talkgroupAllow` / `talkgroupDeny` take glob patterns
  (`rdioscanner_uploader.cc:603`) — worth using on a busy system.
- Find the Mac's LAN address with `ipconfig getifaddr en0`, and make sure macOS
  isn't firewalling the port (System Settings → Network → Firewall). The binary
  already binds `0.0.0.0`, so nothing on our side needs changing.

### Confirming it works

On the Pi, Trunk Recorder logs the plugin's **name** on every failure —
`<name> Upload Error (HTTP <code>): <body>`
(`rdioscanner_uploader.cc:546`) — which is why the entry above is named
"Radio-Scout (dev)": in a two-uploader config the name is how you tell which
server rejected a Call. Success is quiet.

On the Mac, `GET /api/calls?limit=5` should show the Calls, and the response
body Trunk Recorder got is one of our rdio-compatible strings:

| Body | Meaning |
|---|---|
| `Call imported successfully.` | stored |
| `duplicate call rejected` | inside the dedup window (#5) — expected on a re-send |
| `Incomplete call data: no talkgroup` | the recorder sent no talkgroup |
| `invalid api key` | key mismatch — the entry's `apiKey` must equal `RADIO_SCOUT_API_KEY` |

## Troubleshooting

| Symptom | Cause |
|---|---|
| The page loads but looks like a plain HTML notice | `client/dist` wasn't built — the binary served its fallback (`src/web.rs`) |
| Calls ingest but the Live screen stays empty | the live-feed socket didn't connect; check the console and that the page is on the same origin as the API |
| Audio never plays, no error | a browser autoplay refusal — the app records it as paused, so look for the Play button |
| TR logs `Upload Error (HTTP 401)` | wrong `apiKey` for that entry |
| TR logs nothing and nothing arrives | wrong `server` host/port, or the Mac's firewall — curl the URL from the Pi |
