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
