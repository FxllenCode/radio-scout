import { describe, expect, it } from 'vitest'

import type { Call } from '@/types'

import {
  enterLiveFeed,
  enterPlaybackMode,
  next,
  playbackReducer,
  playResults,
  previous,
  selectCurrentCall,
  selectHasNext,
  selectHasPrevious,
  selectIsExhausted,
  selectIsInterrupting,
  selectIsNearingPageEnd,
  selectNextCall,
  selectPlaybackMode,
  selectPlaybackPosition,
  selectRemainingInPage,
  stop,
  type PlaybackState,
} from './playback'

function call(id: number): Call {
  return {
    id,
    systemRef: 100,
    talkgroupRef: 1,
    audioUrl: `/api/call/${id}/audio`,
  }
}

const results = [call(1), call(2), call(3)]

/** Reduce a sequence of actions from the initial state. */
function reduce(...actions: Parameters<typeof playbackReducer>[1][]): PlaybackState {
  return actions.reduce(
    (state, action) => playbackReducer(state, action),
    undefined as unknown as PlaybackState,
  )
}

/** Wrap slice state as the store shape the selectors read. */
function rootState(playback: PlaybackState) {
  return { playback }
}

describe('playback slice', () => {
  it('starts on the live feed with nothing queued', () => {
    const state = reduce({ type: '@@INIT' })

    expect(selectPlaybackMode(rootState(state))).toBe('live')
    expect(selectCurrentCall(rootState(state))).toBeNull()
    expect(selectIsInterrupting(rootState(state))).toBe(false)
    expect(selectHasNext(rootState(state))).toBe(false)
    expect(selectHasPrevious(rootState(state))).toBe(false)
    expect(selectPlaybackPosition(rootState(state))).toEqual({ index: -1, total: 0 })
  })

  describe('playback mode (live feed off)', () => {
    it('plays sequentially through the filtered results from the chosen one', () => {
      let state = reduce(enterPlaybackMode(), playResults({ results, index: 1 }))

      expect(selectPlaybackMode(rootState(state))).toBe('playback')
      expect(selectCurrentCall(rootState(state))).toEqual(call(2))
      expect(selectPlaybackPosition(rootState(state))).toEqual({ index: 1, total: 3 })
      expect(selectHasNext(rootState(state))).toBe(true)
      expect(selectHasPrevious(rootState(state))).toBe(true)

      state = playbackReducer(state, next())
      expect(selectCurrentCall(rootState(state))).toEqual(call(3))
      expect(selectHasNext(rootState(state))).toBe(false)
    })

    it('reports itself exhausted after the last loaded result', () => {
      const state = reduce(
        enterPlaybackMode(),
        playResults({ results, index: 2 }),
        next(),
      )

      expect(selectCurrentCall(rootState(state))).toBeNull()
      // Exhausted, not finished: the screen loads the next page and resumes,
      // which is what makes playback sequential across the whole result set.
      expect(selectIsExhausted(rootState(state))).toBe(true)
      expect(selectPlaybackPosition(rootState(state))).toEqual({ index: -1, total: 3 })
      expect(selectHasNext(rootState(state))).toBe(false)
      expect(selectHasPrevious(rootState(state))).toBe(false)
    })

    /** Spec US 25: playback walks the *filtered results*, not one page of them,
     *  so position counts against the archive and a later page continues the
     *  numbering. */
    it('counts position against the whole filtered set, not the page', () => {
      let state = reduce(
        enterPlaybackMode(),
        playResults({ results, index: 0, offset: 50, total: 421 }),
      )
      expect(selectPlaybackPosition(rootState(state))).toEqual({
        index: 50,
        total: 421,
      })

      state = playbackReducer(state, next())
      expect(selectPlaybackPosition(rootState(state))).toEqual({
        index: 51,
        total: 421,
      })
    })

    it('clears the exhausted cue when the next page is played', () => {
      const state = reduce(
        enterPlaybackMode(),
        playResults({ results, index: 2 }),
        next(),
        playResults({ results: [call(4)], index: 0, offset: 3, total: 4 }),
      )

      expect(selectIsExhausted(rootState(state))).toBe(false)
      expect(selectCurrentCall(rootState(state))).toEqual(call(4))
      expect(selectPlaybackPosition(rootState(state))).toEqual({
        index: 3,
        total: 4,
      })
    })

    it('clears the exhausted cue on stop, so the screen stops chasing pages', () => {
      const state = reduce(
        enterPlaybackMode(),
        playResults({ results, index: 2 }),
        next(),
        stop(),
      )

      expect(selectIsExhausted(rootState(state))).toBe(false)
    })

    it('steps backwards and holds at the first result', () => {
      let state = reduce(
        enterPlaybackMode(),
        playResults({ results, index: 1 }),
        previous(),
      )
      expect(selectCurrentCall(rootState(state))).toEqual(call(1))

      state = playbackReducer(state, previous())
      expect(selectCurrentCall(rootState(state))).toEqual(call(1))
      expect(selectHasPrevious(rootState(state))).toBe(false)
    })

    it('ignores next/previous while nothing is playing', () => {
      const idle = reduce(enterPlaybackMode())

      expect(playbackReducer(idle, next())).toEqual(idle)
      expect(playbackReducer(idle, previous())).toEqual(idle)
    })

    it('ignores an empty or out-of-range result selection', () => {
      const empty = reduce(enterPlaybackMode(), playResults({ results: [], index: 0 }))
      expect(selectCurrentCall(rootState(empty))).toBeNull()

      const past = reduce(enterPlaybackMode(), playResults({ results, index: 9 }))
      expect(selectCurrentCall(rootState(past))).toBeNull()
      expect(selectPlaybackPosition(rootState(past))).toEqual({ index: -1, total: 0 })

      const negative = reduce(enterPlaybackMode(), playResults({ results, index: -1 }))
      expect(selectCurrentCall(rootState(negative))).toBeNull()
    })

    it('replaces the queue when a new result set is played', () => {
      const state = reduce(
        enterPlaybackMode(),
        playResults({ results, index: 2 }),
        playResults({ results: [call(9)], index: 0 }),
      )

      expect(selectCurrentCall(rootState(state))).toEqual(call(9))
      expect(selectPlaybackPosition(rootState(state))).toEqual({ index: 0, total: 1 })
    })

    it('stops on demand without discarding the result set', () => {
      const state = reduce(
        enterPlaybackMode(),
        playResults({ results, index: 1 }),
        stop(),
      )

      expect(selectCurrentCall(rootState(state))).toBeNull()
      expect(selectPlaybackPosition(rootState(state))).toEqual({ index: -1, total: 3 })
    })

    it('ignores next/previous once the results have run out', () => {
      const exhausted = reduce(
        enterPlaybackMode(),
        playResults({ results, index: 2 }),
        next(),
      )

      expect(playbackReducer(exhausted, next())).toEqual(exhausted)
      expect(playbackReducer(exhausted, previous())).toEqual(exhausted)
    })
  })

  describe('playing an archived call while the live feed is on', () => {
    it('interrupts with just that call and resumes the live feed when it ends', () => {
      let state = reduce(playResults({ results, index: 1 }))

      // Still live — the live queue is untouched; this Call is on top of it.
      expect(selectPlaybackMode(rootState(state))).toBe('live')
      expect(selectIsInterrupting(rootState(state))).toBe(true)
      expect(selectCurrentCall(rootState(state))).toEqual(call(2))
      // An interruption is a single Call, not the whole result set: `next`
      // returns to the live feed rather than walking the archive.
      expect(selectHasNext(rootState(state))).toBe(false)
      expect(selectHasPrevious(rootState(state))).toBe(false)

      state = playbackReducer(state, next())
      expect(selectIsInterrupting(rootState(state))).toBe(false)
      expect(selectCurrentCall(rootState(state))).toBeNull()
      expect(selectPlaybackMode(rootState(state))).toBe('live')
    })

    it('ends the interruption on stop', () => {
      const state = reduce(playResults({ results, index: 0 }), stop())

      expect(selectIsInterrupting(rootState(state))).toBe(false)
      expect(selectCurrentCall(rootState(state))).toBeNull()
    })

    /** Nothing precedes an interruption, so "previous" has nothing to mean —
     *  it must not silently double as "back to the live feed". */
    it('ignores previous during an interruption', () => {
      const interrupting = reduce(playResults({ results, index: 0 }))

      expect(playbackReducer(interrupting, previous())).toEqual(interrupting)
    })
  })

  /** #14: what the player warms the HTTP cache with while the current Call
   *  plays, so the next one starts without a network round trip. */
  describe('the next call (prefetch target)', () => {
    it('is the following loaded result', () => {
      const state = reduce(enterPlaybackMode(), playResults({ results, index: 1 }))

      expect(selectNextCall(rootState(state))).toEqual(call(3))
    })

    it('is nothing at the end of the loaded results, or when idle', () => {
      const last = reduce(enterPlaybackMode(), playResults({ results, index: 2 }))
      expect(selectNextCall(rootState(last))).toBeNull()

      expect(selectNextCall(rootState(reduce(enterPlaybackMode())))).toBeNull()
    })

    /** An interruption is one Call and hands back to the live feed after it, so
     *  there is nothing archived to warm. */
    it('is nothing during an interruption', () => {
      const state = reduce(playResults({ results, index: 0 }))

      expect(selectNextCall(rootState(state))).toBeNull()
    })
  })

  /** #32: the boundary between pages is the one transition costing a search
   *  *and* a cold audio fetch, so the screen needs telling before playback gets
   *  there. This selector is only the "the lead has run short" half — whether a
   *  next page exists at all is the screen's to know. */
  describe('nearing the end of the loaded page', () => {
    /** A page with room to spare either side of the threshold. */
    const page = Array.from({ length: 10 }, (_, index) => call(index + 1))

    it.each([
      [6, 3, false],
      [7, 2, true],
      [8, 1, true],
      [9, 0, true],
    ])(
      'at index %i, %i Calls remain -> %s',
      (index, remaining, expected) => {
        const state = reduce(
          enterPlaybackMode(),
          playResults({ results: page, index }),
        )

        expect(selectRemainingInPage(rootState(state))).toBe(remaining)
        expect(selectIsNearingPageEnd(rootState(state))).toBe(expected)
      },
    )

    it('is false while nothing is playing', () => {
      const idle = reduce(enterPlaybackMode())

      expect(selectRemainingInPage(rootState(idle))).toBe(0)
      expect(selectIsNearingPageEnd(rootState(idle))).toBe(false)
    })

    /** Live feed on means the archived Call *interrupts* — one Call, no run to
     *  page through, so fetching a next page would be a request for nothing. */
    it('is false during an interruption, however short the run looks', () => {
      const state = reduce(playResults({ results, index: 0 }))

      expect(selectIsNearingPageEnd(rootState(state))).toBe(false)
    })

  })

  describe('switching modes', () => {
    it('leaving playback mode drops the archive queue so the live feed owns audio', () => {
      const state = reduce(
        enterPlaybackMode(),
        playResults({ results, index: 1 }),
        enterLiveFeed(),
      )

      expect(selectPlaybackMode(rootState(state))).toBe('live')
      expect(selectCurrentCall(rootState(state))).toBeNull()
      expect(selectPlaybackPosition(rootState(state))).toEqual({ index: -1, total: 0 })
      expect(selectIsInterrupting(rootState(state))).toBe(false)
    })

    it('entering playback mode clears an in-flight interruption', () => {
      const state = reduce(playResults({ results, index: 0 }), enterPlaybackMode())

      expect(selectIsInterrupting(rootState(state))).toBe(false)
      expect(selectCurrentCall(rootState(state))).toBeNull()
    })

    it('re-entering the mode already active changes nothing', () => {
      const playing = reduce(enterPlaybackMode(), playResults({ results, index: 1 }))
      expect(playbackReducer(playing, enterPlaybackMode())).toEqual(playing)

      const live = reduce({ type: '@@INIT' })
      expect(playbackReducer(live, enterLiveFeed())).toEqual(live)
    })
  })
})

describe('encrypted results (#42, spec US 9)', () => {
  /** A metadata-only Call, exactly as the archive serves one: flagged, and
   *  carrying no `audioUrl` because there is no audio to fetch. */
  function encrypted(id: number): Call {
    const { audioUrl: _none, ...rest } = call(id)
    return { ...rest, encrypted: true }
  }

  it('never walks a playback run onto a Call it cannot play', () => {
    // The audio element gets `src={undefined}`, so it never fires `ended` — the
    // only thing that advances a run. Playback mode would stop dead on the
    // first encrypted result and look like a bug in the player.
    const state = reduce(
      enterPlaybackMode(),
      playResults({ results: [call(1), encrypted(2), call(3)], index: 0 }),
      next(),
    )

    expect(selectCurrentCall(rootState(state))?.id).toBe(3)
  })

  it('runs out cleanly when everything left is unplayable', () => {
    const state = reduce(
      enterPlaybackMode(),
      playResults({ results: [call(1), encrypted(2)], index: 0 }),
      next(),
    )

    expect(selectIsExhausted(rootState(state))).toBe(true)
  })

  it('walks backwards past one too', () => {
    const state = reduce(
      enterPlaybackMode(),
      playResults({ results: [call(1), encrypted(2), call(3)], index: 2 }),
      previous(),
    )

    expect(selectCurrentCall(rootState(state))?.id).toBe(1)
  })

  it('refuses to start a run on one at all', () => {
    // Nothing offers this — the archive row has no play button — but a run
    // that began on an unplayable Call would be stuck before it started.
    const state = reduce(
      enterPlaybackMode(),
      playResults({ results: [call(1), encrypted(2)], index: 1 }),
    )

    expect(selectCurrentCall(rootState(state))).toBeNull()
  })
})
