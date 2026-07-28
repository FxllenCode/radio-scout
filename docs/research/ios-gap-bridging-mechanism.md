# Bridging the Silent Gap on iOS — Keep‑Alive vs Managed Media Source

**Scope:** Radio‑Scout is an installed iOS home‑screen PWA that plays a queue of short Calls (seconds each) arriving irregularly from a live WebSocket feed. iOS suspends the page shortly after audible audio stops, so the queue cannot self‑advance across a quiet gap. This document picks the gap‑bridging mechanism for [ADR‑0005](../adr/0005-client-audio-media-session-background.md) §"gap‑bridging is prototype‑decided", between:

- **(a) single reused `<audio>` + inaudible keep‑alive** — the one element ADR‑0005 mandates plays an inaudible‑but‑nonzero looping asset whenever the listening queue is empty, so it never reaches `paused`/`ended`; a real Call swaps into `src` when one arrives.
- **(b) Managed Media Source (iOS 17.1+)** — one never‑ending MMS‑backed stream the app appends each Call to, with silence appended into the gaps, so iOS only ever sees one uninterrupted playback.

**Date of research:** 2026‑07‑26. iOS in the field is on the **iOS 26.x** train; caniuse's current column is **iOS Safari 26.5** (StatCounter data month: June 2026, read 2026‑07‑26).

**Builds on:** [`ios-background-audio.md`](ios-background-audio.md) (2026‑07‑04). That file's conclusions are assumed, not restated — HTML5 `<audio>` not WebAudio, `audioSession.type = 'playback'`, Media Session drives the lock screen, ~30 s suspension after audio goes silent, Web Push rules. This file only reports what is **new, changed, or newly sourced** since then.

**Source policy:** identical to the prior file. Primary sources (WebKit source + Bugzilla + blog, MDN, W3C, caniuse, Apple docs) for every capability claim. Real‑world behavior Apple does not document is labelled **[FIELD]** and dated. Every claim carries a URL and a date read. Where I could not verify something, it says **unverified**.

**New in this file:** the prior research treated "iOS keeps the page alive while audio is audible" as an observed black box. It is not a black box — the decision is made by ~20 lines of WebKit that are public, and reading them settles Q2, Q3, and Q6 outright. §2 is that mechanism; everything after it is downstream.

---

## 1. Executive summary

**Recommendation: (a), the inaudible keep‑alive on the single `<audio>` element — with (b) demoted from "the robust option" to a fallback rung it probably never reaches.** The prior research ranked MMS as "most robust in principle." Reading WebKit's actual source inverts that ranking, for three reasons:

1. **MMS and the keep‑alive hold the page alive by the *same* mechanism, and MMS adds nothing to it.** What stops iOS suspending the WebContent process is a RunningBoard assertion named `com.apple.webkit.MediaPlayback`, taken when any page reports `MediaProducerMediaState::IsPlayingAudio`. That flag is computed by `HTMLMediaElement::computeCanProduceAudio()` from four things: the element is playing, it has an audio track, it is not `muted`, and `volume` is not 0. MMS's own contribution to media state is a *different* flag (`HasStreamingActivity`) that buys a **media‑networking** activity token — not the playback assertion. So MMS does not keep the page alive any better than a looping silent file does; it only removes the `ended`/`src`‑swap moment.
2. **MMS cannot ingest Radio‑Scout's audio.** WebKit has exactly two `SourceBufferParser` implementations: `SourceBufferParserAVFObjC` (delegating to AVFoundation's `AVStreamDataParser`, i.e. **fragmented MP4**) and `SourceBufferParserWebM` (`audio/webm`/`video/webm` only). There is no MPEG‑audio byte‑stream parser in the tree, so `audio/mpeg` and `audio/aac` are not appendable. ADR‑0002's per‑Call MP3 and M4A files are both unusable as‑is: M4A needs a container remux to fMP4, and **MP3 needs a full transcode**, because Safari's MSE rejects MP3‑in‑MP4 (`mp4a.40.34`). On a Raspberry Pi, per‑Call transcoding is the cost we spent all of ADR‑0002 avoiding.
3. **MMS is built for VOD look‑ahead and actively punishes a low‑latency live feed.** `ManagedMediaSource::setStreaming(true)` arms a one‑shot timer of `managedMediaSourceHighThreshold` seconds (default **30**). If the app has not buffered 30 s ahead of the playhead within 30 wall‑clock seconds, `streamingTimerFired()` latches `m_streamingAllowed = false` — and nothing in WebKit ever sets it back to true for the life of that `ManagedMediaSource`. A scanner feed that must play a Call *the moment it lands* cannot buffer 30 s ahead without putting every new Call 30 s in the future. It is a direct latency‑vs‑policy conflict with no comfortable middle.

Two further facts push the same way. `disableRemotePlayback` is **mandatory** for MMS on iOS (`defaultManagedMediaSourceNeedsAirPlay()` returns `true` on `PLATFORM(IOS_FAMILY)`), and the escape hatch — an "AirPlay source alternative" — requires a *second, real, playable URL for the same content*, which for a per‑listener live queue does not exist unless we first build the server‑side continuous stream that MMS was supposed to let us avoid. And per‑Call Media Session metadata over one continuous MSE‑backed element has an **open, unanswered field report against it** (icecast‑metadata‑js #193, open since 2023‑11‑17: `mediaSession.metadata` works with a plain `<audio>` tag and does not work through their MediaSource playback path).

**The honest counterweight** — (a) is still fighting a WebKit bug Apple has never fixed. **WebKit bug 261858**, "autoplay in audio element and media session controls not working in standalone web app (pwa) when playback ends," filed 2023‑09‑20 against iOS 16/17, is still **NEW / unresolved** (read 2026‑07‑26), with one comment: a Radar import. That is the bug the keep‑alive works around, and it has sat untouched for nearly three years. The keep‑alive is a workaround for an unowned bug, not a supported path. Which is why §12 is a ladder, not a decision.

---

## 2. The mechanism, from source: what actually keeps the page alive

This section is the load‑bearing one; §3–§9 are consequences of it. All file paths are `github.com/WebKit/WebKit`, branch `main`, read **2026‑07‑26**.

**Step 1 — an element decides whether it "can produce audio."**
`Source/WebCore/html/HTMLMediaElement.cpp`, `computeCanProduceAudio()`:

```cpp
    if (isSuspended())
        return false;

    if (!volume())
        return false;

#if !USE(GSTREAMER)
    if (muted())
        return false;
#endif

    if (m_player && m_readyState >= HAVE_METADATA)
        return hasAudio();

    return hasEverHadAudio();
```

**Step 2 — that becomes a media‑state flag, gated on actually playing.**
Same file, `mediaState()`:

```cpp
    if (!isPlaying())
        return state;

    if (canProduceAudio())
        state.add(MediaProducerMediaState::IsPlayingAudio);
```

**Step 3 — that flag takes an OS process assertion.**
`Source/WebKit/UIProcess/WebProcessProxy.cpp`, `updateAudibleMediaAssertions()`:

```cpp
    if (hasAudibleWebPage) {
        WEBPROCESSPROXY_RELEASE_LOG(ProcessSuspension, "updateAudibleMediaAssertions: Taking MediaPlayback assertion for WebProcess");
        m_audibleMediaActivity = AudibleMediaActivity {
            ProcessAssertion::create(*this, "WebKit Media Playback"_s, ProcessAssertionType::MediaPlayback),
            protect(processPool())->webProcessWithAudibleMediaToken()
        };
    } else {
        WEBPROCESSPROXY_RELEASE_LOG(ProcessSuspension, "updateAudibleMediaAssertions: Releasing MediaPlayback assertion for WebProcess");
        m_audibleMediaActivity = std::nullopt;
    }
```

`ProcessAssertionType::MediaPlayback` maps to RunningBoard assertion name `"MediaPlayback"` in domain `"com.apple.webkit"` (`Source/WebKit/UIProcess/Cocoa/ProcessAssertionCocoa.mm`, `runningBoardAssertionNameForAssertionType` / `runningBoardDomainForAssertionType`).

**Step 4 — the same flag also exempts the process from background CPU termination.**
Same file, `didExceedCPULimit()`:

```cpp
        if (page->isPlayingAudio()) {
            WEBPROCESSPROXY_RELEASE_LOG(PerformanceLogging, "didExceedCPULimit: WebProcess has exceeded the background CPU limit but we are not terminating it because there is audio playing");
            return;
        }
```

**What this settles:**

- The gate is **binary and structural**, not acoustic. Nothing in the chain inspects a sample value, an RMS level, or a loudness meter. §8 (Q6) falls straight out of this.
- `muted = true` and `volume = 0` are the **two** things that provably release the assertion. Never use either as a keep‑alive.
- MMS contributes a *different* flag. `Source/WebCore/html/HTMLMediaElement.cpp`, `mediaState()`:
  ```cpp
  #if ENABLE(MEDIA_SOURCE)
      if (RefPtr mediaSource = m_mediaSource; mediaSource && mediaSource->isStreamingContent())
          state.add(MediaProducerMediaState::HasStreamingActivity);
  #endif
  ```
  and `HasStreamingActivity` is consumed only by `WebProcessProxy::updateMediaStreamingActivity()`, which takes `webProcessWithMediaStreamingToken()` — logged as *"Start Media Networking Activity for WebProcess"*. It is a networking activity token, **not** a `ProcessAssertion`. MMS buys permission to keep fetching; it does not buy staying alive.

So: **(a) and (b) hold the page open through the identical `IsPlayingAudio` → `MediaPlayback` path.** Everything that follows is about their *differences*, and none of those differences is "MMS is more likely to keep JS running."

---

## 3. Q1 — Current iOS 26.x behavior and regressions

**The iOS 26 PWA‑audio regression has a WebKit fix, shipped in Safari 26.2 (2025‑12‑12).** WebKit's release notes for Safari 26.2, under Home Screen Web Apps → Resolved Issues: *"Fixed an issue where an audio element failed to play when re-opening a Home Screen Web App."* (radar 155336513) — [webkit.org/blog/17640](https://webkit.org/blog/17640/webkit-features-for-safari-26-2/), published 2025‑12‑12, read 2026‑07‑26. That is a precise match for the regression the prior research flagged ("audio often works only once after install").

**[FIELD] The community thread the prior research cited stops at 26.2 and never reports 26.3/26.4/26.5.** MacRumors thread 2466839, read 2026‑07‑26: opened 2025‑09‑20 (rubikwizard); "hasn't been fixed" on 26.0.1 (2025‑09‑29); "much, much better in iOS 26.1" (2025‑11‑07); a developer‑program user on 26.2 reporting "while I can still trigger it sometimes… it's much more rare" (2025‑11‑12); "Still some issues in 26.2. Particularly when backgrounding an app there is sometimes a noise like the last sound is being held" (2025‑12‑13); last post 2026‑01‑28, non‑technical. **There is no dated field report in that thread for 26.3, 26.4, or 26.5.** Absence of complaints is weak evidence of a fix — read it as "the loudest symptom got fixed and the thread went quiet," not "PWA audio is healthy."

**26.3, 26.4, 26.5 release notes contain nothing for background audio or Home Screen Web Apps, but two 26.4 fixes matter to us:**

- *"Fixed an issue where changing an `HTMLMediaElement` volume from `0` to `0` did not activate the audio session."* (161691743) — [webkit.org/blog/17862](https://webkit.org/blog/17862/webkit-features-for-safari-26-4/), 2026‑03‑24. Independent confirmation that **volume participates in audio‑session activation**, corroborating `computeCanProduceAudio()`'s `if (!volume()) return false;`.
- *"Fixed an issue where the `ended` event for Media Source Extensions might never fire."* (165430052), same post. An MSE lifecycle bug that survived until **March 2026** — a data point on how much production traffic Safari's MSE audio path actually sees.

Safari 26.5 ([webkit.org/blog/17938](https://webkit.org/blog/17938/webkit-features-for-safari-26-5/), 2026‑05‑11, read 2026‑07‑26): media fixes are control‑bar positioning, `MediaCapabilities.decodingInfo()` `spatialRendering`, and four WebRTC items. **Nothing** for Home Screen Web Apps or background audio.

**The bug that owns our actual failure mode is still open.** [WebKit bug 261858](https://bugs.webkit.org/show_bug.cgi?id=261858), "[iOS 16.x & iOS 17.x] autoplay in audio element and media session controls not working in standalone web app (pwa) when playback ends," reported 2023‑09‑20 by "Aaron," last modified 2023‑09‑27, **status NEW, no resolution, one comment (a Radar import, rdar://problem/116156954)**. Read 2026‑07‑26. Nothing in the 26.x notes claims to fix it.

**Also unfixed, and older:** [Apple Developer Forums 706499](https://developer.apple.com/forums/thread/706499) — "Safari Audio player doesn't play next track of the audio playlist when iPhone screen locked," iPhone 8 Plus / iOS 15.4.1, filed May 2022, **0 replies** (read 2026‑07‑26). The same failure in plain Safari, four years old, never answered.

**[FIELD] The 26.x train broke background media in other engines too.** [firefox-ios #30640](https://github.com/mozilla-mobile/firefox-ios/issues/30640) — "iOS 26.1: Firefox stops all audio/video playback instantly when not in foreground" (read 2026‑07‑26). Since iOS browsers are all WebKit, this reads as an OS‑level backgrounding change in the 26 train that hit more than Safari's own PWA path.

**Net for both mechanisms:** iOS 26.x is *recovering*, not recovered, and the specific WebKit bug that breaks queue advancement at `ended` is untouched since 2023. Neither (a) nor (b) is protected by a fix; both must be validated on the device in front of us (§13).

---

## 4. Q2 — Does MMS's buffer management defeat the keep‑alive purpose?

**Short answer: it does not "defeat" it, because MMS was never providing it (§2) — but MMS's buffer policy actively fights a live, low‑latency queue in three separate ways.**

### 4a. `streaming` is a data‑demand signal, not a liveness guarantee

The spec is explicit that `streaming` is advisory to the *application*: *"When `true`, the user agent needs more data to ensure uninterrupted playback. When `false`, the user agent has enough data buffered and the application can stop fetching new segments."* ([MDN `ManagedMediaSource.streaming`](https://developer.mozilla.org/en-US/docs/Web/API/ManagedMediaSource/streaming), last modified 2026‑03‑23, read 2026‑07‑26). Nothing in MDN, the W3C MSE draft ([w3c.github.io/media-source](https://w3c.github.io/media-source/), Editor's Draft 2025‑11‑04, read 2026‑07‑26), or the WebKit implementation promises anything about background or suspended execution. `streaming` is about bytes, not about process lifetime.

### 4b. The monitoring algorithm, verbatim, and its thresholds

`Source/WebCore/Modules/mediasource/ManagedMediaSource.cpp` (read 2026‑07‑26):

```cpp
void ManagedMediaSource::monitorSourceBuffers()
{
    …
    if (!activeSourceBuffers()->length()) {
        setStreaming(true);
        return;
    }
    …
    if (!m_streaming) {
        PlatformTimeRanges neededBufferedRange { currentTime, std::max(currentTime, limitAhead(*m_lowThreshold)) };
        if (!msp->isBuffered(neededBufferedRange))
            setStreaming(true);
        return;
    }

    if (auto ahead = limitAhead(*m_highThreshold); currentTime < ahead) {
        if (msp->isBuffered({ currentTime,  ahead }))
            setStreaming(false);
    } else
        setStreaming(false);
}
```

The thresholds come from prefs — `Source/WTF/Scripts/Preferences/UnifiedWebPreferences.yaml`, read 2026‑07‑26:

```yaml
ManagedMediaSourceHighThreshold:
  type: double
  defaultValue:
    WebKit:
      default: 30
ManagedMediaSourceLowThreshold:
  type: double
  defaultValue:
    WebKit:
      default: 10
```

So: `endstreaming` when ≥30 s is buffered ahead of the playhead; `startstreaming` again when <10 s is buffered ahead.

### 4c. The one‑way policy latch — the finding that decides this question

`setStreaming(true)` **arms a timer using the same 30 as a wall‑clock duration**:

```cpp
    if (streaming) {
        scheduleEvent(eventNames().startstreamingEvent);
        if (m_streamingAllowed) {
            ensurePrefsRead();
            Seconds delay { *m_highThreshold };
            m_streamingTimer.startOneShot(delay);
        }
    } else {
        if (m_streamingTimer.isActive())
            m_streamingTimer.stop();
        scheduleEvent(eventNames().endstreamingEvent);
    }
```

and if that timer ever fires:

```cpp
void ManagedMediaSource::streamingTimerFired()
{
    ALWAYS_LOG(LOGIDENTIFIER, "Disabling streaming due to policy ", *m_highThreshold);
    m_streamingAllowed = false;
    if (auto* msp = mediaSourcePrivate())
        msp->setStreamingAllowed(false);
    notifyElementUpdateMediaState();
}
```

`m_streamingAllowed` is initialised `true` (`ManagedMediaSource.h`) and **is assigned `false` in exactly one place and `true` in none**. It is a one‑way latch for the lifetime of the `ManagedMediaSource`.

What it latches off: `Source/WebCore/Modules/mediasource/MediaSourceInterfaceMainThread.cpp` —

```cpp
bool MediaSourceInterfaceMainThread::isStreamingContent() const
{
    if (RefPtr managedMediasource = dynamicDowncast<ManagedMediaSource>(m_mediaSource))
        return managedMediasource && managedMediasource->streamingAllowed() && managedMediasource->streaming();
    …
}
```

— which is the `HasStreamingActivity` flag from §2, i.e. the media‑networking activity token. **Once latched, the MMS element never again reports streaming activity to the process throttler.**

**Why this is fatal for a scanner queue.** The latch fires when the app stays in `streaming = true` for 30 continuous seconds — i.e. whenever it *deliberately keeps a shallow buffer*. Radio‑Scout's whole product promise is that a Call plays when it lands. The two ways out both cost something we are not willing to pay:

- **Buffer 30 s of silence ahead** so `streaming` goes false and the timer is cancelled. Now every newly arrived Call sits 30 s in the future. To play it *now* you must `SourceBuffer.remove(currentTime + ε, Infinity)` and re‑append — which drops the buffer below the low threshold, re‑arms `streaming = true`, restarts the 30 s timer, and requires JS to run at exactly the moment we are least sure JS is running. Every Call becomes a buffer‑surgery operation.
- **Keep ~1–2 s buffered** for latency. Then `streaming` is true essentially forever, the timer fires at 30 s, and the latch closes permanently.

### 4d. Eviction is a real, additional hazard

`ManagedSourceBuffer` exists precisely so the UA can evict: *"the user agent can evict content from `ManagedSourceBuffer` objects at any time due to memory or hardware limitations"*, signalled by `bufferedchange` ([MDN `ManagedSourceBuffer`](https://developer.mozilla.org/en-US/docs/Web/API/ManagedSourceBuffer), read 2026‑07‑26; [MDN `ManagedMediaSource`](https://developer.mozilla.org/en-US/docs/Web/API/ManagedMediaSource), last modified 2026‑03‑23). Eviction ahead of the playhead while the page is suspended is unrecoverable without JS. **[FIELD]** hls.js — by far the most‑deployed MMS consumer — treats `bufferedchange` as a logging/repair signal and calls `hls.pauseBuffering()` / `hls.resumeBuffering()` on `endstreaming` / `startstreaming` (`src/controller/buffer-controller.ts`, read 2026‑07‑26). Its whole model assumes JS is running to respond. Ours cannot.

**Answer to Q2:** MMS's buffer management does not defeat a keep‑alive *purpose* it never served. But `streaming` guarantees nothing about background or suspended behavior; the 30 s / 10 s thresholds are tuned for VOD look‑ahead; and the one‑way `streamingAllowed` latch makes shallow‑buffer live playback a policy violation by construction. MMS does buy one real thing: **already‑decoded, already‑buffered audio keeps playing while the WebContent process is suspended** (decode is not in that process), so a deep buffer is genuinely a few extra seconds of survival. That is the only advantage, and it is bought at the price of everything in §4c.

---

## 5. Q3 — Does MMS require `disableRemotePlayback`, and what does that cost?

**Yes, on iOS it is required, and this is in the source, not just the blog.**

WebKit's Safari 17.1 announcement said it plainly: *"Note that support for Managed Media Source is only available when an AirPlay source alternative is present, or remote playback is explicitly disabled."* ([webkit.org/blog/14735](https://webkit.org/blog/14735/webkit-features-in-safari-17-1/), 2023‑10‑25, read 2026‑07‑26). The gate is `HTMLMediaElement::deferredMediaSourceOpenCanProgress()` (read 2026‑07‑26):

```cpp
bool HTMLMediaElement::deferredMediaSourceOpenCanProgress() const
{
#if !ENABLE(WIRELESS_PLAYBACK_TARGET)
    return true;
#else
    return !document().settings().managedMediaSourceNeedsAirPlay()
        || isWirelessPlaybackTargetDisabled()
        || hasWirelessPlaybackTargetAlternative();
#endif
}
```

and the setting defaults to **true on iOS** — `Source/WebKit/Shared/WebPreferencesDefaultValues.cpp`:

```cpp
bool defaultManagedMediaSourceNeedsAirPlay()
{
#if PLATFORM(IOS_FAMILY) || PLATFORM(MAC)
    return true;
#else
    return false;
#endif
}
```

Fail the gate and `sourceopen` simply never fires (MDN states the same consequence).

### What `disableRemotePlayback` actually turns off

`isWirelessPlaybackTargetDisabledChanged()` (read 2026‑07‑26) sets one boolean from any of three attributes:

```cpp
    bool disabled = equalLettersIgnoringASCIICase(attributeWithoutSynchronization(HTMLNames::webkitairplayAttr), "deny"_s)
        || hasAttributeWithoutSynchronization(HTMLNames::webkitwirelessvideoplaybackdisabledAttr)
        || hasAttributeWithoutSynchronization(HTMLNames::disableremoteplaybackAttr);
```

Every consumer of that boolean in `HTMLMediaElement.cpp` is in the **wireless‑playback‑target (AirPlay) subsystem**: route‑availability listeners (`hasEnabledTargetAvailabilityListeners`), the `webkitplaybacktargetavailabilitychanged` event, `showPlaybackTargetPicker()`, `wirelessRoutesAvailableDidChange()`, and the `RequiresPlaybackTargetMonitoring` / `ExternalDeviceAutoPlayCandidate` media‑state bits.

**It touches nothing in the Now Playing / Media Session path.** Lock‑screen transport, Control Center, Bluetooth AVRCP metadata, and CarPlay's Now Playing screen are all fed from the Now Playing info centre, driven by the Media Session API and the element's playback state — an entirely separate subsystem from wireless playback targets. **So the cost of `disableRemotePlayback` is AirPlay, and only AirPlay.** Bluetooth (A2DP/AVRCP — how AirPods, car head units and speakers actually connect) and CarPlay are audio *routes*, not remote playback *targets*, and are unaffected.

I could not find a WebKit or Apple statement that says this in one sentence, so treat "Bluetooth/CarPlay/lock‑screen survive `disableRemotePlayback`" as **strongly implied by the call‑site audit, not documented** — it is on the §13 test plan (Step 6) for that reason.

### The AirPlay "source alternative" escape hatch, and why it does not help us

```cpp
bool HTMLMediaElement::hasWirelessPlaybackTargetAlternative() const
{
    if (m_loadState != LoadingFromSourceElement)
        return false;
    for (Ref source : childrenOfType<HTMLSourceElement>(*this)) {
        auto mediaURL = source->getNonEmptyURLAttribute(srcAttr);
        bool maybeSuitable = !mediaURL.isEmpty();
#if ENABLE(MEDIA_SOURCE)
        maybeSuitable &= !mediaURL.protocolIs(mediaSourceBlobProtocol);
#endif
        if (!maybeSuitable || !isSafeToLoadURL(mediaURL, InvalidURLAction::DoNothing, false))
            continue;

        return true;
    }
    return false;
}
```

Three requirements: the element must load via `<source>` **children** (not a `src` attribute); at least one `<source>` must not be the MediaSource `blob:` URL; and that URL must be loadable. The pattern is exactly the one WebKit documents in *"How to use Media Source Extensions with AirPlay"* (Jean‑Yves Avenard & Jon Davis, [webkit.org/blog/15036](https://webkit.org/blog/15036/how-to-use-media-source-extensions-with-airplay/), 2024‑02‑16, read 2026‑07‑26): MediaSource object URL as the first `<source>`, an AirPlay‑compatible HLS URL as the second, because *"MMS/MSE uses binary blobs appended to a SourceBuffer [so] it won't work with AirPlay"* — AirPlay needs a single shareable URL.

**For Radio‑Scout that second URL does not exist.** A per‑listener queue assembled live from a WebSocket feed has no single shareable HLS URL — building one *is* fallback rung C (§12), the server‑side continuous stream. So with MMS we would either lose AirPlay or first build the thing MMS was meant to save us from.

**[FIELD] The real world just accepts the AirPlay loss.** hls.js unconditionally forces it whenever MMS is in play (`src/controller/buffer-controller.ts`, read 2026‑07‑26):

```ts
media.disableRemotePlayback =
  media.disableRemotePlayback || (MMS && ms instanceof MMS);
```

([hls.js #6197](https://github.com/video-dev/hls.js/issues/6197), opened 2024‑02‑07, closed — the confusion there was a stale code comment claiming the opposite; the shipped behavior is `true`.)

**Answer to Q3:** yes, required on iOS; it kills AirPlay only; Bluetooth/CarPlay/lock‑screen/Media Session are untouched (audited, not documented); the `<source>`‑alternative path exists but needs a real second URL we do not have. **Mechanism (a) needs none of this and keeps AirPlay for free.**

---

## 6. Q4 — Per‑Call Media Session metadata over one continuous MMS stream

**The spec says yes, explicitly, and it names our exact use case.** W3C Media Session (Editor's Draft **2026‑06‑05**, read 2026‑07‑26): *"Whenever the active media session changes or setting `metadata` of the active media session, the user agent MUST run the update metadata algorithm"*, and the non‑normative example is precisely one element playing many logical items: *"For playlists or chapters of an audio book, multiple media elements can share a single media session… the metadata must be updated to reflect what is currently playing."* So `navigator.mediaSession.metadata = new MediaMetadata(...)` mid‑playback is a spec‑supported operation, independent of `src` changes.

**But there is an open, unanswered field report against it for exactly the MSE case.** **[FIELD]** [icecast-metadata-js #193](https://github.com/eshaz/icecast-metadata-js/issues/193), "navigator.mediaSession.metadata not working in iOS," opened **2023‑11‑17** by blantonl, **still open, no maintainer response** (read 2026‑07‑26). The reporter's framing is the important part: *"Setting this property provides metadata to the OS to allow iOS to display a user control interface in iOS on the lock screen when a feed is streaming"* — and the same code **works with a standard HTML `<audio>` tag** and fails through icecast‑metadata‑player. That library's primary playback method is **MediaSource** (with WebAudio and HTML5 as fallbacks — [icecast-metadata-player README](https://github.com/eshaz/icecast-metadata-js/tree/master/src/icecast-metadata-player), read 2026‑07‑26). This is the closest thing to a controlled comparison available: same app, same metadata code, plain `<audio>` works, MediaSource path does not. It is one report, unconfirmed by a second party — **treat the root cause as unverified**, but note that the only public evidence points the wrong way for mechanism (b).

**[FIELD] Repainting the lock screen is not free even on the plain path.** `cboin1996/songbirdweb`'s architecture notes (`docs/STATE.md`, committed 2026‑04‑29, repo last pushed 2026‑07‑10, read 2026‑07‑26) document two behaviors from a shipped iOS PWA music player:

- *"The position state is **re-asserted on every silent-loop `timeupdate`** because iOS otherwise polls `audio.currentTime` for the playhead and the lock-screen position drifts back to 0:00."* — iOS trusts the **element's** clock over `setPositionState()` unless you keep overwriting it.
- *"…re-binds `previoustrack`/`nexttrack` because **iOS forgets them across suspension** and falls back to ±10s seek markers."*

Both are directly relevant to Radio‑Scout: with MMS the element's `currentTime` is a monotonic stream clock that has *no relationship* to the current Call's position, so every lock‑screen scrubber value would have to be synthesised and continuously re‑asserted against an element clock that disagrees. With (a) the element clock *is* the Call clock, and `setPositionState` only has to be corrected during the silent loop.

**Answer to Q4:** spec‑legal and explicitly contemplated; **known‑good on a plain `<audio>` element** (our shipped `setNowPlaying` already does it per Call — `client/src/lib/mediaSession.ts`); **unverified and with one open negative field report over MSE**, plus a structurally harder scrubber story. Mechanism (b) turns a solved problem back into an open one.

---

## 7. Q5 — Codec/container constraints for MMS append

**This is where (b) stops being an engineering trade‑off and becomes a rebuild.**

### What WebKit's MSE can parse, from source

`Source/WebCore/platform/graphics/cocoa/SourceBufferParser.cpp` (read 2026‑07‑26) — there are exactly two parsers, and support is their union:

```cpp
MediaPlayerEnums::SupportsType SourceBufferParser::isContentTypeSupported(const ContentType& type)
{
    MediaPlayerEnums::SupportsType supports = SourceBufferParserWebM::isContentTypeSupported(type);
    if (supports == MediaPlayerEnums::SupportsType::IsSupported)
        return supports;
    return std::max(supports, SourceBufferParserAVFObjC::isContentTypeSupported(type));
}
```

- `SourceBufferParserWebM::isContentTypeSupported` accepts **only** `audio/webm` and `video/webm` (and only if a Vorbis/Opus/VP8/VP9 decoder is available) — anything else returns `IsNotSupported` immediately.
- `SourceBufferParserAVFObjC::isContentTypeSupported` defers entirely to `AVStreamDataParserMIMETypeCache::canDecodeType`, whose cache is populated from `[AVStreamDataParser audiovisualMIMETypes]` (`AVStreamDataParserMIMETypeCache.mm`, read 2026‑07‑26) — i.e. AVFoundation's fragmented‑MP4 stream parser. The ISO BMFF pre‑parser is only constructed for `video/mp4` and `audio/mp4` (`SourceBufferParserAVFObjC.mm`, `makePreParserIfNeeded`).

**There is no MPEG‑audio byte‑stream parser anywhere in WebKit.** The W3C registry does define one — the MSE Byte Stream Format Registry (2026‑06‑04, read 2026‑07‑26) lists WebM, ISO BMFF, MPEG‑2 TS, and **MPEG Audio (`audio/mpeg`, `audio/aac`)**, and the [MPEG Audio Byte Stream Format](https://www.w3.org/TR/mse-byte-stream-format-mpeg-audio/) spec (2024‑07‑23) is beautifully suited to us (*"Every MPEG Audio Frame header is an initialization segment"*, *"Every MPEG Audio Frame is a random access point"* — i.e. concatenate MP3 frames and append). **Safari does not implement it.** Chrome does; that is where the temptation comes from.

**So on iOS Safari: `MediaSource.isTypeSupported('audio/mpeg')` and `'audio/aac'` are expected `false`; `'audio/mp4; codecs="mp4a.40.2"'` (AAC‑LC in fMP4) is the appendable target.** Verify both on device (§13 Step 1) before spending anything on (b) — the parser audit is source‑derived, but AVFoundation's runtime `audiovisualMIMETypes` list is not something I can read from here, so the exact strings are **unverified until probed**.

> ### ⚠️ CORRECTION — measured on device 2026‑07‑28, iPhone 16 Pro / iOS 26.5.2 ([#40](https://github.com/FxllenCode/radio-scout/issues/40))
>
> **The prediction above is wrong, exactly where this paragraph said it was unverified.** The probe found:
>
> | | `MediaSource` | `ManagedMediaSource` |
> |---|---|---|
> | `audio/mpeg` | absent | **`true`** — predicted `false` |
> | `audio/aac` | absent | **`true`** — predicted `false` |
> | `audio/mp4; codecs="mp4a.40.2"` | absent | `true` — as predicted |
> | `audio/mp4; codecs="mp4a.40.34"` | absent | `false` — as predicted |
>
> And it is not merely an optimistic capability hint. A real `SourceBuffer` of type
> `audio/mpeg` was **constructed**, **took 16 680 bytes of real MPEG‑1 Layer III frames**,
> and reported `buffered` = **0.000–1.045 s** — the same duration CoreAudio and Chromium
> independently decode from those bytes. **MP3 is directly appendable on iOS 26.5.2.**
>
> Two things follow:
>
> 1. **`MediaSource` does not exist on iPhone at all** — only `ManagedMediaSource`. The
>    source read above was about `SourceBufferParser`, which is presumably still accurate;
>    what it could not see is AVFoundation's runtime type list, and that is what answers.
>    Any probe must test both constructors and distinguish "no constructor" from
>    "unsupported codec".
> 2. **The MP3 row of the cost table below is void.** It says MP3 needs "a full decode +
>    re‑encode of every Call, on the Pi". The real cost is **zero** — append the bytes.
>    This matters concretely: **SDRTrunk encodes every Call as MP3**
>    (`BroadcastFormat.MP3`, `RdioScannerConfiguration.java:48`).
>
> **What is *not* revived:** the M4A row still stands — a plain `.m4a` is still not a valid
> MSE byte stream and still needs a per‑Call remux. And **WAV**, which this table never
> listed, is what Trunk Recorder commonly sends *and* what enhancement writes
> (`Output::Wav`, 8 kHz) — it is not appendable either. So (b) is **re‑costed, not
> rehabilitated**: free for MP3 Calls, unchanged for everything else.

### What this costs Radio‑Scout per Call

ADR‑0002 serves per‑Call **MP3** and **M4A/AAC** files as they arrive from the recorder.

| Our format | Appendable to a SourceBuffer? | What it would take |
|---|---|---|
| **M4A / AAC (non‑fragmented ISO BMFF)** | **No.** MSE needs an initialization segment (`ftyp`+`moov` with `mvex`) followed by media segments (`moof`+`mdat`). A plain `.m4a` is `ftyp`+`moov`+`mdat` with a static sample table — a valid MP4 file, not a valid MSE byte stream. | **Remux to fMP4.** No re‑encode: rewrite the container, re‑index the samples. Cheap‑ish, but new server code (or a WASM remuxer in the client) on every Call. |
| **MP3** | **No**, and worse: there is no legal repackaging. `audio/mpeg` has no parser, and **MP3‑in‑MP4 is rejected by Safari's MSE**: **[FIELD]** [hls.js #6125](https://github.com/video-dev/hls.js/issues/6125) — `MediaSource.isTypeSupported('audio/mp4;codecs="mp4a.40.34"')` returns **`false`** on Safari 17.2.1 / macOS 14.2.1, with the player logging *"One or more CODECS in variant not supported"* for `CODECS: mp4a.40.34`; fixed in hls.js 1.6.0 by *routing Safari away from that path*, not by making it work (read 2026‑07‑26). | **Transcode MP3 → AAC**, then remux to fMP4. A full decode + re‑encode of every Call, on the Pi. |

**Stated plainly, as asked:** on a Raspberry‑Pi‑class server, mechanism (b) means **remuxing every M4A Call and transcoding every MP3 Call** — per Call, forever, for one client platform. That is the exact cost ADR‑0002 was written to avoid, and it is a hard "first‑class performance" violation under CLAUDE.md. (It could be moved to the client as a WASM remux/transcode — that just relocates the CPU onto the phone whose battery §9 is about, and only works for the remux half; MP3→AAC in WASM on an iPhone, per Call, in the background, is not a serious proposal.)

`SourceBuffer.changeType()` **is** available in WebKit (`SourceBuffer.idl` gates it on `SourceBufferChangeTypeEnabled`, default `true` for non‑GStreamer ports; read 2026‑07‑26), so a single SourceBuffer could in principle switch codecs mid‑stream — but that only matters once every Call is already in an appendable container, which is the expensive part.

### Can silence be appended cheaply?

**Yes — this is the one easy part of (b).** A pre‑encoded fMP4 AAC silence segment is a **static asset**: encode ~1 s of digital silence once at build time, then append the same bytes repeatedly, advancing `SourceBuffer.timestampOffset` each time. Zero server CPU, zero network, a few KB resident. AAC's fixed 1024‑sample frames make the arithmetic exact. So the gap filler is free; it is the *Calls* that are unaffordable.

**Answer to Q5:** raw per‑Call MP3/M4A **cannot** be appended. iOS Safari's MSE accepts fMP4 (via `AVStreamDataParser`) and WebM only. M4A needs a per‑Call remux; MP3 needs a per‑Call transcode because Safari rejects MP3‑in‑MP4. Silence is free. This alone disqualifies (b) as a *first* choice for a Pi‑hosted scanner.

---

## 8. Q6 — Truly‑silent vs near‑silent (nonzero amplitude) buffers

**At the WebKit layer the answer is definitive and it is "amplitude is irrelevant."** §2's chain — `computeCanProduceAudio()` → `IsPlayingAudio` → `MediaPlayback` process assertion — inspects `isSuspended()`, `volume()`, `muted()`, `hasAudio()`/`hasEverHadAudio()`, and `isPlaying()`. **No sample data is examined at any point.** A WAV of digital zeroes at `volume = 1`, unmuted, playing, is indistinguishable from a symphony as far as the assertion is concerned.

**The two things that *do* kill it, from the same source:**

- **`muted = true`** → `computeCanProduceAudio()` returns `false`. This has been deliberate since [WebKit bug 140524](https://bugs.webkit.org/show_bug.cgi?id=140524) (Ada Chan, reported 2015‑01‑15, **RESOLVED FIXED** 2015‑01‑19, r178655; read 2026‑07‑26), filed precisely so *"websites that use a muted video to display an animated background"* would not be treated as audible. Muting is the documented way to tell WebKit you are not producing audio — the opposite of what a keep‑alive wants.
- **`volume = 0`** → `if (!volume()) return false;`. Corroborated independently by the Safari 26.4 fix *"Fixed an issue where changing an `HTMLMediaElement` volume from `0` to `0` did not activate the audio session"* (161691743, 2026‑03‑24).

**So the rule for Radio‑Scout's keep‑alive asset is:** unmuted, `volume > 0`, `loop`, a real audio track, never `paused`. Attenuate by *encoding* a low amplitude into the asset, **never** by `volume` or `muted`.

**Below WebKit, this is unverified.** iOS's own audio arbitration (RunningBoard, `mediaserverd`, AVAudioSession) could in principle notice a silent stream and behave differently, and I found **no** WebKit source, Bugzilla entry, Apple document, or dated field report either asserting or refuting amplitude sensitivity at that layer. The prior research's instinct — *"use a genuine (very low‑level, inaudible‑but‑nonzero) looping asset rather than a digital‑silence file"* — is therefore **unproven but free**. Ship a nonzero asset: at ‑90 dBFS or a ±1‑LSB dither in 16‑bit it is inaudible on any hardware, it is one static file either way, and it removes an entire class of "maybe iOS special‑cases pure silence" from the failure surface. (§13 Step 5 tests silence vs near‑silence head‑to‑head so this stops being a guess.)

**[FIELD] What a shipped app does:** songbirdweb (`docs/STATE.md`, 2026‑04‑29, read 2026‑07‑26) swaps `audio.src` to *"a 4 KB looping `silence.mp3`"* rather than pausing, because *"iOS Safari releases the audio session ~10s after a normal `audio.pause()` in the background, after which the lock-screen play button becomes unresponsive until the user reopens Safari."* Note two things: they report **~10 s** for audio‑session release after `pause()` (the prior research's number was ~30 s to full page suspension — these are different clocks: audio session first, then process), and they report *"After ~20s in the background iOS sometimes suspends the page, killing the silent loop"* — i.e. **the silent loop is not a perfect shield.** They handle that by detecting it on `visibilitychange` and resetting to a tappable state. That defensive pattern is a requirement for us too, not an optional nicety (§13 Step 4, and it is what Web Push exists for).

---

## 9. Q7 — CPU/battery cost on a phone

**Mechanism (a): the cost is the assertion, not the work.** Decoding a looping few‑KB near‑silent file is trivial CPU — the media path is hardware/`mediaserverd`, and the WebContent process does nothing per sample. The real cost is structural: holding `com.apple.webkit.MediaPlayback` prevents the process suspension that exists to save power, and it also **exempts us from background CPU‑limit termination** (`WebProcessProxy::didExceedCPULimit()`, §2 Step 4) — meaning nothing will police us if we get sloppy. Apple's position on the whole category is on the record: WebKit shipped MMS because MSE *"uses a lot of power, which can be especially painful on mobile devices with smaller batteries"* ([webkit.org/blog/14735](https://webkit.org/blog/14735/webkit-features-in-safari-17-1/), 2023‑10‑25). A keep‑alive is the same bargain in the other direction: we are asking the OS not to sleep.

**Mechanism (b): strictly more.** Everything (a) costs — the same assertion is held by the same playing element — **plus** JS wake‑ups on `startstreaming`/`endstreaming`/`bufferedchange`, plus network fetches, plus `appendBuffer` parsing on the main thread or a worker, plus (if we go the client‑remux route) container rewriting per Call, plus the `remove()`+re‑append churn from §4c on every arriving Call. MMS's power story is about *replacing a JS buffering loop that runs constantly* with a UA‑managed one; we would be adding a JS buffering loop where none existed.

**Evidence quality: weak, and I am saying so.** I found **no dated, quantified measurement** of iOS PWA background‑audio battery drain — not for silent keep‑alive, not for MMS. The closest field data is anecdotal and unquantified (Loopy Pro forum thread 28014 on iOS background‑audio standby drain — one user citing "30% of the battery" with no timeframe or device; read 2026‑07‑26), which is not usable. **Treat all battery numbers as unverified.** What *is* primary‑sourced is the direction: (a) ≤ (b) in every component, and both prevent suspension by design.

**Product consequence, unchanged from the prior research and reinforced:** run the keep‑alive **only while the user is actively listening**, with an idle timeout after which we stop fighting iOS and hand off to Web Push. A 24/7 keep‑alive is a battery complaint waiting to be filed, and it is the one thing that would make Radio‑Scout *worse* than rdio on a phone.

---

## 10. Q8 — What real projects actually ship in 2026

Prioritising source code and issue trackers over blog posts, as instructed. All read 2026‑07‑26.

| Project | What it does about this | Evidence |
|---|---|---|
| **hls.js** (the reference MMS consumer) | Uses MMS on Safari, **unconditionally forces `disableRemotePlayback = true`** when MMS is active, loads via `<source>` children, and maps `endstreaming`→`pauseBuffering()` / `startstreaming`→`resumeBuffering()`. Its model assumes JS is alive to respond. | `src/controller/buffer-controller.ts`; [#6197](https://github.com/video-dev/hls.js/issues/6197) (2024‑02‑07, closed); [#6125](https://github.com/video-dev/hls.js/issues/6125) (Safari rejects `mp4a.40.34`, fixed in 1.6.0 by avoiding the path) |
| **songbirdweb** (`cboin1996/songbirdweb`) — self‑hosted music PWA, actively developed | **Ships mechanism (a), on one element.** `pause()` swaps `audio.src` to *"a 4 KB looping `silence.mp3`"* instead of pausing; `resume()` swaps the song back and seeks. Documents three gotchas: `setPositionState` must be re‑asserted **on every silent‑loop `timeupdate`** or the lock screen drifts to 0:00; `playbackRate` must be 1 (0 throws); iOS **forgets `previoustrack`/`nexttrack` across suspension** and they must be re‑bound on `visibilitychange`; and *"after ~20s in the background iOS sometimes suspends the page, killing the silent loop"*, handled by resetting the UI to a tappable state. | `docs/STATE.md` (committed 2026‑04‑29; repo pushed 2026‑07‑10) |
| **audiobookshelf** (web client) | **Never solved it.** [#2655](https://github.com/advplyr/audiobookshelf/issues/2655) "iOS Background audio stops at the end of each audio track (iOS 17+)" — opened 2024‑02‑24, **still open**, 18 comments, last activity 2025‑05‑18. Maintainer (advplyr, 2024‑02‑26): *"I'm not sure if there is anything we can do about this since it is likely a browser issue."* Reporters confirmed it in Safari, Brave, and Orion, both in‑tab and as a PWA. The thread's final resolution is a user recommending **a native iOS client (Plappa)** instead. | issue + comments via GitHub API |
| **Jellyfin web** | Same failure, closed unresolved. [#6113](https://github.com/jellyfin/jellyfin-web/issues/6113) "iOS (Safari): background audio playback broken" (reported 2024‑09‑24, iOS 18, Jellyfin 10.9.9) — **closed as not planned / stale**. [#5425](https://github.com/jellyfin/jellyfin-web/issues/5425): on iOS backgrounded, *pause* works but *resume* and *track change* do not — the §2 signature exactly (pause is an OS→element command; resume/next need our JS). | issue trackers |
| **icecast‑metadata‑js** (internet‑radio player, MediaSource‑first) | Per‑track metadata over a continuous MediaSource stream: [#193](https://github.com/eshaz/icecast-metadata-js/issues/193) — `mediaSession.metadata` **works with a plain `<audio>` tag, does not work through their player**. Open, unanswered, since 2023‑11‑17. | issue |
| **rdio‑scanner** (the thing we are replacing) | Ships a **native iOS app** ([App Store id1563065667](https://apps.apple.com/us/app/rdio-scanner/id1563065667)), marketed on *"seamless background audio"* and a Now Playing widget toggle. It did not solve this on the web; it left the web. | App Store listing |

**The pattern is unambiguous.** Nobody ships MMS for gap‑bridging — MMS's entire deployed footprint is adaptive **video** streaming (hls.js, dash.js, Bitmovin, rx‑player), where a 30 s look‑ahead buffer is the goal rather than the enemy. Every project that tried to solve *our* problem either (i) shipped the silent‑source swap on one element (songbirdweb), (ii) gave up and told users to install a native app (audiobookshelf, rdio‑scanner), or (iii) closed the issue as not‑planned (Jellyfin). Radio‑Scout's stated differentiator is being the one that does *not* have to say "install our native app" — which means (a), done carefully, plus an honest Web Push floor.

---

## 11. Known‑good vs must‑prototype

| Claim | Status | Source class |
|---|---|---|
| Page stays alive via `IsPlayingAudio` → `com.apple.webkit.MediaPlayback` process assertion | **Known‑good** | WebKit source (`HTMLMediaElement.cpp`, `WebProcessProxy.cpp`, `ProcessAssertionCocoa.mm`) |
| Assertion ignores sample amplitude entirely | **Known‑good** | WebKit source — `computeCanProduceAudio()` reads no samples |
| `muted = true` releases the assertion | **Known‑good** | WebKit source + bug 140524 (fixed 2015) |
| `volume = 0` releases the assertion | **Known‑good** | WebKit source + Safari 26.4 note 161691743 |
| Playing audio also exempts the process from background CPU‑limit termination | **Known‑good** | WebKit source — `didExceedCPULimit()` |
| MMS gives a *networking* token (`HasStreamingActivity`), not a playback assertion | **Known‑good** | WebKit source — `updateMediaStreamingActivity()` vs `updateAudibleMediaAssertions()` |
| MMS `streaming` thresholds are 10 s low / 30 s high | **Known‑good** | `UnifiedWebPreferences.yaml` |
| MMS latches `streamingAllowed = false` permanently after 30 s of continuous `streaming` | **Known‑good** | `ManagedMediaSource.cpp` — one‑way, never reset |
| MMS on iOS requires `disableRemotePlayback` or a non‑blob `<source>` alternative | **Known‑good** | `deferredMediaSourceOpenCanProgress()` + `defaultManagedMediaSourceNeedsAirPlay()` + WebKit blog 14735 |
| `disableRemotePlayback` affects the AirPlay subsystem only | **Known‑good (audited), not documented** | WebKit source call‑site audit — **§13 Step 6 confirms on device** |
| Bluetooth / CarPlay / lock‑screen survive `disableRemotePlayback` | **MUST PROTOTYPE** | Implied by the audit; no Apple statement found |
| iOS Safari MSE accepts fMP4 + WebM only; no `audio/mpeg` / `audio/aac` parser | **Known‑good** | `SourceBufferParser.cpp`, `SourceBufferParserWebM.cpp`, `SourceBufferParserAVFObjC.mm` |
| Exact `isTypeSupported` strings on the device (AVFoundation runtime list) | **MUST PROTOTYPE** | §13 Step 1 |
| MP3‑in‑MP4 (`mp4a.40.34`) rejected by Safari MSE | **Known‑good** | [FIELD] hls.js #6125 |
| Per‑Call `mediaSession.metadata` updates on a plain `<audio>` repaint the iOS lock screen | **MUST PROTOTYPE** (spec‑legal, already shipped in our code) | W3C Media Session ED 2026‑06‑05 + §13 Step 3 |
| Per‑Call metadata over an MSE‑backed element on iOS | **MUST PROTOTYPE — one open negative report** | [FIELD] icecast‑metadata‑js #193 |
| Silent‑source swap keeps the audio session + JS alive through a gap | **MUST PROTOTYPE** | [FIELD] songbirdweb (works, with caveats); WebKit bug 261858 still NEW |
| Truly‑silent vs near‑silent difference below the WebKit layer | **MUST PROTOTYPE / unverified either way** | no source found — §13 Step 5 |
| iOS forgets `previoustrack`/`nexttrack` across suspension; must re‑bind | **MUST PROTOTYPE** | [FIELD] songbirdweb |
| Battery cost of either mechanism, quantified | **Unverified** | no dated measurement found |
| iOS 26 PWA‑audio regression fixed | **Known‑good for the "plays only once after install" symptom** (Safari 26.2, 155336513); background/queue behavior **not** claimed fixed | WebKit release notes + [FIELD] MacRumors 2466839 |
| WebKit bug 261858 (PWA won't advance / MS controls dead at `ended`) | **Known‑open since 2023‑09‑20** | bugs.webkit.org |

---

## 12. Recommendation

**Ship (a): the inaudible keep‑alive on the single `<audio>` element, with a near‑silent (not digitally silent) looping asset, `loop`, unmuted, `volume` untouched — and treat Managed Media Source as a rung we escalate to only if (a) fails the device gate *and* we are willing to pay for per‑Call remuxing.**

The reasoning, in one paragraph you can argue with: the prior research called MMS "most robust in principle," and that intuition rested on an assumption the WebKit source falsifies — that a never‑ending MSE stream would hold the page open more firmly than a looping file. It does not. Both mechanisms keep the WebContent process alive through the identical `IsPlayingAudio` → `MediaPlayback` assertion, and MMS's own state flag buys only a networking token; MMS's real and only advantage is that the element never transitions through `ended`, which is worth something because WebKit bug 261858 says `ended` is exactly where the PWA dies. But that single advantage arrives bundled with: a codec wall that makes every MP3 Call a Pi‑side transcode and every M4A Call a Pi‑side remux (§7), a one‑way 30‑second policy latch that a low‑latency live queue trips by construction (§4c), a mandatory `disableRemotePlayback` whose only escape hatch presupposes the server‑side stream MMS was meant to avoid (§6), a lock‑screen metadata story that regresses from "already shipped and working in `mediaSession.ts`" to "one open negative field report" (§6), and strictly higher phone CPU (§9). Meanwhile (a) is a static asset and about thirty lines in `CallPlayer.tsx`, costs the Pi nothing, keeps AirPlay, keeps the element clock equal to the Call clock, and is the mechanism the one comparable shipped 2026 PWA actually uses. Against a workaround for a three‑year‑old unowned WebKit bug, the correct engineering move is the *cheapest* workaround that might work, not the most elaborate one — because the expensive one has to be thrown away just as fast when Apple changes something, and MMS's throw‑away cost includes an audio pipeline.

**Concrete implementation shape** (for #15, not built here):

- One near‑silent looping asset, ~1 s, `loop`, served from the app shell. Encode the attenuation into the file; never touch `volume` or `muted`.
- Enter the keep‑alive **before** `ended`, not on it — swap `src` to the silence loop when the queue is empty and the current Call is within ~0.2 s of its end, so the element never reaches `ended`/`paused` and never re‑enters `HAVE_NOTHING` from an idle state. This is the difference between working around bug 261858 and stepping on it.
- While the loop runs: hold `mediaSession.playbackState`, re‑assert `setPositionState` on every `timeupdate` (songbirdweb's drift finding), and keep the last Call's `metadata` on screen rather than clearing it.
- Re‑bind all four Media Session action handlers on `visibilitychange` → visible (songbirdweb's "iOS forgets them" finding). Our `bindTransport` already returns a release function, so this is a re‑run, not new machinery.
- **Idle timeout.** Stop the keep‑alive after a configurable quiet period and let iOS suspend us; Web Push takes over (§9's battery argument, and ADR‑0005's guaranteed fallback).
- Detect the loss: on foreground, if the keep‑alive flag is set but `audio.paused` is true, we were suspended — reset to a tappable "resume" state rather than a stuck spinner.

---

## 13. Fallback ladder

Escalate in this order, only when the rung above fails the real‑device gate:

**A — Near‑silent keep‑alive on the single `<audio>` (the recommendation).** Static asset, no server work, keeps AirPlay. Fails if §14 Step 4 shows the queue does not advance across a gap with the screen locked.

**B — Keep‑alive + aggressive re‑entry.** Same mechanism, but treat suspension as expected: shorten the keep‑alive to the active‑listening window, re‑bind handlers and re‑assert position state on every visibility change, and make the resume path a single obvious tap. This is A plus the songbirdweb defensive pattern; it does not deliver unattended advancement across long silences, it delivers "you lose the minimum and recover in one tap."

**C — Server‑side continuous stream (the robust re‑architecture).** The Rust backend produces one uninterrupted per‑listener stream — Calls concatenated with near‑silent filler between — exposed as a single HLS or chunked `<audio src>`. iOS sees one playback that never stops, so the suspension clock never starts. This is the prior research's fallback A, and it is *above* MMS in this ladder now, not below it, because it (i) solves the same problem more completely, (ii) needs no `disableRemotePlayback` and therefore keeps AirPlay, (iii) is the thing that would *also* provide MMS's required AirPlay `<source>` alternative if we ever wanted MMS, and (iv) puts the muxing cost somewhere we can measure and cache. Costs: Pi CPU for muxing/segmentation, added latency, and per‑Call Media Session metadata must be driven from stream cue points via the live feed rather than from `src` changes. Ship it as an opt‑in "reliable background mode," not the default.

**D — Managed Media Source.** Only if A–C have all failed *and* someone has funded the per‑Call remux/transcode. Requires: an fMP4/AAC ingest path (so ADR‑0002 grows a normalisation step), `disableRemotePlayback` (AirPlay dies unless C already exists to be the `<source>` alternative), a buffer strategy that threads the §4c needle, and re‑proving Media Session metadata over MSE against icecast #193. If we are already building C to get the AirPlay alternative, C alone is strictly better — which is why in practice D is unreachable.

**E — Accept suspension; Web Push only.** The honest floor, already committed in ADR‑0005. Keep playing while the queue has audio; when iOS suspends us, a Web Push notification says a watched talkgroup is active and one tap resumes. This is not a failure state to hide — it is what audiobookshelf, Jellyfin and rdio‑scanner all ended up at, except they said "install our native app" instead. Saying "we'll ping you" is better.

---

## 14. Real‑device test plan (the manual gate checklist)

**Executable by one person with an iPhone and no debugger.** Everything is observed with eyes and ears.

**Setup (once):**
1. iPhone on the current iOS 26.x. Record the exact version (Settings → General → About → Software Version) — every result below is only valid for that build.
2. Build and run the embedded binary per `docs/agents/live-testing.md`, reachable at `http://<MAC-LAN-IP>:3000` on the phone's Wi‑Fi.
3. In Safari, open the app → Share → **Add to Home Screen**. Launch it **from the Home Screen icon**, never from Safari. (Every claim in this document is standalone‑only.)
4. Silent switch **off** (ringer on) for the first pass. Volume at a level where a normal Call is clearly audible.
5. Start the feeder with a long gap so gaps are the thing under test: `cargo run --example feed -- --interval 90s --seconds 6`.
6. Have a stopwatch (a second phone, or the Clock app on this one — note that leaving the app to use Clock *is* an app switch, so prefer a second device or a wall clock).

---

**Step 1 — Codec probe (decides whether (b) is even on the table).**
In Safari (a normal tab is fine for this one), open the app and paste into the URL bar a `javascript:` bookmarklet, or add a temporary debug button that `alert()`s the results of:
`MediaSource.isTypeSupported('audio/mpeg')`, `('audio/aac')`, `('audio/mp4; codecs="mp4a.40.2"')`, `('audio/mp4; codecs="mp4a.40.34"')`, and `typeof ManagedMediaSource`.

> **⚠️ Probe `ManagedMediaSource.isTypeSupported` too — it is the only one that exists on iPhone.** Corrected 2026‑07‑28 ([#40](https://github.com/FxllenCode/radio-scout/issues/40)): on iOS 26.5.2 `window.MediaSource` is `undefined`, so every call above throws or returns nothing, and reading that as `false` yields the **opposite** verdict to the truth — "no MSE, (b) impossible", when in fact all four types answer on `ManagedMediaSource`. Test both constructors and keep "no constructor" (`n/a`) distinct from "unsupported codec" (`false`). And treat `isTypeSupported` as a hint only: construct a `SourceBuffer` and `appendBuffer` real bytes before believing it.
> **Do not use `alert()` while the Claude browser extension is driving anything** (CLAUDE.md). Render the results into the page instead.
- **PASS for (b) being possible:** `audio/mp4; codecs="mp4a.40.2"` is `true` **and** `ManagedMediaSource` is `"function"`.
- **CONFIRMS §7:** `audio/mpeg` and `audio/aac` are `false`, and `mp4a.40.34` is `false`.
- **If `audio/mpeg` is `true`** — the parser audit is wrong for this iOS build, MP3 could be appended directly, and (b) deserves a second look. Record it and stop the gate here for re‑research.

---

**Step 2 — Baseline: audible background playback still works on this build.**
With a Call playing, press the side button to lock the screen.
- **PASS:** audio continues to the end of the Call, uninterrupted.
- **FAIL:** audio cuts on lock → this iOS build has regressed below iOS 15.4 behavior; nothing below this step is meaningful. Record the version and escalate — this is a §3‑class OS regression, not our bug.

---

**Step 3 — Lock‑screen metadata repaints per Call (Q4, mechanism (a) path).**
Foreground, let two Calls from **different talkgroups** play back to back (`--interval 2s` briefly). Then lock the screen and watch the lock screen across a talkgroup change.
- **PASS:** the title line changes to the new talkgroup, the artist line to the new system, and the artwork colour changes, **without touching the phone**.
- **PARTIAL:** text updates but artwork does not, or artwork is stale → note it; artwork is polish, text is the product.
- **FAIL:** nothing repaints until you unlock → per‑Call metadata is not reaching the lock screen; `setNowPlaying` needs work before any gap testing means anything.

---

**Step 4 — THE decisive test: does the queue advance across a silent gap while locked? (mechanism (a))**
With the keep‑alive build installed and `--interval 90s`:
1. Let one Call play to completion.
2. **Immediately** lock the screen. Do not touch the phone again.
3. Start the stopwatch. Wait for the next Call (≈90 s later).
- **PASS:** the next Call becomes audible from the locked phone with no interaction, and the lock screen shows its metadata. **This is the whole gate.**
- **PARTIAL PASS:** the Call plays but the lock screen still shows the previous Call's metadata → keep‑alive works, metadata refresh across suspension does not; ship A, file the metadata bug.
- **FAIL:** silence. Record **when** it died: wake the screen (do not unlock, do not open the app) and look at the lock‑screen player — if the transport buttons are present but dead, the page is suspended; if the player is gone entirely, the audio session was released.
4. Repeat **three times**. This behavior is reported as intermittent (songbirdweb: *"iOS sometimes suspends the page"*), so 1‑of‑3 is a fail, not a pass.
5. Repeat once more with the app **switched away** (Home gesture to another app) instead of screen‑locked. Both paths must pass.

---

**Step 5 — Truly‑silent vs near‑silent, head to head (Q6).**
Build two keep‑alive assets: `silence-zero.wav` (all sample values 0) and `silence-dither.wav` (±1 LSB / ≈‑90 dBFS tone). Expose a toggle in settings. Run **Step 4 three times with each**.
- **PASS for near‑silent being necessary:** dither passes ≥2/3 and zero passes ≤1/3.
- **PASS for it being irrelevant:** both pass 3/3 → §2 holds all the way down; ship whichever, prefer dither anyway (free insurance).
- Either way, record the result — this is the only way to settle a question no public source answers.

---

**Step 6 — `disableRemotePlayback` cost, ONLY if the ladder reaches rung D (Q3).**
On a build where the element carries `disableremoteplayback`:
1. Pair a **Bluetooth** speaker or AirPods. Play a Call.
   - **PASS:** audio routes to Bluetooth; the device's play/pause/next buttons control the queue; the head unit or Watch shows the talkgroup title.
2. Lock the screen. **PASS:** lock‑screen transport is present and functional.
3. Open Control Center and tap the AirPlay/output picker.
   - **EXPECTED:** system output routing still works (this is a route, not a remote playback target); the element does not offer to hand playback to an Apple TV / AirPlay 2 speaker.
   - **FAIL for the audit in §6:** lock‑screen controls disappear, or Bluetooth transport stops working → `disableRemotePlayback` costs more than AirPlay and rung D is dead.
4. If a CarPlay head unit is available: **PASS** if the Now Playing screen shows the talkgroup and its transport buttons drive the queue.

---

**Step 7 — Battery sanity (Q7, coarse but real).**
Charge to 100%. Start the app with the keep‑alive active and `--interval 300s` (mostly silence). Lock the screen, leave it untouched for **2 hours**. Then Settings → Battery → last 24 hours → the web app's entry.
- **RECORD** the percentage consumed and the "background activity" split. There is no published number to compare against (§9), so this establishes *our* baseline.
- **CONCERN if** >15%/hour, or if iOS lists substantial background activity during periods with zero Calls — that would mean the idle timeout is not firing.

---

**Step 8 — Suspension recovery (the thing we must not get wrong).**
Force a suspension: after a Step 4 FAIL (or by leaving the phone locked for 10+ minutes with no Calls), unlock and open the app from the Home Screen.
- **PASS:** the app shows a tappable play/resume affordance, reconnects the live feed, rebuilds the queue, and one tap starts audio. Lock‑screen `next`/`previous` work again afterwards.
- **FAIL:** a stuck spinner, a pause button over silence, or dead lock‑screen skip buttons after resuming → the `visibilitychange` recovery path is missing or the Media Session handlers were not re‑bound.

---

**Recording the result.** Every run gets: exact iOS build, mechanism variant, pass/fail per step, and the 3‑of‑3 tally for Step 4. This checklist is per‑iOS‑point‑release — a pass on 26.5 says nothing about 26.6 (§3 is the evidence for that).

---

## 15. Sources (with dates)

**WebKit source — `github.com/WebKit/WebKit`, branch `main`, all read 2026‑07‑26:**
- `Source/WebCore/html/HTMLMediaElement.cpp` — `computeCanProduceAudio()`, `mediaState()`, `deferredMediaSourceOpenCanProgress()`, `isWirelessPlaybackTargetDisabledChanged()`, `hasWirelessPlaybackTargetAlternative()`
- `Source/WebCore/Modules/mediasource/ManagedMediaSource.cpp` / `.h` — `setStreaming()`, `monitorSourceBuffers()`, `streamingTimerFired()`, `ensurePrefsRead()`, `m_streamingAllowed`
- `Source/WebCore/Modules/mediasource/MediaSourceInterfaceMainThread.cpp` — `isStreamingContent()`
- `Source/WebCore/Modules/mediasource/SourceBuffer.idl` — `changeType` / `SourceBufferChangeTypeEnabled`
- `Source/WebCore/platform/graphics/cocoa/SourceBufferParser.cpp`; `.../cocoa/SourceBufferParserWebM.cpp`; `.../avfoundation/objc/SourceBufferParserAVFObjC.mm`; `.../avfoundation/objc/AVStreamDataParserMIMETypeCache.mm`
- `Source/WebCore/page/MediaProducer.h` — `IsPlayingAudio`, `HasStreamingActivity`
- `Source/WebKit/UIProcess/WebProcessProxy.cpp` — `updateAudibleMediaAssertions()`, `updateMediaStreamingActivity()`, `didExceedCPULimit()`
- `Source/WebKit/UIProcess/ProcessAssertion.h`; `Source/WebKit/UIProcess/Cocoa/ProcessAssertionCocoa.mm` — `ProcessAssertionType::MediaPlayback` → `"MediaPlayback"` / `"com.apple.webkit"`
- `Source/WebKit/Shared/WebPreferencesDefaultValues.cpp` — `defaultManagedMediaSourceEnabled()`, `defaultManagedMediaSourceNeedsAirPlay()`
- `Source/WTF/Scripts/Preferences/UnifiedWebPreferences.yaml` — `ManagedMediaSourceHighThreshold: 30`, `ManagedMediaSourceLowThreshold: 10`, `ManagedMediaSourceNeedsAirPlay`, `SourceBufferChangeTypeEnabled`

**WebKit blog / release notes (all read 2026‑07‑26):**
- "WebKit Features in Safari 17.1" (2023‑10‑25) — MMS to iPhone; AirPlay‑alternative‑or‑disable requirement; MSE power cost — https://webkit.org/blog/14735/webkit-features-in-safari-17-1/
- Jean‑Yves Avenard & Jon Davis, "How to use Media Source Extensions with AirPlay" (2024‑02‑16) — https://webkit.org/blog/15036/how-to-use-media-source-extensions-with-airplay/
- "WebKit Features in Safari 26.0" (2025‑09‑15) — every site can be a Home Screen web app — https://webkit.org/blog/17333/webkit-features-in-safari-26-0/
- "WebKit Features for Safari 26.2" (2025‑12‑12) — **"Fixed an issue where an audio element failed to play when re-opening a Home Screen Web App." (155336513)** — https://webkit.org/blog/17640/webkit-features-for-safari-26-2/
- "WebKit Features for Safari 26.4" (2026‑03‑24) — volume 0→0 audio session (161691743); MSE `ended` may never fire (165430052) — https://webkit.org/blog/17862/webkit-features-for-safari-26-4/
- "WebKit Features for Safari 26.5" (2026‑05‑11) — nothing for HSWA/background audio — https://webkit.org/blog/17938/webkit-features-for-safari-26-5/

**Specs / MDN / caniuse (all read 2026‑07‑26):**
- W3C Media Source Extensions™, Editor's Draft 2025‑11‑04 — https://w3c.github.io/media-source/
- W3C MSE Byte Stream Format Registry, 2026‑06‑04 — https://www.w3.org/TR/mse-byte-stream-format-registry/
- W3C MPEG Audio Byte Stream Format, 2024‑07‑23 — https://www.w3.org/TR/mse-byte-stream-format-mpeg-audio/
- W3C Media Session, Editor's Draft 2026‑06‑05 — https://w3c.github.io/mediasession/
- MDN `ManagedMediaSource` / `ManagedSourceBuffer` / `ManagedMediaSource.streaming` (last modified 2026‑03‑23) — https://developer.mozilla.org/en-US/docs/Web/API/ManagedMediaSource
- caniuse `mdn-api_managedmediasource` — Safari 17.0+, iOS Safari 17.1 → 26.5 supported (data month June 2026) — https://caniuse.com/mdn-api_managedmediasource
- caniuse `mdn-api_navigator_mediasession` — iOS Safari through 26.5‑TP (data month June 2026) — https://caniuse.com/mdn-api_navigator_mediasession

**WebKit Bugzilla / Apple Developer Forums (all read 2026‑07‑26):**
- Bug 261858 — "[iOS 16.x & iOS 17.x] autoplay in audio element and media session controls not working in standalone web app (pwa) when playback ends" — reported 2023‑09‑20, **NEW/unresolved**, rdar://116156954 — https://bugs.webkit.org/show_bug.cgi?id=261858
- Bug 140524 — "HTMLMediaElement::isPlayingAudio() returns true even when the element is explicitly muted by script" — 2015‑01‑15 → RESOLVED FIXED 2015‑01‑19 (r178655) — https://bugs.webkit.org/show_bug.cgi?id=140524
- Apple Developer Forums 706499 — "Safari Audio player doesn't play next track of the audio playlist when iPhone screen locked" — iOS 15.4.1, May 2022, 0 replies — https://developer.apple.com/forums/thread/706499
- Apple Developer Forums 805900 — "PWA video playback stopped working after updating iOS to 26.0.1" — https://developer.apple.com/forums/thread/805900

**[FIELD] — dated developer reports and project source/issue trackers (all read 2026‑07‑26):**
- MacRumors thread 2466839 — "iOS 26 Audio issues in PWA web apps (not fixed in 26.1 or 26.2, but much better)" — 2025‑09‑20 → last post 2026‑01‑28; **no 26.3/26.4/26.5 reports** — https://forums.macrumors.com/threads/ios-26-audio-issues-in-pwa-web-apps-not-fixed-in-26-1-or-26-2-but-much-better.2466839/
- `cboin1996/songbirdweb`, `docs/STATE.md` — silence.mp3 keep‑alive, `setPositionState` drift, handler re‑binding, ~10 s session release / ~20 s suspension (committed 2026‑04‑29; repo pushed 2026‑07‑10) — https://github.com/cboin1996/songbirdweb/blob/main/docs/STATE.md
- `video-dev/hls.js`, `src/controller/buffer-controller.ts` — MMS detection, forced `disableRemotePlayback`, `startstreaming`/`endstreaming` → `resumeBuffering`/`pauseBuffering`, `bufferedchange` logging — https://github.com/video-dev/hls.js/blob/master/src/controller/buffer-controller.ts
- hls.js #6197 — "ManagedMediaSource + disableRemotePlayback in Safari" (2024‑02‑07, closed) — https://github.com/video-dev/hls.js/issues/6197
- hls.js #6125 — Safari MSE rejects `audio/mp4;codecs="mp4a.40.34"` (MP3‑in‑MP4) on 17.2.1 (fixed in hls.js 1.6.0 by avoiding the path) — https://github.com/video-dev/hls.js/issues/6125
- audiobookshelf #2655 — "iOS Background audio stops at the end of each audio track (iOS 17+)" — opened 2024‑02‑24, **still open**, 18 comments, last activity 2025‑05‑18; maintainer 2024‑02‑26: *"I'm not sure if there is anything we can do about this since it is likely a browser issue"*; resolved by users switching to a native client — https://github.com/advplyr/audiobookshelf/issues/2655
- jellyfin-web #6113 — "iOS (Safari): background audio playback broken" (2024‑09‑24, iOS 18) — **closed as not planned / stale** — https://github.com/jellyfin/jellyfin-web/issues/6113
- jellyfin-web #5425 — iOS background: pause works, resume/track-change do not — https://github.com/jellyfin/jellyfin-web/issues/5425
- icecast-metadata-js #193 — "navigator.mediaSession.metadata not working in iOS" — opened 2023‑11‑17, **open, no maintainer response**; works with a plain `<audio>` tag, fails through the MediaSource‑first player — https://github.com/eshaz/icecast-metadata-js/issues/193
- icecast-metadata-player README — playback methods: MediaSource / WebAudio / HTML5 — https://github.com/eshaz/icecast-metadata-js/tree/master/src/icecast-metadata-player
- mozilla-mobile/firefox-ios #30640 — "iOS 26.1: Firefox stops all audio/video playback instantly when not in foreground" — https://github.com/mozilla-mobile/firefox-ios/issues/30640
- Rdio Scanner on the App Store (native iOS client, "seamless background audio") — https://apps.apple.com/us/app/rdio-scanner/id1563065667
- Loopy Pro forum 28014 — "Battery drain on standby with background audio" — anecdotal only, **not usable as a measurement** — https://forum.loopypro.com/discussion/28014/battery-drain-on-standby-with-background-audio
