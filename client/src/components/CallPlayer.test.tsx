import { act, screen, waitFor } from '@testing-library/react'
import { http, HttpResponse } from 'msw'
import { beforeEach, describe, expect, it, onTestFinished, vi } from 'vitest'

import { ARTWORK_SIZES } from '@/lib/artwork'
import { keepAliveLoopUrl } from '@/lib/silence'
import { enterPlaybackMode, next, playResults, stop } from '@/store/playback'
import { makeStore, type AppStore } from '@/store/store'
import { ARCHIVE, ORIGIN } from '@/test/handlers'
import { audioSessionType, installMediaSession } from '@/test/mediaSession'
import { server } from '@/test/setup'
import { renderWithProviders } from '@/test/utils'
import { advance, received, replay } from '@/store/live'
import {
  KEEP_ALIVE_LIMIT_MS,
  pause,
  progressed,
  resume,
  selectIsBridging,
  selectIsPaused,
  selectNowPlaying,
  selectProgress,
} from '@/store/transport'

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

/** Put the element's playhead somewhere and let it say so, the way a browser
 *  does a few times a second. */
function playhead({
  currentTime,
  duration,
}: {
  currentTime: number
  duration: number
}) {
  for (const [name, value] of Object.entries({ currentTime, duration })) {
    // Writable, as a browser's `currentTime` is: the player rewinds a newly
    // loaded Call, and a read-only stand-in would make that throw.
    Object.defineProperty(player(), name, {
      value,
      configurable: true,
      writable: true,
    })
  }
  act(() => {
    player().dispatchEvent(new Event('timeupdate'))
  })
}

/** Say whether the element is stopped — which is how a page that iOS suspended
 *  looks when it comes back. jsdom has no playback, so both answers have to be
 *  said out loud. */
function elementPaused(paused: boolean) {
  Object.defineProperty(player(), 'paused', { value: paused, configurable: true })
}

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

    it('follows the playhead, which is what the display draws', () => {
      const store = playFrom(0)

      Object.defineProperty(player(), 'duration', {
        value: 10,
        configurable: true,
      })
      Object.defineProperty(player(), 'currentTime', {
        value: 4,
        configurable: true,
      })
      act(() => {
        player().dispatchEvent(new Event('timeupdate'))
      })

      expect(selectProgress(store.getState())).toBeCloseTo(0.4)
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

    /** Spec US 15: pause "suspends playback without losing the queue" — and
     *  without losing the listener's place in the Call, either. */
    it('picks a paused Call up where it left off', () => {
      const store = playFrom(0)
      const audio = player()
      Object.defineProperty(audio, 'currentTime', {
        value: 6,
        writable: true,
        configurable: true,
      })

      act(() => {
        store.dispatch(pause())
      })
      act(() => {
        store.dispatch(resume())
      })

      expect(audio.currentTime).toBe(6)
    })

    it('starts a replayed Call over, though its source never changed', () => {
      const store = makeStore()
      renderWithProviders(<CallPlayer />, { store })
      act(() => {
        store.dispatch(received(ARCHIVE[0], 1))
      })
      const audio = player()
      Object.defineProperty(audio, 'currentTime', {
        value: 6,
        writable: true,
        configurable: true,
      })

      act(() => {
        store.dispatch(replay(ARCHIVE[0].id))
      })

      expect(audio.currentTime).toBe(0)
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

  /** Keeping the page — and therefore the queue — alive across the quiet
   *  between Calls (spec US 31; docs/research/ios-gap-bridging-mechanism.md). */
  describe('bridging the gap', () => {
    /** The live feed plays one Call and then has nothing left. */
    function afterTheLastCall(store = makeStore()) {
      renderWithProviders(<CallPlayer />, { store })
      act(() => {
        store.dispatch(received(ARCHIVE[0], 1))
      })
      act(() => {
        store.dispatch(advance())
      })
      return store
    }

    it('holds the element on an inaudible loop instead of letting it stop', () => {
      afterTheLastCall()

      // Never `paused`, never `ended` — the two states iOS takes as permission
      // to suspend the page.
      expect(player().src).toBe(keepAliveLoopUrl())
      expect(player().loop).toBe(true)
      expect(player().muted).toBe(false)
      expect(player().volume).toBe(1)
    })

    it('drops the loop the moment a Call arrives', () => {
      const store = afterTheLastCall()

      act(() => {
        store.dispatch(received(ARCHIVE[1], 2))
      })

      expect(player().src).toContain(ARCHIVE[1].audioUrl)
      expect(player().loop).toBe(false)
    })

    // The element reaching `ended` with nothing to follow is the exact moment
    // WebKit bug 261858 bites, so the last Call hands over just before it.
    it('hands over just before the last Call ends, rather than on it', () => {
      const store = makeStore()
      renderWithProviders(<CallPlayer />, { store })
      act(() => {
        store.dispatch(received(ARCHIVE[0], 1))
      })

      playhead({ currentTime: 7.9, duration: 8 })

      expect(selectNowPlaying(store.getState())).toBeNull()
      expect(player().src).toBe(keepAliveLoopUrl())
    })

    it('lets a Call with another behind it play all the way out', () => {
      const store = makeStore()
      renderWithProviders(<CallPlayer />, { store })
      act(() => {
        store.dispatch(received(ARCHIVE[0], 1))
        store.dispatch(received(ARCHIVE[1], 2))
      })

      playhead({ currentTime: 7.9, duration: 8 })

      expect(selectNowPlaying(store.getState())).toEqual(ARCHIVE[0])
    })

    // The archive is a finite list the listener is walking, and running out of
    // it is an ending, not a gap — so there is no keep-alive to hand over to,
    // and clipping the last result's tail would buy nothing at all.
    it('never clips the tail of an archived Call', () => {
      const store = playFrom(ARCHIVE.length - 1)

      playhead({ currentTime: 7.9, duration: 8 })

      expect(selectNowPlaying(store.getState())).toEqual(ARCHIVE.at(-1))
    })

    // The loop has its own clock and its own length; drawing them would put a
    // waveform of nothing over the last Call's readout.
    it('does not let the loop drive the display', () => {
      const store = afterTheLastCall()
      act(() => {
        store.dispatch(progressed({ position: 2, duration: 8 }))
      })

      playhead({ currentTime: 0.5, duration: 1 })

      expect(selectProgress(store.getState())).toBeCloseTo(0.25)
    })
    // Holding the audio session open forever is the one thing that would make
    // us worse than rdio on a phone: it blocks the suspension that exists to
    // save power. After a long enough lull we stop, and Web Push (#16) takes
    // over the job of saying something happened.
    it('gives up the session after a long enough lull', () => {
      vi.useFakeTimers()
      onTestFinished(() => vi.useRealTimers())
      const store = afterTheLastCall()
      expect(player().src).toBe(keepAliveLoopUrl())

      act(() => vi.advanceTimersByTime(KEEP_ALIVE_LIMIT_MS))

      expect(selectIsBridging(store.getState())).toBe(false)
      expect(player()).not.toHaveAttribute('src')
    })

    // iOS forgets an app's Media Session handlers across a backgrounding, and
    // an app that doesn't put them back has dead lock-screen buttons.
    it('re-binds the lock-screen buttons on coming back to the foreground', () => {
      const session = installMediaSession()
      renderWithProviders(<CallPlayer />)
      session.handlers.clear()

      act(() => document.dispatchEvent(new Event('visibilitychange')))

      expect([...session.handlers.keys()]).toEqual([
        'play',
        'pause',
        'nexttrack',
        'previoustrack',
      ])
    })

    // The keep-alive is a workaround for an unfixed WebKit bug, not a
    // guarantee — iOS can still suspend us. Coming back to find the element
    // stopped means it did, and the honest answer is a play button, not a UI
    // insisting it is playing.
    it('admits a suspension instead of claiming to play silence', () => {
      const store = afterTheLastCall()
      elementPaused(true)

      act(() => document.dispatchEvent(new Event('visibilitychange')))

      expect(selectIsPaused(store.getState())).toBe(true)
    })

    it('says nothing when it comes back still playing', () => {
      const store = afterTheLastCall()
      elementPaused(false)

      act(() => document.dispatchEvent(new Event('visibilitychange')))

      expect(selectIsPaused(store.getState())).toBe(false)
    })

    // Going *away* is not coming back: the app is being backgrounded, which is
    // the thing the keep-alive exists to survive, not a moment to declare it
    // failed.
    it('reads nothing into being backgrounded', () => {
      const store = afterTheLastCall()
      elementPaused(true)
      Object.defineProperty(document, 'visibilityState', {
        value: 'hidden',
        configurable: true,
      })
      onTestFinished(() =>
        Reflect.deleteProperty(Document.prototype, 'visibilityState'),
      )

      act(() => document.dispatchEvent(new Event('visibilitychange')))

      expect(selectIsPaused(store.getState())).toBe(false)
    })

    // The lock screen keeps drawing a scrubber from the last position state it
    // was given, advancing it on its own clock — so a stale one runs off the
    // end of a Call that finished. Clearing it leaves the metadata (which is
    // still true) without a progress bar that is not.
    it('clears the lock screen scrubber rather than letting it run on', () => {
      const session = installMediaSession()
      const store = makeStore()
      renderWithProviders(<CallPlayer />, { store })
      act(() => {
        store.dispatch(received(ARCHIVE[0], 1))
      })
      Object.defineProperty(player(), 'duration', {
        value: 8,
        configurable: true,
      })
      act(() => {
        player().dispatchEvent(new Event('loadedmetadata'))
      })
      expect(session.positions.at(-1)).toEqual({
        duration: 8,
        position: 0,
        playbackRate: 1,
      })

      act(() => {
        store.dispatch(advance())
      })

      expect(session.positions.at(-1)).toBeUndefined()
    })

    // The loop is a second long and restarts forever; publishing *its* clock
    // would be the same lie in the other direction.
    it('never publishes the loop as a position', () => {
      const session = installMediaSession()
      afterTheLastCall()
      const published = session.positions.length

      act(() => {
        player().dispatchEvent(new Event('loadedmetadata'))
      })

      expect(session.positions).toHaveLength(published)
    })
  })
})
