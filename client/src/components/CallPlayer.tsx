import { useEffect, useRef, useState } from 'react'

import {
  CATCHUP_RATE,
  NORMAL_RATE,
  stepAt,
} from '@/lib/catchup'
import {
  bindTransport,
  preferPlaybackAudioSession,
  setNowPlaying,
  setPlaybackState,
  setPositionState,
} from '@/lib/mediaSession'
import { prefetchAudio } from '@/lib/prefetch'
import { keepAliveLoopUrl } from '@/lib/silence'
import { useAppDispatch, useAppSelector } from '@/store/hooks'
import { selectPlayId } from '@/store/live'
import {
  KEEP_ALIVE_LIMIT_MS,
  keepAliveExpired,
  nextCall,
  pause,
  previousCall,
  progressed,
  resume,
  selectHasKeepAlive,
  selectIsBridging,
  selectIsHurrying,
  selectIsPaused,
  selectNowPlaying,
  selectSeek,
  selectUpcomingCall,
  sourceChanged,
} from '@/store/transport'

/** How far before a Call's end the queue hands over, in seconds. Short enough
 *  that the tail it clips is the squelch, long enough to beat a `timeupdate`
 *  interval (browsers fire roughly every 250 ms). */
const HANDOVER = 0.3

/**
 * The app's one `<audio>` element and everything that drives it (#14).
 *
 * ADR-0005: a **single reused element**, never WebAudio. iOS classifies
 * WebAudio output as ambient and mutes it the moment the app is backgrounded —
 * which is exactly why rdio-scanner has no working background audio — and a
 * fresh element per Call tears the audio session down in the gaps between them.
 * So one element lives here in the shell, outside the router outlet, and every
 * Call flows through it by changing `src`.
 *
 * It is deliberately incurious about *where* a Call came from: the live feed
 * (#11) and the archive (#13) both reach it through `@/store/transport`, which
 * is also where its controls — in-app and lock-screen alike — land.
 */
export function CallPlayer() {
  const dispatch = useAppDispatch()
  const current = useAppSelector(selectNowPlaying)
  const upcoming = useAppSelector(selectUpcomingCall)
  const paused = useAppSelector(selectIsPaused)
  const bridging = useAppSelector(selectIsBridging)
  const hasKeepAlive = useAppSelector(selectHasKeepAlive)
  // Replaying the Call already loaded leaves `src` untouched, so the element
  // needs a separate nudge to start it over (spec US 13).
  const playId = useAppSelector(selectPlayId)
  // The two levers ADR-0005 leaves us — `playbackRate` and `currentTime` — and
  // nothing else: there is no WebAudio here and there never will be. Asked of
  // the transport rather than of a source, so this is **Catch-up** draining the
  // listening queue (#59) or a **DVR** run being hurried through (#63) without
  // the player having to know which.
  const hurrying = useAppSelector(selectIsHurrying)
  const rate = hurrying ? CATCHUP_RATE : NORMAL_RATE
  /** A seek the Listener asked for and the element has not made yet (#63). */
  const seek = useAppSelector(selectSeek)
  const element = useRef<HTMLAudioElement>(null)
  /** Bumped on every return to the foreground — what re-binds the lock screen. */
  const [foregrounded, setForegrounded] = useState(0)
  /** The `playId` the element was last started at — what tells a fresh Call
   *  apart from a resume. */
  const started = useRef<number>(-1)

  // A playback-category session: plays over the ringer switch, and isn't
  // treated as mixable ambient audio that iOS silences in the background.
  useEffect(preferPlaybackAudioSession, [])

  // The lock screen drives the same actions the in-app controls do, so the two
  // can never disagree about what is playing. Re-bound on every return to the
  // foreground: iOS forgets an app's handlers across a backgrounding, and an
  // app that doesn't put them back has dead lock-screen buttons.
  useEffect(
    () =>
      bindTransport({
        play: () => dispatch(resume()),
        pause: () => dispatch(pause()),
        nexttrack: () => dispatch(nextCall()),
        previoustrack: () => dispatch(previousCall()),
      }),
    [dispatch, foregrounded],
  )

  // Coming back from the background. The keep-alive works around a WebKit bug
  // Apple has never fixed, so it is a best effort, not a guarantee: finding the
  // element stopped means iOS suspended us anyway. Saying so gives the listener
  // a play button, which is one tap; insisting we are playing gives them
  // silence and no way out of it.
  useEffect(() => {
    const returned = () => {
      if (document.visibilityState !== 'visible') return
      setForegrounded((count) => count + 1)
      if (bridging && element.current?.paused) dispatch(pause())
    }
    document.addEventListener('visibilitychange', returned)
    return () => document.removeEventListener('visibilitychange', returned)
  }, [bridging, dispatch])

  // Stop holding the audio session open once a lull has outlasted its budget,
  // and let the OS have the power savings back.
  useEffect(() => {
    if (!bridging) return
    const timer = setTimeout(
      () => dispatch(keepAliveExpired()),
      KEEP_ALIVE_LIMIT_MS,
    )
    return () => clearTimeout(timer)
  }, [bridging, dispatch])

  // The lock screen keeps the last Call while the gap is being bridged: the
  // session is still ours, still playing, and still skippable — clearing it
  // would tell the OS we had stopped when we deliberately have not.
  //
  // The scrubber is the exception, and goes. The OS advances it on its own
  // clock from whatever position state it was last given, so a stale one runs
  // off the end of a Call that has finished; no scrubber is honest, a running
  // one is not.
  useEffect(() => {
    if (bridging) setPositionState(null)
    else setNowPlaying(current)
  }, [current, bridging])

  useEffect(() => {
    if (current || bridging) setPlaybackState(paused ? 'paused' : 'playing')
    else setPlaybackState('none')
  }, [current, bridging, paused])

  // A different Call (or the same one again): the transport's pause and
  // progress belong to what was playing, not to what is about to.
  //
  // Keyed on **which Call**, not on which object. A Call in the store is
  // rewritten in place when something learns more about it — #59's quiet spans
  // arrive behind the Call already playing — and keying on identity would read
  // that as a new source: the pause a Listener had just pressed would be
  // cleared, and the audio would carry on under them.
  const playingId = current?.id ?? null
  useEffect(() => {
    dispatch(sourceChanged())
  }, [playingId, playId, dispatch])

  // The element follows the store rather than owning "is it playing", so a
  // lock-screen pause and an in-app pause are the same state.
  useEffect(() => {
    const audio = element.current
    if (!audio) return

    if (!current && !bridging) {
      // Clearing `src` alone leaves a playing element playing — the media
      // resource it already loaded outlives the attribute.
      audio.pause()
      audio.load()
      return
    }
    if (paused) {
      audio.pause()
      return
    }
    // Rewind only what is newly on the element: a replay hands back the Call
    // already loaded, and one sitting at its end would otherwise "play" in
    // silence. Resuming from a pause must *not* rewind — the Call keeps its
    // place (spec US 15).
    if (started.current !== playId) {
      started.current = playId
      audio.currentTime = 0
    }
    // A browser may refuse to start audio without a user gesture. Record that
    // rather than showing a pause button over silence — but only if this Call
    // is still the one playing: loading the next Call *also* rejects the
    // pending `play()`, and that rejection means the queue moved on, not that
    // the browser refused us.
    let superseded = false
    audio.play()?.catch(() => {
      if (!superseded) dispatch(pause())
    })
    return () => {
      superseded = true
    }
  }, [current, bridging, playId, paused, dispatch])

  // The rate is re-applied on every new source, not only when it changes: an
  // element that has been handed a new `src` does not reliably keep the rate it
  // was set to, and a Call that quietly played at 1× would look exactly like
  // Catch-up having stopped. `preservesPitch` is set beside it because the
  // acceptance criterion is *speech intact* — every browser that matters
  // defaults it on, and a browser that did not would turn dispatch traffic into
  // a chipmunk at the one moment a Listener is trying to follow it.
  //
  // The keep-alive loop is excluded: it is a hundred-hertz square wave nobody
  // hears, and playing it fast would shorten the gap it exists to hold open.
  useEffect(() => {
    const audio = element.current
    if (!audio) return
    audio.preservesPitch = true
    audio.playbackRate = bridging ? NORMAL_RATE : rate

    // ...and tell the lock screen, because the OS runs that scrubber off its
    // own clock at whatever rate it was last given. Catch-up is engaged and
    // stopped from a sheet the Listener opens *while a Call is playing*, so a
    // rate change mid-Call is the ordinary case, not an edge — and left
    // unpublished it leaves the lock screen disagreeing with the audio coming
    // out of the phone for the rest of that Call.
    //
    // Only where the element knows how long the Call is: `NaN` until it has
    // read the container header, which is every new source, and `loadedmetadata`
    // publishes that one. A replay reaches here with the position already
    // rewound, because the effect above runs first.
    const { duration, currentTime } = audio
    if (!bridging && Number.isFinite(duration) && duration > 0) {
      setPositionState(duration, currentTime, rate)
    }
  }, [rate, bridging, current, playId])

  // Move the playhead where the Listener put it (#63, spec US 39). The element
  // fetches whatever bytes it does not have with a range request, which is what
  // makes "seeking lands within calls" free at the server: `src/serve.rs` has
  // answered those since #10, and with the S3 backend the request never reaches
  // Radio-Scout at all.
  //
  // Keyed on the **nonce alone**, deliberately. Depending on `rate` or on the
  // Call would re-apply the last seek whenever either moved — so turning the
  // speed up mid-Call would drag the Listener back to wherever they last
  // scrubbed, minutes after they had listened past it.
  const seekNonce = seek?.nonce
  useEffect(() => {
    const audio = element.current
    if (!audio || !seek || bridging) return
    audio.currentTime = seek.toSeconds
    // The OS runs its scrubber off its own clock, so a jump it was not told
    // about leaves the lock screen wrong by however far the Listener moved.
    const { duration } = audio
    if (Number.isFinite(duration) && duration > 0) {
      setPositionState(duration, seek.toSeconds, rate)
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [seekNonce])

  // Warm the next Call while this one plays (see lib/prefetch), and drop that
  // download if the queue moves somewhere else first.
  useEffect(() => {
    if (!upcoming) return
    const controller = new AbortController()
    void prefetchAudio(upcoming.audioUrl, controller.signal)
    return () => controller.abort()
  }, [upcoming])

  return (
    <audio
      ref={element}
      data-testid="call-player"
      preload="auto"
      // In a gap the element holds the keep-alive loop rather than nothing at
      // all — never `paused`, never `ended`, which are the two states iOS
      // reads as permission to suspend the page (spec US 31).
      src={bridging ? keepAliveLoopUrl() : current?.audioUrl}
      loop={bridging}
      onEnded={() => dispatch(nextCall())}
      onLoadedMetadata={(event) => {
        if (bridging) return
        setPositionState(event.currentTarget.duration, 0, rate)
        dispatch(
          progressed({ position: 0, duration: event.currentTarget.duration }),
        )
      }}
      // Drives the display's waveform (#11) — a few times a second, per spec.
      onTimeUpdate={(event) => {
        // The loop has its own clock and its own length; drawing them would
        // put a waveform of nothing over the last Call's readout.
        if (bridging) return
        const { currentTime, duration } = event.currentTarget
        dispatch(progressed({ position: currentTime, duration }))
        // Skip the stretches nobody is talking in (#59, #63). The spans are
        // the server's — a browser with no WebAudio cannot look at samples —
        // and a Call that has none simply plays through at the raised rate,
        // which is the documented fallback and not a failure. An archived Call
        // carries its own spans, so a DVR needs no equivalent of #59's pull.
        if (hurrying) {
          const step = stepAt(current?.quiet, currentTime, duration)
          if (step?.do === 'seek') {
            event.currentTarget.currentTime = step.toSeconds
            // The OS runs the scrubber off its own clock, so a jump it was not
            // told about leaves the lock screen wrong by the whole gap.
            setPositionState(duration, step.toSeconds, rate)
            return
          }
          if (step?.do === 'advance') {
            dispatch(nextCall())
            return
          }
        }
        // Hand over *before* the last Call ends rather than on it: an element
        // that reaches `ended` with nothing to follow is the exact moment iOS
        // lets go of a backgrounded PWA (WebKit bug 261858), and the moment
        // after that there is no JS left to start the keep-alive.
        // …but only where a keep-alive is actually waiting: clipping the tail
        // off the last archive result would cost a listener audio and buy
        // nothing, since playback mode never bridges.
        if (
          hasKeepAlive &&
          !upcoming &&
          duration > 0 &&
          duration - currentTime <= HANDOVER
        ) {
          dispatch(nextCall())
        }
      }}
    />
  )
}
