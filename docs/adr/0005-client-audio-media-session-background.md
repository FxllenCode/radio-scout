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

The fragile part is **bridging silent gaps**: iOS suspends the page ~30 s after audio goes silent, after which JS stops and the queue cannot self-advance. The "inaudible keep-alive" trick is undocumented, fragile, and regressed in iOS 26. The discrete-call model is kept as the core (it powers per-call metadata, skip/replay/avoid/queue and is confirmed correct for the foreground); the **gap-bridging mechanism was prototype-decided** among:
1. Inaudible keep-alive on the single `<audio>` (simplest, least reliable).
2. **Managed Media Source** (iOS 17.1+) fed continuously (more robust).
3. A **background-only continuous server-side stream** (calls + filler; never silent, so never suspended — most robust, at the cost of server-side per-listener streaming).

**Guaranteed fallback** if none hold up: the robust baseline (continuous playback *while the queue has audio*) + Web Push to notify of new activity when suspended (tap to resume). This must be validated on a real device before being considered done.

### Gap-bridging: decided (#15) — option 1, and MMS is demoted

[`docs/research/ios-gap-bridging-mechanism.md`](../research/ios-gap-bridging-mechanism.md) (2026-07-26) settled the choice above from WebKit's own source, and **inverts** this ADR's original ranking of option 2 as "more robust". The three facts that decided it:

- **Options 1 and 2 hold the page alive by the identical mechanism, and MMS adds nothing to it.** What prevents suspension is a `com.apple.webkit.MediaPlayback` process assertion, taken when a page reports `MediaProducerMediaState::IsPlayingAudio` — computed by `HTMLMediaElement::computeCanProduceAudio()` from four things: playing, has an audio track, not `muted`, `volume != 0`. **No sample value is examined anywhere.** MMS's own contribution (`HasStreamingActivity`) buys a media-*networking* token, not the playback assertion.
- **MMS cannot ingest our audio.** WebKit ships exactly two `SourceBufferParser`s — AVFoundation (fragmented MP4) and WebM. There is no MPEG-audio parser, and Safari rejects MP3-in-MP4 (`mp4a.40.34`). Every M4A Call would need a remux and every MP3 Call a **transcode**, on the Pi — the cost [ADR-0002](0002-audio-object-storage.md) exists to avoid.
- **MMS's buffering policy is hostile to a live queue.** `ManagedMediaSource::streamingTimerFired()` latches `m_streamingAllowed = false` after 30 s of streaming without a 30 s look-ahead buffer, and **nothing in WebKit ever sets it back**. A feed that must play a Call the moment it lands trips this by construction.

Two more push the same way: `disableRemotePlayback` is mandatory for MMS on iOS (costing AirPlay, whose only escape hatch presupposes option 3), and per-Call Media Session metadata over MSE has an open negative field report (icecast-metadata-js #193). **The revised ladder is 1 → 1-with-aggressive-re-entry → 3 (server-side continuous stream) → 2 → Web-Push-only**; MMS is effectively unreachable, because anything that would make it viable makes option 3 strictly better.

Two rules fall out of the same source reading and are **hard constraints on the implementation**: the keep-alive asset must carry a real audio track, and attenuation must be **encoded into the asset** — `muted` and `volume = 0` each provably zero out `computeCanProduceAudio()` (muting has been the documented "I am not audible" signal since WebKit bug 140524, 2015).

**Still owed: the real-device gate.** The keep-alive is a workaround for [WebKit bug 261858](https://bugs.webkit.org/show_bug.cgi?id=261858), open and unassigned since 2023 — so it is a best effort, not a guarantee, and only an iPhone can say whether it holds on the current iOS. The executable checklist is **§14 of the research file**; it is the single source of truth for that gate, deliberately not duplicated here.

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

## Implementation notes (#15)

The gap-bridging half shipped in #15, alongside the PWA that makes it reachable at all (installing is what gives iOS the standalone display mode, and is the only way to Web Push in #16). Sub-decisions, all reversible:

- **The keep-alive occupies the one element, and hands over *before* `ended`.** When the live feed runs dry, `src` becomes a generated ±1-LSB WAV loop (`client/src/lib/silence.ts`) — never `paused`, never `ended`, the two states iOS reads as permission to suspend. The last Call hands over 0.3 s early (`HANDOVER` in `CallPlayer.tsx`) rather than on `ended`, because `ended` with nothing to follow *is* WebKit bug 261858. The tail that costs is squelch.
- **It runs only while someone is demonstrably listening** (`selectIsBridging`): the live feed owns the audio, it has already played a Call, nothing is playing, nothing is paused. Before the first Call there is no audio session to hold open and no gesture to have opened one, so a `play()` there would simply be refused.
- **It has a budget** — `KEEP_ALIVE_LIMIT_MS`, 5 minutes. Holding the assertion forever blocks exactly the suspension iOS performs to save power, which is the one thing that would make us *worse* than rdio on a phone. Past it we stop, and #16's Web Push becomes how a lull ends. The budget is restored by the Call itself (a `received` extra-reducer), never by a component noticing one.
- **A suspension is admitted, not hidden.** On returning to the foreground, an element found paused while we believed we were bridging means iOS suspended us anyway; the transport records `pause()`, so the listener gets a play button rather than a UI insisting on silence. The same handler re-binds the Media Session actions, which iOS forgets across a backgrounding.
- **A deploy never reloads a listening session.** The worker installs and waits; `applyUpdate()` — a listener's tap — is the only thing that sends `SKIP_WAITING`, and the only `controllerchange` that reloads is the one we asked for (`client/src/lib/serviceWorker.ts`). This is why registration is ours rather than `vite-plugin-pwa`'s injected script.
- **The worker never touches the server's namespace.** `/api/*`, `/healthz` and `/rdio-scanner` are denied the navigation fallback and given no runtime cache: a cached API response would be stale, a cached `/healthz` would lie about the server being up, and cached Call audio would fill a phone with an archive nobody asked for.

Owed after this ticket: the **real-device gate** (research §14 — the mechanism is unproven on hardware until someone runs it), and **Vitest Browser Mode**, which [ADR-0010](0010-coverage-policy-and-test-tooling.md) names for the audio-player component. #15 brought Playwright in for the PWA/service-worker/offline layer that has no other home, but Browser Mode re-tests #14's component wiring rather than #15's features, so it stayed out.
