import { describe, expect, it } from 'vitest'

import type { Call } from '@/types'

import { enterLiveFeed, enterPlaybackMode } from './playback'
import {
  AVOID_UNDO_MS,
  HISTORY_DEPTH,
  QUEUE_LIMIT,
  SESSION_LOG_LIMIT,
  advance,
  avoid,
  avoidTalkgroup,
  clearAvoid,
  clearAvoids,
  dismissAvoidUndo,
  dropQueued,
  jumpToNewest,
  playQueued,
  togglePriority,
  togglePriorityShown,
  toggleHoldOn,
  undoAvoid,
  connected,
  connecting,
  disconnected,
  expireAvoids,
  lagged,
  liveReducer,
  received,
  replay,
  selectAvoidUndo,
  selectAvoidedCount,
  selectAvoids,
  selectHistory,
  selectHold,
  selectIsAvoided,
  selectLiveCall,
  chooseEverything,
  chooseSystem,
  chooseTalkgroups,
  selectAudibleSelection,
  selectLiveMatrix,
  selectLiveStatus,
  selectMissed,
  selectPlayId,
  selectPriority,
  selectIsPriority,
  selectQueue,
  selectQueueDepth,
  selectSessionLog,
  selectFeedStatus,
  selectSelection,
  selectSince,
  toggleHoldSystem,
  toggleHoldTalkgroup,
  turnFeedOff,
  turnFeedOn,
  type LiveState,
} from './live'

function call(id: number, systemRef = 11, talkgroupRef = 100): Call {
  return {
    id,
    systemRef,
    talkgroupRef,
    talkgroupLabel: `Talkgroup ${talkgroupRef}`,
    audioUrl: `/api/call/${id}/audio`,
  }
}

/** Reduce a sequence of actions from the initial state. */
function reduce(...actions: Parameters<typeof liveReducer>[1][]): LiveState {
  return actions.reduce(
    (state, action) => liveReducer(state, action),
    undefined as unknown as LiveState,
  )
}

function rootState(live: LiveState) {
  return { live }
}

/** The moment every Call in this file arrives. Handed in rather than left to
 *  `Date.now()`, because an arriving Call judges itself against the **Avoid**
 *  deadlines in force (#91) — so a file that let the wall clock in would decide
 *  differently depending on when it ran. Every `until:` below is relative to
 *  this. */
const NOW = 1_000

/** Play `call` and let it arrive as the live feed would. */
/** Calls arriving live, each at the emission its id happens to match — these
 *  tests are about what the store does with a Call, not about the cursor. */
const arrive = (...calls: Call[]) => calls.map((one) => received(one, one.id, NOW))

/** A Call on an encrypted talkgroup: flagged, and with no audio to fetch — the
 *  shape `GET /api/calls` and the live feed both deliver for one (#42). */
function encryptedCall(id: number, talkgroupRef = 100): Call {
  const { audioUrl: _dropped, ...rest } = call(id, 11, talkgroupRef)
  return { ...rest, encrypted: true }
}

describe('live slice', () => {
  describe('the connection', () => {
    it('starts offline with nothing playing', () => {
      const state = reduce({ type: '@@INIT' })

      expect(selectLiveStatus(rootState(state))).toBe('offline')
      expect(selectLiveCall(rootState(state))).toBeNull()
      expect(selectQueueDepth(rootState(state))).toBe(0)
      expect(selectSince(rootState(state))).toBeUndefined()
    })

    it('reports connecting, connected, and the drop back to offline', () => {
      let state = reduce(connecting())
      expect(selectLiveStatus(rootState(state))).toBe('connecting')

      state = liveReducer(state, connected())
      expect(selectLiveStatus(rootState(state))).toBe('connected')

      state = liveReducer(state, disconnected())
      expect(selectLiveStatus(rootState(state))).toBe('offline')
    })

    /** The cursor the client hands back as `since` on reconnect, so the server
     *  can backfill what it missed (ADR-0004).
     *
     *  The **emission**, never the Call's id (#94): the Call arriving last here
     *  carries the *lowest* id, exactly as one a **Delay** held back would — and
     *  a cursor over ids would ask the server to send it all over again. */
    it('tracks the highest emission it has been sent', () => {
      const state = reduce(
        received(call(7), 1, NOW),
        received(call(9), 2, NOW),
        received(call(2), 3, NOW),
      )

      expect(selectSince(rootState(state))).toBe(3)
    })

    it('counts Calls the server says it dropped, so the UI can admit the gap', () => {
      const state = reduce(lagged(12), lagged(3))

      expect(selectMissed(rootState(state))).toBe(15)
    })
  })

  /**
   * Feed off (#80) — the hard off rdio's LIVE FEED button has and Pause is not.
   *
   * Pause is transport-only: the subscription keeps streaming and the queue
   * keeps filling. Off means off (CONTEXT.md **Feed off**): the Call playing
   * stops, the queue clears, the socket closes, and rejoining starts from now.
   */
  describe('feed off (#80)', () => {
    it('is on until the listener says otherwise, so nobody has to opt in', () => {
      const state = reduce(connected())

      expect(selectFeedStatus(rootState(state))).toBe('live')
    })

    /** A deliberate off is not "missed". The counter admits traffic the listener
     *  *wanted* and did not get — dropped by a lagged notice or the queue cap —
     *  so charging them for silence they chose would make it a lie. */
    it('stops the Call and clears the queue without counting it missed', () => {
      const listening = reduce(...arrive(call(1), call(2), call(3)), lagged(4))
      expect(selectQueueDepth(rootState(listening))).toBe(2)

      const off = liveReducer(listening, turnFeedOff())

      expect(selectFeedStatus(rootState(off))).toBe('off')
      expect(selectLiveCall(rootState(off))).toBeNull()
      expect(selectQueueDepth(rootState(off))).toBe(0)
      expect(selectMissed(rootState(off))).toBe(4)
    })

    /** Rejoining starts from **now**. Keeping the cursor would make turning the
     *  feed back on replay the silence the listener asked for — the opposite of
     *  what they chose, and a burst of stale audio on a phone. */
    it('drops the catch-up cursor, so coming back never backfills the silence', () => {
      const state = reduce(...arrive(call(7), call(9)), turnFeedOff())

      expect(selectSince(rootState(state))).toBeUndefined()
    })

    /** Nothing arriving is playable while off — a Call in flight when the socket
     *  closed, or one the server sent before it noticed. */
    it('ignores a Call that arrives anyway', () => {
      const state = reduce(turnFeedOff(), ...arrive(call(1)))

      expect(selectLiveCall(rootState(state))).toBeNull()
      expect(selectQueueDepth(rootState(state))).toBe(0)
      expect(selectMissed(rootState(state))).toBe(0)
    })

    it('comes back on, and the feed can fill again', () => {
      let state = reduce(connected(), ...arrive(call(1)), turnFeedOff())
      state = liveReducer(state, turnFeedOn())

      expect(selectFeedStatus(rootState(state))).toBe('live')
      state = liveReducer(state, received(call(5), 5))
      expect(selectLiveCall(rootState(state))).toEqual(call(5))
    })

    /** Nothing plays while off, by any route. Replay is reachable from three
     *  places the disabled button does not cover — the RECENT list, the
     *  lock-screen previous button (`previousCall`), and Control Center — and a
     *  Call started from any of them would be audio playing under a FEED OFF
     *  header with the socket shut. */
    it('will not replay a Call from history while off', () => {
      let state = reduce(...arrive(call(1), call(2)), advance())
      expect(selectHistory(rootState(state))).toEqual([call(1)])

      state = liveReducer(state, turnFeedOff())
      state = liveReducer(state, replay(1))

      expect(selectLiveCall(rootState(state))).toBeNull()
    })

    /** The Call cut off is still filed under history, so it is there to replay
     *  once the feed is back on — being switched off is not the same as never
     *  having heard it. */
    it('keeps what it cut off replayable for when the feed returns', () => {
      let state = reduce(...arrive(call(1)), turnFeedOff())
      expect(selectHistory(rootState(state))).toEqual([call(1)])

      state = liveReducer(state, turnFeedOn())
      state = liveReducer(state, replay(1))

      expect(selectLiveCall(rootState(state))).toEqual(call(1))
    })

    /** `missed` is a promise about traffic the listener wanted. A `lagged` notice
     *  can land after the switch — buffered on the socket, or in flight while it
     *  closes — and charging them for it would break that promise by the same
     *  reasoning that keeps the counter untouched by the switch itself. */
    it('does not count a lagged notice that lands after the switch', () => {
      const state = reduce(turnFeedOff(), lagged(9))

      expect(selectMissed(rootState(state))).toBe(0)
    })

    /** The Selection survives it. Off is a switch, not a reset — a Listener who
     *  spent time narrowing their Profile must not have to do it again. */
    it('leaves the Selection alone', () => {
      const chosen = reduce(
        chooseTalkgroups({
          keys: [{ systemRef: 11, talkgroupRef: 100 }],
          on: false,
        }),
      )
      const before = selectSelection(rootState(chosen))

      const state = liveReducer(chosen, turnFeedOff())

      expect(selectSelection(rootState(state))).toEqual(before)
    })
  })

  describe('the listening queue (spec US 9-10)', () => {
    it('plays the first Call and queues the ones that arrive during it', () => {
      const state = reduce(...arrive(call(1), call(2), call(3)))

      expect(selectLiveCall(rootState(state))).toEqual(call(1))
      // The Q count a listener reads off the display.
      expect(selectQueueDepth(rootState(state))).toBe(2)
    })

    it('plays the queue in arrival order, remembering what it played', () => {
      let state = reduce(...arrive(call(1), call(2)))

      state = liveReducer(state, advance())
      expect(selectLiveCall(rootState(state))).toEqual(call(2))
      expect(selectQueueDepth(rootState(state))).toBe(0)
      expect(selectHistory(rootState(state))).toEqual([call(1)])

      // Nothing queued: the feed goes quiet but keeps its history.
      state = liveReducer(state, advance())
      expect(selectLiveCall(rootState(state))).toBeNull()
      expect(selectHistory(rootState(state))).toEqual([call(2), call(1)])
    })

    it('remembers only the last few Calls', () => {
      const many = Array.from({ length: HISTORY_DEPTH + 3 }, (_, at) => call(at + 1))
      const state = reduce(
        ...arrive(...many),
        ...Array.from({ length: many.length }, advance),
      )

      expect(selectHistory(rootState(state))).toHaveLength(HISTORY_DEPTH)
      // Newest first, so "replay the last one" is the top of the list.
      expect(selectHistory(rootState(state))[0]).toEqual(many.at(-1))
    })

    /** A phone that fell far behind must not grow an unbounded queue. The
     *  oldest waiting Calls go first — they are the stalest — and they're
     *  counted as missed rather than vanishing quietly. */
    it('caps the queue, dropping the stalest and admitting it', () => {
      const flood = Array.from({ length: QUEUE_LIMIT + 5 }, (_, at) => call(at + 1))
      const state = reduce(...arrive(...flood))

      expect(selectQueueDepth(rootState(state))).toBe(QUEUE_LIMIT)
      expect(selectMissed(rootState(state))).toBe(4)
      // The newest Call survived; the oldest queued ones did not.
      expect(selectQueue(rootState(state)).at(-1)).toEqual(flood.at(-1))
    })

    it('ignores an advance while nothing is playing', () => {
      const idle = reduce(connected())

      expect(liveReducer(idle, advance())).toEqual(idle)
    })
  })

  describe('replay (spec US 13)', () => {
    it('replays a Call from the history without losing the queue', () => {
      let state = reduce(...arrive(call(1), call(2)), advance(), ...arrive(call(3)))
      expect(selectLiveCall(rootState(state))).toEqual(call(2))

      state = liveReducer(state, replay(1))

      expect(selectLiveCall(rootState(state))).toEqual(call(1))
      expect(selectQueueDepth(rootState(state))).toBe(1)
    })

    /** Replaying what's already playing has to restart it — the element only
     *  reloads when told, since the source hasn't changed. */
    it('restarts the current Call', () => {
      const playing = reduce(...arrive(call(1)))
      const again = liveReducer(playing, replay(1))

      expect(selectLiveCall(rootState(again))).toEqual(call(1))
      expect(selectPlayId(rootState(again))).toBeGreaterThan(
        selectPlayId(rootState(playing)),
      )
    })

    it('ignores a Call it no longer remembers', () => {
      const state = reduce(...arrive(call(1)))

      expect(liveReducer(state, replay(999))).toEqual(state)
    })

    /** Reaching back past what was playing has to keep it reachable, or a
     *  replay quietly costs the listener the Call they interrupted. */
    it('keeps the interrupted Call replayable in its turn', () => {
      let state = reduce(...arrive(call(1), call(2)), advance())
      expect(selectLiveCall(rootState(state))).toEqual(call(2))

      state = liveReducer(state, replay(1))

      expect(selectLiveCall(rootState(state))).toEqual(call(1))
      expect(selectHistory(rootState(state))[0]).toEqual(call(2))
    })

    /**
     * A Call that is playing is not "recently played".
     *
     * Replaying one from RECENT filed the Call being interrupted into history —
     * correctly — but never took the replayed Call *out* of it, so it sat in the
     * list while it was the thing playing.
     */
    it('takes the replayed Call out of RECENT, because it is playing now', () => {
      let state = reduce(...arrive(call(1), call(2)), advance())
      expect(selectHistory(rootState(state))).toEqual([call(1)])

      state = liveReducer(state, replay(1))

      expect(selectLiveCall(rootState(state))).toEqual(call(1))
      expect(selectHistory(rootState(state))).toEqual([call(2)])
    })

    /**
     * ...and the duplicate that followed from it.
     *
     * Left in history while playing, the Call was unshifted a *second* time when
     * it finished — so RECENT listed it twice, and `<li key={call.id}>` handed
     * React two children with the same key.
     */
    it('never lists the same Call twice in RECENT', () => {
      let state = reduce(...arrive(call(1), call(2)), advance(), replay(1))

      // Call 1 finishes with nothing queued behind it.
      state = liveReducer(state, advance())

      const history = selectHistory(rootState(state))
      expect(history.map((one) => one.id)).toEqual([1, 2])
    })

    /**
     * ...including when the feed has fallen quiet, which is its own path.
     *
     * With nothing playing there is no Call to file, so `replay` used to set
     * `current` directly and skip `play` altogether — and skip the invariant with
     * it. This is the commonest way a listener replays: the feed goes silent and
     * they reach for the last thing they heard.
     */
    it('takes it out of RECENT even with nothing playing', () => {
      let state = reduce(...arrive(call(1), call(2)), advance(), advance())
      expect(selectLiveCall(rootState(state))).toBeNull()
      expect(selectHistory(rootState(state))).toEqual([call(2), call(1)])

      state = liveReducer(state, replay(2))

      expect(selectLiveCall(rootState(state))).toEqual(call(2))
      expect(selectHistory(rootState(state))).toEqual([call(1)])

      // ...and it comes back exactly once when it finishes.
      state = liveReducer(state, advance())
      expect(selectHistory(rootState(state)).map((one) => one.id)).toEqual([2, 1])
    })

    /** Walked repeatedly — a listener going back and forth through the RECENT
     *  rows — the list stays exactly the Calls that are not playing, in the order
     *  they were left. Asserting the whole list rather than just its uniqueness, because a
     *  reordering would be a regression the set size cannot see. */
    it('stays correct when replay is walked repeatedly', () => {
      let state = reduce(...arrive(call(1), call(2), call(3)), advance(), advance())
      expect(selectHistory(rootState(state))).toEqual([call(2), call(1)])

      state = liveReducer(state, replay(1))
      expect(selectHistory(rootState(state))).toEqual([call(3), call(2)])

      state = liveReducer(state, replay(2))
      expect(selectHistory(rootState(state))).toEqual([call(1), call(3)])

      state = liveReducer(state, replay(1))
      expect(selectHistory(rootState(state))).toEqual([call(2), call(3)])
      expect(selectLiveCall(rootState(state))).toEqual(call(1))
    })

    /**
     * The cap still binds, and removing the replayed Call must not cost a slot.
     *
     * This is where the order inside `play` shows: the Call now playing is
     * filtered out *before* the list is trimmed. Trimming first would drop a
     * Call the listener could still have reached.
     */
    it('keeps RECENT full when replaying out of a full list', () => {
      // Seven Calls, six played: history is at its cap with a seventh playing.
      const many = Array.from({ length: HISTORY_DEPTH + 2 }, (_, at) => call(at + 1))
      let state = reduce(
        ...arrive(...many),
        ...Array.from({ length: HISTORY_DEPTH + 1 }, advance),
      )
      expect(selectHistory(rootState(state))).toHaveLength(HISTORY_DEPTH)
      const playing = selectLiveCall(rootState(state))!
      const oldest = selectHistory(rootState(state)).at(-1)!

      state = liveReducer(state, replay(oldest.id))

      expect(selectLiveCall(rootState(state))).toEqual(oldest)
      const ids = selectHistory(rootState(state)).map((one) => one.id)
      // Still full: the Call that was playing took the slot the replayed one
      // vacated.
      expect(ids).toHaveLength(HISTORY_DEPTH)
      expect(ids).toContain(playing.id)
      expect(ids).not.toContain(oldest.id)
      expect(new Set(ids).size).toBe(ids.length)
    })

    /** With the feed already quiet there is no Call to take the freed slot, so
     *  the list is one shorter — correct, and worth pinning so it is not read as
     *  the cap failing. */
    it('is one shorter when replaying with nothing playing', () => {
      const many = Array.from({ length: HISTORY_DEPTH + 2 }, (_, at) => call(at + 1))
      let state = reduce(
        ...arrive(...many),
        ...Array.from({ length: many.length }, advance),
      )
      expect(selectLiveCall(rootState(state))).toBeNull()
      expect(selectHistory(rootState(state))).toHaveLength(HISTORY_DEPTH)

      state = liveReducer(state, replay(selectHistory(rootState(state))[0].id))

      expect(selectHistory(rootState(state))).toHaveLength(HISTORY_DEPTH - 1)
    })
  })

  /** ADR-0004: catch-up delivery is *at-least-once*, so a Call ingested in the
   *  window between connect and the backfill query arrives twice. Hearing it
   *  twice is what the client dedups away. */
  describe('a Call that arrives twice', () => {
    it('is played once', () => {
      const state = reduce(...arrive(call(1), call(2)), received(call(2), 2))

      expect(selectQueueDepth(rootState(state))).toBe(1)
      expect(selectLiveCall(rootState(state))).toEqual(call(1))
    })

    /** Concurrent ingests can broadcast out of id order (ADR-0004), so ids are
     *  matched as a set — a high-water mark would drop the late arrival. */
    it('does not drop a Call that simply arrived out of order', () => {
      const state = reduce(...arrive(call(9), call(4)))

      expect(selectQueueDepth(rootState(state))).toBe(1)
      expect(selectQueue(rootState(state))[0]).toEqual(call(4))
    })
  })

  describe('hold (spec US 11)', () => {
    it('holds the current System and narrows the subscription to it', () => {
      const state = reduce(...arrive(call(1, 11, 100)), toggleHoldSystem())

      expect(selectHold(rootState(state))).toEqual({
        systemRef: 11,
        talkgroupRef: null,
      })
      // The client can't enumerate a System's Talkgroups, so it holds with the
      // wildcard the server understands (ADR-0004).
      expect(selectLiveMatrix(rootState(state))).toEqual({
        all: false,
        sel: { '11': { '*': true } },
      })
    })

    it('holds the current Talkgroup alone', () => {
      const state = reduce(...arrive(call(1, 11, 100)), toggleHoldTalkgroup())

      expect(selectHold(rootState(state))).toEqual({
        systemRef: 11,
        talkgroupRef: 100,
      })
      expect(selectLiveMatrix(rootState(state))).toEqual({
        all: false,
        sel: { '11': { '100': true } },
      })
    })

    it('restores the whole selection when the hold is released', () => {
      const state = reduce(
        ...arrive(call(1)),
        toggleHoldSystem(),
        toggleHoldSystem(),
      )

      expect(selectHold(rootState(state))).toBeNull()
      expect(selectLiveMatrix(rootState(state))).toEqual({ all: true, sel: {} })
    })

    it('releases a Talkgroup hold on a second press', () => {
      const state = reduce(
        ...arrive(call(1, 11, 100)),
        toggleHoldTalkgroup(),
        toggleHoldTalkgroup(),
      )

      expect(selectHold(rootState(state))).toBeNull()
      expect(selectLiveMatrix(rootState(state))).toEqual({ all: true, sel: {} })
    })

    it('swaps a System hold for a Talkgroup hold rather than stacking them', () => {
      const state = reduce(
        ...arrive(call(1, 11, 100)),
        toggleHoldSystem(),
        toggleHoldTalkgroup(),
      )

      expect(selectHold(rootState(state))).toEqual({
        systemRef: 11,
        talkgroupRef: 100,
      })
    })

    /** Holding means "only this, from now on" — Calls already waiting that the
     *  hold excludes are dropped, or the listener hears them anyway. */
    it('purges the queue of Calls the hold excludes', () => {
      const state = reduce(
        ...arrive(call(1, 11, 100), call(2, 22, 100), call(3, 11, 200)),
        toggleHoldSystem(),
      )

      expect(selectQueueDepth(rootState(state))).toBe(1)
      expect(selectQueue(rootState(state))[0]).toEqual(call(3, 11, 200))
    })

    it('turns away Calls the hold excludes', () => {
      const state = reduce(
        ...arrive(call(1, 11, 100)),
        toggleHoldTalkgroup(),
        ...arrive(call(2, 11, 999)),
      )

      expect(selectQueueDepth(rootState(state))).toBe(0)
    })

    it('does nothing when there is no Call to hold on to', () => {
      const idle = reduce(connected())

      expect(liveReducer(idle, toggleHoldSystem())).toEqual(idle)
      expect(liveReducer(idle, toggleHoldTalkgroup())).toEqual(idle)
    })

    it('releases a hold even after the Call that set it is gone', () => {
      const state = reduce(
        ...arrive(call(1)),
        toggleHoldSystem(),
        advance(),
        toggleHoldSystem(),
      )

      expect(selectHold(rootState(state))).toBeNull()
    })

    /**
     * **The Call being shown, not the one on the air** (#56).
     *
     * The display keeps the last Call up between transmissions, so a Listener
     * pressing *Hold TG* a second after a Talkgroup stops keying is pressing it
     * at the Call in front of them. Before this, the reducer read `current`,
     * found it null, and did nothing — while the button was still lit, because
     * a Hold is exactly the case #88's control gate could not refuse.
     */
    it('holds the last Call once it has finished, which is what the screen shows', () => {
      const state = reduce(...arrive(call(1, 11, 100)), advance(), toggleHoldTalkgroup())

      expect(selectHold(rootState(state))).toEqual({
        systemRef: 11,
        talkgroupRef: 100,
      })
    })

    it('holds the last Call’s System too', () => {
      const state = reduce(...arrive(call(1, 11, 100)), advance(), toggleHoldSystem())

      expect(selectHold(rootState(state))).toEqual({
        systemRef: 11,
        talkgroupRef: null,
      })
    })

    /** A Call on the air outranks the one behind it: the display is showing the
     *  new one, and that is what the button means. */
    it('holds what is playing rather than what was, when both exist', () => {
      const state = reduce(
        ...arrive(call(1, 11, 100), call(2, 11, 200)),
        advance(),
        toggleHoldTalkgroup(),
      )

      expect(selectHold(rootState(state))).toEqual({
        systemRef: 11,
        talkgroupRef: 200,
      })
    })
  })

  describe('avoid (spec US 14)', () => {
    it('mutes the current Talkgroup and moves on', () => {
      const state = reduce(
        ...arrive(call(1, 11, 100), call(2, 11, 200)),
        avoid({ until: 0 }),
      )

      // The avoided Call stops immediately — that's what the button is for.
      expect(selectLiveCall(rootState(state))).toEqual(call(2, 11, 200))
      expect(selectIsAvoided(rootState(state), 11, 100)).toBe(true)
      expect(selectIsAvoided(rootState(state), 11, 200)).toBe(false)
    })

    it('sends the avoidance as an exception to the rest of the selection', () => {
      const state = reduce(...arrive(call(1, 11, 100)), avoid({ until: 0 }))

      expect(selectLiveMatrix(rootState(state))).toEqual({
        all: true,
        sel: { '11': { '100': false } },
      })
    })

    it('keeps avoiding inside a held System', () => {
      const state = reduce(
        ...arrive(call(1, 11, 100), call(2, 11, 200)),
        toggleHoldSystem(),
        avoid({ until: 0 }),
      )

      expect(selectLiveMatrix(rootState(state))).toEqual({
        all: false,
        sel: { '11': { '*': true, '100': false } },
      })
    })

    it('turns away and purges Calls for an avoided Talkgroup', () => {
      const state = reduce(
        ...arrive(call(1, 11, 100), call(2, 11, 100), call(3, 11, 200)),
        avoid({ until: 0 }),
        ...arrive(call(4, 11, 100)),
      )

      expect(selectLiveCall(rootState(state))).toEqual(call(3, 11, 200))
      expect(selectQueueDepth(rootState(state))).toBe(0)
    })

    /** The timed cycle (30/60/120 min): the reducer takes the moment it lapses,
     *  never reads the clock itself. */
    it('lapses on its own once its time is up', () => {
      const avoided = reduce(...arrive(call(1, 11, 100)), avoid({ until: 5_000 }))
      expect(selectIsAvoided(rootState(avoided), 11, 100)).toBe(true)

      const early = liveReducer(avoided, expireAvoids(4_999))
      expect(selectIsAvoided(rootState(early), 11, 100)).toBe(true)

      const lapsed = liveReducer(avoided, expireAvoids(5_000))
      expect(selectIsAvoided(rootState(lapsed), 11, 100)).toBe(false)
      expect(selectLiveMatrix(rootState(lapsed))).toEqual({ all: true, sel: {} })
    })

    it('never lapses an indefinite avoid', () => {
      const state = reduce(
        ...arrive(call(1, 11, 100)),
        avoid({ until: 0 }),
        expireAvoids(Number.MAX_SAFE_INTEGER),
      )

      expect(selectIsAvoided(rootState(state), 11, 100)).toBe(true)
    })

    it('re-avoiding replaces the deadline rather than stacking one', () => {
      const state = reduce(
        ...arrive(call(1, 11, 100), call(2, 11, 100)),
        avoid({ until: 5_000 }),
        ...arrive(call(3, 11, 100)),
      )
      // Nothing for that talkgroup got through to be re-avoided…
      expect(selectLiveCall(rootState(state))).toBeNull()
      expect(state.avoided).toEqual({ '11:100': 5_000 })
    })

    it('does nothing when there is no Call to avoid', () => {
      const idle = reduce(connected())

      expect(liveReducer(idle, avoid({ until: 0 }))).toEqual(idle)
    })

    /** The reason this ticket exists, in one line: a chatty Talkgroup stops
     *  keying and *that* is the moment a Listener reaches for Avoid. It acts on
     *  the Call the display is still showing (#56). */
    it('avoids the last Call once it has finished, which is what the screen shows', () => {
      const state = reduce(...arrive(call(1, 11, 100)), advance(), avoid({ until: 0 }))

      expect(selectIsAvoided(rootState(state), 11, 100)).toBe(true)
    })

    /** An indefinite avoid has no deadline to lapse, so there has to be a way
     *  back — US 14's timed mode is the *optional* one. */
    it('can be lifted before its time', () => {
      const state = reduce(
        ...arrive(call(1, 11, 100)),
        avoid({ until: 0 }),
        clearAvoids(),
      )

      expect(selectAvoidedCount(rootState(state))).toBe(0)
      expect(selectLiveMatrix(rootState(state))).toEqual({ all: true, sel: {} })
    })

    it('gives up a Talkgroup hold when that same Talkgroup is avoided', () => {
      const state = reduce(
        ...arrive(call(1, 11, 100)),
        toggleHoldTalkgroup(),
        avoid({ until: 0 }),
      )

      expect(selectHold(rootState(state))).toBeNull()
      expect(selectLiveMatrix(rootState(state))).toEqual({
        all: true,
        sel: { '11': { '100': false } },
      })
    })
  })

  /** CONTEXT.md: the live feed and playback mode are mutually exclusive. Going
   *  to the archive silences the feed *and* stops the server sending it. */
  describe('playback mode, which the feed is mutually exclusive with', () => {
    it('goes quiet when the listener switches to playback mode', () => {
      const state = reduce(...arrive(call(1), call(2)), enterPlaybackMode())

      expect(selectLiveCall(rootState(state))).toBeNull()
      expect(selectQueueDepth(rootState(state))).toBe(0)
      // The catch-up cursor survives, so returning to the feed doesn't refetch
      // the world.
      expect(selectSince(rootState(state))).toBe(2)
    })

    /**
     * Emptying the queue on the way in is not enough (#88): the subscription
     * goes empty but the socket stays open, so a Call already in flight — or one
     * the server sent before it saw the new matrix — lands *after* the clear,
     * finds nothing playing, and starts playing over the archive the listener
     * chose. Mutual exclusion has to be a standing state, not a one-off sweep.
     */
    it('will not start a Call that lands after the switch', () => {
      const state = reduce(enterPlaybackMode(), ...arrive(call(1)))

      expect(selectLiveCall(rootState(state))).toBeNull()
      expect(selectQueueDepth(rootState(state))).toBe(0)
    })

    /** Returning to the live feed lifts it, or the feed would never come back. */
    it('takes the Calls again once the listener returns to the feed', () => {
      const state = reduce(enterPlaybackMode(), enterLiveFeed(), ...arrive(call(1)))

      expect(selectLiveCall(rootState(state))?.id).toBe(1)
    })
  })
})

describe('selection (spec US 19–22)', () => {
  const tg = (systemRef: number, talkgroupRef: number) => ({
    systemRef,
    talkgroupRef,
  })

  it('starts on everything, so a zero-config listener hears the first Call', () => {
    const state = reduce({ type: '@@INIT' })

    expect(selectSelection(rootState(state))).toEqual({ all: true, sel: {} })
    expect(selectLiveMatrix(rootState(state))).toEqual({ all: true, sel: {} })
  })

  it('asks the server to stop sending a deselected Talkgroup', () => {
    const state = reduce(chooseTalkgroups({ keys: [tg(11, 100)], on: false }))

    expect(selectLiveMatrix(rootState(state))).toEqual({
      all: true,
      sel: { '11': { '100': false } },
    })
  })

  /** The server stops sending, but Calls already queued would still play — so
   *  the same rule that shapes the matrix shapes the queue. */
  it('drops what is playing and what is queued for a deselected Talkgroup', () => {
    const state = reduce(
      ...arrive(call(1, 11, 100), call(2, 11, 100), call(3, 11, 200)),
      chooseTalkgroups({ keys: [tg(11, 100)], on: false }),
    )

    expect(selectLiveCall(rootState(state))).toEqual(call(3, 11, 200))
    expect(selectQueueDepth(rootState(state))).toBe(0)
  })

  it('refuses a Call for a deselected Talkgroup that arrives anyway', () => {
    const state = reduce(
      chooseTalkgroups({ keys: [tg(11, 100)], on: false }),
      ...arrive(call(1, 11, 100)),
    )

    expect(selectLiveCall(rootState(state))).toBeNull()
    expect(selectSince(rootState(state))).toBe(1)
  })

  it('turns a whole System off and on (spec US 21)', () => {
    const off = reduce(chooseSystem({ systemRef: 11, on: false }), ...arrive(call(1, 11, 100)))
    expect(selectLiveCall(rootState(off))).toBeNull()
    expect(selectLiveMatrix(rootState(off))).toEqual({
      all: true,
      sel: { '11': { '*': false } },
    })

    const on = liveReducer(off, chooseSystem({ systemRef: 11, on: true }))
    expect(selectLiveMatrix(rootState(on))).toEqual({ all: true, sel: {} })
  })

  it('turns everything off and on (spec US 21)', () => {
    const off = reduce(
      ...arrive(call(1, 11, 100)),
      chooseEverything(false),
    )
    expect(selectLiveCall(rootState(off))).toBeNull()
    expect(selectLiveMatrix(rootState(off))).toEqual({ all: false, sel: {} })

    expect(selectLiveMatrix(rootState(liveReducer(off, chooseEverything(true))))).toEqual({
      all: true,
      sel: {},
    })
  })

  /** Selecting a Talkgroup that is being avoided has to mean something, or the
   *  panel would show it on while the avoid kept it silent. rdio has no such
   *  contradiction to resolve — its avoid *is* its selection. */
  it('lifts an avoid on a Talkgroup that is selected again', () => {
    const state = reduce(
      ...arrive(call(1, 11, 100)),
      avoid({ until: 0 }),
      chooseTalkgroups({ keys: [tg(11, 100)], on: true }),
    )

    expect(selectIsAvoided(rootState(state), 11, 100)).toBe(false)
    expect(selectLiveMatrix(rootState(state))).toEqual({ all: true, sel: {} })
  })

  it('leaves avoids alone when a Talkgroup is deselected', () => {
    const state = reduce(
      ...arrive(call(1, 11, 100)),
      avoid({ until: 5_000 }),
      chooseTalkgroups({ keys: [tg(11, 200)], on: false }),
    )

    expect(selectIsAvoided(rootState(state), 11, 100)).toBe(true)
  })

  it('narrows a hold to what is selected inside it', () => {
    const state = reduce(
      ...arrive(call(1, 11, 100)),
      chooseTalkgroups({ keys: [tg(11, 200)], on: false }),
      toggleHoldSystem(),
    )

    expect(selectLiveMatrix(rootState(state))).toEqual({
      all: false,
      sel: { '11': { '*': true, '200': false } },
    })
  })

  /** Spec US 18: a patched Call reaches a listener subscribed to any Talkgroup
   *  in the patch, and the client must not throw away what the server
   *  deliberately sent. */
  it('keeps a patched Call the listener selected through the patch', () => {
    const patched: Call = { ...call(1, 11, 900), patches: [100] }
    const state = reduce(
      chooseEverything(false),
      chooseTalkgroups({ keys: [tg(11, 100)], on: true }),
      received(patched, 1),
    )

    expect(selectLiveCall(rootState(state))).toEqual(patched)
  })
})

describe('the selection as the panel draws it', () => {
  /** #91: the panel is drawn from this, and the Live screen dispatches several
   *  times a second while a Call plays. A selector that allocates a fresh
   *  matrix each call misses every reference comparison downstream, so the
   *  whole panel redraws precisely *because* audio is playing. */
  it('keeps its identity while nothing it reads has changed', () => {
    const state = rootState(reduce(...arrive(call(1, 11, 100)), avoid({ until: 0 })))

    expect(selectAudibleSelection(state)).toBe(selectAudibleSelection(state))
    expect(selectLiveMatrix(state)).toBe(selectLiveMatrix(state))
  })

  /** The Talkgroups panel shows what the listener will actually hear, so an
   *  avoided Talkgroup reads off there — but a **hold** is a temporary
   *  narrowing shown on the Live screen, and must not make the panel claim the
   *  listener deselected three-quarters of their systems. */
  it('layers avoids onto the selection but not the hold', () => {
    const state = reduce(
      ...arrive(call(1, 11, 100)),
      avoid({ until: 0 }),
      ...arrive(call(2, 11, 200)),
      toggleHoldSystem(),
    )

    expect(selectAudibleSelection(rootState(state))).toEqual({
      all: true,
      sel: { '11': { '100': false } },
    })
  })

  it('lets a System’s avoided Talkgroups back in when it is turned all on', () => {
    const state = reduce(
      ...arrive(call(1, 11, 100)),
      avoid({ until: 0 }),
      chooseSystem({ systemRef: 11, on: true }),
    )

    expect(selectIsAvoided(rootState(state), 11, 100)).toBe(false)
    expect(selectAudibleSelection(rootState(state))).toEqual({ all: true, sel: {} })
  })

  it('leaves another System’s avoids alone', () => {
    const state = reduce(
      ...arrive(call(1, 11, 100)),
      avoid({ until: 0 }),
      chooseSystem({ systemRef: 12, on: true }),
    )

    expect(selectIsAvoided(rootState(state), 11, 100)).toBe(true)
  })

  it('lets every avoided Talkgroup back in when everything is turned on', () => {
    const state = reduce(
      ...arrive(call(1, 11, 100)),
      avoid({ until: 0 }),
      chooseEverything(true),
    )

    expect(selectAvoidedCount(rootState(state))).toBe(0)
  })

  /** Turning things *off* is not a reason to forget an avoid: the timed one
   *  would come back on its own and the listener never said to. */
  it('keeps avoids when a System or everything is turned off', () => {
    const state = reduce(
      ...arrive(call(1, 11, 100)),
      avoid({ until: 5_000 }),
      chooseSystem({ systemRef: 11, on: false }),
      chooseEverything(false),
    )

    expect(selectIsAvoided(rootState(state), 11, 100)).toBe(true)
  })
})

describe('encrypted Calls (#42, spec US 9)', () => {
  it('shows the activity without ever making it the current Call', () => {
    // An encrypted Call has no audio to play. Making it `current` would leave
    // the audio element with no `src`, so it would never fire `ended` — and the
    // live feed would sit on it forever, silent, with the queue behind it
    // frozen. That is the failure this guards.
    const state = reduce(connected(), ...arrive(encryptedCall(1)))

    expect(selectLiveCall(rootState(state))).toBeNull()
    expect(selectQueueDepth(rootState(state))).toBe(0)
    expect(selectHistory(rootState(state)).map((one) => one.id)).toEqual([1])
  })

  it('never queues one behind a Call that is playing', () => {
    const state = reduce(
      connected(),
      ...arrive(call(1), encryptedCall(2), call(3)),
    )

    expect(selectLiveCall(rootState(state))?.id).toBe(1)
    expect(selectQueueDepth(rootState(state))).toBe(1)
    expect(selectHistory(rootState(state)).map((one) => one.id)).toEqual([2])
  })

  it('still advances the cursor, so a reconnect does not ask for it again', () => {
    const state = reduce(connected(), ...arrive(encryptedCall(7)))

    expect(selectSince(rootState(state))).toBe(7)
  })

  it('leaves one out of the history when the listener is not selected to it', () => {
    // Encrypted or not, a Call the listener did not ask for is not their
    // business — the Selection decides first.
    const state = reduce(
      connected(),
      chooseTalkgroups({ keys: [{ systemRef: 11, talkgroupRef: 100 }], on: false }),
      ...arrive(encryptedCall(1, 100)),
    )

    expect(selectHistory(rootState(state))).toEqual([])
  })
})

/**
 * #58 — the queue as a tool, Priority, the Avoid undo, and the session log.
 *
 * Everything below is the *slice*; `lib/queue.test.ts` owns the ordering
 * algebra and enumerates it, and the screens own the rendering. What is proven
 * here is the wiring the screens depend on and the ordering module cannot see:
 * which Call a control acts on, and what a change costs the queue in hand.
 */
describe('the queue as a tool (#58, spec US 24)', () => {
  /** Two Calls waiting behind one playing. */
  const listening = () => reduce(connected(), ...arrive(call(1), call(2), call(3)))

  it('shows what is waiting, in the order it will play', () => {
    const state = listening()

    expect(selectQueue(rootState(state)).map((one) => one.id)).toEqual([2, 3])
  })

  describe('playing one now', () => {
    it('takes it out of the queue and puts it on the air', () => {
      const state = liveReducer(listening(), playQueued(3))

      expect(selectLiveCall(rootState(state))?.id).toBe(3)
      expect(selectQueue(rootState(state)).map((one) => one.id)).toEqual([2])
    })

    /** The Call it interrupted is not lost — it goes where every displaced Call
     *  goes, so RECENT can replay it. */
    it('files the Call it interrupted under history', () => {
      const state = liveReducer(listening(), playQueued(3))

      expect(selectHistory(rootState(state))[0]?.id).toBe(1)
    })

    /** The Listener chose it. Counting it would make the missed number a lie in
     *  the other direction from the one it exists to prevent. */
    it('counts nothing as missed', () => {
      const state = liveReducer(listening(), playQueued(3))

      expect(selectMissed(rootState(state))).toBe(0)
    })

    /**
     * The moving-target case (#58's fifth criterion), at the reducer. Between
     * the tap and the dispatch the Call can have played, or a purge can have
     * taken it — and a reducer that guessed would put a *different* Call on the
     * air than the one the finger went down on.
     */
    it('does nothing at all for a Call the queue is no longer holding', () => {
      const before = listening()
      const state = liveReducer(before, playQueued(99))

      expect(state).toBe(before)
    })
  })

  describe('dropping one', () => {
    it('takes it out and leaves everything else alone', () => {
      const state = liveReducer(listening(), dropQueued(2))

      expect(selectQueue(rootState(state)).map((one) => one.id)).toEqual([3])
      expect(selectLiveCall(rootState(state))?.id).toBe(1)
    })

    /**
     * Deliberately not missed, and the rule is `turnFeedOff`'s: `missed` admits
     * traffic the Listener *wanted* and did not get. They read this one in the
     * sheet and let it go, one at a time. Jumping is the other case — there
     * they did not look, which is why that one counts.
     */
    it('counts nothing as missed, because they looked at it and let it go', () => {
      const state = liveReducer(listening(), dropQueued(2))

      expect(selectMissed(rootState(state))).toBe(0)
    })

    it('does nothing for a Call the queue is no longer holding', () => {
      const before = listening()

      expect(liveReducer(before, dropQueued(99))).toBe(before)
    })
  })

  describe('jumping to the newest', () => {
    it('plays the newest and gives up everything behind it', () => {
      const state = liveReducer(listening(), jumpToNewest())

      expect(selectLiveCall(rootState(state))?.id).toBe(3)
      expect(selectQueueDepth(rootState(state))).toBe(0)
    })

    /** The ticket's own words: "counted as missed, never silent". A tap that
     *  discards a backlog owes the Listener the number. */
    it('counts what it gave up as missed', () => {
      const state = liveReducer(listening(), jumpToNewest())

      expect(selectMissed(rootState(state))).toBe(1)
    })

    /**
     * *Newest* is the last to have arrived, which under **Priority** is not the
     * tail of the queue — the tail is the lowest-ranked Call there is. Jumping
     * to it would hand the Listener the stalest routine chatter in the queue.
     */
    it('is the last Call to arrive, not the last in play order', () => {
      const state = reduce(
        connected(),
        togglePriority('11:200'),
        ...arrive(call(1), call(2, 11, 200), call(3)),
      )
      // 2 is Priority, so it plays first: the queue stands [2, 3].
      expect(selectQueue(rootState(state)).map((one) => one.id)).toEqual([2, 3])

      const jumped = liveReducer(state, jumpToNewest())
      expect(selectLiveCall(rootState(jumped))?.id).toBe(3)
    })

    it('does nothing with an empty queue', () => {
      const before = reduce(connected(), ...arrive(call(1)))

      expect(liveReducer(before, jumpToNewest())).toBe(before)
    })
  })
})

describe('Priority (#58, spec US 27)', () => {
  const priority = '11:200'

  it('is nothing until the Listener marks something', () => {
    const state = reduce({ type: '@@INIT' })

    expect(selectPriority(rootState(state))).toEqual([])
    expect(selectIsPriority(rootState(state), 11, 200)).toBe(false)
  })

  it('marks and unmarks a Talkgroup', () => {
    const on = reduce(togglePriority(priority))
    expect(selectIsPriority(rootState(on), 11, 200)).toBe(true)

    expect(selectIsPriority(rootState(liveReducer(on, togglePriority(priority))), 11, 200)).toBe(
      false,
    )
  })

  /** CONTEXT.md **Priority**: "makes its calls jump the listening queue instead
   *  of waiting their turn". */
  it('puts an arriving Priority Call ahead of the routine traffic waiting', () => {
    const state = reduce(
      connected(),
      togglePriority(priority),
      ...arrive(call(1), call(2), call(3, 11, 200)),
    )

    expect(selectQueue(rootState(state)).map((one) => one.id)).toEqual([3, 2])
  })

  /**
   * The reason #95 handed this ticket an arrival stamp. A Listener reaches for
   * Priority precisely when they are already far behind — so a promotion that
   * only applied to Calls arriving *after* it would do nothing on screen at the
   * one moment it was asked for.
   */
  it('lifts Calls already waiting when the Talkgroup is promoted', () => {
    const behind = reduce(
      connected(),
      ...arrive(call(1), call(2), call(3, 11, 200), call(4)),
    )
    expect(selectQueue(rootState(behind)).map((one) => one.id)).toEqual([2, 3, 4])

    const state = liveReducer(behind, togglePriority(priority))
    expect(selectQueue(rootState(state)).map((one) => one.id)).toEqual([3, 2, 4])
  })

  /**
   * The direction the stamp was actually needed for. A demoted Call falls back
   * among Calls that are now all tied with it, and only its arrival ordinal can
   * say where — position would leave it at the head its old Priority won it.
   */
  it('drops a demoted Call back to where it arrived, not where it sat', () => {
    const state = reduce(
      connected(),
      togglePriority(priority),
      ...arrive(call(1), call(2), call(3, 11, 200), call(4)),
    )
    expect(selectQueue(rootState(state)).map((one) => one.id)).toEqual([3, 2, 4])

    const off = liveReducer(state, togglePriority(priority))
    expect(selectQueue(rootState(off)).map((one) => one.id)).toEqual([2, 3, 4])
  })

  /** The Live screen's own control, which acts on the Call the display is
   *  showing — the same subject *Hold* and *Avoid* use (#56), so three lit
   *  buttons cannot be about three different Calls. */
  it('is markable from the Call on the display', () => {
    const state = reduce(connected(), ...arrive(call(1, 11, 200)), togglePriorityShown())

    expect(selectIsPriority(rootState(state), 11, 200)).toBe(true)
  })

  /** It goes on meaning something after the transmission ends — that is what
   *  the display outliving the Call is for. */
  it('acts on the Call the display kept up after it ended', () => {
    const state = reduce(
      connected(),
      ...arrive(call(1, 11, 200)),
      advance(),
      togglePriorityShown(),
    )

    expect(selectIsPriority(rootState(state), 11, 200)).toBe(true)
  })

  it('does nothing with nothing on the display', () => {
    const before = reduce(connected())

    expect(liveReducer(before, togglePriorityShown())).toBe(before)
  })

  /** CONTEXT.md: "Queue order, not selection — a priority talkgroup still has
   *  to be selected to be heard." */
  it('does not make an unselected Talkgroup audible', () => {
    const state = reduce(
      connected(),
      chooseTalkgroups({ keys: [{ systemRef: 11, talkgroupRef: 200 }], on: false }),
      togglePriority(priority),
      ...arrive(call(1), call(2, 11, 200)),
    )

    expect(selectLiveCall(rootState(state))?.id).toBe(1)
    expect(selectQueueDepth(rootState(state))).toBe(0)
  })

  /**
   * The ordering, enumerated **through the reducer** — #58's third criterion
   * asks for it here and not only in `lib/queue`, which is where the algebra
   * and its 81×81 oracle live.
   *
   * What this layer adds is the wiring the pure module cannot see: that the
   * function the slice orders by really is built from the Talkgroups a Listener
   * marked, that it is applied on arrival *and* re-applied when the set changes,
   * and that the two agree. Every subset of three Talkgroups, each fed as four
   * arrivals and then re-marked — so a slice that ordered correctly on the way
   * in and forgot to re-order, or the reverse, fails on the subset that shows it.
   */
  const CHANNELS = [100, 200, 300] as const
  const SUBSETS = Array.from({ length: 8 }, (_, mask) =>
    CHANNELS.filter((_one, at) => (mask & (1 << at)) !== 0),
  ).map((marked) => [marked] as const)

  /** Highest Priority first, then arrival order — worked out independently of
   *  the code under test. */
  const expected = (calls: readonly Call[], marked: readonly number[]) =>
    [...calls]
      .sort(
        (a, b) =>
          Number(marked.includes(b.talkgroupRef)) -
            Number(marked.includes(a.talkgroupRef)) || a.id - b.id,
      )
      .map((one) => one.id)

  it.each(SUBSETS)('orders arrivals by the marked set: %j', (marked) => {
    // Ids are arrival order, and the channels cycle — so every marked set has
    // Calls on both sides of it, and the oracle can read staleness off the id.
    const heard = Array.from({ length: 6 }, (_at, index) =>
      call(index + 1, 11, CHANNELS[index % CHANNELS.length]),
    )
    const marks = marked.map((ref) => togglePriority(`11:${ref}`))

    // Marked first, then heard: the ordering `enqueue` applied on arrival.
    const onArrival = reduce(connected(), ...marks, ...arrive(...heard))
    // Heard first, then marked: the ordering `reorder` applied afterwards.
    const afterwards = reduce(connected(), ...arrive(...heard), ...marks)

    // The Call that was playing is off the queue either way; the rest is the
    // play order of what is left.
    const waiting = heard.slice(1)
    expect(selectQueue(rootState(onArrival)).map((one) => one.id)).toEqual(
      expected(waiting, marked),
    )
    expect(selectQueue(rootState(afterwards)).map((one) => one.id)).toEqual(
      expected(waiting, marked),
    )
  })

  /** A **Patch** reaches the channel it was patched onto, and Priority follows
   *  it — `lib/queue`'s rule, proven here to be the one the slice runs. */
  it('promotes a patched Call reaching a Priority Talkgroup', () => {
    const patched = { ...call(3), patches: [200] }
    const state = reduce(
      connected(),
      togglePriority(priority),
      ...arrive(call(1), call(2), patched),
    )

    expect(selectQueue(rootState(state)).map((one) => one.id)).toEqual([3, 2])
  })
})

describe('undoing an Avoid (#58, spec US 25)', () => {
  const heard = () => reduce(connected(), ...arrive(call(1)))

  it('offers nothing until something is avoided', () => {
    expect(selectAvoidUndo(rootState(heard()))).toBeNull()
  })

  it('offers an undo naming the Talkgroup that was silenced', () => {
    const state = liveReducer(heard(), avoid({ until: 0, at: NOW }))
    const offer = selectAvoidUndo(rootState(state))

    expect(offer?.key).toBe('11:100')
    expect(offer?.expiresAt).toBe(NOW + AVOID_UNDO_MS)
  })

  it('lets the Talkgroup back in', () => {
    const state = reduce(
      connected(),
      ...arrive(call(1)),
      avoid({ until: 0, at: NOW }),
      undoAvoid(),
    )

    expect(selectIsAvoided(rootState(state), 11, 100)).toBe(false)
    expect(selectAvoidUndo(rootState(state))).toBeNull()
  })

  /** Avoiding a Talkgroup you are holding releases the hold — the avoid is the
   *  newer intent. Undoing the avoid has to put the hold back, or the undo
   *  would be a half-undo that silently cost the Listener their hold. */
  it('restores the Hold the Avoid released', () => {
    const state = reduce(
      connected(),
      ...arrive(call(1)),
      toggleHoldTalkgroup(),
      avoid({ until: 0, at: NOW }),
      undoAvoid(),
    )

    expect(selectHold(rootState(state))).toEqual({ systemRef: 11, talkgroupRef: 100 })
  })

  /** Avoiding something already avoided — re-arming a lapsing 30-minute avoid
   *  as an indefinite one, say — must undo to the deadline that was there,
   *  never to "not avoided at all". */
  it('restores the deadline that was already in force', () => {
    const state = reduce(
      connected(),
      ...arrive(call(1)),
      avoid({ until: NOW + 60_000, at: NOW }),
      avoid({ until: 0, at: NOW }),
      undoAvoid(),
    )

    expect(selectAvoids(rootState(state))).toEqual({ '11:100': NOW + 60_000 })
  })

  /**
   * The window is eight seconds, and a Listener can place a **Hold** inside it —
   * from the Live controls, or from the session log's own quick action. Undo
   * puts back what the *Avoid* took, so it must not overwrite a later choice:
   * it fills a Hold that is missing and never replaces one that is there.
   */
  it('leaves a Hold placed since the Avoid alone', () => {
    const state = reduce(
      connected(),
      ...arrive(call(1), call(2, 11, 200)),
      toggleHoldTalkgroup(),
      avoid({ until: 0, at: NOW }),
      toggleHoldOn({ systemRef: 11, talkgroupRef: 200 }),
      undoAvoid(),
    )

    expect(selectHold(rootState(state))).toEqual({ systemRef: 11, talkgroupRef: 200 })
    expect(selectIsAvoided(rootState(state), 11, 100)).toBe(false)
  })

  it('is dismissable, which is what the grace window running out means', () => {
    const state = reduce(
      connected(),
      ...arrive(call(1)),
      avoid({ until: 0, at: NOW }),
      dismissAvoidUndo(),
    )

    expect(selectAvoidUndo(rootState(state))).toBeNull()
    expect(selectIsAvoided(rootState(state), 11, 100)).toBe(true)
  })

  /** One offer, and it names the last thing silenced — undoing an avoid two
   *  taps ago while the newer one stands would be an undo of something that is
   *  no longer on screen. */
  it('replaces the offer when a second Talkgroup is avoided', () => {
    const state = reduce(
      connected(),
      ...arrive(call(1), call(2, 11, 200)),
      avoid({ until: 0, at: NOW }),
      advance(),
      avoid({ until: 0, at: NOW }),
    )

    expect(selectAvoidUndo(rootState(state))?.key).toBe('11:200')
  })

  it('does nothing with no offer standing', () => {
    const before = heard()

    expect(liveReducer(before, undoAvoid())).toBe(before)
  })
})

describe('the Avoid sheet (#58, spec US 25)', () => {
  const twoAvoided = () =>
    reduce(
      connected(),
      ...arrive(call(1), call(2, 11, 200)),
      avoid({ until: 0, at: NOW }),
      advance(),
      avoid({ until: NOW + 60_000, at: NOW }),
    )

  it('lists every Avoid in force with the moment each lapses', () => {
    expect(selectAvoids(rootState(twoAvoided()))).toEqual({
      '11:100': 0,
      '11:200': NOW + 60_000,
    })
  })

  /** The individual removal spec US 25 asks for — `clearAvoids` is all or
   *  nothing, which is exactly the blunt instrument this replaces. */
  it('clears exactly one, leaving the rest in force', () => {
    const state = liveReducer(twoAvoided(), clearAvoid('11:100'))

    expect(selectAvoids(rootState(state))).toEqual({ '11:200': NOW + 60_000 })
  })

  it('ignores a Talkgroup that is not avoided', () => {
    const state = liveReducer(twoAvoided(), clearAvoid('11:999'))

    expect(selectAvoidedCount(rootState(state))).toBe(2)
  })
})

describe('the session log (#58, spec US 28)', () => {
  it('holds everything heard this session, newest first', () => {
    const state = reduce(connected(), ...arrive(call(1), call(2)), advance())

    expect(selectSessionLog(rootState(state)).map((one) => one.id)).toEqual([2, 1])
  })

  /** The point of it: RECENT reaches back five (spec US 13), and "what was that
   *  ten minutes ago" is further back than that. */
  it('reaches further back than RECENT does', () => {
    const many = Array.from({ length: HISTORY_DEPTH + 4 }, (_, at) => call(at + 1))
    const played = many.reduce(
      (sofar) => liveReducer(sofar, advance()),
      reduce(connected(), ...arrive(...many)),
    )

    expect(selectHistory(rootState(played))).toHaveLength(HISTORY_DEPTH)
    expect(selectSessionLog(rootState(played))).toHaveLength(many.length)
  })

  /**
   * Each Call once, in first-heard order. A replay is the Listener hearing it
   * again, not a new thing happening — and a log that moved it would answer
   * "what was that ten minutes ago" with a list that reorders itself under the
   * question. It is also what keeps `key={call.id}` unique (#82's lesson).
   */
  it('does not duplicate or reorder a Call that was replayed', () => {
    const state = reduce(connected(), ...arrive(call(1), call(2)), advance(), replay(1))

    expect(selectSessionLog(rootState(state)).map((one) => one.id)).toEqual([2, 1])
  })

  /**
   * The session log's own replay (#58, spec US 28) reaches further than RECENT
   * does, which is the whole point of having it — so `replay` looks the Call up
   * *there* rather than in the five-deep history.
   */
  it('replays a Call further back than RECENT could reach', () => {
    const many = Array.from({ length: HISTORY_DEPTH + 4 }, (_, at) => call(at + 1))
    const played = many.reduce(
      (sofar) => liveReducer(sofar, advance()),
      reduce(connected(), ...arrive(...many)),
    )
    const oldest = many[0]
    expect(selectHistory(rootState(played))).not.toContainEqual(oldest)

    const again = liveReducer(played, replay(oldest.id))
    expect(selectLiveCall(rootState(again))).toEqual(oldest)
  })

  /** An encrypted Call never plays (#42) but it is activity the Listener saw —
   *  RECENT shows it, and the session log is what RECENT is a window onto. */
  it('holds an encrypted Call, which is the only record it was busy', () => {
    const state = reduce(connected(), ...arrive(call(1), encryptedCall(2)))

    expect(selectSessionLog(rootState(state)).map((one) => one.id)).toEqual([2, 1])
  })

  it('is bounded, so a phone left on all day does not grow without limit', () => {
    const flood = Array.from({ length: SESSION_LOG_LIMIT + 20 }, (_, at) =>
      encryptedCall(at + 1),
    )
    const state = reduce(connected(), ...arrive(...flood))

    expect(selectSessionLog(rootState(state))).toHaveLength(SESSION_LOG_LIMIT)
    // The newest survive: the oldest is what a Listener is least likely to be
    // asking about.
    expect(selectSessionLog(rootState(state))[0]?.id).toBe(flood.length)
  })

  /** It is the *session*'s, not the feed's. Switching off, or going to the
   *  archive, empties the queue and the display and must not erase what was
   *  already heard. */
  it('survives the feed being switched off and playback mode', () => {
    const state = reduce(
      connected(),
      ...arrive(call(1), call(2)),
      advance(),
      turnFeedOff(),
      enterPlaybackMode(),
    )

    expect(selectSessionLog(rootState(state)).map((one) => one.id)).toEqual([2, 1])
  })

  describe('its quick actions act on the Call they were opened on', () => {
    const twoHeard = () =>
      reduce(connected(), ...arrive(call(1), call(2, 11, 200)), advance())

    it('holds the Talkgroup of a Call further down the log', () => {
      const state = liveReducer(twoHeard(), toggleHoldOn({ systemRef: 11, talkgroupRef: 100 }))

      expect(selectHold(rootState(state))).toEqual({ systemRef: 11, talkgroupRef: 100 })
    })

    it('releases a hold it already names, so the one control does both', () => {
      const held = liveReducer(twoHeard(), toggleHoldOn({ systemRef: 11, talkgroupRef: 100 }))
      const state = liveReducer(held, toggleHoldOn({ systemRef: 11, talkgroupRef: 100 }))

      expect(selectHold(rootState(state))).toBeNull()
    })

    /** A hold on a *different* Talkgroup is replaced rather than released —
     *  from a list, naming a channel means "hold this one". */
    it('moves a hold that names a different Talkgroup', () => {
      const held = liveReducer(twoHeard(), toggleHoldOn({ systemRef: 11, talkgroupRef: 200 }))
      const state = liveReducer(held, toggleHoldOn({ systemRef: 11, talkgroupRef: 100 }))

      expect(selectHold(rootState(state))).toEqual({ systemRef: 11, talkgroupRef: 100 })
    })

    it('avoids the Talkgroup of a Call further down the log, undoably', () => {
      const state = liveReducer(
        twoHeard(),
        avoidTalkgroup({ systemRef: 11, talkgroupRef: 100, until: 0, at: NOW }),
      )

      expect(selectIsAvoided(rootState(state), 11, 100)).toBe(true)
      expect(selectAvoidUndo(rootState(state))?.key).toBe('11:100')
    })
  })
})

describe('what an Avoid releases (#58)', () => {
  /** A **Hold** on the Talkgroup being silenced is a contradiction, and the
   *  Avoid is the newer intent. */
  it('releases a Hold on the Talkgroup it silences', () => {
    const state = reduce(
      connected(),
      ...arrive(call(1)),
      toggleHoldTalkgroup(),
      avoid({ until: 0, at: NOW }),
    )

    expect(selectHold(rootState(state))).toBeNull()
  })

  /**
   * A Ref is unique only within one System, and #58 lets an Avoid be placed on
   * a Talkgroup that is *not* the one on the display — so comparing bare Refs
   * would release a hold on another System's channel of the same number, which
   * on screen looks like the hold simply vanishing.
   */
  it('leaves a Hold on another System’s Talkgroup of the same number alone', () => {
    const held = reduce(connected(), ...arrive(call(1, 11, 100)), toggleHoldTalkgroup())
    expect(selectHold(rootState(held))).toEqual({ systemRef: 11, talkgroupRef: 100 })

    const state = liveReducer(
      held,
      avoidTalkgroup({ systemRef: 12, talkgroupRef: 100, until: 0, at: NOW }),
    )

    expect(selectHold(rootState(state))).toEqual({ systemRef: 11, talkgroupRef: 100 })
  })

  /** A System **Hold** carries no Talkgroup, so no Avoid can name it — and one
   *  released by an unrelated mute would be a narrowing that undid itself. */
  it('leaves a System Hold alone', () => {
    const state = reduce(
      connected(),
      ...arrive(call(1)),
      toggleHoldSystem(),
      avoidTalkgroup({ systemRef: 11, talkgroupRef: 200, until: 0, at: NOW }),
    )

    expect(selectHold(rootState(state))).toEqual({ systemRef: 11, talkgroupRef: null })
  })
})
