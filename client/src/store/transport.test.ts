import { describe, expect, it } from 'vitest'

import { ARCHIVE } from '@/test/handlers'
import type { Call } from '@/types'

import { advance, received, selectLiveCall, selectHistory } from './live'
import { enterPlaybackMode, playResults, selectCurrentCall } from './playback'
import { makeStore, type AppStore } from './store'
import {
  nextCall,
  pause,
  previousCall,
  progressed,
  resume,
  selectIsPaused,
  selectNowPlaying,
  selectProgress,
  selectSubscription,
  sourceChanged,
  togglePause,
} from './transport'

function call(id: number, systemRef = 11, talkgroupRef = 100): Call {
  return {
    id,
    systemRef,
    talkgroupRef,
    audioUrl: `/api/call/${id}/audio`,
  }
}

/** A store with the live feed playing `calls`, in order. */
function listening(...calls: Call[]): AppStore {
  const store = makeStore()
  for (const one of calls) store.dispatch(received({ call: one }))
  return store
}

describe('transport', () => {
  /** One element plays both sources (ADR-0005), so "what is playing" is one
   *  question. An archived Call is either interrupting the feed or playing with
   *  the feed off — either way it is what the listener hears. */
  describe('what is playing', () => {
    it('is the live Call when only the feed is running', () => {
      const store = listening(call(1))

      expect(selectNowPlaying(store.getState())).toEqual(call(1))
    })

    it('is the archived Call when one interrupts the feed', () => {
      const store = listening(call(1))

      store.dispatch(playResults({ results: ARCHIVE, index: 0 }))

      expect(selectNowPlaying(store.getState())).toEqual(ARCHIVE[0])
      // The feed's own Call is untouched underneath, and comes back after.
      expect(selectLiveCall(store.getState())).toEqual(call(1))
    })

    it('is nothing when neither has anything', () => {
      expect(selectNowPlaying(makeStore().getState())).toBeNull()
    })
  })

  describe('pause (spec US 15)', () => {
    it('is one flag, whichever source is playing', () => {
      const store = listening(call(1))

      store.dispatch(pause())
      expect(selectIsPaused(store.getState())).toBe(true)

      store.dispatch(resume())
      expect(selectIsPaused(store.getState())).toBe(false)
    })

    it('toggles from a single control', () => {
      const store = listening(call(1))

      store.dispatch(togglePause())
      expect(selectIsPaused(store.getState())).toBe(true)
      store.dispatch(togglePause())
      expect(selectIsPaused(store.getState())).toBe(false)
    })

    /** The element auto-plays whatever it is handed, so a pause that outlived
     *  the Call it applied to would leave the store claiming "paused" over
     *  audible audio. */
    it('lifts when the audio moves to another Call', () => {
      const store = listening(call(1))
      store.dispatch(pause())

      store.dispatch(sourceChanged())

      expect(selectIsPaused(store.getState())).toBe(false)
    })

    /** Pause suspends playback *without losing the queue* — Calls keep
     *  arriving and wait their turn. */
    it('keeps the queue filling while it holds', () => {
      const store = listening(call(1))
      store.dispatch(pause())

      store.dispatch(received({ call: call(2) }))

      expect(selectIsPaused(store.getState())).toBe(true)
      expect(selectNowPlaying(store.getState())).toEqual(call(1))
      expect(store.getState().live.queue).toHaveLength(1)
    })
  })

  describe('progress (what the waveform draws)', () => {
    it('follows the element', () => {
      const store = listening(call(1))

      store.dispatch(progressed({ position: 3, duration: 12 }))

      expect(selectProgress(store.getState())).toBeCloseTo(0.25)
    })

    it('reads as nothing before a duration is known', () => {
      const store = listening(call(1))

      store.dispatch(progressed({ position: 2, duration: Number.NaN }))

      expect(selectProgress(store.getState())).toBe(0)
    })

    it('starts over when the audio moves to another Call', () => {
      const store = listening(call(1))
      store.dispatch(progressed({ position: 3, duration: 12 }))

      store.dispatch(sourceChanged())

      expect(selectProgress(store.getState())).toBe(0)
    })
  })

  /** The lock screen and the in-app controls press the same buttons, and those
   *  buttons have to reach whichever source owns the audio (#14 + #11). */
  describe('routing the transport controls', () => {
    it('walks the live queue when the feed owns the audio', () => {
      const store = listening(call(1), call(2))

      store.dispatch(nextCall())

      expect(selectLiveCall(store.getState())).toEqual(call(2))
    })

    it('walks the archive results when an archived Call owns the audio', () => {
      const store = listening(call(1))
      store.dispatch(enterPlaybackMode())
      store.dispatch(playResults({ results: ARCHIVE, index: 0 }))

      store.dispatch(nextCall())

      expect(selectCurrentCall(store.getState())).toEqual(ARCHIVE[1])
    })

    /** There is no "previous" in a live feed — the Call before this one is
     *  history, so "previous" replays it (spec US 13). */
    it('replays the last Call when the feed owns the audio', () => {
      const store = listening(call(1), call(2))
      store.dispatch(advance())
      expect(selectHistory(store.getState())).toEqual([call(1)])

      store.dispatch(previousCall())

      expect(selectLiveCall(store.getState())).toEqual(call(1))
    })

    it('does nothing on previous when the feed has no history yet', () => {
      const store = listening(call(1))

      store.dispatch(previousCall())

      expect(selectLiveCall(store.getState())).toEqual(call(1))
    })

    it('steps back through the archive when it owns the audio', () => {
      const store = makeStore()
      store.dispatch(enterPlaybackMode())
      store.dispatch(playResults({ results: ARCHIVE, index: 1 }))

      store.dispatch(previousCall())

      expect(selectCurrentCall(store.getState())).toEqual(ARCHIVE[0])
    })

    it('does nothing at all when neither source is playing', () => {
      const store = makeStore()
      const before = store.getState()

      store.dispatch(nextCall())
      store.dispatch(previousCall())

      expect(store.getState().live).toEqual(before.live)
      expect(store.getState().playback).toEqual(before.playback)
    })
  })

  describe('what the server is asked for', () => {
    it('is the live selection while the feed is on', () => {
      const store = listening(call(1))

      expect(selectSubscription(store.getState())).toEqual({ all: true, sel: {} })
    })

    /** Playback mode turns the live feed off (CONTEXT.md), so the server should
     *  stop sending — not just the client stop playing. */
    it('is nothing at all in playback mode', () => {
      const store = listening(call(1))

      store.dispatch(enterPlaybackMode())

      expect(selectSubscription(store.getState())).toEqual({ all: false, sel: {} })
    })
  })
})
