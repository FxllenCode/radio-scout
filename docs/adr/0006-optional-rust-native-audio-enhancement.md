# Optional, Rust-native audio enhancement pipeline; AAC/M4A output

## Context

Audio quality is a deliberate product differentiator: scanner audio is noisy, artifact-laden, and has wild level swings between talkgroups. Radio-Scout must stay single-binary and keep audio playable on iOS Safari `<audio>` ([ADR-0002](0002-audio-object-storage.md), [ADR-0005](0005-client-audio-media-session-background.md)). Research (`docs/research/audio-pipeline.md`, primary-source-cited) confirmed a fully Rust-native pipeline is feasible without an external ffmpeg binary.

## Decision

- **Default = passthrough** (store the recorder's audio as received; all of WAV/AAC-M4A/MP3 are iOS-playable). Enhancement is **opt-in**, off by default.
- **Optional per-call enhancement pipeline, all Rust** (encoders static-link vendored C; no external binary): `symphonia` decode → `rubato` resample to 48 kHz → `nnnoiseless` (RNNoise) denoise → `biquad` voice band-pass ~250–3400 Hz → optional `fundsp` dynamics (compress/limit) → `ebur128` two-pass loudness normalization → encode.
- **Output codec = AAC-LC in M4A/fMP4** (`fdk-aac`) for universal iOS `<audio>` + Media Session/background compatibility. **Opus-in-Ogg** (`opus` + `ogg`, royalty-free, ~half the bitrate) is offered as an efficiency mode for iOS 18.4+ fleets.
- **No ffmpeg dependency.** Licensing is clean: libopus = BSD/royalty-free; fdk-aac = redistributable standalone (must NOT be combined with GPL ffmpeg); MP3/LAME avoided.

## Status, risks, open items

- **AAC-in-MP4 muxing in pure Rust is unproven** (the `mp4` crate is stale). To be prototyped. **Fallback:** shell out to an operator-installed *system* ffmpeg (LGPL native AAC) for muxing only — preserves a single-file, license-clean binary; we do not link `ffmpeg-next`.
- **AAC patent/royalty posture** to be legal-reviewed before distributing binaries containing an AAC encoder.
- Enhancement parameters (denoise strength, band-pass edges, target LUFS) need tuning on real scanner audio. **RNNoise's benefit is unproven on already-vocoder-decoded digital (P25/DMR) audio and must be validated before shipping** — loudness normalization is the proven win; denoise is a hypothesis.
- To stay within a Pi's budget, enhancement runs in a **bounded work-queue with backpressure** — it must never block or fall behind live ingest; on a busy system it may be enabled only for selected systems/talkgroups.
- The C-source encoders (`fdk-aac`, `libopus`) are **gated behind a cargo feature** so the default binary stays pure-Rust and cross-compiles to arm64 cleanly; only enhancement builds opt in.

## Consequences

- Enhancement runs server-side at ingest, per-call, opt-in (~1 s/call on a Pi 5 core, dominated by RNNoise); the client stays on a plain `<audio>` path.
- Loudness normalization to a broadcast standard is the biggest audible win over rdio; noise suppression is the flashiest.
- Because enhancement transcodes to AAC/M4A, enabling it also normalizes archive format; passthrough keeps whatever the recorder sent.
