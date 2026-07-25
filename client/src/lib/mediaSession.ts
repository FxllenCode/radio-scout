/**
 * The Media Session API, wrapped (#14, spec US 30).
 *
 * This is what puts a Call on a lock screen, a Bluetooth head unit, and
 * CarPlay: metadata + artwork + transport buttons that reach our queue. It is
 * also the piece rdio-scanner has no answer for — its WebAudio playback can't
 * attach OS transport controls at all (ADR-0005).
 *
 * Everything here is feature-detected and failure-tolerant on purpose: browsers
 * differ in which actions they implement (`setActionHandler` *throws* for ones
 * they don't), `setPositionState` is "limited availability", and
 * `navigator.audioSession` is iOS 16.4+. A missing control must cost us that
 * control, never the audio.
 */
import type { Call } from '@/types'

import { ARTWORK_SIZES, ledArtworkUrl } from './artwork'
import { ledForTalkgroup } from './led'

/** The lock-screen buttons we answer — the four a queue needs. iOS also renders
 *  the seek actions (research §4), but a Call is seconds long, so scrubbing it
 *  buys nothing; `stop` is the one iOS does *not* reliably render. */
export interface Transport {
  play: () => void
  pause: () => void
  nexttrack: () => void
  previoustrack: () => void
}

declare global {
  interface Navigator {
    /** iOS Safari 16.4+ / W3C Audio Session. Absent elsewhere. */
    audioSession?: { type: string }
  }
}

/** `navigator.mediaSession`, or `undefined` where the API doesn't exist. */
function session(): MediaSession | undefined {
  return navigator.mediaSession as MediaSession | undefined
}

/** Put `call` on the lock screen, or clear it when nothing is playing. */
export function setNowPlaying(call: Call | null): void {
  const media = session()
  if (!media) return

  // Whatever the last Call left on the scrubber is wrong now: the next one has
  // its own duration, and a finished one has none at all.
  setPositionState(null)

  if (!call) {
    media.metadata = null
    return
  }

  const Metadata = globalThis.MediaMetadata
  if (!Metadata) return
  media.metadata = new Metadata({
    // The talkgroup is what a listener follows, so it takes the title line —
    // the one a lock screen shows biggest.
    title: call.talkgroupLabel ?? `Talkgroup ${call.talkgroupRef}`,
    artist: call.systemLabel ?? `System ${call.systemRef}`,
    album: [call.talkgroupTag, call.talkgroupGroup].filter(Boolean).join(' · ')
      || 'Radio-Scout',
    // Several sizes, all small: iOS picks one, and small is what it has
    // historically rendered without pixelating (research §4).
    artwork: ARTWORK_SIZES.map((size) => ({
      src: ledArtworkUrl(ledForTalkgroup(call.systemRef, call.talkgroupRef), size),
      sizes: `${size}x${size}`,
      type: 'image/png',
    })),
  })
}

/** Whether the OS should draw a play button or a pause button. */
export function setPlaybackState(state: MediaSessionPlaybackState): void {
  const media = session()
  if (!media) return
  media.playbackState = state
}

/** Publish the lock-screen scrubber for a Call of `duration` seconds, starting
 *  at the top. `null` — or a duration the element doesn't know yet (`NaN`) or
 *  can't bound (`Infinity`) — clears it instead. */
export function setPositionState(duration: number | null): void {
  const media = session()
  if (!media?.setPositionState) return

  try {
    if (duration === null || !Number.isFinite(duration)) {
      media.setPositionState()
      return
    }
    media.setPositionState({ duration, position: 0, playbackRate: 1 })
  } catch {
    // "Limited availability" (MDN): a browser may reject a state we think is
    // valid. The scrubber is polish; losing it must not take the player down.
  }
}

/** Bind the transport buttons; returns the release to run on unmount. */
export function bindTransport(handlers: Transport): () => void {
  const media = session()
  if (!media) return () => {}

  const bound: MediaSessionAction[] = []
  for (const [action, handler] of Object.entries(handlers)) {
    try {
      media.setActionHandler(action as MediaSessionAction, handler)
      bound.push(action as MediaSessionAction)
    } catch {
      // An action this browser doesn't implement. The rest still work.
    }
  }
  return () => {
    for (const action of bound) media.setActionHandler(action, null)
  }
}

/**
 * Ask for a **playback**-category audio session (iOS 16.4+).
 *
 * It is what makes our audio play over the ringer switch and keeps it from
 * being treated as mixable "ambient" audio — which iOS mutes the moment the app
 * leaves the foreground (docs/research/ios-background-audio.md §4).
 */
export function preferPlaybackAudioSession(): void {
  const audio = navigator.audioSession
  if (!audio) return
  audio.type = 'playback'
}
