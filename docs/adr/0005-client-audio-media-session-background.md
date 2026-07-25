# Client plays audio via HTML5 `<audio>` + Media Session, not WebAudio

## Context

A headline requirement is working iOS background audio with lock-screen transport controls — the thing rdio-scanner fails at. rdio decodes and plays calls through the WebAudio `AudioContext`, which iOS suspends in the background and will not attach lock-screen/hardware media controls to. That design choice is the root cause of rdio's broken iOS experience (and why they ship separate native apps).

## Decision

The client plays each call through an **HTML5 `<audio>` element** pointed at the call's HTTP audio URL ([ADR-0002](0002-audio-object-storage.md)), driven by the **Media Session API** for lock-screen/hardware transport controls, metadata, and artwork. We do **not** use WebAudio for playback.

Target background behavior (chosen — the most complete tier):
1. Continuous call-to-call queue playback while backgrounded, with Media Session controls (play/pause/next/prev).
2. An inaudible **keep-alive** to hold the audio session open through quiet gaps so new calls auto-play after a lull.
3. **Web Push** (installed PWA, iOS 16.4+) to notify of new activity on watched talkgroups when the app is fully suspended; tapping resumes the feed. A service worker handles push events; the server stores push subscriptions and sends VAPID web-push (`web-push` crate) on matching calls — **coalesced/throttled** (at most one notification per talkgroup per configurable interval, summarizing activity) so a busy system never storms the device. Push cannot auto-start audio — it notifies.

## Status and caveat

Research (`docs/research/ios-background-audio.md`, primary-source-cited) confirms the core: **HTML5 `<audio>` + Media Session, never Web Audio** (Web Audio is treated as "ambient" and muted when backgrounded). Background playback of an audible element works from iOS 15.4; Media Session lock-screen controls from iOS 15; Web Push from iOS 16.4 (home-screen install required, every push must show a notification, a push cannot start/resume audio — only a user tap can, ~70–85% delivery).

The fragile part is **bridging silent gaps**: iOS suspends the page ~30 s after audio goes silent, after which JS stops and the queue cannot self-advance. The "inaudible keep-alive" trick is undocumented, fragile, and regressed in iOS 26. The discrete-call model is kept as the core (it powers per-call metadata, skip/replay/avoid/queue and is confirmed correct for the foreground); the **gap-bridging mechanism is prototype-decided** among:
1. Inaudible keep-alive on the single `<audio>` (simplest, least reliable).
2. **Managed Media Source** (iOS 17.1+) fed continuously (more robust).
3. A **background-only continuous server-side stream** (calls + filler; never silent, so never suspended — most robust, at the cost of server-side per-listener streaming).

**Guaranteed fallback** if none hold up: the robust baseline (continuous playback *while the queue has audio*) + Web Push to notify of new activity when suspended (tap to resume). This must be validated on a real device before being considered done.

## Consequences

- Playback is per-call file playback, not a mixed/streamed WebAudio buffer; gapless/crossfade would be extra work if ever wanted.
- Requires audio served as real URLs with HTTP range support — already committed in ADR-0002.
- Unlocks lock-screen controls, Bluetooth/CarPlay metadata, and background continuity — the core differentiator over rdio.
- Any per-call DSP enhancement ([research pending](../research/audio-pipeline.md)) happens server-side at ingest, not in the client, keeping the client on a plain `<audio>` path.

## Implementation notes (#14)

The player half of this decision shipped in #14 (`client/src/components/CallPlayer.tsx` + `client/src/lib/mediaSession.ts`); the gap-bridging half is still open and belongs to #15. Three sub-decisions were made while building it, all reversible:

- **Pause is store state, not element state.** The lock-screen buttons and the in-app controls dispatch the same actions, and the element follows the store — so the two surfaces can't disagree about what is playing. A `play()` the browser refuses (autoplay policy) is recorded as paused rather than silently swallowed.
- **Artwork is generated, not an asset.** The LED color is the app's one meaningful color and the only glanceable signal on a lock screen, so artwork is a per-talkgroup-colored indexed PNG encoded at runtime (`client/src/lib/artwork.ts` over `client/src/lib/png.ts`), published at the small sizes research §4 names (96, 128). Research says iOS renders *small raster* artwork reliably and SVG unreliably, which rules out a data-URL SVG; a static asset per palette entry per size is the alternative if the encoder ever becomes a burden. Whether iOS actually paints it stays on the real-device manual gate.
- **Prefetch is `fetch`, not a second `<audio preload>`.** iOS ignores `preload` and won't buffer media without a user gesture, so a hidden element would prefetch on every platform except the one that needs it. A plain GET does run, and `GET /api/call/{id}/audio` now answers `Cache-Control: private, max-age=604800, immutable` (the bytes behind a Call id never change) so the element's later — ranged — request is a cache hit.

Two known limits of that prefetch, both open:

- **It only pays off on the filesystem blob backend.** With S3/Garage the route 307s to a *freshly signed* URL per request ([ADR-0002](0002-audio-object-storage.md)), and the redirect deliberately isn't cached because the signature expires — so the prefetch warms one URL and the element then asks for another. Closing it means caching the presigned URL server-side for a slice of its lifetime and letting the 307 carry that as its `max-age`.
- **It stops at the loaded page.** `selectNextCall` only sees the Calls the archive screen has loaded, so the last result on a page — the transition that costs the most, since it needs a search *and* a cold audio fetch — is the one not warmed.

The layer this did **not** add is Vitest Browser Mode, which [ADR-0010](0010-coverage-policy-and-test-tooling.md) names for exactly this component ("real-browser component tests for the audio player + Media-Session wiring"). The wiring is covered in jsdom against a fake Media Session, with the real-browser and real-device layers still owed — Browser Mode belongs with #15, which brings the browser tooling in for PWA install/offline anyway.
