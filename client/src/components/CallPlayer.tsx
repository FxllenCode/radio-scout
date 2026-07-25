import { useEffect, useRef } from 'react'

import {
  bindTransport,
  preferPlaybackAudioSession,
  setNowPlaying,
  setPlaybackState,
  setPositionState,
} from '@/lib/mediaSession'
import { prefetchAudio } from '@/lib/prefetch'
import { useAppDispatch, useAppSelector } from '@/store/hooks'
import {
  nextCall,
  pause,
  previousCall,
  progressed,
  resume,
  selectIsPaused,
  selectNowPlaying,
  selectSourceId,
  selectUpcomingCall,
  sourceChanged,
} from '@/store/transport'

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
  // Replaying the Call already loaded leaves `src` untouched, so the element
  // needs a separate nudge to start it over (spec US 13).
  const playId = useAppSelector(selectSourceId)
  const element = useRef<HTMLAudioElement>(null)
  /** The `playId` the element was last started at — what tells a fresh Call
   *  apart from a resume. */
  const started = useRef<number>(-1)

  // A playback-category session: plays over the ringer switch, and isn't
  // treated as mixable ambient audio that iOS silences in the background.
  useEffect(preferPlaybackAudioSession, [])

  // The lock screen drives the same actions the in-app controls do, so the two
  // can never disagree about what is playing.
  useEffect(
    () =>
      bindTransport({
        play: () => dispatch(resume()),
        pause: () => dispatch(pause()),
        nexttrack: () => dispatch(nextCall()),
        previoustrack: () => dispatch(previousCall()),
      }),
    [dispatch],
  )

  useEffect(() => setNowPlaying(current), [current])

  useEffect(() => {
    setPlaybackState(current ? (paused ? 'paused' : 'playing') : 'none')
  }, [current, paused])

  // A different Call (or the same one again): the transport's pause and
  // progress belong to what was playing, not to what is about to.
  useEffect(() => {
    dispatch(sourceChanged())
  }, [current, playId, dispatch])

  // The element follows the store rather than owning "is it playing", so a
  // lock-screen pause and an in-app pause are the same state.
  useEffect(() => {
    const audio = element.current
    if (!audio) return

    if (!current) {
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
  }, [current, playId, paused, dispatch])

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
      src={current?.audioUrl}
      onEnded={() => dispatch(nextCall())}
      onLoadedMetadata={(event) => {
        setPositionState(event.currentTarget.duration)
        dispatch(
          progressed({ position: 0, duration: event.currentTarget.duration }),
        )
      }}
      // Drives the display's waveform (#11) — a few times a second, per spec.
      onTimeUpdate={(event) =>
        dispatch(
          progressed({
            position: event.currentTarget.currentTime,
            duration: event.currentTarget.duration,
          }),
        )
      }
    />
  )
}
