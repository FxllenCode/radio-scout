# Robust Background Audio for an Installed iOS PWA — Research

**Scope:** What is actually achievable on **current (2026) iOS Safari / installed home‑screen PWA** for Radio‑Scout's three product targets: (1) continuous call‑to‑call queue playback in the background with working lock‑screen transport; (2) an inaudible keep‑alive so playback survives quiet gaps between calls; (3) Web Push to re‑engage the user when the app is fully suspended.

**Date of research:** 2026‑07‑04. iOS in the field at this date is on the **iOS 26.x** train (caniuse shows iOS Safari 26.5 as current).

**Source policy:** Primary sources (Apple/WebKit, MDN, W3C, caniuse, web.dev) are used for every *capability* claim. Real‑world *behavior/gotchas* (which Apple does not document) are drawn from Apple Developer Forums, WebKit Bugzilla, GitHub issues, and dated developer field reports — these are explicitly labelled **[FIELD]** below and each is dated. Every source has a URL + date inline.

---

## 1. Executive summary

**The short version:** On a home‑screen‑installed iOS PWA in 2026 you *can* get (a) background + lock‑screen playback of HTML5 `<audio>` while audio is actively audible, (b) working lock‑screen transport controls and metadata via the Media Session API, and (c) Web Push to re‑engage a suspended app. What you **cannot** reliably get is *unattended queue advancement across a silent gap*: iOS suspends the page roughly **30 seconds after audible audio stops**, after which JavaScript no longer runs, so the queue can't advance itself and lock‑screen controls go dead until the app is foregrounded. The inaudible keep‑alive is the mechanism that fights this, and it is **real but fragile** — it must be prototyped per iOS point‑release, and it got measurably worse in the iOS 26 cycle.

**Recommended client architecture (confirmed correct for this app):**

- **HTML5 `<audio>`, a single reused element — NOT Web Audio.** Web Audio output is classified as "ambient" by iOS and is muted the instant the app leaves the foreground (Jer Noble, WebKit bug 198277). Only an HTML5 media element gets the "playback" audio session that survives backgrounding.
- **Set `navigator.audioSession.type = 'playback'`** (iOS Safari 16.4+, on by default) so the session is playback‑category: plays over the ringer/silent switch and does not behave as mixable ambient audio.
- **Media Session API** for lock‑screen transport + metadata + artwork + scrubber. `play`, `pause`, `previoustrack`, `nexttrack`, `seekbackward`, `seekforward`, `seekto` are the useful handlers on iOS.
- **Keep the single `<audio>` element continuously "playing."** Never let it reach a `paused`/`ended` idle state between calls — bridge every gap with an inaudible source so iOS keeps the page alive. This is the keep‑alive.
- **Service worker** owns: PWA app‑shell caching; the `push` handler (which **must** `showNotification()` on every push); and `notificationclick` (focus/open the app). The SW **cannot** play audio and **cannot** run silently.
- **Web Push (iOS 16.4+, home‑screen only)** is the *only* way to reach a fully‑suspended app. A push **cannot start or resume audio** — it can only show a notification the user taps to bring the app forward, where a user gesture may then start audio.

**The one honest caveat up front:** the keep‑alive is the load‑bearing, least‑documented, most‑regression‑prone piece. Plan for it to work *and* build the Web Push fallback so the product degrades gracefully to "we'll notify you" when iOS suspends anyway. See §7.

---

## 2. Recommended architecture (concrete)

```
                    ┌─────────────────────────────────────────────┐
                    │  Radio-Scout installed PWA (standalone)     │
                    │                                             │
  live feed (WS/SSE)│   ┌───────────────┐   Media Session API    │
  ───────────────────►  │ Queue manager │◄──────────────┐        │
                    │   └──────┬────────┘   lock-screen: │        │
                    │          │ set .src, play()        │ play/pause/
                    │   ┌──────▼────────┐                │ next/prev/seek
                    │   │ ONE <audio>   │────────────────┘        │
                    │   │ element       │  navigator.audioSession │
                    │   │ (reused)      │      .type='playback'   │
                    │   └──────┬────────┘                         │
                    │          │ on 'ended' → next call OR        │
                    │          │ bridge gap with inaudible source │
                    │   ┌──────▼────────┐                         │
                    │   │ KEEP-ALIVE    │ silent loop / MMS stream│
                    │   └───────────────┘                         │
                    │                                             │
                    │   Service Worker: push→showNotification,    │
                    │   notificationclick→focus, app-shell cache  │
                    └─────────────────────────────────────────────┘
                                        ▲
                    Web Push (VAPID, APNs *.push.apple.com)  │
  Rust/Axum backend ──────────────────────────────────────────┘
  (web-push crate) — fires ONLY when client is disconnected/suspended
                     and a watched talkgroup has new activity
```

**`<audio>` element strategy — single reused element (not per‑call).** Create exactly one `<audio>` at app start; for each call set `audio.src = callUrl; audio.play()`. On the element's `ended` event, pull the next call from the listening queue and repeat. A single element that transitions src→src keeps one continuous "playback" session; spawning a fresh element per call risks the session tearing down between calls and is the pattern that fails in the background (see the queue‑gap failures in §4, esp. audiobookshelf #2655).

**Queue advancement.** Two triggers: (1) the element's `ended` event fires the next call; (2) the Media Session `nexttrack`/`previoustrack` handlers let the user skip from the lock screen. Both paths run the *same* "load next call" function. Critically, **both require JS to be running** — which is only guaranteed while audio is audibly playing. This is why the keep‑alive matters: it is what keeps JS alive through the gap so `ended`→next actually fires in the background.

**Keep‑alive + Media Session + Web Push division of labor:**
- **Keep‑alive** = stay resident *while the user is listening* (short quiet gaps between calls).
- **Media Session** = give the resident session real OS transport controls + metadata.
- **Web Push** = re‑engage *after* iOS has suspended us (long idle, screen locked for minutes, app switched away and evicted).

---

## 3. Q1 — Background playback of HTML5 `<audio>` in an installed iOS PWA

**Does audible HTML5 audio continue when the standalone PWA is backgrounded / screen‑locked? — YES, since iOS 15.4.**
The historical bug where audio stopped the moment a *standalone* web app left the foreground (in‑tab Safari was fine) was **WebKit bug 198277**, reported May 2019 and **fixed in iOS 15.4** (shipped March 2022; the bug was closed as a dup of 232909 in May 2022 with the note "this was a change shipped in iOS 15.4"). ([bugs.webkit.org/show_bug.cgi?id=198277], read 2026‑07‑04). So on iOS 15.4+ a standalone PWA playing an HTML5 `<audio>` element keeps playing when you lock the screen or switch apps — *as long as the audio remains audible*.

**Web Audio is different and must be avoided for the audible path.** In that same bug, Apple's Jer Noble stated WebAudio output "is considered 'Ambient' audio from the system's perspective, and ambient audio is blocked by the system once the app producing it is no longer foreground." Jeremy Keith independently documented Web Audio being silenced / only playing when the ringer is on ([adactio.com/journal/19929], 2023‑02‑22). **Conclusion: use an HTML5 `<audio>` element for the calls, not Web Audio.**

**Does continuous playback keep the page from being suspended? — While audio is audibly playing, effectively yes. When audio stops, NO.** This is the crux. Field reports converge on a ~30‑second grace window:
- **[FIELD]** Apple Developer Forums thread 762582 "iOS Audio Lockscreen Problem in PWA" (opened Aug 2024, no Apple engineer response): lock‑screen play/pause work initially, but **after ~30 seconds paused in the background the controls stop responding**; only foregrounding the PWA restores it. Attributed to iOS suspending the PWA's execution. Wake Lock is *not* a fix (it keeps the screen on and is unsupported in Mobile Safari — WebKit bug 254545). ([developer.apple.com/forums/thread/762582], 2024).
- **[FIELD]** audiobookshelf issue #2655 "iOS Background audio stops at the end of each audio track (iOS 17+)": when a track ends while backgrounded, playback halts and the next track does **not** auto‑start despite auto‑play being on. ([github.com/advplyr/audiobookshelf/issues/2655], reported 2024‑02‑24). *This is exactly Radio‑Scout's between‑calls gap.*

**Timeline / behavior when audio stops:** audible playback continues in background indefinitely; on `pause`/`ended`/silence the page continues briefly (~30 s per the forum report) then iOS **suspends the process** — JS timers, event handlers, and Media Session handlers all stop firing until the app is foregrounded again. There is no documented API to prevent this (Wake Lock is unsupported on iOS Safari).

**Screen‑lock vs app‑switch:** both cause the same suspension pathway once audio stops; while audio is audibly playing, both keep it playing. Historically screen‑lock was slightly more forgiving than app‑switch, but the governing factor today is *audible audio present or not*, not which backgrounding gesture was used.

**standalone (installed) vs in‑tab Safari:** Pre‑15.4, in‑tab Safari kept background audio and standalone did not (the `display: minimal-ui` "open in Safari" trick was a known workaround — bug 198277). Post‑15.4 both play in the background. **You must use standalone anyway**, because Web Push (Q4) is *only* available to home‑screen standalone web apps.

**Current 2026 regression warning. [FIELD]** The iOS 26 cycle regressed PWA audio: audio often works only once after install (needs a restart/Safari‑data clear to recover), background audio stops when the screen locks, and "MP3 player PWAs won't advance tracks." Reported since the June 2025 beta; **"much, much better" but not fully restored to iOS 18 levels as of Jan 2026** in 26.1/26.2 developer testing. ([MacRumors: "iOS 26 Audio issues in PWA web apps…"], forums.macrumors.com/threads/…2466839, 2025‑06 → 2026‑01). **Implication:** background audio robustness is a moving target across point releases — it must be re‑verified on the actual current iOS on a real device, not assumed.

---

## 4. Q2 — Media Session API on iOS

**Supported since iOS Safari 15**, and it drives the lock screen. caniuse `mdn-api_navigator_mediasession` shows iOS Safari **"15 – 26.5: Supported"** ([caniuse.com/mdn-api_navigator_mediasession], read 2026‑07‑04). Early confirmation it reached the lock screen came with iOS 15 (John Spurlock, 2021‑06‑07: "Media Session API works on the lock screen in Safari on iOS 15" — [johnspurlock.com]).

**Which action handlers actually work on iOS:** `play`, `pause`, `previoustrack`, `nexttrack`, `seekbackward`, `seekforward`, and `seekto` are usable and render on the lock screen / Control Center. Custom skip offsets via `seekbackward`/`seekforward` work (David Bushell used them for 30‑second skips) ([dbushell.com/2023/03/20/ios-pwa-media-session-api/], 2023‑03‑20, updated through Oct 2024). The full action list (`play, pause, stop, seekbackward, seekforward, seekto, previoustrack, nexttrack, skipad, togglecamera, togglemicrophone, hangup, previousslide, nextslide`) is per MDN ([developer.mozilla.org/…/Media_Session_API], updated 2024‑08‑07), but iOS only surfaces the transport‑relevant ones. `stop` is not reliably rendered as a distinct control on the iOS lock screen — treat it as best‑effort. **Always wrap `setActionHandler` in try/catch** (unsupported actions throw) — MDN/web.dev.

**Lock‑screen metadata + artwork (`MediaMetadata`):** `title`, `artist`, `album`, and `artwork` display on the iOS lock screen / Control Center. Artwork was the historically flaky bit: images often failed to appear or came out pixelated; the practical fixes are to **supply small artwork (e.g. 96×96 / 128×128)** and multiple sizes. Bushell reported the artwork scaling issue resolved by **iOS 18** ([dbushell.com], updated Oct 2024). For Radio‑Scout, map: `title` = talkgroup/alias, `artist` = system, `album`/artwork = group/tag icon.

**`setPositionState`:** present but flagged "Limited availability" by MDN ([developer.mozilla.org/…/MediaSession/setPositionState], read 2026‑07‑04). On iOS it populates the lock‑screen scrubber/elapsed time. Call it after each metadata change and after seeks: `setPositionState({duration, playbackRate, position})`; reset with `null` between calls. Because calls are short, position state is low‑value but harmless — worth setting for polish, not worth blocking on.

**Does driving a *queue* (advancing tracks) via `nexttrack` work on the lock screen? — Yes, WITH the suspension caveat.** The `nexttrack`/`previoustrack` handlers fire your JS, which loads the next/previous call. This works fine while the session is resident (audio recently/actively playing). It **stops working once iOS has suspended the page** (~30 s after audio goes silent — see §3), because the handler is JS that can't run in a suspended process. So: lock‑screen skip works during active listening; it does not work to "wake" a long‑idle suspended app. That gap is Web Push's job, not Media Session's.

**Also available (iOS 16.4+): `navigator.audioSession.type`.** The AudioSession API is enabled by default on iOS Safari since 16.4 — caniuse `mdn-api_audiosession_type` shows **"16.4 – 26.5: Supported"** ([caniuse.com/mdn-api_audiosession_type], read 2026‑07‑04). Types: `auto`, `playback`, `transient`, `transient-solo`, `ambient`, `play-and-record` (W3C Audio Session explainer; whatpwacando.today/audiosession). Set `type = 'playback'` so calls behave as media playback (over the silent switch, not mixable ambient). Note: whatpwacando's live demo reported "not supported on your device" for some types on some hardware — treat non‑`playback` types as **must‑prototype**, but `playback` itself is the one we need and is broadly supported.

---

## 5. Q3 — Keep‑alive through gaps

**The goal:** keep the single `<audio>` session resident (JS running, page not suspended) during the quiet seconds/minutes between calls so the queue keeps advancing and lock‑screen controls stay live. The only lever iOS gives us is: *keep audible‑category media "playing."*

**Approaches and their iOS status:**

| Approach | Works on iOS backgrounded? | Notes |
|---|---|---|
| **Looping silent/near‑silent HTML5 `<audio>`** between calls (or the SAME element playing a silent buffer) | **Probably — fragile, must prototype** | Simplest. Keeps a "playback" session alive so the page isn't suspended. But iOS may still not treat a *truly* silent element as active media in all versions; iOS 26 regressed this. Use a genuine (very low‑level, inaudible‑but‑nonzero) looping asset rather than a digital‑silence file, and never let the element hit `paused`. |
| **Web Audio silent source** (oscillator/buffer at 0 gain) | **NO** | Web Audio is "ambient" and muted when backgrounded (Jer Noble, WebKit 198277; adactio 2023‑02‑22). Do not rely on it for keep‑alive. |
| **MediaSource / Managed Media Source — one never‑ending stream** (append call audio, append silence in the gaps, element never stops) | **Most robust in principle — must prototype; iPhone‑support caveat** | iPhone Safari only got **Managed Media Source (MMS)** in **Safari 17.1 / iOS 17.1** (Oct 2023); plain MSE was never on iPhone. MMS is backwards‑compatible with `MediaSource` but hands buffering control to the browser and **requires `disableRemotePlayback` or an AirPlay source alternative** to be usable ([webkit.org/blog/14735 Safari 17.1], 2023; radiantmediaplayer.com blog 2023). This makes "one continuous stream" viable on iOS 17.1+ and is the strongest keep‑alive, but it's more engineering and unproven for this exact use. |
| **Server‑side single continuous stream** (backend muxes calls + silence into one HLS/`<audio src>`; iOS only ever sees one uninterrupted playback) | **Most robust overall — architectural fallback** | iOS never sees a gap, so it never starts the suspension timer. Cost: server must mux/segment audio, adds latency, and per‑call Media Session metadata must be driven from stream timing rather than element `src` changes. See §7. |

**Have any of these been closed by Apple?** No blanket closure, but: the Web Audio route was effectively closed by the ambient‑audio rule (deliberate). Silent HTML5 keep‑alive has never been officially blessed *or* blocked — it lives in undocumented territory and **degraded in the iOS 26 cycle** ([MacRumors thread] 2025–2026). Apple's stated philosophy (battery) is *against* long silent background runtime, so treat any silent keep‑alive as tolerated‑but‑unsupported and subject to regression.

**Battery implications:** a continuously "playing" session with no page suspension prevents the OS power savings that suspension exists to deliver. Expect meaningfully higher battery drain during long quiet periods. This argues for keep‑alive **only while actively listening**, with a timeout that lets the app suspend (and hands off to Web Push) after a configurable idle period, rather than 24/7 keep‑alive.

**Honest fallback if keep‑alive is unreliable:** see §7. In short — either move to the single continuous server stream (robust, more work) or accept suspension and lean on Web Push to re‑engage.

---

## 6. Q4 — Web Push on iOS

**Requirements (all confirmed against WebKit/Apple primary sources):**

- **Must be installed to the Home Screen.** Web Push on iOS is *only* for home‑screen web apps whose manifest sets `display` to `standalone` or `fullscreen`. In‑tab Safari cannot subscribe. ([webkit.org/blog/13878 "Web Push for Web Apps on iOS and iPadOS"], 2023‑02‑16).
- **Minimum iOS 16.4** (March 2023). caniuse `push-api` shows iOS Safari **partial support 16.4 → current** ([caniuse.com/push-api], read 2026‑07‑04; [webkit.org/blog/13966 Safari 16.4], 2023‑03‑27). By early 2026, >95% of iPhones are on ≥16.4 ([webscraft.org PWA push 2026], 2026‑03‑12 / upd 2026‑06‑30).
- **Permission requires a user gesture.** `Notification.requestPermission()` must be called from a real tap (e.g. a "Enable alerts" button). Requests on page load are silently blocked ([webkit.org/blog/13878], 2023; [webscraft.org], 2026).
- **Every push MUST show a user‑visible notification — no silent push.** This is the hard rule for iOS/WebKit. From "Meet Web Push": *"It also requires you set the `userVisibleOnly` flag to true, and fulfill that promise by always showing a notification… The Web Push API is not an invitation for silent background runtime… Violations of the `userVisibleOnly` promise will result in a push subscription being revoked."* ([webkit.org/blog/12945 "Meet Web Push"], 2022‑06‑07). Field guidance is even blunter: if the service worker's `push` handler does not call `showNotification()` (inside `event.waitUntil`), iOS treats it as a silent push and **cancels the subscription** ([webscraft.org], 2026). So the SW `push` handler must *always* show a notification.
- **A push cannot start or resume audio in the background.** Same primary rule ("not an invitation for silent background runtime"). The realistic flow is: push → notification → **user taps** → app foregrounds → the tap is a user gesture that can then start `<audio>`. Whether the `notificationclick`→`clients.openWindow`/`focus` path alone provides a gesture strong enough to autoplay is **not documented and must be prototyped**; assume you may need to show a "Tap to resume" affordance on foreground.
- **Reliability / throttling. [FIELD]** iOS PWA push delivery is roughly **70–85%** (vs ~90–95% on Android), and subscriptions can silently disappear after prolonged inactivity or repeated silent‑push violations ([webscraft.org], 2026). Plan to re‑subscribe on every app launch and re‑sync the subscription to the server.
- **iOS 18.4+ (March 2025): Declarative Web Push.** A simpler model where the push payload itself declares the notification and **no service worker `push` handler is strictly required** ([webkit.org/blog/16574 Safari 18.4], 2025‑03‑31). Worth adopting as the primary path (with the classic SW `push` handler as fallback for 16.4–18.3), since it sidesteps the "forgot to call showNotification → revoked" foot‑gun.
- **Badging API** (`setAppBadge`/`clearAppBadge`) works for home‑screen web apps since iOS 16.4 ([webkit.org/blog/13878], 2023) — use it to show unread watched‑talkgroup activity count.

**Server side (Rust):** use the **`web-push` crate** (pimeys/rust-web-push, current v0.11.0) with VAPID ([crates.io/crates/web-push]; [github.com/pimeys/rust-web-push], read 2026‑07‑04). Flow: generate a VAPID EC P‑256 keypair (`openssl ecparam -genkey -name prime256v1`); store each client's `PushSubscription` (`endpoint`, `keys.p256dh`, `keys.auth`) in the DB keyed to the browser/selection; build `SubscriptionInfo` + `VapidSignatureBuilder` from the PEM key; send the encrypted payload via the crate's client. **Egress to `*.push.apple.com` must be allowed** (Apple's APNs endpoints back Web Push) ([webkit.org/blog/13878], 2023). Handle `410 Gone`/`404` responses by deleting the dead subscription.

**End‑to‑end flow — "new call on a watched talkgroup → push → tap → resume":**
1. User installs the PWA to the Home Screen, taps "Enable alerts," grants permission; client `pushManager.subscribe({userVisibleOnly:true, applicationServerKey: <VAPID pub>})`; POST the `PushSubscription` + the user's watched‑talkgroup selection to the Rust backend.
2. Backend ingests a new call. If a client that watches that talkgroup is **not currently connected** (its live‑feed WS/SSE is gone → app suspended/closed), the backend fires a VAPID Web Push to that subscription with the call's talkgroup, system, and a resume URL/tag.
3. iOS wakes the service worker; the `push` handler **must** `self.registration.showNotification(...)` (and `setAppBadge`) inside `event.waitUntil` — otherwise the subscription is revoked.
4. User taps the notification → `notificationclick` → `clients.openWindow('/')` or focus existing → app foregrounds. The user's tap provides the gesture; the app reconnects the live feed, rebuilds the listening queue, and starts playback (show an explicit resume control if autoplay is refused).

---

## 7. Fallback plan if keep‑alive fails on iOS

Because the inaudible keep‑alive is undocumented and regression‑prone (it degraded in iOS 26), design so the product still works when it fails. In priority order:

**A. Single continuous server‑side stream (the robust re‑architecture).** The Rust backend produces **one uninterrupted audio stream per listener session** — it concatenates each watched call and inserts real (inaudible‑but‑present) silence during gaps, exposed as a single HLS or chunked `<audio src>`. iOS never sees the element stop, so it never starts the suspension timer; background playback "just works" like a live radio stream. Trade‑offs: server must mux/segment (CPU on the Pi — mind the performance constraint), added end‑to‑end latency, and Media Session metadata must be driven from stream cue points rather than per‑`src` changes. This is the strongest option and the recommended fallback if the pure‑client keep‑alive can't be made reliable on the current iOS. (Client‑side **Managed Media Source** — §5 — is the middle‑ground variant: one MMS stream you append to, iOS 17.1+, with `disableRemotePlayback`.)

**B. Accept suspension; degrade to Web Push re‑engagement (the graceful‑degradation option).** Keep the client keep‑alive *only while actively listening*; after a configurable idle timeout, stop fighting iOS and let the app suspend. When new watched‑talkgroup activity arrives, the backend sends a Web Push; the user taps to return and resume. This is the honest "we can't hold the session open forever on iOS" contract and costs almost no extra engineering beyond Web Push (which you're building anyway). It does **not** deliver truly uninterrupted background listening across long silences — it delivers "you won't miss activity, tap to jump back in."

**C. Combination (recommended product behavior):** keep‑alive for continuous listening during active sessions and short gaps → if iOS suspends despite it (detectable: the app resumes and finds it was backgrounded/evicted) → Web Push covers the fully‑suspended window. Offer option A (continuous server stream) as a "reliable background mode" toggle for users who prioritize gapless background listening over latency/battery.

---

## 8. Known‑good vs must‑prototype

| Capability | Status on installed iOS PWA (2026) | Since | Confidence / source |
|---|---|---|---|
| Audible HTML5 `<audio>` continues when backgrounded/locked (standalone) | **Known‑good** | iOS 15.4 | High — WebKit bug 198277 (fixed 15.4) |
| Use HTML5 `<audio>` not Web Audio (Web Audio muted in background) | **Known‑good (rule)** | — | High — Jer Noble, WebKit 198277; adactio 2023 |
| `navigator.audioSession.type = 'playback'` | **Known‑good** | iOS 16.4 | High — caniuse `mdn-api_audiosession_type` |
| Media Session lock‑screen transport (`play/pause/next/prev/seek`) | **Known‑good** | iOS 15 | High — caniuse; dbushell 2023; Spurlock 2021 |
| Media Session metadata + artwork on lock screen | **Known‑good** (use small artwork sizes) | iOS 15 (artwork solid by 18) | High — dbushell (upd 2024) |
| `setPositionState` (scrubber) | **Known‑good, low‑value** | — | Med — MDN "limited availability" |
| Advance queue via `<audio>` `ended` + `nexttrack` **while session resident** | **Known‑good** | iOS 15.4 | High |
| Advance queue in background **across a silent gap** (the keep‑alive) | **MUST PROTOTYPE** | — | Low — Apple forum 762582 (~30 s suspend), audiobookshelf #2655, MacRumors iOS 26 |
| Silent looping `<audio>` keeps page alive through gap | **MUST PROTOTYPE** (fragile; regressed in iOS 26) | — | Low |
| Managed Media Source single continuous stream, client‑side | **MUST PROTOTYPE** | iOS 17.1 | Med — WebKit Safari 17.1; needs `disableRemotePlayback` |
| Web Push to home‑screen PWA | **Known‑good** | iOS 16.4 | High — WebKit 13878 |
| Every push must `showNotification()`; no silent push; else revoked | **Known‑good (hard rule)** | iOS 16.4 | High — "Meet Web Push" |
| Push starts/resumes audio in background | **Known‑impossible** | — | High — "Meet Web Push" |
| `notificationclick`→openWindow provides gesture to autoplay audio | **MUST PROTOTYPE** | — | Low — undocumented |
| Declarative Web Push (no SW handler) | **Known‑good** | iOS 18.4 | High — WebKit 16574 |
| Wake Lock API (to prevent suspension) | **Not available on iOS** | — | High — WebKit bug 254545; Apple forum 762582 |
| Badging API for unread count | **Known‑good** | iOS 16.4 | High — WebKit 13878 |

---

## 9. Sources (with dates)

Primary — Apple/WebKit/W3C/MDN/caniuse:
- WebKit bug 198277 "Audio stops playing when standalone web app is no longer in foreground" (2019–2022; fixed iOS 15.4) — https://bugs.webkit.org/show_bug.cgi?id=198277
- WebKit "Meet Web Push" (2022‑06‑07) — https://webkit.org/blog/12945/meet-web-push/
- WebKit "Web Push for Web Apps on iOS and iPadOS" (2023‑02‑16) — https://webkit.org/blog/13878/web-push-for-web-apps-on-ios-and-ipados/
- WebKit "Features in Safari 16.4" (2023‑03‑27) — https://webkit.org/blog/13966/webkit-features-in-safari-16-4/
- WebKit "Features in Safari 17.1" (Managed Media Source on iPhone, 2023) — https://webkit.org/blog/14735/webkit-features-in-safari-17-1/
- WebKit "Features in Safari 18.4" (Declarative Web Push, 2025‑03‑31) — https://webkit.org/blog/16574/webkit-features-in-safari-18-4/
- MDN Media Session API (upd 2024‑08‑07) — https://developer.mozilla.org/en-US/docs/Web/API/Media_Session_API
- MDN MediaSession.setPositionState — https://developer.mozilla.org/en-US/docs/Web/API/MediaSession/setPositionState
- MDN PushManager.subscribe — https://developer.mozilla.org/en-US/docs/Web/API/PushManager/subscribe
- web.dev "Customize media notifications… Media Session API" (upd 2024‑06‑10) — https://web.dev/articles/media-session
- caniuse: mediaSession — https://caniuse.com/mdn-api_navigator_mediasession ; audioSession type — https://caniuse.com/mdn-api_audiosession_type ; push-api — https://caniuse.com/push-api (all read 2026‑07‑04)
- W3C Audio Session explainer — https://github.com/w3c/audio-session/blob/main/explainer.md
- Apple Developer doc "Sending web push notifications in web apps and browsers" (title only; body is behind a JS shell and could not be fetched — corroborated by WebKit blogs above) — https://developer.apple.com/documentation/usernotifications/sending-web-push-notifications-in-web-apps-and-browsers

Field reports (dated, labelled [FIELD] in text):
- Apple Developer Forums 762582 "iOS Audio Lockscreen Problem in PWA" (~30 s suspend; Wake Lock unsupported) (2024) — https://developer.apple.com/forums/thread/762582
- audiobookshelf #2655 "Background audio stops at end of each track (iOS 17+)" (2024‑02‑24) — https://github.com/advplyr/audiobookshelf/issues/2655
- MacRumors "iOS 26 Audio issues in PWA web apps…" (2025‑06 → 2026‑01) — https://forums.macrumors.com/threads/…2466839
- David Bushell "iOS Web Apps and Media Session API" (2023‑03‑20, upd 2024) — https://dbushell.com/2023/03/20/ios-pwa-media-session-api/
- John Spurlock (Media Session on iOS 15 lock screen) (2021‑06‑07) — https://johnspurlock.com/
- Jeremy Keith "Web Audio API update on iOS" (2023‑02‑22) — https://adactio.com/journal/19929
- Prototyp "What we learned about PWAs and audio playback" (upd 2026‑05‑16; original iOS 12–13 era) — https://prototyp.digital/blog/what-we-learned-about-pwas-and-audio-playback
- webscraft "PWA Push Notifications on iOS in 2026" (2026‑03‑12 / upd 2026‑06‑30) — https://webscraft.org/blog/…?lang=en
- whatpwacando.today AudioSession demo — https://whatpwacando.today/audiosession/
- Managed Media Source overview (Radiant Media Player, 2023) — https://www.radiantmediaplayer.com/blog/at-last-safari-17.1-now-brings-the-new-managed-media-source-api-to-iphone.html

Server (Rust):
- web-push crate (v0.11.0) — https://crates.io/crates/web-push ; https://github.com/pimeys/rust-web-push (read 2026‑07‑04)
