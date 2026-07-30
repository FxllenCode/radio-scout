# Optional, Rust-native audio enhancement pipeline; AAC/M4A output

> **Amended by #20 (2026-07-27).** The gate moved from **build time to runtime**, the **AAC encoder was dropped entirely**, and the default enhanced output is **WAV at 8 kHz**, not AAC-LC/M4A. The original decision below is kept as the record of what was decided in July 2026 and why; read [the amendment](#amendment-20-2026-07-27--runtime-gate-no-aac-wav-default) for what is actually built.
>
> **Pointer corrected (2026-07-30).** Below, `output = "opus"` is said to "land with #23". #23 shipped without `libopus`, so it now lands with **#100**, which carries the reasoning — including that #23's own amendment removed cross-compilation from the release path, which is most of why Opus was deferred here. The refusal-at-boot is unchanged.

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

---

## Amendment: #20 (2026-07-27) — runtime gate, no AAC, WAV default

Building the pipeline surfaced three things the original decision got wrong. The pipeline itself — decode → 48 kHz → denoise → band-pass → loudness-normalize → encode — survives unchanged. What changed is **who turns it on**, **what comes out**, and **where it sits relative to ingest**.

### 1. The cargo feature is gone. Enhancement is a runtime setting.

The original gate was build-time: enhancement (and the C-source encoders with it) behind an off-by-default cargo feature, so the published artifact stayed pure-Rust and patent-clean.

**The flaw: essentially nobody compiles this.** Radio-Scout's first promise is a one-command install, and #23 exists to publish prebuilt binaries for exactly that reason. A capability behind a cargo feature is a capability the operator population does not have — it would have shipped a differentiator that only its own maintainer could reach. Worse, the fallback ("the operator builds their own with `--features enhancement`") means compiling ~111k SLoC of vendored C++ **on a Raspberry Pi**.

Enhancement is therefore **in the default binary and gated at runtime**, following the auto-populate precedent from #8 rather than rdio's model:

- **`[enhancement]` in TOML** carries policy — `mode`, `output`, target loudness, queue depth. Headless-configurable and version-controllable, per [ADR-0012](0012-configuration-model.md).
- **`systems.enhancement` / `talkgroups.enhancement`** carry scope, `NULL` meaning inherit. This is the half of spec US 34 that keeps one chatty System from eating a Pi.

rdio puts the whole thing in its database behind its admin UI (`server/options.go:56-59`, a four-value `audioConversion`), which is precisely what ADR-0012 refuses: a headless install cannot configure it and nothing is version-controllable. Splitting policy (file) from scope (rows) gets US 34 and US 36 both, with neither compromise.

### 2. No AAC encoder ships. Output is WAV, with Opus as the efficiency mode.

The original decision made **AAC-LC/M4A via `fdk-aac`** the primary enhanced output, because it is the only codec that plays in iOS Safari `<audio>` on *every* iOS version — accepting a live AAC patent question ([`docs/research/aac-muxing-and-patent-posture.md`](../research/aac-muxing-and-patent-posture.md): Via LA licenses "developers of end-user encoder products"; baseline patents run to 2028) on the grounds that the cargo feature kept it out of the default artifact.

Once the gate moved to runtime, that mitigation evaporated: shipping enhancement in the default binary would mean **the project publishing an AAC encoder**, turning a documented legal review into a release blocker.

**Measuring real scanner audio showed the trade was never worth making.** The maintainer's live archive — 60 Calls from a real Trunk Recorder — is **96 kHz mono AAC at 320 kbps**, for P25 voice carrying roughly 4 kHz of bandwidth. Against that baseline, an 8 s Call:

| Enhanced output | Bitrate | 8 s Call | vs. the real archive | Plays on | Patent surface |
|---|---|---|---|---|---|
| *(the archive today)* | 320 kbps | ~320 KB | — | every iOS | — |
| **WAV, 8 kHz 16-bit mono** | 128 kbps | ~128 KB | **2.5× smaller** | **every iOS** | **none** |
| Opus/Ogg, 24 kbps | 24 kbps | ~24 KB | 13× smaller | iOS 18.4+ only | none |
| AAC-LC, 32 kbps | 32 kbps | ~32 KB | 10× smaller | every iOS | live to 2028 |

AAC's one advantage — small *and* universal in a single file — buys about 4× over WAV while carrying the only patent question in the set. WAV covers universality outright and still shrinks the archive, because 8 kHz is Nyquist-sufficient for the 250–3400 Hz voice band the band-pass already limits to: it discards nothing the P25 vocoder produced. So the default enhanced output is **WAV at 8 kHz 16-bit mono**, and the published binary contains **no AAC encoder at all** — zero exposure by construction, no legal review on the release path.

**`output = "opus"`** stays the efficiency mode for fleets known to be iOS 18.4+, but lands with **#23**: `libopus` is vendored C built through cmake, the fussiest of the C-source crates under cross-compilation, and the four `Build …` jobs are required status checks on `master`. It belongs in the ticket whose job is proving the arm64 toolchain, not in this one. Until then the setting parses and **refuses to boot**, naming the ticket — an unbuilt option must never silently fall back to a different format than the operator asked for.

`fundsp` is also dropped: after `ebur128` sets the gain, the true-peak limiter is a few lines, not a dependency.

### 3. Enhancement is off the ingest path entirely, and swaps the object underneath.

rdio calls `FFMpeg.Convert()` **inline** in ingest (`server/controller.go:335`), so a slow ffmpeg directly slows every recorder upload — and if ffmpeg is missing it warns *once* and then silently passes every Call through forever. Both are avoided:

- **Ingest is unchanged**: store the audio object, insert the row, publish to the live feed, return `200`. A `200` still means the Call is durable, so a recorder that deletes its local copy on success loses nothing — the property any enqueue-before-storing design would have given away.
- **A worker enhances afterwards**, off a bounded [enhancement queue](../../CONTEXT.md), one core at a time: read the object, run the chain, write a **new** object key, update the row. The chain is decode → 48 kHz → *[denoise]* → band-pass → **8 kHz** → loudness-normalize → encode. Note the resample sits **before** normalization rather than after, which is the reverse of the order the original decision listed: loudness and peak are then measured on exactly the samples that get written, where a gain computed at 48 kHz and applied to a resampled file could clip after the fact. The old object becomes an orphan and #10's existing grace-period GC reclaims it — no new reclamation code, and no object mutated in place (which also keeps the S3 backend consistent).
- **The audio URL is stable** (`/api/call/{id}/audio`, resolved through the row at fetch time), so the swap is invisible. Because the client queues Calls and prefetches at play time, a live listener gets the enhanced audio in the common case and the passthrough version only when they were sitting idle; archive and replay are always enhanced.
- **`calls.enhancement`** (`none` / `pending` / `done` / `skipped`) makes the state observable. A `pending` Call is served **without `immutable`** in its `Cache-Control`, so nothing caches a version about to be replaced. Boot re-enqueues `pending` Calls, and deliberately never touches `none` — enabling enhancement must not silently rewrite an existing archive. Explicit backfill is a later ticket.

The loudness stage is one **static** gain held under a **sample-peak** ceiling of -1.5 dBFS, not a limiter and not a true-peak measurement. A compressor riding the level would pump the noise floor between words; and true-peak oversampling buys headroom that matters before a lossy re-encode at a high rate, not for 16-bit PCM band-limited to 3.4 kHz at 8 kHz. The trade is that a Call with one loud transient lands below `target_lufs` — consistent-and-slightly-quiet beats consistent-and-clipped for a scanner.

### 4. Denoise stays a hypothesis, and stays opt-in

The original status section flagged RNNoise's benefit on vocoder-decoded P25/DMR as unproven. It still is; nothing about this amendment validates it. `mode = "normalize"` (band-pass + EBU R128 — the proven win) is what enabling enhancement means, and `mode = "denoise"` is opt-in on top.

**Decision (2026-07-27, #36): denoise remains opt-in.** The maintainer's call, taken with the A/B renders from the real P25 archive in hand. This is a decision *not to promote*, not a measurement that denoise is harmful — the hypothesis is neither retired nor confirmed, and it should not be described as either.

The asymmetry is what settles it. Loudness normalization has a stated mechanism and an audible, reproducible effect: every Call lands on the same level. RNNoise has neither here — it was trained on noisy microphone signal, and a P25 vocoder's output is not that. It may remove hiss; it may equally chew on codec artefacts and take the quiet ends of words with them. Making the unproven stage the default would mean every operator who typed `mode = "normalize"`'s successor got a transformation nobody has evidence for, applied irreversibly to their archive — because enhancement replaces the object, and the original is reclaimed by orphan-GC.

Opt-in costs an operator one word of configuration. Default-on costs anyone it hurts their audio, silently, with no way back.

**What would change this:** a listening comparison on real P25 or DMR traffic where denoise is clearly better — not merely different — recorded here with the reasoning. `cargo run --example enhance_ab` produces the material; the renderer is kept for exactly that. A verdict on analog FM would be a separate finding, since the objection is specifically about vocoder output.

### Consequences of the amendment

- The published binary stays **100% pure Rust with no vendored C** — a stronger cross-compile position for #23 than the original decision left, not a weaker one.
- Enhancement is reachable by every operator rather than only those who compile, which is the whole point of building it.
- The archive-format normalization the original decision noted still happens, just to WAV rather than M4A — and for a recorder configured like the maintainer's, it *shrinks* the archive rather than growing it.
- **Unrelated but worth recording:** that 96 kHz/320 kbps measurement is a Trunk Recorder misconfiguration. Fixing it upstream shrinks storage ~10× with no Radio-Scout code at all; enhancement should not be the reason an operator keeps paying 320 kbps for 4 kHz of bandwidth.
