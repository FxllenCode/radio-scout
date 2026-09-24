# Radio-Scout's Trunk Recorder status plugin

Dials Radio-Scout's status WebSocket — `/api/recorder-status` — and pushes what your SDRs
are doing: active calls and why any of them are *not* being recorded, control-channel decode
rates, and every demodulator's state. Radio-Scout renders it live under
**Settings → Admin → Recorders**.

Audio does not go through here. Uploading calls is a separate thing, and you almost certainly
already have it set up — see the operator guide linked below.

## Why this exists

Trunk Recorder has a status plugin of its own (`plugins/stat_socket`) and **does not compile
it**: the recorder's top-level `CMakeLists.txt` builds five plugins by name, and that is not
one of them. So a stock recorder cannot dial a status socket at all, whatever `statusServer`
says. This is that plugin, built against the same recorder, with three fixes — an uninitialised
member that is read on every poll, a reconnect backoff that grows without bound, and a `server`
key of its own so it can run beside whatever else is already reading `statusServer`.

The wire format is upstream's, unchanged and deliberately so: Radio-Scout reads Trunk Recorder's
own status dialect, so an Operator running the original plugin gets the same dashboard.

## Installing it

This directory goes in Trunk Recorder's `user_plugins/`, and Trunk Recorder is then rebuilt:

```bash
cd /path/to/trunk-recorder
mkdir -p user_plugins
tar -xzf radio-scout-tr-status-plugin.tar.gz -C user_plugins
cmake -B build && cmake --build build -j"$(nproc)" && sudo cmake --install build
```

The configure step prints `Added user plugin: radio-scout-status` when it has found this
directory.

Then add it to the `plugins` array in your `config.json`, pointing `server` at your instance
with the **same API key your recorder already uploads with**:

```json
{
  "plugins": [
    {
      "name": "radio_scout_status",
      "library": "libradio_scout_status.so",
      "server": "ws://radio-scout.lan:3000/api/recorder-status?key=YOUR_API_KEY"
    }
  ]
}
```

Leave `server` out and it falls back to the recorder's global `statusServer` — which is how to
use it if you have nothing else reading that.

**The rest — what the dashboard shows, how to read a refused connection, and the health charts
that come from your uploads rather than from here — is in the operator guide**, which is the one
place it is kept up to date:
<https://github.com/FxllenCode/radio-scout/blob/master/docs/recorders.md>

## What is in here

| File | |
| --- | --- |
| `radio_scout_status.cc` | The whole plugin: a WebSocket client that pushes Trunk Recorder's own status messages. Forked from the recorder's `plugins/stat_socket`; every difference is commented at the top of the file. |
| `CMakeLists.txt` | Builds it as a `user_plugins` module. |
