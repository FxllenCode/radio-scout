# AAC encoding, M4A/MP4 muxing, and AAC patent posture for opt-in enhancement

Research date: 2026-07-15. Scope: the codec + muxing strategy for Radio-Scout v1's
**opt-in, off-by-default** audio-enhancement output ([ADR-0006](../adr/0006-optional-rust-native-audio-enhancement.md)),
under three hard constraints:

1. Ship as a **single self-contained binary** that cross-compiles to Raspberry Pi 5 (arm64).
2. The **default** binary stays **pure-Rust with zero patent/licensing liability**.
3. Enhancement is **cargo-feature-gated and off by default**.

This deepens the muxing and patent sections of
[`audio-pipeline.md`](audio-pipeline.md) (which established the full Rust-native decode →
DSP → encode pipeline). New since that doc: **two maintained (2025–26) pure-Rust MP4 muxers
that write AAC and Opus** (`muxide`, `mp4e`), the **exact Via LA AAC licensing terms** from
the pool operator's own FAQ, and confirmation of the **`fdk-aac` crate's ADTS transport**
that feeds those muxers.

---

## Bottom line / recommendation

**Keep the default binary pure-Rust and patent-clean; put AAC entirely behind the opt-in
enhancement feature; mux AAC with a pure-Rust crate (`muxide` primary, `mp4e` secondary),
not the stale `mp4` crate and not ffmpeg.**

- **Default binary (unchanged, ADR-0002/0006 default = passthrough):** contains **no AAC
  encoder, no C codec, no patent surface**. This is the single most important structural
  decision — because AAC is behind a cargo feature that is *off by default*, the artifact the
  project publishes for general use has zero AAC patent exposure by construction.

- **Enhancement output codec = AAC-LC (`fdk-aac` crate).** It is still the only codec that
  plays in **iOS Safari `<audio>` on every iOS version** and reliably drives Media
  Session/lock-screen background audio (Radio-Scout's #1 platform). The `fdk-aac` crate
  (v0.8.0, 2026-05-26, MIT wrapper) vendors and static-links libfdk-aac, so it stays
  single-binary. Its copyright license is **redistributable on its own but grants no
  patents** — that is a real, if practically low, exposure, mitigated by feature-gating (below).

- **Muxing = pure Rust, no longer a blocker.** The `audio-pipeline.md` "muxing is unproven /
  the `mp4` crate is stale" risk is largely retired: **`muxide`** (v0.2.1, 2026-05-19,
  Apache-2.0 OR MIT, pure Rust, zero-deps, 200+ tests) muxes **AAC (ADTS) and Opus** into
  MP4 and **fragmented MP4**, and **`mp4e`** (v1.0.5, 2025-10-14, MIT) does the same. The
  `fdk-aac` crate emits **ADTS** frames (`Transport::Adts`), which is exactly what `muxide`
  ingests. The remaining unknown is not "can we mux in Rust" but "does the muxed file play in
  **iOS Safari `<audio>` + Media Session**" — a real-device prototype gate, not a library gap.

- **Opus-in-Ogg = the efficiency / patent-free mode, not the universal default.** libopus is
  BSD-3 + royalty-free (the cleanest option on both axes), and pure-Rust Ogg muxing (`ogg`
  crate) is solid. **But Opus/Ogg in Safari `<audio>` only works on iOS 18.4+ (Mar 2025).**
  As of mid-2026 a meaningful slice of the iOS fleet is still older, so Opus/Ogg cannot be the
  *only* enhanced output without breaking iOS playback. Offer it as an opt-in efficiency mode
  for modern fleets (or via content negotiation), not as the default enhanced format.

- **System ffmpeg for muxing = demoted to a tertiary fallback.** With two maintained pure-Rust
  muxers, "shell out to an operator-installed ffmpeg for muxing only" is now a break-glass
  option if both fail iOS validation — not a planned path. **Never** bundle or link an ffmpeg
  built `--enable-libfdk-aac --enable-nonfree` (that binary is legally non-distributable).

- **Patent posture, plainly:** distributing an **AAC encoder** *is* the act the AAC patent
  pool licenses (the pool explicitly does **not** charge for AAC *bitstreams*). The pool
  (Via LA) is aimed at commercial makers of "end-user encoder/decoder products," priced
  per-unit-on-sale, and **says nothing about free/open-source software**. Baseline AAC patents
  run to **2028**, extensions to **2031**. This is genuine-but-bounded uncertainty; the
  mitigation is architectural (feature-gate AAC off by default; prefer Opus where the client
  supports it; leave building the AAC-enabled binary to the operator) plus a legal review
  before the *project itself* publishes prebuilt binaries with the AAC feature turned on.

---

## Q1 — AAC-LC encoding from Rust: the real options

### Option A (recommended): `fdk-aac` crate → vendored, static-linked libfdk-aac

- **Crate:** `fdk-aac` **v0.8.0 (released 2026-05-26)**, **MIT** wrapper, over `fdk-aac-sys`
  `^0.5`, which **vendors the libfdk-aac C/C++ source and static-links it** — no system
  library, no runtime `.so`
  ([docs.rs/fdk-aac](https://docs.rs/fdk-aac/latest/fdk_aac/),
  [github.com/haileys/fdk-aac-rs](https://github.com/haileys/fdk-aac-rs)). Highest-quality
  open AAC encoder ("generally preferred over FFmpeg's native AAC encoder",
  [trac.ffmpeg.org/wiki/Encode/AAC](https://trac.ffmpeg.org/wiki/Encode/AAC)).
- **Transport format (confirmed from source):** the encoder exposes
  `pub enum Transport { Adts, Raw }`, selected via the `EncoderParams { bit_rate, sample_rate,
  transport, channels, audio_object_type }` struct passed to `Encoder::new(...)`
  ([src/enc.rs, haileys/fdk-aac-rs](https://github.com/haileys/fdk-aac-rs/blob/main/src/enc.rs)).
  `Transport::Adts` produces self-describing ADTS frames — **exactly the input `muxide` and
  `mp4e` expect** (see Q3). `Transport::Raw` emits bare access units and would require handing
  the muxer a separately-built `AudioSpecificConfig`.
- **Licensing — copyright is fine, patents are not granted.** The Fraunhofer FDK AAC license
  is BSD-derived and **redistributable in source or binary without copyright fees**, but with
  two clauses that matter here
  ([SPDX FDK-AAC](https://spdx.org/licenses/FDK-AAC.html),
  [Fedora Licensing/FDK-AAC](https://fedoraproject.org/wiki/Licensing/FDK-AAC)):
  > "NO EXPRESS OR IMPLIED LICENSES TO ANY PATENT CLAIMS, including without limitation the
  > patents of Fraunhofer, ARE GRANTED BY THIS SOFTWARE LICENSE." … "You may use this FDK AAC
  > Codec software or modifications thereto **only for purposes that are authorized by
  > appropriate patent licenses**."

  It also forbids charging a copyright license fee and requires offering source. So: **the
  code is freely redistributable; the *patents practiced by running it* are a separate
  question** handled in Q2. (This is the "not free" clause the brief flagged — it is a
  *patent-scope* limitation, not a copyleft or redistribution bar.)
- **Single-binary implication: clean.** Vendored + static → one binary, no runtime deps.
  Cross-compiles to arm64 via `cross`/`cargo-zigbuild` (C source builds with `cc`). Adds
  roughly +1–2 MB stripped (estimate — measure on target).

### Option B: a pure-Rust AAC encoder — does not exist (production-usable)

There is **no production-grade pure-Rust AAC *encoder***. `symphonia` — the pure-Rust media
framework — is **decode-only** by design
([github.com/pdeljanov/Symphonia](https://github.com/pdeljanov/Symphonia)); it can read
AAC-in-MP4 but cannot write AAC. No maintained Rust crate implements an AAC *encoder* without
binding C. So "pure-Rust AAC encode" is off the table for v1; any AAC path routes through
libfdk-aac (Option A) or an external tool (Option C). (This is why the pure-Rust *default*
binary simply contains no AAC encoder at all — see recommendation.)

### Option C: shell out to a system-installed encoder (e.g. ffmpeg)

Encoding via an operator-installed `ffmpeg`/`afconvert` is license-isolating (separate
process the operator installed) but **breaks the single-binary guarantee** (operator must
install and maintain a native encoder on the Pi) and gives up the pure-Rust story. It is
strictly worse than Option A for *encoding* (fdk-aac already static-links cleanly). Keep
"shell out" only for the *muxing* fallback discussed in Q5, and never for encoding.

**Verdict:** encode with the `fdk-aac` crate under `Transport::Adts`, gated behind the
enhancement cargo feature. Copyright licensing is clean and single-binary-friendly; the only
open item is AAC *patent* posture (Q2).

---

## Q2 — AAC patent posture as of 2026

### The pool: Via LA (formerly Via Licensing)

The AAC patent pool is administered by **Via Licensing Alliance ("Via LA")**, formed by the
**May 2023 merger of Via Licensing Corporation and MPEG LA** — now the largest SEP pool
administrator ([design-reuse.com, merger announcement](https://www.design-reuse.com/news/14062-via-licensing-and-mpeg-la-unite-to-form-via-licensing-alliance-the-largest-patent-pool-administrator-in-the-consumer-electronics-industry/)).
The AAC program remains active and commercially aggressive: a **25% rate adjustment took
effect for new licensees on 2026-01-01**, and the pool reports **~80% penetration of the
mobile market** with licensees including Apple, Samsung, Xiaomi, etc.
([ip fray, Feb 2026](https://ipfray.com/access-advance-via-la-position-multimedia-patent-pools-for-further-growth-with-price-stability-offer-regionalization-for-more-standards/),
[via-la.com news](https://www.via-la.com/news/)). This is **not** a dormant/expired pool.

### What the pool actually licenses (from Via LA's own FAQ)

The distinction that matters for an open-source encoder, quoted from the pool operator
([via-la.com AAC FAQ](https://www.via-la.com/licensing-2/aac/aac-faqs/),
[via-la.com AAC program](https://www.via-la.com/licensing-programs/aac/)):

- **Who needs a license:** "An AAC patent license is needed by **manufacturers or developers
  of end-user encoder and/or decoder products**."
- **Bitstreams are free:** "There are **no patent license fees due for the distribution of
  bit-streams encoded in AAC**, whether such bit-streams are broadcast, streamed over a
  network, or provided on physical media."
- **Fees are per-unit on *sale*:** the rate card runs from **$0.98/unit** (first 500k) down
  to **$0.10/unit** (75M+), with a **$15,000 one-time fee ($1,000 for small entities)**;
  Wikipedia summarizes the software case as "**each computer running the software is
  considered a separate 'unit'**"
  ([en.wikipedia.org/wiki/Advanced_Audio_Coding](https://en.wikipedia.org/wiki/Advanced_Audio_Coding)).
- **Components aren't covered:** "Licensee's products that are **not** end-user products
  (e.g. components or implementations) are **not** covered by the license and would require
  that the party incorporating such a component … obtain a license."
- **Open source / free software:** the FAQ and program page make **no mention** of
  open-source, free, or no-charge software. This is the crux of the uncertainty: the entire
  fee structure is framed around *product sales*, and there is no stated carve-out or stated
  obligation for zero-price open-source distribution.

**Reading for Radio-Scout:** distributing **an AAC encoder** is squarely the licensable act
(the pool licenses "encoder … products"); distributing AAC *files* our app produced is
explicitly **not**. So the exposure surface is precisely "does the project distribute an AAC
*encoder*," which is exactly what feature-gating removes from the default artifact.

### Which patents are expired vs. still live

- Wikipedia, citing the pool's SEP list: "the **last baseline AAC patent expires in 2028**,
  and the **last patent for all AAC extensions … expires in 2031**"
  ([en.wikipedia.org/wiki/Advanced_Audio_Coding](https://en.wikipedia.org/wiki/Advanced_Audio_Coding)).
  (One Debian-side analysis reads the tails slightly later — "~2031 base, ~2038 extensions" —
  and explicitly flags its sources as uncertain,
  [tookmund.com AAC and Debian](https://tookmund.com/2024/02/aac-and-debian).)
- The **oldest MPEG-2 AAC patents have expired** (mid-1990s filings; some parties treat
  MPEG-2 AAC-LC as expired ~2017–2018 — e.g. Wikimedia enabled AAC-LC uploads in 2018 on that
  basis, [phabricator.wikimedia.org T166025](https://phabricator.wikimedia.org/T166025)). But
  **AAC-LC is not cleanly patent-free**: MPEG-4 AAC-LC pulls in later SEPs, which is why the
  pool still lists baseline patents running to 2028. Treat "AAC-LC is already patent-free" as
  **contested, not settled**.

### How the free-software world actually handles it (practical precedent)

- **Fedora** ships **`fdk-aac-free`** — a build with the **patented profiles stripped, keeping
  essentially AAC-LC** — and continues to package it; it treats the *copyright* license as
  Free while acknowledging the "no patent licenses" clause as the reason full fdk-aac is
  problematic ([fedoraproject.org/wiki/Licensing/FDK-AAC](https://fedoraproject.org/wiki/Licensing/FDK-AAC)).
- **Debian** classifies the FDK license as **non-free** — partly the DFSG "no discrimination
  against fields of endeavor" issue (you may not charge a distribution fee), partly the patent
  clause — and has kept `fdk-aac-free` stuck in NEW review
  ([tookmund.com](https://tookmund.com/2024/02/aac-and-debian),
  [news.ycombinator.com discussion](https://news.ycombinator.com/item?id=39503266)).

**Net:** shipping an AAC-LC encoder in open source is **common and tolerated in practice**
(Fedora does it) but **not risk-free in theory** (Debian won't put it in main; the pool is
active and the patents live to 2028+). This is exactly why feature-gating is the right posture.

### Is distributing an AAC *encoder* a licensing exposure for a self-hosted OSS project?

**Yes, in principle — and it is the encoder, not the audio, that matters.** But the exposure
is (a) bounded (baseline patents expire 2028), (b) aimed by the pool at commercial per-unit
product makers, not free self-hosted software, and (c) **fully removable from the default
artifact** by keeping AAC behind an off-by-default cargo feature. What remains *uncertain* and
must be legal-reviewed: whether the *project* publishing a prebuilt binary **with the AAC
feature enabled** would itself be a "developer of an end-user encoder product" in the pool's
sense. Sidestep by: defaulting to Opus where the client supports it, and/or leaving the
AAC-enabled build to the operator.

---

## Q3 — M4A/MP4 muxing in pure Rust (the risk that mostly retired)

`audio-pipeline.md` flagged pure-Rust AAC-in-MP4 muxing as the highest-risk step, with only
the stale `mp4` crate (v0.14.0, 2023-08) as a candidate. Two **currently-maintained** pure-Rust
muxers now cover it:

| Crate | Latest | License | AAC in? | Opus in? | fMP4? | Input | Maturity notes |
|---|---|---|---|---|---|---|---|
| **`muxide`** | **v0.2.1 (2026-05-19)** | Apache-2.0 OR MIT | **Yes — all profiles (LC/Main/SSR/LTP/HE/HEv2), ADTS in** | Yes — raw 48 kHz packets | **Yes (DASH/HLS init+media segments)** | **pre-encoded frames** | **Pure Rust, zero deps, no ffmpeg, "no unsafe"; 200+ tests + property-based; "fast-start" layout; 70★, active** ([github.com/Michael-A-Kuykendall/muxide](https://github.com/Michael-A-Kuykendall/muxide)) |
| **`mp4e`** | **v1.0.5 (2025-10-14)** | MIT | Yes — LC/Main/SSR/LTP/HE/HEv2 | Yes | Yes (`new_with_fragment`) | pre-encoded frames | Pure Rust, inspired by `minimp4`; **1 video + 1 audio track max**; small (~1.5 kSLoC) ([github.com/xjunl22/mp4e](https://github.com/xjunl22/mp4e)) |
| ~~`mp4` (alfg)~~ | v0.14.0 (2023-08) | MIT | write path unverified | — | — | — | **Stale (~3 yr), AAC write unproven** ([github.com/alfg/mp4-rust](https://github.com/alfg/mp4-rust)) |
| `symphonia` | current | MPL-2.0 | **decode only** | — | — | — | Cannot write MP4/AAC at all ([github.com/pdeljanov/Symphonia](https://github.com/pdeljanov/Symphonia)) |

**Key fit:** both `muxide` and `mp4e` take **already-encoded** audio frames (not PCM — they do
not encode), so the pipeline is `fdk-aac` (`Transport::Adts`) → `muxide.write` → `.m4a`/fMP4.
`muxide`'s "AAC … ADTS" input matches `fdk-aac`'s ADTS output directly (the muxer parses the
ADTS header to synthesize the `esds`/`AudioSpecificConfig`). `muxide` is the stronger pick on
maturity (dual-permissive license, explicit test suite, active in 2026, fragmented-MP4 for the
progressive/range playback ADR-0002 wants); `mp4e` is a viable fallback with the same feature
surface but a smaller/newer codebase and a hard 1-audio-track cap (fine for our mono voice).

**Caveat — assessed on library maturity, not on iOS acceptance.** Neither crate documents that
its output has been validated in **iOS Safari `<audio>` + Media Session**. "Standards-compliant
MP4" is necessary but not sufficient for Safari, which is picky about `esds`, `stsd`, mono
channel config, and `moov`/`fast-start` placement for range requests. **This is the one real
prototype gate that remains** (see final section) — but it is now "validate one of two
maintained muxers against a real iPhone," not "write an MP4 muxer from scratch."

---

## Q4 — Opus-in-Ogg as the patent-free alternative

### Encoder + muxer are clean and pure-Rust-adjacent

- **Encode:** the **`opus`** crate (MIT OR Apache-2.0, current) over **`audiopus_sys`**, which
  **static-builds libopus from source** (static by default on Windows/macOS/musl)
  ([lib.rs/crates/opus](https://lib.rs/crates/opus),
  [github.com/Lakelezz/audiopus_sys](https://github.com/Lakelezz/audiopus_sys)). **libopus is
  BSD-3-Clause and royalty-free** — the Opus patents (Xiph, Broadcom, Microsoft, Skype) are
  granted royalty-free ([opus-codec.org/license](https://opus-codec.org/license/)). This is the
  **cleanest option on both copyright and patents** — no pool, no fees, no uncertainty.
- **Mux:** the **`ogg`** crate (BSD-3-Clause, pure Rust) muxes Opus packets into Ogg with the
  standard `OpusHead`/`OpusTags` pages. This is a well-trodden, low-risk pure-Rust path
  (unlike AAC-in-MP4, Ogg muxing has no `esds`-style trap).

### The blocker is iOS `<audio>` playback, not licensing

From [`audio-pipeline.md` Q3](audio-pipeline.md) (primary-source verified there):

- **Opus-in-Ogg** in Safari `<audio>` landed only at **Safari 18.4 / iOS 18.4 / iPadOS 18.4 /
  macOS 15.4**, announced **2025-03-31**
  ([webkit.org/blog/16574](https://webkit.org/blog/16574/webkit-features-in-safari-18-4/);
  [caniuse.com/opus](https://caniuse.com/opus): partial ◐ ≤18.3, full ✅ ≥18.4).
- **Opus-in-MP4/fMP4 is *not* accepted** by Safari's `<audio>` element (so you cannot smuggle
  Opus into the MP4 container to reuse it on iOS).
- **Opus-in-CAF** has decoded on iOS 11+ since 2017 and *could* extend Opus back to old iOS in
  one file, but remains **prototype-only** for `<audio>` + Media Session (secondary sources).

### Browser-support contrast for the `<audio>` element

| Format | iOS Safari `<audio>` | Desktop Safari | Chrome / Edge | Firefox |
|---|---|---|---|---|
| **AAC-LC in M4A/MP4** | ✅ every version | ✅ | ✅ | ✅ |
| **MP3** | ✅ every version | ✅ | ✅ | ✅ |
| **Opus in Ogg** | ✅ **only ≥ iOS 18.4** (Mar 2025); ✗ older | ✅ ≥ macOS 15.4 | ✅ | ✅ |
| **Opus in MP4** | ✗ (Safari rejects) | ✗ | ✅ | ✅ |
| **Opus in CAF** | ◐ decodable ≥ iOS 11, unverified for `<audio>`+MediaSession | ◐ | ✗ | ✗ |

**Is Opus a viable *default* for scanner voice on iOS Safari?** **Not as the sole default in
2026.** iOS is Radio-Scout's #1 platform, and a non-trivial share of real-world iPhones are
still below 18.4 (older hardware, users who don't update). Shipping Opus/Ogg as the *only*
enhanced output would silently fail to play on those devices. Opus is the right **efficiency
mode** (≈16–24 kbps mono vs ≈24–32 kbps AAC, better quality per bit, zero patent worry) for
fleets you can guarantee are ≥18.4, or via **content negotiation** (serve Opus/Ogg to capable
clients, AAC/M4A to the rest) at the cost of dual-encode/store. AAC-LC/M4A stays the
**universal** enhanced default.

---

## Q5 — Fallback: system ffmpeg for muxing only

**(a) Shell out to an ffmpeg the operator already installed — license-clean, but not
single-binary.** Invoking a separate `ffmpeg` process the *operator* installed keeps *our*
binary pure-Rust and license-isolated (it's not linked into us). ffmpeg's **native** AAC
encoder is LGPL and fine; muxing AAC/Opus into MP4/Ogg via a system ffmpeg is unambiguously
redistributable on our side. The cost is the ADR-0007 single-binary promise: the operator must
now install and maintain ffmpeg on the Pi.

**(b) Bundling ffmpeg — avoid.** Shipping an ffmpeg binary (or linking `ffmpeg-next`/libav*)
pulls FFmpeg's LGPL/GPL terms into our distribution and wrecks cross-compilation. The specific
trap ([ffmpeg.org/general.html](https://ffmpeg.org/general.html),
[ffmpeg.org/legal.html](https://www.ffmpeg.org/legal.html)): an ffmpeg built
`--enable-libfdk-aac` requires `--enable-nonfree`, and **`--enable-nonfree` makes the binary
legally non-distributable**. So a bundled "best AAC" ffmpeg is the one build we can never ship.

**When is "mux only via system ffmpeg" the right call?** **Only as a break-glass tertiary
fallback** — if, at prototype time, *both* `muxide` and `mp4e` produce M4A that iOS Safari
`<audio>` refuses to play and neither is quickly patchable. Given two maintained pure-Rust
muxers now exist, this should not be the planned path; keep it documented as the escape hatch,
gated behind detection of an operator-installed ffmpeg, and **never** bundle or link it.

---

## Q6 — Recommendation

**Codec + muxing path for v1's opt-in enhancement output:**

1. **Default binary: pure-Rust, passthrough, no AAC, no C codec.** Zero patent surface by
   construction. This is the artifact the project publishes for general use.
2. **Enhancement feature (cargo-gated, off by default) → AAC-LC / M4A:**
   - Encode with **`fdk-aac`** (`Transport::Adts`), vendored + static-linked (single binary,
     arm64-cross-friendly).
   - Mux with **`muxide`** (primary) into **M4A / fragmented-MP4**; keep **`mp4e`** as the
     drop-in secondary. No ffmpeg.
   - Rationale: only AAC-LC guarantees iOS Safari `<audio>` + Media Session/background audio on
     **every** iOS version — the enhancement's whole purpose is quality that plays everywhere.
3. **Opus-in-Ogg = opt-in efficiency mode** (`opus` + `ogg`), for ≥ iOS 18.4 fleets or via
   content negotiation. Cleanest licensing (BSD-3 + royalty-free), ~half the bitrate; **not**
   the universal default because of the iOS 18.4 playback floor.
4. **System-ffmpeg muxing = documented break-glass only**, never bundled/linked, never
   `--enable-nonfree`.

**Browser-compat tradeoff, stated plainly:** choosing AAC/M4A as the enhanced default buys
**universal iOS playback** at the price of a **live-but-bounded AAC patent question** (baseline
patents to 2028) and the FDK "no patent grant" clause. Choosing Opus/Ogg buys **clean
royalty-free licensing** at the price of **breaking playback on pre-18.4 iOS**. Radio-Scout's
iOS-first mandate makes **AAC the default enhanced output and Opus the efficiency option** the
correct balance — with the patent risk neutralized on the default artifact by feature-gating.

---

## Still to validate (prototype-gated)

1. **iOS Safari `<audio>` + Media Session playback of `muxide`/`mp4e` output (highest
   priority).** Encode a real scanner call with `fdk-aac` (`Transport::Adts`, mono, ~24–32
   kbps), mux to M4A **and** fragmented-MP4 with `muxide`, serve via the ADR-0002 range
   endpoint, and confirm on a **real iPhone** that: (a) it plays in `<audio>`, (b) lock-screen
   / Media Session transport controls attach and work in the background, (c) HTTP range /
   progressive playback works (fast-start `moov` placement). Repeat with `mp4e`. This is the
   single gate that decides whether the pure-Rust muxing path holds or the system-ffmpeg
   fallback is needed.
2. **`fdk-aac` ADTS → `muxide` `esds`/`AudioSpecificConfig` correctness.** Confirm `muxide`
   derives a correct `AudioSpecificConfig` (object type, sample-rate index, channel config)
   from `fdk-aac`'s ADTS frames, or feed `Transport::Raw` + an explicitly-built ASC if the
   ADTS-derived path is wrong. Verify the resulting `stsd`/`mp4a`/`esds` box on a known-good
   validator and on iOS.
3. **AAC patent legal review before the *project* publishes an AAC-enabled prebuilt binary.**
   Confirm whether feature-gating + "operator builds the AAC binary" is sufficient, or whether
   distributing any prebuilt AAC-enabled artifact makes the project a "developer of an end-user
   encoder product" under Via LA's terms. Decide the default-fleet policy (AAC vs Opus) in
   light of that. (Baseline patents expire 2028; extensions 2031.)
4. **Opus/Ogg on the real iOS fleet.** Measure what fraction of the target audience is ≥ iOS
   18.4 before enabling Opus by default anywhere; prototype **content negotiation**
   (Opus/Ogg ↔ AAC/M4A) if dual-encode is warranted. Optionally test **Opus-in-CAF** in
   `<audio>` + Media Session on a pre-18.4 device (only if a single universal Opus file is
   worth chasing).
5. **arm64 cross-compile of the enhancement feature.** Build `fdk-aac` + `opus` (both C
   source) for `aarch64-unknown-linux-gnu` via `cross` / `cargo-zigbuild`; record stripped
   binary-size delta and per-call encode+mux CPU/latency on the Pi 5.
6. **`muxide` maintenance bet.** It is young (v0.2.1) and largely one-maintainer; confirm it
   keeps pace, or vendor/fork it, before committing it as the sole muxer. `mp4e` is the hedge.

---

## Sources

- ADR-0006 (this project): [`docs/adr/0006-optional-rust-native-audio-enhancement.md`](../adr/0006-optional-rust-native-audio-enhancement.md);
  prior pipeline research: [`docs/research/audio-pipeline.md`](audio-pipeline.md).
- `fdk-aac` crate & transport: [docs.rs/fdk-aac](https://docs.rs/fdk-aac/latest/fdk_aac/),
  [github.com/haileys/fdk-aac-rs](https://github.com/haileys/fdk-aac-rs),
  [src/enc.rs](https://github.com/haileys/fdk-aac-rs/blob/main/src/enc.rs).
- FDK license: [SPDX FDK-AAC](https://spdx.org/licenses/FDK-AAC.html),
  [Fedora Licensing/FDK-AAC](https://fedoraproject.org/wiki/Licensing/FDK-AAC),
  [tookmund.com — AAC and Debian](https://tookmund.com/2024/02/aac-and-debian).
- AAC patent pool: [Via LA AAC program](https://www.via-la.com/licensing-programs/aac/),
  [Via LA AAC FAQ](https://www.via-la.com/licensing-2/aac/aac-faqs/),
  [Via LA news](https://www.via-la.com/news/),
  [Via/MPEG LA merger](https://www.design-reuse.com/news/14062-via-licensing-and-mpeg-la-unite-to-form-via-licensing-alliance-the-largest-patent-pool-administrator-in-the-consumer-electronics-industry/),
  [ip fray, Feb 2026](https://ipfray.com/access-advance-via-la-position-multimedia-patent-pools-for-further-growth-with-price-stability-offer-regionalization-for-more-standards/).
- AAC patent expiry: [en.wikipedia.org/wiki/Advanced_Audio_Coding](https://en.wikipedia.org/wiki/Advanced_Audio_Coding),
  [phabricator.wikimedia.org T166025](https://phabricator.wikimedia.org/T166025).
- Pure-Rust MP4 muxers: [github.com/Michael-A-Kuykendall/muxide](https://github.com/Michael-A-Kuykendall/muxide),
  [github.com/xjunl22/mp4e](https://github.com/xjunl22/mp4e),
  [github.com/alfg/mp4-rust](https://github.com/alfg/mp4-rust),
  [github.com/pdeljanov/Symphonia](https://github.com/pdeljanov/Symphonia).
- Opus: [lib.rs/crates/opus](https://lib.rs/crates/opus),
  [github.com/Lakelezz/audiopus_sys](https://github.com/Lakelezz/audiopus_sys),
  [opus-codec.org/license](https://opus-codec.org/license/),
  [webkit.org/blog/16574 (Safari 18.4)](https://webkit.org/blog/16574/webkit-features-in-safari-18-4/),
  [caniuse.com/opus](https://caniuse.com/opus).
- ffmpeg licensing: [ffmpeg.org/legal.html](https://www.ffmpeg.org/legal.html),
  [ffmpeg.org/general.html](https://ffmpeg.org/general.html),
  [trac.ffmpeg.org/wiki/Encode/AAC](https://trac.ffmpeg.org/wiki/Encode/AAC).
