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
  next,
  pause,
  previous,
  resume,
  selectCurrentCall,
  selectIsPaused,
  selectNextCall,
} from '@/store/playback'

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
 * Around it: the Media Session (lock screen, Bluetooth, CarPlay) reads the
 * current Call and its buttons reach back into the same queue the in-app
 * controls use, and the Call queued behind the current one is prefetched so it
 * starts without a round trip.
 */
export function CallPlayer() {
  const dispatch = useAppDispatch()
  const current = useAppSelector(selectCurrentCall)
  const upcoming = useAppSelector(selectNextCall)
  const paused = useAppSelector(selectIsPaused)
  const element = useRef<HTMLAudioElement>(null)

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
        nexttrack: () => dispatch(next()),
        previoustrack: () => dispatch(previous()),
      }),
    [dispatch],
  )

  useEffect(() => setNowPlaying(current), [current])

  useEffect(() => {
    setPlaybackState(current ? (paused ? 'paused' : 'playing') : 'none')
  }, [current, paused])

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
  }, [current, paused, dispatch])

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
      onEnded={() => dispatch(next())}
      onLoadedMetadata={(event) =>
        setPositionState(event.currentTarget.duration)
      }
    />
  )
}
