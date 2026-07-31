# Recorder-compatibility golden fixtures (ticket #7)

Each `*.multipart` file is a complete `multipart/form-data` request **body** — the
exact bytes a real recorder POSTs to the ingest endpoint, byte-for-byte: CRLF line
endings, the recorder's own field order and part-header quirks, and a real audio
container header. `tests/golden.rs` replays each body over the real HTTP boundary
(with the matching `Content-Type` boundary + `User-Agent`) and asserts both the
**exact response string + status** and the **parsed rows** in the database. This is
the regression guard on the drop-in-replacement promise (spec "Testing Decisions",
ADR-0009).

These were reconstructed from each recorder's own source (cited below) rather than
captured off the wire — no live TR/SDRTrunk instance was reachable from the dev
environment. They are byte-accurate to what those sources emit, and are structured
so a literal pcap capture can drop in later without touching the test.

## `trunk-recorder-call-upload.multipart` → `POST /api/call-upload`

Trunk Recorder's `rdioscanner_uploader` plugin (the endpoint the maintainer's
scanner actually uses). Source: `trunk-recorder/plugins/rdioscanner_uploader/rdioscanner_uploader.cc`
`upload()` (lines 331–401 build the 16 parts, in this exact order):

`audio` (file, `Content-Type: application/octet-stream` — hard-coded at line 335,
regardless of the true MIME), `audioName`, `audioType` (`audio/wav`|`audio/mp4`),
`dateTime` (epoch **seconds**), `frequencies` (JSON), `frequency` (int Hz), `key`,
`patches` (JSON), `talkgroup`, `talkgroupGroup`, `talkgroupLabel`, `talkgroupTag`,
`talkgroupName`, `sources` (JSON), `system`, `systemLabel`. `User-Agent:
TrunkRecorder1.0` (line 421). Filename format: `call_concluder.cc:1045`.

## `sdrtrunk-call-upload.multipart` → `POST /api/call-upload`

SDRTrunk's rdio-scanner broadcaster. Source:
`sdrtrunk/src/main/java/io/github/dsheirer/audio/broadcast/rdioscanner/RdioScannerBuilder.java`
(+ `RdioScannerBroadcaster.java:259–271` for field order). 12 parts, in order:
`key`, `system`, `dateTime` (epoch **seconds**), `talkgroup`, `source` (singular
radio id), `frequency`, `talkerAlias` (often empty; rdio-scanner ignores it, so we
do too), `talkgroupLabel`, `talkgroupGroup`, `systemLabel`, `patches`, `audio`
(file). Quirks reproduced: the header boundary is the 2-dash
`--sdrtrunk-sdrtrunk-sdrtrunk` while the body delimiter is `--`+that =
`----sdrtrunk-sdrtrunk-sdrtrunk` (`RdioScannerBuilder.java:34-35,105`); **no
`Content-Type` on any part**; the audio part writes `filename` **before** `name`
(`RdioScannerBuilder.java:122-124`); MP3 audio. `User-Agent: sdrtrunk`.

## `sdrtrunk-call-upload-patched.multipart` → `POST /api/call-upload`

The same SDRTrunk body, broadcasting a call on a **patch group** (#81). Source:
`RdioScannerBroadcaster.java:546-574` (`getPatches()`), which builds one flat
array — `"[" + patchGroup + ("," + patchedTalkgroup)* + ("," + patchedRadio)* +
"]"` — with **no separator and no type marker** between the talkgroups and the
radios, under the same `patches` field name Trunk Recorder's uploader uses. The
fixture carries patch group `54000` (also the `talkgroup`), patched talkgroups
`54241`/`54242`, then patched radios `1610051`/`1610092`. Nothing on the wire
says where the talkgroups stop, so the split is recovered as rdio-scanner
recovers it (`call.go:572-582`): a ref is a patch member only if the System has
a Talkgroup for it.

## `trunk-recorder-native-meta.multipart` → `POST /api/trunk-recorder-call-upload`

Trunk Recorder's native `.wav`+`.json` upload: metadata rides as one JSON `meta`
part. Mirrors rdio-scanner `TrunkRecorderCallUploadHandler` (`api.go:120`) +
`ParseTrunkRecorderMeta` (`parsers.go`). Parts: `key`, `meta` (JSON), `audio`
(file). The meta carries `start_time` with **no** `timestamp` (locks the
start_time-not-`now()` behavior — rdio's parser has a `// DBEUG`
`call.Timestamp = time.Now()` line that clobbers it; #6 deliberately does not), and
a `talkgroup_group_tag` of `"-"` (locks rdio's `len>0 && != "-"` placeholder drop).

Since #42 the meta is the **complete** field set `create_call_json` writes, in
TR's own key order (`trunk-recorder/trunk-recorder/call_concluder/call_concluder.cc:785`):
`emergency`, `encrypted`, `priority`, `audio_type`, `call_length`/`call_length_ms`,
`stop_time`, and the per-source `time`/`emergency`/`signal_system`/`tag`/`tag_ota`
that rdio-scanner's parser walks past. Keys we deliberately do **not** model
(`freq_error`, `signal`, `noise`, `tdma_slot`, `color_code`, …) are present in the
fixture on purpose: they prove an unmodelled key is ignored rather than fatal.

Because the fixture is the full contract, it is also what pins what the shipped
`uploadScript` (#43) and the first-party plugin (#44) must emit — neither can
drift from the parser without failing here.

## `trunk-recorder-native-encrypted.multipart` → `POST /api/trunk-recorder-call-upload`

The same native contract with `encrypted: 1` — a call on an encrypted talkgroup.
The audio part is present and real, because TR writes a file either way; what is
in it is the vocoder's noise, not speech. Radio-Scout stores the row and writes
no object (#42, spec US 9), and the recorder still gets
`Call imported successfully.` so it never retries. This is the fixture pinning
the metadata-only half of the plugin's contract (#44).

## Regenerating

Byte layout and values are produced by `gen_fixtures.py` in this directory
(`python3 tests/fixtures/gen_fixtures.py`) — it documents each field's source
line. To swap in a real capture, replace the `.multipart` file and update the
matching `Content-Type` constant + expected-row assertions in `tests/golden.rs`.
