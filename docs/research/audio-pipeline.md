# Audio transcoding & enhancement pipeline for Radio-Scout

Research date: 2026-07-04. Scope: an **opt-in, off-by-default** transcode/enhancement
pipeline for scanner-call audio (Trunk Recorder = WAV or AAC/M4A, SDRTrunk = MP3),
serving an HTML5 `<audio>` + Media Session client, with a strong preference for a
**single self-contained Rust binary that cross-compiles to Raspberry Pi 5 (arm64)**.

Related decisions: [ADR-0002 (audio in object storage, served via our own range
endpoint, played through `<audio>` + Media Session, not WebAudio)](../adr/0002-audio-object-storage.md)
and [ADR-0001 (ingest compatibility)](../adr/0001-ingest-compatible-own-live-feed-protocol.md).

---

## Executive summary & recommendation

**A fully Rust-native, single-binary transcode + enhancement pipeline is feasible.**
Every stage has a maintained crate. Decoders and DSP are pure Rust; the two encoders
we'd use (Opus, AAC) ship their C source and static-link, so there is still no external
`ffmpeg` binary and no runtime shared-library dependency. FFmpeg is **not** needed and
should be avoided — linking it wrecks the single-binary/cross-compile goal and, in the
one configuration that gives best-in-class AAC (ffmpeg + libfdk-aac), produces a
**non-distributable** binary (see Q1).

Concrete recommendation:

1. **Default (unchanged): store-as-received, no transcode.** Passthrough of WAV / AAC-M4A
   / MP3. This is the ADR-0002 default and stays the default.

2. **Optional enhancement pipeline (all Rust, single binary), processing order:**
   1. **Decode → f32 PCM** with **`symphonia`** (enable `wav`/`pcm`, `mp3`, `aac`, `isomp4`
      features — covers every input format we ingest).
   2. **Resample to 48 kHz mono** (`rubato` or `dasp`) — required by RNNoise.
   3. **Denoise** with **`nnnoiseless`** (pure-Rust RNNoise; 48 kHz, 480-sample/10 ms frames).
   4. **Voice band-pass** ~250–3400 Hz (high-pass to kill rumble/hum, low-pass to kill hiss)
      with **`biquad`** (or `fundsp`).
   5. **Gentle dynamics / AGC** (optional) with **`fundsp`** (compressor/look-ahead limiter).
   6. **Loudness-normalize to a fixed target** (recommend ≈ **-18 to -16 LUFS** for speech,
      or -23 LUFS if you want strict EBU R128 broadcast) with **`ebur128`** (two-pass:
      measure integrated loudness, apply a single gain, limit true-peak). This is the stage
      that fixes the biggest real-world scanner problem: **level swings between talkgroups**.
   7. **Encode + mux** to the target container.

3. **Target codec + container — recommend AAC-LC in M4A/fragmented-MP4 as the primary
   enhanced output**, because it is the only choice that plays in **iOS Safari `<audio>` on
   every iOS version** and reliably drives **lock-screen / Media Session background audio**.
   Encode with the **`fdk-aac`** crate (best-quality AAC; C source is vendored and
   static-linked). Its underlying Fraunhofer license is BSD-derived and **redistributable on
   its own** — it only becomes non-distributable when combined with *GPL* FFmpeg (which we
   are not using).
   - **Opus-in-Ogg** (`opus` crate + `ogg` crate) is the storage-efficiency alternative:
     ~half the bitrate for equal speech quality, and the cleanest licensing (BSD-3 +
     royalty-free patents). **But** Safari only gained Ogg-Opus in **`<audio>` at iOS 18.4 /
     macOS 15.4 (Mar 2025)**; older iOS needs Opus-in-CAF (works but exotic — see Q3) or an
     AAC fallback. Use Opus only if you can require modern iOS or are willing to do
     content-negotiation / dual-encode. For a "just works everywhere" single output, pick AAC-LC/M4A.

4. **Stays Rust-native / single binary: yes.** No ffmpeg, no runtime `.so`/`.dll`.
   Cross-compile to arm64 with `cross` or `cargo-zigbuild` (the C-source crates build with
   `cc`/`cmake`/autotools + a cross toolchain).

5. **Only genuinely fiddly gap → the fallback:** robust **MP4/M4A muxing of an AAC track in
   pure Rust** is the least-proven step (the `mp4` crate's AAC *write* path is unverified —
   see open questions). If that proves too painful, the fallback is **shelling out to an
   operator-installed system `ffmpeg`** (keeps *our* binary clean and license-clean), *not*
   linking `ffmpeg-next`. Never ship an `ffmpeg` built with `--enable-libfdk-aac
   --enable-nonfree`: that binary is legally non-distributable (Q1).

---

## Q1 — Rust-native transcoding vs FFmpeg

### Decoding: `symphonia` (pure Rust, decode-only)

`symphonia` is a pure-Rust, decode-only media framework, **MPL-2.0** licensed, "100% safe
Rust" with minimal dependencies; by default it enables only royalty-free codecs and gates
patent-encumbered ones behind non-default feature flags
([README / support matrix, github.com/pdeljanov/Symphonia](https://github.com/pdeljanov/Symphonia)).
Relevant support levels for our inputs:

| Input | Symphonia support | Feature flag |
|---|---|---|
| WAV / PCM | **Excellent** | `pcm` / `wav` (default) |
| MP3 (SDRTrunk) | **Excellent** | `mp3` (non-default) |
| AAC-LC in ISO-MP4 / M4A (Trunk Recorder) | **Great** | `aac` + `isomp4` (non-default) |
| FLAC | Excellent | `flac` (default) |
| ALAC | Great | `alac` |

Source: [Symphonia README support matrix](https://github.com/pdeljanov/Symphonia).
Trunk Recorder's AAC output is AAC-LC wrapped in an **M4A (MPEG-4)** container
([M4A = AAC-in-MP4, en.wikipedia.org/wiki/Advanced_Audio_Coding](https://en.wikipedia.org/wiki/Advanced_Audio_Coding)),
which Symphonia's `isomp4` + `aac` path covers. **Caveat:** Symphonia has **no standalone
raw-ADTS demuxer** — AAC is only read inside MP4/ISOBMFF. This is fine for us (TR writes
M4A, not raw ADTS), but note it if a recorder ever emits raw `.aac`/ADTS.
Symphonia does **not** decode Opus — irrelevant, since we *produce* Opus, never ingest it.

**Verdict:** `symphonia` decodes 100% of Radio-Scout's ingest formats, pure Rust, no C, no
cross-compile friction.

### Encoding

There is **no production-grade pure-Rust encoder** for Opus, AAC, or MP3; all three route
through the reference C libraries. The good news: the standard binding crates **vendor the C
source and static-link it**, so they still fit "single self-contained binary."

**Opus — `opus` (or `audiopus`) → libopus.** Recommend the **`opus`** crate (MIT OR
Apache-2.0, **v0.3.1 released 2026-01-03**, ~114k downloads/mo, 83 dependents), which binds
libopus through `audiopus_sys`
([lib.rs/crates/opus](https://lib.rs/crates/opus)). `audiopus_sys` **statically builds
libopus from source via cmake** (static by default on Windows/macOS/musl; overridable)
([audiopus README, github.com/lakelezz/audiopus](https://github.com/lakelezz/audiopus)).
Prefer `opus` over `audiopus` directly: `audiopus`'s own last release is a 2021 release
candidate (`0.3.0-rc.0`, 2021-04-22) — effectively dormant
([docs.rs/crate/audiopus](https://docs.rs/crate/audiopus/latest)), whereas the `opus`
wrapper on top of `audiopus_sys` is current. **libopus is BSD-3-Clause and royalty-free**
(patents from Xiph, Broadcom, Microsoft granted royalty-free)
([opus-codec.org/license](https://opus-codec.org/license/)) — the cleanest option on both
copyright and patents.

**AAC — `fdk-aac` → Fraunhofer FDK AAC.** The **`fdk-aac`** crate (**MIT** wrapper,
**v0.8.0 released 2025-09-25**) binds `libfdk-aac` via `fdk-aac-sys`, which **vendors and
static-builds the ~111k-SLoC C/C++ source** — no system library needed
([lib.rs/crates/fdk-aac](https://lib.rs/crates/fdk-aac),
[github.com/haileys/fdk-aac-rs](https://github.com/haileys/fdk-aac-rs)). FDK AAC is the
highest-quality open AAC encoder, "generally preferred over FFmpeg's native AAC encoder"
([trac.ffmpeg.org/wiki/Encode/AAC](https://trac.ffmpeg.org/wiki/Encode/AAC)).
**Licensing (important):** the underlying library uses the Fraunhofer FDK AAC license —
"based on BSD, free, **no patent grant**" ([lib.rs/crates/fdk-aac](https://lib.rs/crates/fdk-aac)).
Copyright-wise it is permissive and **redistributable on its own**. The "no patent grant"
means AAC *patent* licensing (the AAC patent pool) is a separate legal question — flagged
below for legal review; most AAC-LC core patents are old, but the FDK license explicitly
grants no patent rights.

**MP3 — `mp3lame-encoder` → LAME.** `mp3lame-encoder` (**v0.2.4, 2026-04-23**) over
`mp3lame-sys` (**v0.1.11, 2025-12-04**), which **bundles LAME 3.100 source and static-builds
it** (cc on Windows, autotools on Unix) — no system lib
([lib.rs/crates/mp3lame-encoder](https://lib.rs/crates/mp3lame-encoder),
[lib.rs/crates/mp3lame-sys](https://lib.rs/crates/mp3lame-sys)). **License: LGPL-3.0.** MP3
is the *worst* output choice for us: it's the least storage-efficient of the three for voice,
and **LGPL static-linking into a single (possibly closed/permissive) binary imposes relink /
object-availability obligations** that Opus (BSD) and FDK-AAC (BSD-derived) do not. Only use
MP3 output if a client demands it.

**Static-link / arm64 / binary-size notes.** All three C-source crates build with a C
toolchain at compile time and static-link the result, so the runtime is a single binary with
no `.so` dependencies. Cross-compiling to arm64 works via `cross` (Docker cross toolchains)
or `cargo-zigbuild` (zig as the cross C compiler) — the autotools build in `mp3lame-sys` is
the most likely to need a correctly-configured cross host/target triple; libopus (cmake) and
fdk-aac (cc) are usually smoother. Rough static-size adds (release, stripped, estimates —
**measure on target**): libopus ≈ +0.5–1 MB, libfdk-aac ≈ +1–2 MB, symphonia (Rust, pure
code) small. All well within a "one binary" footprint.

### FFmpeg as an alternative — and why to avoid it

**(a) Shell out to an external `ffmpeg` binary.** Simple and license-isolating (it's a
separate process the *operator* installs), but it breaks "single self-contained binary": the
operator must install ffmpeg on the Pi, and we lose the pure-Rust guarantee. Acceptable
**only** as the last-resort muxing fallback (see recommendation #5).

**(b) Link `ffmpeg-next` / `rusty_ffmpeg` (libav\*).** These bind the FFmpeg C libraries and
**require FFmpeg dev libraries + headers + pkg-config + a C toolchain at build time**;
`ffmpeg-next` is in maintenance mode and "cross-compilation adds another layer of
complexity" (you must supply cross-built libav\* and set per-target `FFMPEG_*` env vars)
([lib.rs/crates/ffmpeg-next](https://lib.rs/crates/ffmpeg-next), search corroboration).
This is the **opposite** of the single-binary/easy-arm64 goal.

**Licensing consequences of linking FFmpeg (primary source, ffmpeg.org):**
FFmpeg is **LGPL 2.1+ by default**, with optional **GPL** components enabled by
`--enable-gpl` ([ffmpeg.org/legal.html](https://www.ffmpeg.org/legal.html)). The Fraunhofer
FDK AAC library is the trap:

> "The license of the Fraunhofer AAC library is incompatible with the GPL … for GPL builds,
> you have to pass `--enable-nonfree` to configure in order to use it." (it is *compatible*
> with the LGPL) — [ffmpeg.org/general.html](https://ffmpeg.org/general.html)

`--enable-nonfree` marks the resulting binary as **non-redistributable**. So **FDK-AAC +
GPL-configured FFmpeg = a binary you legally cannot distribute** — the exact combination the
brief asked about. To ship an FFmpeg build at all you'd have to either (i) drop FDK-AAC and
use FFmpeg's lower-quality native AAC encoder under LGPL, or (ii) keep FFmpeg entirely out of
the shipped binary. Note the asymmetry the brief probes: **standalone `fdk-aac` (our path) is
distributable; `fdk-aac` *inside GPL ffmpeg* is not.** This is a strong, concrete reason to
stay with the Rust-native encoders.

---

## Q2 — Audio enhancement as a differentiator

All enhancement crates below are pure Rust (except none need C) and cheap enough to run
per-call on a Pi 5. CPU figures are estimates unless a citation is given; the only figure
with a hard source is RNNoise.

### ML noise suppression — `nnnoiseless` (pure-Rust RNNoise)

`nnnoiseless` is "a (safe) rust port of the RNNoise C library" — **BSD-3-Clause**,
**v0.5.2 released 2025-12-18**, actively maintained (~33k downloads/mo), depends only on
Rust crates (`dasp`, `easyfft`, `once_cell`); mostly-safe Rust with some `unsafe` for perf
([lib.rs/crates/nnnoiseless](https://lib.rs/crates/nnnoiseless),
[github.com/jneem/nnnoiseless](https://github.com/jneem/nnnoiseless)).

**Fixed constraints (from RNNoise):** operates on **48 kHz, mono, 16-bit** audio in
**10 ms / 480-sample frames** with only ~10 ms lookahead
([jmvalin.ca/demo/rnnoise](https://jmvalin.ca/demo/rnnoise/),
[nnnoiseless README](https://github.com/jneem/nnnoiseless)). So the pipeline must resample to
48 kHz before this stage. Scanner audio is typically narrowband (8 kHz analog/P25) upsampled
to 48 kHz — RNNoise will suppress hiss/hum/background well but cannot *recover* highs that
were never captured.

**CPU cost:** RNNoise "runs about **60× faster than real-time on an x86 CPU**" and
"**about 7× faster than real-time on a Raspberry Pi 3**"; model is 85 KB, 3 GRU layers, no
GPU ([jmvalin.ca/demo/rnnoise](https://jmvalin.ca/demo/rnnoise/)). A **Pi 5** (Cortex-A76 @
2.4 GHz) is ~5–6× the per-core throughput of a Pi 3 (Cortex-A53 @ 1.2–1.4 GHz), so expect
**well over ~20–30× real-time** → a 10–15 s call denoised in **~0.3–0.7 s of one core**.
Comfortably per-call at ingest, even with several concurrent.

**Quality win:** the single biggest perceptual improvement for noisy scanner voice —
removes static, hum, engine/wind/HVAC background between and under speech. Concrete and
audible.

### Loudness normalization — `ebur128` (EBU R128 / ITU-R BS.1770)

`ebur128` is a **pure-Rust port of libebur128**, **MIT** licensed, **v0.1.10 (2024-10-26)**,
maintained by Sebastian Dröge / Tim-Philipp Müller (GStreamer). It implements momentary (M),
short-term (S) and integrated (I) loudness, **loudness range (LRA)**, and **true-peak**
scanning, at any sample rate ([lib.rs/crates/ebur128](https://lib.rs/crates/ebur128)).

**Single-pass vs two-pass:** the crate can measure incrementally (feed frames, read
integrated loudness) — usable single-pass — but for a *file* the clean approach is **two-pass
per call**: pass 1 measures integrated LUFS (and true-peak), then apply one gain to hit the
target and true-peak-limit. For short 5–15 s calls, two passes are trivially cheap (a couple
of biquads + a histogram per sample).

**What it fixes for scanner audio:** the **inter-talkgroup / inter-call level swings** that
make scanning fatiguing — one talkgroup blares, the next is barely audible. Normalizing every
call to a fixed LUFS target (≈ -18…-16 LUFS for speech intelligibility, or -23 LUFS for
strict broadcast) makes the whole archive play back at a consistent, comfortable level. High
value, low cost.

### Band-pass, AGC, dead-air trim — `biquad` / `fundsp` / `dasp`

- **Voice band-pass 300–3400 Hz (`biquad`)** — `biquad` is **MIT OR Apache-2.0**,
  **v0.6.0 (2026-03-22)**, `#![no_std]` pure Rust, providing 1st/2nd-order IIR filter
  coefficient calculation (low/high/band-pass/notch, DF1 and DF2T forms)
  ([docs.rs/biquad](https://docs.rs/crate/biquad/latest)). Cost is a handful of
  multiply-adds per sample — **negligible**. For already-narrowband scanner audio, a
  high-pass ~250–300 Hz (kill rumble/hum/CTCSS bleed) plus low-pass ~3.4 kHz (kill hiss)
  measurably improves intelligibility and helps the encoder spend bits on the voice band.
- **AGC / dynamics (`fundsp`)** — `fundsp` is **MIT OR Apache-2.0**, **v0.23.0 (2026-01-07)**,
  pure Rust `no_std`, actively developed; provides biquad/band-pass banks, a **look-ahead
  limiter** and smoothing/compressor primitives (`afollow`) suitable for AGC / dynamic-range
  control ([lib.rs/crates/fundsp](https://lib.rs/crates/fundsp)). Cheap. Use gentle
  compression + a true-peak limiter; a heavy AGC can pump up noise floor, so pair it with the
  RNNoise stage and prefer EBU-R128 normalization for the *level* target.
- **Silence / dead-air trim** — no dedicated crate needed; a simple RMS/energy gate over the
  decoded PCM (optionally informed by the short-term loudness from `ebur128`) trims leading/
  trailing dead air and long inter-transmission gaps. Trivial CPU; shrinks files and tightens
  playback.
- **`dasp`** (rust-audio) — **MIT OR Apache-2.0**, but **v0.11.0 dates to 2020-05-29** and is
  effectively **dormant** ([docs.rs/dasp](https://docs.rs/crate/dasp/latest)). Still fine for
  sample-type/frame primitives and it's a transitive dep of `nnnoiseless`, but for *active*
  DSP prefer `fundsp`; for resampling prefer `rubato` (maintained, high-quality) over dasp's
  interpolators.

**Net enhancement CPU budget on a Pi 5:** decode + resample + RNNoise + biquads + EBU-R128 +
encode for a 10–15 s call is dominated by RNNoise (~sub-second) and Opus/AAC encode (many×
real-time, negligible). Estimate **≈ 1 s of one core per call** end-to-end — fine for opt-in
processing at ingest. (RNNoise figure sourced; the rest are engineering estimates — benchmark
on real hardware.)

---

## Q3 — Playback codec/container for iOS (verified 2026)

Constraints: must play in **iOS Safari `<audio>`** and reliably attach **lock-screen /
Media Session** background transport controls. Per ADR-0002 the client already uses a real
`<audio>` element + real range-served URLs + Media Session API (not WebAudio) — that
architecture is what *enables* iOS background audio; the remaining question is purely
**which codec+container the `<audio>` element will decode.**

### AAC / M4A and MP3 — confirmed, universal

AAC-LC in MP4/M4A and MP3 have been playable in iOS Safari `<audio>` since the earliest
versions and remain the universally safe baseline (caniuse "AAC"/"mp3" show full Safari/iOS
support across all tracked versions). No version-gating concerns.

### Opus — now playable on iOS, but container support is version-fragmented

**Verified facts (primary + dated):**

- **Opus in Ogg** landed in Safari `<audio>` at **Safari 18.4 / iOS 18.4 / iPadOS 18.4 /
  macOS Sequoia 15.4 / visionOS 2.4**, announced **2025-03-31**:
  > "WebKit for Safari 18.4 rounds out our support for media formats by adding **Ogg
  > container support for both Opus and Vorbis audio** on macOS Sequoia 15.4, iOS 18.4,
  > iPadOS 18.4, and visionOS 2.4." — [webkit.org/blog/16574/webkit-features-in-safari-18-4](https://webkit.org/blog/16574/webkit-features-in-safari-18-4/)

  caniuse corroborates: iOS Safari **partial (◐) through 18.3, full (✅) from 18.4**
  ([caniuse.com/opus](https://caniuse.com/opus)).

- **Opus in CAF (Core Audio Format)** has been decodable on **iOS 11 / macOS High Sierra
  (2017)** — years before Ogg-Opus — and is the technique used to get Opus into Safari's
  `<audio>` on iOS < 18.4. *(Secondary sources: [testmuai.com Opus browser support](https://www.testmuai.com/learning-hub/opus-audio-codec-browser-support/),
  [opus-media-recorder issue #17](https://github.com/kbumsik/opus-media-recorder/issues/17).
  Needs prototype confirmation for `<audio>` + Media Session specifically.)*

- **Opus in MP4/fMP4** is **not** accepted by Safari's `<audio>` element (Safari rejects the
  MIME type, unlike Chrome/Firefox/Edge). *(Secondary:
  [testmuai.com](https://www.testmuai.com/learning-hub/opus-audio-codec-browser-support/);
  cross-checked against the WebKit 18.4 notes, which add Ogg/WebM but not MP4 for `<audio>`.)*
  So the "Opus in fMP4 to reuse the MP4 container" idea does **not** work on iOS.

- Opus in **WebM** is supported on newer Safari (macOS Sonoma+), but WebM is a poor fit for a
  simple range-served `<audio>` archive and doesn't help older iOS.

### Recommendation for the playback target

- **Universal safe target = AAC-LC in M4A / fragmented-MP4.** Works in iOS Safari `<audio>`
  on **every** iOS version and reliably drives Media Session/lock-screen background audio.
  This is the correct default enhanced-output container. AAC-LC mono ≈ 24–32 kbps is plenty
  for scanner voice and far smaller than the source WAV.
- **Storage-efficient modern target = Opus-in-Ogg** (≈ 16–24 kbps mono, better quality per
  bit, BSD/royalty-free). Only fully safe on **iOS 18.4+**. Use it if you can require modern
  iOS, or serve it via **content negotiation** (Opus/Ogg to capable clients, AAC/M4A to the
  rest) — at the cost of dual-encoding/storing. **Opus-in-CAF** could extend Opus back to
  iOS 11+ in one file, but treat it as prototype-only until verified against `<audio>` +
  Media Session on a real device.
- **Do not** target Opus-in-MP4 (iOS won't play it) and avoid MP3 output (larger, LGPL).

**Bottom line:** ship **AAC-LC/M4A** as the single enhanced output for guaranteed universal
iOS background playback; offer **Opus/Ogg** as an efficiency option/flag for modern fleets.

---

## Crate shortlist

| Crate | Purpose | License | Pure Rust? | Maintained? (latest) | arm64 / cross notes |
|---|---|---|---|---|---|
| `symphonia` | Decode WAV/MP3/AAC-MP4/FLAC → PCM | MPL-2.0 | Yes (100% safe Rust) | Yes | No C; trivial cross. Enable `mp3`,`aac`,`isomp4` (non-default). No raw-ADTS demux. |
| `rubato` | High-quality resample to 48 kHz | MIT | Yes | Yes | No C. (Prefer over dormant `dasp` resamplers.) |
| `nnnoiseless` | RNNoise denoise (48 kHz/480-frame) | BSD-3-Clause | Yes (mostly safe) | Yes (v0.5.2, 2025-12) | No C; ~7× real-time on Pi 3 → faster on Pi 5. |
| `ebur128` | EBU R128 / BS.1770 loudness (M/S/I, LRA, true-peak) | MIT | Yes (port of libebur128) | Yes (v0.1.10, 2024-10) | No C; cheap. |
| `biquad` | IIR band-pass/high-pass/low-pass coeffs | MIT OR Apache-2.0 | Yes (`no_std`) | Yes (v0.6.0, 2026-03) | No C; negligible cost. |
| `fundsp` | DSP graph, limiter/compressor/AGC, filters | MIT OR Apache-2.0 | Yes (`no_std`) | Yes (v0.23.0, 2026-01) | No C. |
| `dasp` | Sample/frame primitives (transitive) | MIT OR Apache-2.0 | Yes (`no_std`) | Dormant (v0.11.0, 2020-05) | No C; use `fundsp`/`rubato` for active DSP. |
| `opus` (over `audiopus_sys`) | Opus encode via libopus | MIT OR Apache-2.0 (wrapper); **libopus BSD-3 + royalty-free** | No (binds C libopus) | Yes (v0.3.1, 2026-01) | `audiopus_sys` static-builds libopus via cmake; cross-friendly. |
| `fdk-aac` (over `fdk-aac-sys`) | AAC-LC encode via libfdk-aac | MIT (wrapper); **libfdk-aac = FDK license, BSD-derived, no patent grant** | No (vendors C/C++) | Yes (v0.8.0, 2025-09) | Vendored + static-linked; **redistributable standalone; NON-distributable if combined with GPL ffmpeg**. |
| `ogg` | Mux Opus packets into Ogg | BSD-3-Clause | Yes | Yes (v0.9.2, 2025-01) | No C. Pairs with `opus`. |
| `mp4` | Mux AAC into MP4/M4A | MIT | Yes | Stale (v0.14.0, 2023-08) | No C, but AAC **write** path unverified — see open questions. |
| `mp3lame-encoder` (over `mp3lame-sys`) | MP3 encode via LAME | **LGPL-3.0** | No (vendors LAME 3.100) | Yes (v0.2.4/0.1.11, 2025–26) | Static (autotools) build; **LGPL relink obligations** — avoid unless required. |
| ~~`ffmpeg-next` / `rusty_ffmpeg`~~ | (not recommended) | LGPL/GPL depending on build | No (binds libav\*) | `ffmpeg-next` maintenance mode | Needs cross-built libav\* + env vars; **breaks single-binary; GPL+fdk-aac = non-distributable**. |

---

## Open questions / needs-prototype

1. **Pure-Rust AAC-in-M4A muxing (highest risk).** Confirm the `mp4` crate (or an
   alternative) can *write* a valid AAC-LC track that iOS Safari `<audio>` plays, including
   correct `esds`/`stsd`/`stco` and fragmented-MP4 for progressive/range playback. The `mp4`
   crate is stale (2023) and its AAC write path is unverified. Fallbacks in order: (a) find/
   patch a maintained MP4 muxer; (b) have `fdk-aac` emit ADTS and write a minimal MP4/ADTS
   wrapper; (c) last resort, shell out to a system `ffmpeg` (LGPL, native AAC — keeps our
   binary clean and distributable).
2. **Opus-in-CAF on iOS < 18.4 via `<audio>` + Media Session.** Verify on a real device that
   an Opus/CAF file plays in `<audio>` *and* attaches lock-screen controls. If yes, Opus
   could be a single universal output back to iOS 11. Currently only secondary-source
   supported.
3. **AAC patent posture (legal review).** The FDK license grants copyright but "no patent
   grant." Decide whether shipping an AAC encoder needs any AAC patent-pool consideration for
   the project's distribution model, or whether to sidestep entirely by defaulting to Opus
   (BSD + royalty-free) where the client supports it.
4. **LGPL static-linking policy.** If MP3 output (LAME) or an LGPL FFmpeg is ever used in the
   single static binary, define how the relink/source-availability obligation is met (e.g.
   publish object files / offer dynamic linking). Avoidable by sticking to Opus + FDK-AAC.
5. **Cross-compile validation.** Build the full pipeline (with `opus` + `fdk-aac` C source)
   for `aarch64-unknown-linux-gnu` via `cross` and `cargo-zigbuild`; confirm the autotools
   `mp3lame-sys` path (if included) cross-builds, and record actual stripped binary size and
   per-call CPU/latency on the Pi 5.
6. **Enhancement parameter tuning.** Pin the target LUFS, band-pass corners, compressor
   settings, and dead-air gate thresholds against real scanner recordings (P25, analog FM),
   and A/B RNNoise-on vs -off for intelligibility on upsampled-narrowband audio.
7. **Resampler choice.** Confirm `rubato` for 8 kHz→48 kHz (RNNoise in) and 48 kHz→output —
   quality vs CPU — over `dasp`/`fundsp` resamplers.
