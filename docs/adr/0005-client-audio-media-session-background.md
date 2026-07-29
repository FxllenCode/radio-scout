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

Two known limits of that prefetch were recorded here when #14 shipped:

- ~~**It only pays off on the filesystem blob backend.**~~ **Closed by #31**, and it took two changes rather than one, because a stable URL is necessary and not sufficient.

  With S3/Garage the route 307s to a signed URL ([ADR-0002](0002-audio-object-storage.md)); while every request signed a *fresh* one, the prefetch warmed a URL the element never asked for, and every prefetched Call was downloaded twice. So the signature is now cached server-side per object key and reused while more than a 60 s margin of its 300 s life remains, and the 307 carries the remainder as its `max-age`. **And** the stored object itself is written carrying the same `Cache-Control` the proxied path sets as a response header — with a presigned redirect the store answers the client directly, so a header we set on *our* response is never seen, and a browser given no freshness information falls back to a heuristic that for a just-written object is zero. Without that second half the element would revalidate every prefetched Call, and the stable URL would have bought a 304 instead of silence. It is set only on the S3 backend: `object_store` errors on an attribute the backend cannot store, and the filesystem path needs none because nothing fetches a local object directly.

  Bounds, all deliberate:

  - The advertised `max-age` is the remaining validity **less** the margin, so a cached redirect drops out of the client's cache before the signature it points at expires. A stale one is a 403 mid-playback — worse than the double download it replaced.
  - A Call still queued for enhancement is clamped to the same 30 s the proxied response uses, because the worker is about to point that row at a *different object*.
  - Expiry is tracked on the **wall clock**, not a monotonic `Instant`: SigV4 measures its own life in wall time, and an `Instant` frozen across a machine suspend would wake believing dead signatures were live.
  - The margin is also the whole tolerance for **clock skew against the store**, which judges the signature by its own clock. A minute is generous for two machines with any time sync, but it is an assumption rather than an invariant.
  - The cache is capped at 1024 entries, evicting whatever expires soonest. Expired entries are pruned on the way past, so in practice it holds only what has been served in the last five minutes.
  - Objects written *before* #31 carry no `Cache-Control`, so a pre-existing archive keeps revalidating until those Calls age out of retention. New Calls are correct from the first one.
  - A cached redirect outlives a retention delete by up to its `max-age`: a client that fetches it in that window gets a 404 from the store rather than from Radio-Scout. Bounded and accepted — nothing invalidates the cache on delete.
- **It stops at the loaded page.** `selectNextCall` only sees the Calls the archive screen has loaded, so the last result on a page — the transition that costs the most, since it needs a search *and* a cold audio fetch — is the one not warmed. Open: #32.

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

## Implementation notes (#16)

Web Push shipped in #16 — the third tier of the target behavior above, and the
one that ends a lull the keep-alive's five-minute budget could not. Five
sub-decisions, all reversible:

- **The `web-push` crate named above was rejected, and the RFCs implemented
  directly.** `web-push` 0.11 depends on `ece` 2.3, whose *only* backend is
  OpenSSL — there is no pure-Rust option in that crate — which would have taken
  `openssl-sys` (and a C toolchain per target) into a project whose CI
  cross-builds `aarch64-unknown-linux-gnu` and `x86_64-pc-windows-msvc` from a
  plain `cargo build`, and whose whole distribution story is one static binary
  ([ADR-0007](0007-single-binary-embedded-frontend-distribution.md)). So
  `src/webpush.rs` implements RFC 8291 (aes128gcm) and RFC 8292 (VAPID) over
  RustCrypto — `p256`, `hkdf`, `aes-gcm` — in about 200 lines, **pinned to RFC
  8291 §5's own worked example**: the salt and the ephemeral key are parameters
  precisely so the standard's expected ciphertext is reproduced byte for byte
  rather than round-tripped against our own decrypt.
- **Notifications ride the live-feed fanout, not the ingest path.** A recorder's
  upload gets its `200` without waiting on a push service on the other side of
  the internet, and a Call reaches a notification through the same broadcast
  that reaches a socket — so the two cannot disagree about what a listener
  hears. The Selection algebra moved to `src/selection.rs` for the same reason:
  one rule, three consumers (socket, push, client).
- **Nothing is pushed to a listener who is demonstrably listening.** The `sub`
  frame carries the subscription's Id; while that socket is open the sender
  skips it, and #9's heartbeat reaps a phone iOS suspended within a period — so
  notifications take over at almost exactly the moment listening stopped. This
  is the piece that makes the feature usable rather than annoying, and it has no
  rdio equivalent, because rdio has no push and no heartbeat.
- **Coalescing is leading-edge, and counts what it swallows.** The
  first Call of a quiet Talkgroup notifies at once (a scanner is only useful if
  it is prompt); everything inside the window is counted into the *next*
  notification, so a lull ending with twelve Calls says twelve. No timers — a
  suppressed Call needs no wake-up of its own, which matters on a Pi. The RFC
  8030 `Topic` header is the second half: a phone that was off for an hour wakes
  to one notification per Talkgroup rather than a queue, because the push
  service itself replaced the undelivered ones.
- **The identity must survive a restart, so it is a file, not memory.** A
  browser pins the VAPID public key when it subscribes; a server that generated
  a new one each boot would silently orphan every existing subscription. So
  `RADIO_SCOUT_VAPID_PRIVATE_KEY` joins the ingest key and the admin password in
  `.env` (`0600`, only the path and the *public* half ever logged) — and an
  identity that cannot be saved, or cannot be parsed, leaves notifications
  **off** with an ERROR rather than running on one that will not outlive the
  process.

Two platform rules shaped the client half. `Notification.requestPermission()` is
spent once per origin, so it is spent on a tap on the Settings switch and
nowhere else — asked on load it is refused, and on iOS the only way back is
deleting the home-screen app. And **every push must show a notification**: iOS
revokes a subscription that receives one and shows nothing, so a payload the
worker cannot read still becomes a generic notification (`lib/pushMessage.ts`).

The worker itself is now ours (`src/sw.ts`, `injectManifest`) rather than
generated: a generated worker cannot have a `push` handler at all. Everything in
it with a decision lives in `lib/pushMessage.ts` at 100% coverage; the glue is
proven by a Playwright spec that delivers a real push through CDP and asserts on
what the worker asks the platform to show.

Still owed, unchanged: the **real-device gate** (research §14), and Vitest
Browser Mode. Deep-linking a tap to the specific Call that woke the listener is
deliberately left open — the notification carries the Call id (`/?call=<id>`),
and the app currently uses the tap to resume the feed rather than to open one
Call.
