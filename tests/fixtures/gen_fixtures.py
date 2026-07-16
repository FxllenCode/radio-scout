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


def sdrtrunk() -> tuple[bytes, str]:
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
    parts.append(text_part(boundary, "dateTime", "1763216122"))
    parts.append(text_part(boundary, "talkgroup", "54241"))
    parts.append(text_part(boundary, "source", "1610092"))
    parts.append(text_part(boundary, "frequency", "851000000"))
    parts.append(text_part(boundary, "talkerAlias", ""))
    parts.append(text_part(boundary, "talkgroupLabel", "PD Disp"))
    parts.append(text_part(boundary, "talkgroupGroup", "Law Dispatch"))
    parts.append(text_part(boundary, "systemLabel", "metropd"))
    parts.append(text_part(boundary, "patches", "[]"))
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


def tr_native() -> tuple[bytes, str]:
    """Trunk Recorder native .wav+.json -> POST /api/trunk-recorder-call-upload.

    key + meta(JSON) + audio(file). Mirrors rdio ParseTrunkRecorderMeta; the meta
    carries talkgroup_group_tag "-" to lock the placeholder-cleaning on the native
    path, and start_time (no timestamp) to lock the start_time-not-now() fix (#6).
    """
    boundary = b"------------------------0f1e2d3c4b5a69788796a5b4"
    wav = minimal_wav()
    audio_name = "54155-1669740338_771093750.wav"
    meta = (
        '{\n'
        '  "freq": 771093750,\n'
        '  "start_time": 1669740338,\n'
        '  "stop_time": 1669740344,\n'
        '  "call_length": 5.76,\n'
        '  "talkgroup": 54155,\n'
        '  "talkgroup_tag": "EMS DISP",\n'
        '  "talkgroup_description": "EMS Dispatch",\n'
        '  "talkgroup_group_tag": "-",\n'
        '  "talkgroup_group": "EMS",\n'
        '  "audio_type": "digital",\n'
        '  "short_name": "butco",\n'
        '  "freqList": [{"freq": 771093750, "time": 1669740338, "pos": 0.0, "len": 5.76, "error_count": 3, "spike_count": 1}],\n'
        '  "srcList": [{"src": 1610092, "pos": 0.0}],\n'
        '  "patched_talkgroups": [54155, 54156]\n'
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


def main():
    for name, (body, ct) in {
        "trunk-recorder-call-upload.multipart": tr_generic(),
        "sdrtrunk-call-upload.multipart": sdrtrunk(),
        "trunk-recorder-native-meta.multipart": tr_native(),
    }.items():
        path = os.path.join(OUT, name)
        with open(path, "wb") as f:
            f.write(body)
        print(f"{name}: {len(body)} bytes")
        print(f"    Content-Type: {ct}")


if __name__ == "__main__":
    main()
