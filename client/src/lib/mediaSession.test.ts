import { describe, expect, it, vi } from 'vitest'

import { audioSessionType, installMediaSession } from '@/test/mediaSession'
import type { Call } from '@/types'

import { ARTWORK_SIZES, ledArtworkUrl } from './artwork'
import { ledForTalkgroup } from './led'
import {
  bindTransport,
  preferPlaybackAudioSession,
  setNowPlaying,
  setPlaybackState,
  setPositionState,
} from './mediaSession'

const call: Call = {
  id: 7,
  systemRef: 100,
  systemLabel: 'Fulton County',
  talkgroupRef: 54241,
  talkgroupLabel: 'Fire Dispatch',
  talkgroupTag: 'Fire',
  talkgroupGroup: 'Emergency',
  audioUrl: '/api/call/7/audio',
}

function transport() {
  return {
    play: vi.fn(),
    pause: vi.fn(),
    nexttrack: vi.fn(),
    previoustrack: vi.fn(),
  }
}

describe('media session', () => {
  describe('now playing', () => {
    it('reads as talkgroup · system · category, with the LED as artwork', () => {
      const session = installMediaSession()

      setNowPlaying(call)

      // Talkgroup as the title: it is what a listener is actually following,
      // and it is the line a lock screen shows biggest.
      expect(session.metadata).toMatchObject({
        title: 'Fire Dispatch',
        artist: 'Fulton County',
        album: 'Fire · Emergency',
      })
      expect(session.metadata?.artwork).toEqual(
        ARTWORK_SIZES.map((size) => ({
          src: ledArtworkUrl(ledForTalkgroup(100, 54241), size),
          sizes: `${size}x${size}`,
          type: 'image/png',
        })),
      )
    })

    it('falls back to refs when the recorder sent no labels', () => {
      const session = installMediaSession()

      setNowPlaying({
        id: 8,
        systemRef: 11,
        talkgroupRef: 42,
        audioUrl: '/api/call/8/audio',
      })

      expect(session.metadata).toMatchObject({
        title: 'Talkgroup 42',
        artist: 'System 11',
        album: 'Radio-Scout',
      })
    })

    it('names the one category it has when a talkgroup lacks the other', () => {
      const session = installMediaSession()

      setNowPlaying({ ...call, talkgroupGroup: undefined })

      expect(session.metadata?.album).toBe('Fire')
    })

    it('clears the lock screen when nothing is playing', () => {
      const session = installMediaSession()
      setNowPlaying(call)

      setNowPlaying(null)

      expect(session.metadata).toBeNull()
      // The scrubber from the finished Call must not linger over the next one.
      expect(session.positions.at(-1)).toBeUndefined()
    })
  })

  describe('transport controls', () => {
    it('routes each lock-screen button to its handler, and unbinds on release', () => {
      const session = installMediaSession()
      const handlers = transport()

      const release = bindTransport(handlers)

      session.fire('play')
      session.fire('pause')
      session.fire('nexttrack')
      session.fire('previoustrack')
      expect(handlers.play).toHaveBeenCalledOnce()
      expect(handlers.pause).toHaveBeenCalledOnce()
      expect(handlers.nexttrack).toHaveBeenCalledOnce()
      expect(handlers.previoustrack).toHaveBeenCalledOnce()

      release()
      expect([...session.handlers.values()]).toEqual([null, null, null, null])
    })

    /** `setActionHandler` throws for actions a browser doesn't implement (MDN),
     *  so one unsupported control must not cost us the others. */
    it('keeps the controls a browser does support when one is rejected', () => {
      const session = installMediaSession({ unsupported: ['previoustrack'] })
      const handlers = transport()

      expect(() => bindTransport(handlers)).not.toThrow()

      session.fire('nexttrack')
      expect(handlers.nexttrack).toHaveBeenCalledOnce()
      expect(session.handlers.has('previoustrack')).toBe(false)
      expect(() => bindTransport(handlers)()).not.toThrow()
    })

    it('reports playing and paused, so the lock screen shows the right button', () => {
      const session = installMediaSession()

      setPlaybackState('playing')
      expect(session.playbackState).toBe('playing')

      setPlaybackState('paused')
      expect(session.playbackState).toBe('paused')
    })
  })

  describe('position (the lock-screen scrubber)', () => {
    it('publishes the duration once the audio reports one', () => {
      const session = installMediaSession()

      setPositionState(12.5)

      expect(session.positions).toEqual([
        { duration: 12.5, position: 0, playbackRate: 1 },
      ])
    })

    it.each([
      ['unknown before metadata loads', Number.NaN],
      ['a live stream', Number.POSITIVE_INFINITY],
      ['nothing playing', null],
    ])('clears the scrubber when the duration is %s', (_case, duration) => {
      const session = installMediaSession()

      setPositionState(duration)

      expect(session.positions).toEqual([undefined])
    })

    it('skips a browser without setPositionState rather than throwing', () => {
      installMediaSession({ positionState: false })

      expect(() => setPositionState(3)).not.toThrow()
    })

    /** The scrubber is polish; a browser that rejects the state it was given
     *  must not take the player down with it. */
    it('swallows a browser that rejects the state', () => {
      installMediaSession({ positionState: 'throws' })

      expect(() => setPositionState(3)).not.toThrow()
    })
  })

  describe('audio session', () => {
    /** iOS 16.4+: a `playback` session plays over the ringer switch and is not
     *  treated as mixable ambient audio (docs/research/ios-background-audio.md
     *  §4) — the difference between audio that survives a lock and audio that
     *  doesn't. */
    it('asks for a playback-category session', () => {
      installMediaSession()

      preferPlaybackAudioSession()

      expect(audioSessionType()).toBe('playback')
    })

    it('does nothing on a browser without the AudioSession API', () => {
      installMediaSession({ audioSession: false })

      expect(() => preferPlaybackAudioSession()).not.toThrow()
      expect(audioSessionType()).toBeUndefined()
    })
  })

  /** Desktop Safari before 15, older Firefox, and any non-browser render (a
   *  future SSR pass) simply have no `navigator.mediaSession`. */
  describe('a browser without Media Session', () => {
    it('degrades to plain audio instead of throwing', () => {
      const handlers = transport()

      expect(() => setNowPlaying(call)).not.toThrow()
      expect(() => setPlaybackState('playing')).not.toThrow()
      expect(() => setPositionState(4)).not.toThrow()
      expect(() => bindTransport(handlers)()).not.toThrow()
    })

    it('skips metadata a browser cannot construct', () => {
      const session = installMediaSession()
      vi.stubGlobal('MediaMetadata', undefined)

      expect(() => setNowPlaying(call)).not.toThrow()
      expect(session.metadata).toBeNull()
    })
  })
})
