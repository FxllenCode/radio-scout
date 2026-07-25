import { act, screen, waitFor } from '@testing-library/react'
import { http, HttpResponse } from 'msw'
import { beforeEach, describe, expect, it, vi } from 'vitest'

import { ARTWORK_SIZES } from '@/lib/artwork'
import {
  enterPlaybackMode,
  next,
  pause,
  playResults,
  selectIsPaused,
  stop,
} from '@/store/playback'
import { makeStore, type AppStore } from '@/store/store'
import { ARCHIVE, ORIGIN } from '@/test/handlers'
import { audioSessionType, installMediaSession } from '@/test/mediaSession'
import { server } from '@/test/setup'
import { renderWithProviders } from '@/test/utils'

import { CallPlayer } from './CallPlayer'

/** Audio URLs anything asked the network for — i.e. what got prefetched. */
let fetched: string[] = []

beforeEach(() => {
  fetched = []
  server.use(
    http.get(`${ORIGIN}/api/call/:id/audio`, ({ request }) => {
      fetched.push(new URL(request.url).pathname)
      return new HttpResponse('audio-bytes')
    }),
  )
})

const player = () => screen.getByTestId('call-player') as HTMLAudioElement

/** Mount the player over a store, and start the archive queue at `index`. */
function playFrom(index: number, store: AppStore = makeStore()) {
  renderWithProviders(<CallPlayer />, { store })
  act(() => {
    store.dispatch(enterPlaybackMode())
    store.dispatch(playResults({ results: ARCHIVE, index }))
  })
  return store
}

describe('CallPlayer', () => {
  it('asks for a playback-category audio session on mount', () => {
    installMediaSession()

    renderWithProviders(<CallPlayer />)

    // Without this, iOS treats our audio as ambient and mutes it the moment the
    // app is backgrounded (ADR-0005).
    expect(audioSessionType()).toBe('playback')
  })

  describe('lock screen', () => {
    it('shows the playing call, and clears when playback stops', () => {
      const session = installMediaSession()
      const store = playFrom(0)

      expect(session.metadata).toMatchObject({
        title: 'Beta Dispatch',
        artist: 'Beta',
        album: 'Fire · Public',
      })
      expect(session.metadata?.artwork).toHaveLength(ARTWORK_SIZES.length)
      expect(session.playbackState).toBe('playing')

      act(() => {
        store.dispatch(stop())
      })
      expect(session.metadata).toBeNull()
      expect(session.playbackState).toBe('none')
    })

    it('walks the queue from the lock-screen skip buttons', async () => {
      const session = installMediaSession()
      playFrom(0)
      expect(player()).toHaveAttribute('src', '/api/call/3/audio')

      act(() => session.fire('nexttrack'))
      await waitFor(() =>
        expect(player()).toHaveAttribute('src', '/api/call/2/audio'),
      )
      expect(session.metadata).toMatchObject({ title: 'Alpha Law' })

      act(() => session.fire('previoustrack'))
      await waitFor(() =>
        expect(player()).toHaveAttribute('src', '/api/call/3/audio'),
      )
    })

    it('pauses and resumes the audio itself, not just the display', () => {
      const session = installMediaSession()
      const play = vi.spyOn(HTMLMediaElement.prototype, 'play')
      const halt = vi.spyOn(HTMLMediaElement.prototype, 'pause')
      playFrom(0)
      play.mockClear()

      act(() => session.fire('pause'))
      expect(halt).toHaveBeenCalled()
      expect(session.playbackState).toBe('paused')

      act(() => session.fire('play'))
      expect(play).toHaveBeenCalled()
      expect(session.playbackState).toBe('playing')
    })

    it('publishes the duration once the audio reports one', () => {
      const session = installMediaSession()
      playFrom(0)

      Object.defineProperty(player(), 'duration', {
        value: 8.25,
        configurable: true,
      })
      act(() => {
        player().dispatchEvent(new Event('loadedmetadata'))
      })

      expect(session.positions.at(-1)).toEqual({
        duration: 8.25,
        position: 0,
        playbackRate: 1,
      })
    })
  })

  describe('the element itself', () => {
    it('plays each call through the one reused element', async () => {
      const store = playFrom(0)
      const element = player()

      act(() => {
        store.dispatch(playResults({ results: ARCHIVE, index: 1 }))
      })

      // Same element, new source: a fresh element per Call is what tears the
      // audio session down between calls on iOS (ADR-0005).
      await waitFor(() =>
        expect(player()).toHaveAttribute('src', '/api/call/2/audio'),
      )
      expect(player()).toBe(element)
    })

    /** Dropping the `src` attribute does not stop a playing element — only
     *  pausing (and reloading) does. */
    it('actually silences the element when playback stops', () => {
      const halt = vi.spyOn(HTMLMediaElement.prototype, 'pause')
      const store = playFrom(0)
      halt.mockClear()

      act(() => {
        store.dispatch(stop())
      })

      expect(halt).toHaveBeenCalled()
      expect(player()).not.toHaveAttribute('src')
    })

    it('advances the queue when a call ends', async () => {
      playFrom(0)

      act(() => {
        player().dispatchEvent(new Event('ended'))
      })

      await waitFor(() =>
        expect(player()).toHaveAttribute('src', '/api/call/2/audio'),
      )
    })

    /** A browser that refuses to autoplay leaves the audio silent; the store
     *  has to hear about it, or the app claims to be playing nothing. */
    it('falls back to paused when the browser blocks autoplay', async () => {
      const session = installMediaSession()
      vi.spyOn(HTMLMediaElement.prototype, 'play').mockRejectedValue(
        new DOMException('blocked', 'NotAllowedError'),
      )

      playFrom(0)

      await waitFor(() => expect(session.playbackState).toBe('paused'))
    })

    /** Loading the next Call rejects the pending `play()` for the last one.
     *  That is the queue working; reading it as a refusal would pause the very
     *  Call that just started. */
    it('does not mistake being interrupted by the next call for a refusal', async () => {
      let refuse: (error: unknown) => void = () => {}
      vi.spyOn(HTMLMediaElement.prototype, 'play').mockImplementationOnce(
        () => new Promise((_, reject) => (refuse = reject)),
      )
      const store = playFrom(0)

      act(() => {
        store.dispatch(next())
      })
      await act(async () => {
        refuse(new DOMException('interrupted by a new load', 'AbortError'))
      })

      expect(selectIsPaused(store.getState())).toBe(false)
      expect(player()).toHaveAttribute('src', '/api/call/2/audio')
    })

    it('leaves a paused element paused across a re-render', () => {
      const play = vi.spyOn(HTMLMediaElement.prototype, 'play')
      const store = playFrom(0)
      act(() => {
        store.dispatch(pause())
      })
      play.mockClear()

      act(() => {
        store.dispatch(pause())
      })

      expect(play).not.toHaveBeenCalled()
    })
  })

  describe('prefetch', () => {
    it('warms the next call while the current one plays', async () => {
      playFrom(0)

      // Call 3 is playing, so call 2 — the next in the result order — is the
      // one to have ready.
      await waitFor(() => expect(fetched).toEqual(['/api/call/2/audio']))
    })

    it('does not prefetch past the end of the loaded results', async () => {
      playFrom(ARCHIVE.length - 1)

      await Promise.resolve()
      expect(fetched).toEqual([])
    })

    it('leaves the current call to the element, never fetching it twice', async () => {
      playFrom(1)

      await waitFor(() => expect(fetched).toEqual(['/api/call/1/audio']))
      expect(fetched).not.toContain('/api/call/2/audio')
    })
  })
})
