#!/usr/bin/env python3
"""Generate byte-accurate recorder-upload golden fixtures for ticket #7.

Each fixture is the exact multipart/form-data body a real recorder POSTs,
reconstructed from the recorder's own source (cited in tests/fixtures/README.md).
CRLF line endings, exact field order, exact part headers, and the real audio
container header are preserved so the golden tests exercise our multipart reader
against true wire bytes — not a reqwest-synthesised body.
"""
import os
import struct

OUT = os.path.dirname(os.path.abspath(__file__))
os.makedirs(OUT, exist_ok=True)

CRLF = b"\r\n"


def minimal_wav(data_bytes: bytes = b"\x00" * 16) -> bytes:
    """A canonical 44-byte PCM WAV header + a little sample data."""
    n = len(data_bytes)
    return (
        b"RIFF"
        + struct.pack("<I", 36 + n)
        + b"WAVE"
        + b"fmt "
        + struct.pack("<I", 16)
        + struct.pack("<HHIIHH", 1, 1, 8000, 16000, 2, 16)
        + b"data"
        + struct.pack("<I", n)
        + data_bytes
    )


def minimal_mp3() -> bytes:
    """A minimal MPEG-1 Layer III frame header + padding (opaque to us)."""
    return b"\xff\xfb\x90\x00" + b"\x00" * 32


def text_part(boundary: bytes, name: str, value: str) -> bytes:
    return (
        b"--" + boundary + CRLF
        + b'Content-Disposition: form-data; name="' + name.encode() + b'"' + CRLF
        + CRLF
        + value.encode()
        + CRLF
    )


def tr_generic() -> tuple[bytes, str]:
    """Trunk Recorder rdioscanner_uploader -> POST /api/call-upload.

    16 parts in the exact append order of rdioscanner_uploader.cc:331-401;
    audio part first with Content-Type: application/octet-stream (the plugin
    hard-codes that; the real MIME rides in audioType).
    """
    boundary = b"------------------------d1e2f3a4b5c6d7e8f9a0b1c2"
    wav = minimal_wav()
    audio_name = "54241-1669740338.123_774031250-call_9.wav"
    parts = []
    # 1. audio (file)
    parts.append(
        b"--" + boundary + CRLF
        + b'Content-Disposition: form-data; name="audio"; filename="'
        + audio_name.encode() + b'"' + CRLF
        + b"Content-Type: application/octet-stream" + CRLF
        + CRLF + wav + CRLF
    )
    # 2..16 text parts, exact order & names
    parts.append(text_part(boundary, "audioName", audio_name))
    parts.append(text_part(boundary, "audioType", "audio/wav"))
    parts.append(text_part(boundary, "dateTime", "1669740338"))
    parts.append(text_part(
        boundary, "frequencies",
        '[{"freq": 774031250, "time": 1669740338, "pos": 0.00, "len": 5.76, '
        '"errorCount": 2, "spikeCount": 0}]',
    ))
    parts.append(text_part(boundary, "frequency", "774031250"))
    parts.append(text_part(boundary, "key", "tr-plugin-key"))
    parts.append(text_part(boundary, "patches", "[]"))
    parts.append(text_part(boundary, "talkgroup", "54241"))
    parts.append(text_part(boundary, "talkgroupGroup", "Fire"))
    parts.append(text_part(boundary, "talkgroupLabel", "TDB A1"))
    parts.append(text_part(boundary, "talkgroupTag", "Fire Dispatch"))
    parts.append(text_part(boundary, "talkgroupName", "Fire Department Dispatch A1"))
    parts.append(text_part(
        boundary, "sources",
        '[{ "pos": 0.00, "src": 1610092 }, { "pos": 3.20, "src": 1610051, "tag": "Engine 5" }]',
    ))
    parts.append(text_part(boundary, "system", "8"))
    parts.append(text_part(boundary, "systemLabel", "butco"))
    body = b"".join(parts) + b"--" + boundary + b"--" + CRLF
    ct = "multipart/form-data; boundary=" + boundary.decode()
    return body, ct


def sdrtrunk(
    talkgroup: str = "54241",
    patches: str = "[]",
    date_time: str = "1763216122",
) -> tuple[bytes, str]:
    """SDRTrunk RdioScannerBuilder -> POST /api/call-upload.

    Header boundary is the 2-dash '--sdrtrunk-sdrtrunk-sdrtrunk'; body delimiters
    are '--' + that = '----sdrtrunk-sdrtrunk-sdrtrunk'. No Content-Type on any
    part; the audio part puts filename BEFORE name (RdioScannerBuilder.java:122-124).
    """
    boundary = b"--sdrtrunk-sdrtrunk-sdrtrunk"
    mp3 = minimal_mp3()
    audio_name = "20261115_143022.123.mp3"
    parts = []
    parts.append(text_part(boundary, "key", "sdrtrunk-key"))
    parts.append(text_part(boundary, "system", "11"))
    parts.append(text_part(boundary, "dateTime", date_time))
    parts.append(text_part(boundary, "talkgroup", talkgroup))
    parts.append(text_part(boundary, "source", "1610092"))
    parts.append(text_part(boundary, "frequency", "851000000"))
    parts.append(text_part(boundary, "talkerAlias", ""))
    parts.append(text_part(boundary, "talkgroupLabel", "PD Disp"))
    parts.append(text_part(boundary, "talkgroupGroup", "Law Dispatch"))
    parts.append(text_part(boundary, "systemLabel", "metropd"))
    parts.append(text_part(boundary, "patches", patches))
    # audio (file) last: filename before name, no Content-Type
    parts.append(
        b"--" + boundary + CRLF
        + b'Content-Disposition: form-data; filename="' + audio_name.encode()
        + b'"; name="audio"' + CRLF
        + CRLF + mp3 + CRLF
    )
    body = b"".join(parts) + b"--" + boundary + b"--" + CRLF
    ct = "multipart/form-data; boundary=" + boundary.decode()
    return body, ct


def sdrtrunk_patched() -> tuple[bytes, str]:
    """SDRTrunk broadcasting a call on a PATCH GROUP -> POST /api/call-upload.

    Same 12 parts as sdrtrunk(), but `talkgroup` is the patch group's own value
    and `patches` carries what getPatches() builds for a PatchGroupIdentifier
    (RdioScannerBroadcaster.java:546-574):

        "[" + patchGroup + ("," + eachPatchedTalkgroup)* + ("," + eachPatchedRadio)* + "]"

    One flat array, no separator and no type marker between the talkgroups and
    the radios — so the wire itself cannot say where one ends and the other
    begins. Here: patch group 54000, patched talkgroups 54241 and 54242, then
    patched radios 1610051 and 1610092 (24-bit APCO25 radio ids, which is what
    PatchGroup.getPatchedRadioIdentifiers() holds).
    """
    return sdrtrunk(
        talkgroup="54000",
        patches="[54000,54241,54242,1610051,1610092]",
        date_time="1763216200",
    )


def tr_native() -> tuple[bytes, str]:
    """Trunk Recorder native .wav+.json -> POST /api/trunk-recorder-call-upload.

    key + meta(JSON) + audio(file). The meta is the **full** field set
    `create_call_json` writes, in its own key order
    (`trunk-recorder/call_concluder/call_concluder.cc:785`) — rdio-scanner's
    ParseTrunkRecorderMeta reads six of those keys and ignores the rest, and #42
    reads the transmission truth in the remainder. This is therefore also the
    contract the shipped uploadScript (#43) and the first-party plugin (#44) must
    satisfy.

    Two behaviors are deliberately locked here: talkgroup_group_tag "-" pins the
    placeholder-cleaning on the native path, and start_time with **no** timestamp
    pins the start_time-not-now() fix (#6).
    """
    boundary = b"------------------------0f1e2d3c4b5a69788796a5b4"
    wav = minimal_wav()
    audio_name = "54155-1669740338_771093750.wav"
    meta = (
        '{\n'
        '  "call_num": 4171,\n'
        '  "freq": 771093750,\n'
        '  "freq_error": -12,\n'
        '  "signal": -74,\n'
        '  "noise": -96,\n'
        '  "source_num": 0,\n'
        '  "recorder_num": 3,\n'
        '  "tdma_slot": 0,\n'
        '  "phase2_tdma": 0,\n'
        '  "start_time": 1669740338,\n'
        '  "stop_time": 1669740344,\n'
        '  "start_time_ms": 1669740338000,\n'
        '  "stop_time_ms": 1669740344000,\n'
        '  "emergency": 1,\n'
        '  "priority": 2,\n'
        '  "mode": 1,\n'
        '  "duplex": 0,\n'
        '  "encrypted": 0,\n'
        '  "call_length": 5.76,\n'
        '  "call_length_ms": 5760,\n'
        '  "talkgroup": 54155,\n'
        '  "talkgroup_tag": "EMS DISP",\n'
        '  "talkgroup_description": "EMS Dispatch",\n'
        '  "talkgroup_group_tag": "-",\n'
        '  "talkgroup_group": "EMS",\n'
        '  "color_code": 0,\n'
        '  "audio_type": "digital",\n'
        '  "short_name": "butco",\n'
        '  "patched_talkgroups": [54155, 54156],\n'
        '  "freqList": [{"freq": 771093750, "time": 1669740338, "pos": 0.0, "len": 5.76, "error_count": 3, "spike_count": 1}],\n'
        '  "srcList": [{"src": 1610092, "time": 1669740339, "pos": 0.0, "emergency": 1, "signal_system": "P25", "tag": "EMS 1", "tag_ota": "MEDIC7"}]\n'
        '}'
    )
    parts = []
    parts.append(text_part(boundary, "key", "tr-native-key"))
    parts.append(text_part(boundary, "meta", meta))
    parts.append(
        b"--" + boundary + CRLF
        + b'Content-Disposition: form-data; name="audio"; filename="'
        + audio_name.encode() + b'"' + CRLF
        + b"Content-Type: application/octet-stream" + CRLF
        + CRLF + wav + CRLF
    )
    body = b"".join(parts) + b"--" + boundary + b"--" + CRLF
    ct = "multipart/form-data; boundary=" + boundary.decode()
    return body, ct


def tr_native_encrypted() -> tuple[bytes, str]:
    """Trunk Recorder native upload of an ENCRYPTED call (#42, spec US 9).

    Same contract as `tr_native`, with `encrypted: 1`. The audio part is present
    and real — TR writes a file either way — and Radio-Scout stores the row and
    discards the bytes, because what is in them is the vocoder's noise rather
    than speech. This fixture is what pins that half of the contract for the
    first-party plugin (#44).
    """
    boundary = b"------------------------0f1e2d3c4b5a69788796a5b4"
    wav = minimal_wav()
    audio_name = "54999-1669740400_771093750.wav"
    meta = (
        '{\n'
        '  "call_num": 4172,\n'
        '  "freq": 771093750,\n'
        '  "start_time": 1669740400,\n'
        '  "stop_time": 1669740403,\n'
        '  "emergency": 0,\n'
        '  "priority": 0,\n'
        '  "encrypted": 1,\n'
        '  "call_length": 3.2,\n'
        '  "call_length_ms": 3200,\n'
        '  "talkgroup": 54999,\n'
        '  "talkgroup_tag": "SO TAC 1",\n'
        '  "talkgroup_description": "Sheriff Tactical 1",\n'
        '  "talkgroup_group_tag": "Law Tac",\n'
        '  "talkgroup_group": "Law",\n'
        '  "audio_type": "digital",\n'
        '  "short_name": "butco",\n'
        '  "freqList": [{"freq": 771093750, "time": 1669740400, "pos": 0.0, "len": 3.2, "error_count": 0, "spike_count": 0}],\n'
        '  "srcList": []\n'
        '}'
    )
    parts = [
        text_part(boundary, "key", "tr-native-key"),
        text_part(boundary, "meta", meta),
        b"--" + boundary + CRLF
        + b'Content-Disposition: form-data; name="audio"; filename="'
        + audio_name.encode() + b'"' + CRLF
        + b"Content-Type: application/octet-stream" + CRLF
        + CRLF + wav + CRLF,
    ]
    body = b"".join(parts) + b"--" + boundary + b"--" + CRLF
    ct = "multipart/form-data; boundary=" + boundary.decode()
    return body, ct


def main():
    for name, (body, ct) in {
        "trunk-recorder-call-upload.multipart": tr_generic(),
        "sdrtrunk-call-upload.multipart": sdrtrunk(),
        "sdrtrunk-call-upload-patched.multipart": sdrtrunk_patched(),
        "trunk-recorder-native-meta.multipart": tr_native(),
        "trunk-recorder-native-encrypted.multipart": tr_native_encrypted(),
    }.items():
        path = os.path.join(OUT, name)
        with open(path, "wb") as f:
            f.write(body)
        print(f"{name}: {len(body)} bytes")
        print(f"    Content-Type: {ct}")


if __name__ == "__main__":
    main()
