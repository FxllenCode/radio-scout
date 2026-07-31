# Radio-Scout's Trunk Recorder plugin

Posts each finished Call to Radio-Scout's **native** endpoint —
`POST /api/trunk-recorder-call-upload` — which takes Trunk Recorder's own call metadata
object, untouched, rather than the rdio-scanner dialect that has no field for most of it.
Which fields those are, and how this compares to the other two ways in, is in the operator
guide linked below.

## Installing it

This directory goes in Trunk Recorder's `user_plugins/`, and Trunk Recorder is then rebuilt:

```bash
cd /path/to/trunk-recorder
mkdir -p user_plugins
tar -xzf radio-scout-tr-plugin.tar.gz -C user_plugins
cmake -B build && cmake --build build -j"$(nproc)" && sudo cmake --install build
```

The configure step prints `Added user plugin: radio-scout` when it has found this directory.

**The `config.json` block, the talkgroup filters, and how to read a failure are in the
operator guide**, which is the one place they are kept up to date:
<https://github.com/FxllenCode/radio-scout/blob/master/docs/recorders.md>

## What is in here

| File | |
| --- | --- |
| `radio_scout_uploader.cc` | The plugin: the only file that includes a Trunk Recorder header, and it holds no decisions of its own — it reads fields off a `Call_Data_t` and hands them over. |
| `radio_scout_upload.h` / `.cc` | The upload core: which audio file goes, which talkgroups leave the recorder, and the POST itself. Deliberately free of every Trunk Recorder header, so it can be compiled and exercised against a real Radio-Scout without a recorder — which is what Radio-Scout's `tests/trplugin.rs` does on every test run. |
| `harness.cc` | A `main()` for that core, used by those tests. Trunk Recorder never compiles it: `CMakeLists.txt` names its sources one by one. |
