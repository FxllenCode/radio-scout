import { describe, expect, it } from 'vitest'

import { searchCall, searchPage } from '@/test/handlers'

import {
  enterLiveFeed,
  enterPlaybackMode,
  next,
  playbackReducer,
  previous,
  runPaged,
  searchChanged,
  selectCurrentCall,
  selectHasNext,
  selectHasPrevious,
  selectIsInterrupting,
  selectIsRolling,
  selectNextCall,
  selectPlaybackMode,
  selectPlaybackPosition,
  selectWantedPage,
  startRun,
  stop,
  type PlaybackState,
} from './playback'

const call = searchCall
const page = (partial: Parameters<typeof searchPage>[0] = {}) =>
  searchPage({ results: [call(1), call(2), call(3)], limit: 3, ...partial })

const NEWEST = { sort: 'newest' } as const

/** Reduce a sequence of actions from the initial state. */
function reduce(
  ...actions: Parameters<typeof playbackReducer>[1][]
): PlaybackState {
  return actions.reduce(
    (state, action) => playbackReducer(state, action),
    undefined as unknown as PlaybackState,
  )
}

/** Wrap slice state as the store shape the selectors read. */
function rootState(playback: PlaybackState) {
  return { playback }
}

/**
 * The slice (#13, #89).
 *
 * Every *transition* a Run makes belongs to `@/lib/run` and is tested there as
 * a table. What is left here is only what a pure Run cannot answer: which mode
 * the Listener is in, that each gesture reaches the Run, and that the selectors
 * read the Run's one answer back out.
 */
describe('playback slice', () => {
  it('starts on the live feed with no Run at all', () => {
    const state = reduce({ type: '@@INIT' })

    expect(selectPlaybackMode(rootState(state))).toBe('live')
    expect(selectCurrentCall(rootState(state))).toBeNull()
    expect(selectIsInterrupting(rootState(state))).toBe(false)
    expect(selectHasNext(rootState(state))).toBe(false)
    expect(selectHasPrevious(rootState(state))).toBe(false)
    expect(selectNextCall(rootState(state))).toBeNull()
    expect(selectWantedPage(rootState(state))).toBeNull()
    expect(selectIsRolling(rootState(state))).toBe(false)
    expect(selectPlaybackPosition(rootState(state))).toEqual({
      index: -1,
      total: 0,
    })
  })

  /**
   * The one decision the slice takes for itself: what the live feed is doing
   * turns the same gesture — tapping a search result — into either a walk
   * through the results or a single Call interrupting the feed (spec US 26).
   */
  describe('what tapping a result means', () => {
    it('walks the results while the live feed is off', () => {
      const state = reduce(
        enterPlaybackMode(),
        startRun({ search: NEWEST, page: page(), index: 1 }),
      )

      expect(selectIsInterrupting(rootState(state))).toBe(false)
      expect(selectCurrentCall(rootState(state))).toEqual(call(2))
      expect(selectHasNext(rootState(state))).toBe(true)
    })

    it('interrupts with just that Call while the live feed is on', () => {
      const state = reduce(startRun({ search: NEWEST, page: page(), index: 1 }))

      expect(selectPlaybackMode(rootState(state))).toBe('live')
      expect(selectIsInterrupting(rootState(state))).toBe(true)
      expect(selectCurrentCall(rootState(state))).toEqual(call(2))
      expect(selectHasNext(rootState(state))).toBe(false)
    })
  })

  /** Each gesture reaches the Run. What the Run then *does* is its own table;
   *  this is the wiring, which is all a slice can get wrong. */
  describe('the gestures reaching the Run', () => {
    const walking = reduce(
      enterPlaybackMode(),
      startRun({
        search: NEWEST,
        page: page({ count: 6, hasMore: true }),
        index: 1,
      }),
    )

    it('advances', () => {
      const state = playbackReducer(walking, next())

      expect(selectCurrentCall(rootState(state))).toEqual(call(3))
    })

    it('steps back', () => {
      const state = playbackReducer(walking, previous())

      expect(selectCurrentCall(rootState(state))).toEqual(call(1))
    })

    it('stops', () => {
      const state = playbackReducer(walking, stop())

      expect(selectCurrentCall(rootState(state))).toBeNull()
    })

    it('hears that the search changed', () => {
      const state = playbackReducer(
        walking,
        searchChanged({ sort: 'newest', tag: 'Fire Dispatch' }),
      )

      // Still playing what it holds; no longer asking for pages of a search the
      // Listener has moved on from.
      expect(selectCurrentCall(rootState(state))).toEqual(call(2))
      expect(selectWantedPage(rootState(state))).toBeNull()
    })

    it('takes the page it asked for, and reports where it is', () => {
      const rolled = playbackReducer(playbackReducer(walking, next()), next())
      expect(selectIsRolling(rootState(rolled))).toBe(true)
      expect(selectWantedPage(rootState(rolled))).toEqual({
        sort: 'newest',
        limit: 3,
        offset: 3,
      })

      const state = playbackReducer(
        rolled,
        runPaged({
          window: { ...NEWEST, limit: 3, offset: 3 },
          page: page({ results: [call(4)], count: 6, offset: 3 }),
        }),
      )

      expect(selectCurrentCall(rootState(state))).toEqual(call(4))
      expect(selectIsRolling(rootState(state))).toBe(false)
      expect(selectPlaybackPosition(rootState(state))).toEqual({
        index: 3,
        total: 6,
      })
    })
  })

  describe('switching modes', () => {
    it('leaving playback mode ends the Run so the live feed owns audio', () => {
      const state = reduce(
        enterPlaybackMode(),
        startRun({ search: NEWEST, page: page(), index: 1 }),
        enterLiveFeed(),
      )

      expect(selectPlaybackMode(rootState(state))).toBe('live')
      expect(selectCurrentCall(rootState(state))).toBeNull()
      expect(selectPlaybackPosition(rootState(state))).toEqual({
        index: -1,
        total: 0,
      })
    })

    it('entering playback mode clears an in-flight interruption', () => {
      const state = reduce(
        startRun({ search: NEWEST, page: page(), index: 0 }),
        enterPlaybackMode(),
      )

      expect(selectIsInterrupting(rootState(state))).toBe(false)
      expect(selectCurrentCall(rootState(state))).toBeNull()
    })

    it('re-entering the mode already active changes nothing', () => {
      const playing = reduce(
        enterPlaybackMode(),
        startRun({ search: NEWEST, page: page(), index: 1 }),
      )
      expect(playbackReducer(playing, enterPlaybackMode())).toEqual(playing)

      const live = reduce({ type: '@@INIT' })
      expect(playbackReducer(live, enterLiveFeed())).toEqual(live)
    })
  })

  /** The Run is one value, so the eight selectors that read it must not each
   *  rebuild their own — which is what makes every reader re-render on every
   *  action in the store (#91's finding, one slice over). */
  it('answers from one memoized view of the Run', () => {
    const state = rootState(
      reduce(
        enterPlaybackMode(),
        startRun({
          search: NEWEST,
          page: page({ count: 6, hasMore: true }),
          index: 0,
        }),
      ),
    )

    expect(selectWantedPage(state)).toBe(selectWantedPage(state))
  })
})
