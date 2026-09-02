import { describe, expect, it } from 'vitest'

import { ARCHIVE, searchPage } from '@/test/handlers'
import type { Call } from '@/types'

import {
  advance,
  engageCatchup,
  received,
  selectLiveCall,
  selectHistory,
  turnFeedOff,
} from './live'
import {
  enterPlaybackMode,
  selectCurrentCall,
  startRun,
  toggleHurry,
} from './playback'
import { makeStore, type AppStore } from './store'
import {
  nextCall,
  pause,
  previousCall,
  progressed,
  resume,
  keepAliveExpired,
  selectIsBridging,
  selectIsPaused,
  selectIsHurrying,
  selectNowPlaying,
  selectProgress,
  selectSeek,
  seekTo,
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
  for (const one of calls) store.dispatch(received(one, one.id))
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

      store.dispatch(startRun({ search: {}, page: searchPage(), index: 0 }))

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

      store.dispatch(received(call(2), 2))

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
      store.dispatch(startRun({ search: {}, page: searchPage(), index: 0 }))

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
      store.dispatch(startRun({ search: {}, page: searchPage(), index: 1 }))

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

  /** Bridging the quiet between Calls, so iOS doesn't suspend the page and
   *  strand the queue (spec US 31; docs/research/ios-gap-bridging-mechanism.md).
   *  The rule is deliberately narrow: it costs the OS's power savings, so it
   *  runs only when someone is demonstrably listening. */
  describe('bridging the gap', () => {
    /** The feed played a Call and has nothing left — the gap the keep-alive
     *  exists for. */
    function inTheGap(): AppStore {
      const store = listening(call(1))
      store.dispatch(advance())
      return store
    }

    it('bridges once the feed has played something and run dry', () => {
      expect(selectIsBridging(inTheGap().getState())).toBe(true)
    })

    // Before the first Call there is no audio session to hold open, and no
    // gesture to have opened it — playing here would be refused, and would
    // leave the transport claiming to be paused before anything ever played.
    it('does not bridge before the feed has ever played a Call', () => {
      expect(selectIsBridging(makeStore().getState())).toBe(false)
    })

    it('does not bridge while a Call is actually playing', () => {
      expect(selectIsBridging(listening(call(1)).getState())).toBe(false)
    })

    // A listener who paused is not listening; holding the session open would
    // cost them battery for nothing.
    it('does not bridge while paused', () => {
      const store = inTheGap()

      store.dispatch(pause())

      expect(selectIsBridging(store.getState())).toBe(false)
    })

    /** Feed off is silence the listener *asked for* (#80), not a gap to be held
     *  open across. Bridging there would keep the audio session alive — blocking
     *  the very suspension that saves the battery — for someone who switched the
     *  feed off. It is the same reasoning as `paused`, and a stronger case:
     *  with the feed off the socket is gone too, so there is nothing coming that
     *  the held session could deliver. */
    it('does not bridge once the feed has been switched off', () => {
      const store = inTheGap()
      expect(selectIsBridging(store.getState())).toBe(true)

      store.dispatch(turnFeedOff())

      expect(selectIsBridging(store.getState())).toBe(false)
    })

    // Playback mode is a finite list the listener is walking, not a feed that
    // might speak again. Running out of it is an ending, not a gap.
    it('does not bridge in playback mode', () => {
      const store = inTheGap()

      store.dispatch(enterPlaybackMode())

      expect(selectIsBridging(store.getState())).toBe(false)
    })

    // Holding the session open forever is the one thing that would make us
    // worse than rdio on a phone. After a long enough lull we stop fighting
    // the OS and let it suspend us; nothing covers what follows (ADR-0014).
    it('stops bridging once the lull has gone on too long', () => {
      const store = inTheGap()

      store.dispatch(keepAliveExpired())

      expect(selectIsBridging(store.getState())).toBe(false)
    })

    it('bridges again as soon as the feed speaks after a timeout', () => {
      const store = inTheGap()
      store.dispatch(keepAliveExpired())

      store.dispatch(received(call(2), 2))
      store.dispatch(advance())

      expect(selectIsBridging(store.getState())).toBe(true)
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

  /**
   * The two levers `lib/catchup` owns — a raised rate and the **Quiet span**
   * trim — belong to whichever source owns the element (#59, #63).
   *
   * There are two flags because there are two things being hurried through:
   * **Catch-up** drains the listening queue and ends when it empties, and the
   * **DVR**'s runs to the end of a range. Asking one question of the transport
   * is what stops the player caring which.
   */
  describe('hurrying, whichever source owns the element', () => {
    it('is off with nothing asked for', () => {
      expect(selectIsHurrying(listening(call(1)).getState())).toBe(false)
    })

    it('is Catch-up while the feed owns the audio', () => {
      const store = listening(call(1), call(2))
      store.dispatch(engageCatchup())

      expect(selectIsHurrying(store.getState())).toBe(true)
    })

    it('is the Run\'s own lever while an archived Call owns it', () => {
      const store = listening(call(1))
      store.dispatch(enterPlaybackMode())
      store.dispatch(startRun({ search: {}, page: searchPage(), index: 0 }))

      expect(selectIsHurrying(store.getState())).toBe(false)
      store.dispatch(toggleHurry())
      expect(selectIsHurrying(store.getState())).toBe(true)
    })

    it("never lets one source's answer reach the other", () => {
      // A Listener catching up on the queue who opens a DVR must not find it
      // already running at 1.5x, and a DVR left hurrying must not speed up the
      // live feed it hands back to.
      const store = listening(call(1), call(2))
      store.dispatch(engageCatchup())
      store.dispatch(enterPlaybackMode())
      store.dispatch(startRun({ search: {}, page: searchPage(), index: 0 }))

      expect(selectIsHurrying(store.getState())).toBe(false)
    })
  })

  /**
   * Seeking inside a Call (#63) — the half of "seeking lands within calls" the
   * client owns. The other half is the range request the element makes, which
   * `src/serve.rs` has answered since #10.
   */
  describe('seeking within the Call on the element', () => {
    it('carries the second asked for, and moves the readout at once', () => {
      // Without moving `position` here the control would snap back to where the
      // element last reported until the next `timeupdate` — a scrubber that
      // fights the thumb.
      const store = listening(call(1))
      store.dispatch(progressed({ position: 1, duration: 30 }))

      store.dispatch(seekTo(12))

      expect(selectSeek(store.getState())?.toSeconds).toBe(12)
      expect(selectProgress(store.getState())).toBeCloseTo(12 / 30)
    })

    it('is a fresh request every time, even to the same second', () => {
      const store = listening(call(1))
      store.dispatch(seekTo(12))
      const first = selectSeek(store.getState())?.nonce
      store.dispatch(seekTo(12))

      expect(selectSeek(store.getState())?.nonce).not.toBe(first)
    })

    it('never lands on the Call after the one it was asked about', () => {
      const store = listening(call(1))
      store.dispatch(seekTo(12))

      store.dispatch(sourceChanged())

      expect(selectSeek(store.getState())).toBeNull()
    })

    it('cannot ask for a moment before the Call began', () => {
      const store = listening(call(1))
      store.dispatch(seekTo(-5))

      expect(selectSeek(store.getState())?.toSeconds).toBe(0)
    })
  })
})
