import { describe, expect, it } from 'vitest'

import type { Call } from '@/types'

import { enterPlaybackMode } from './playback'
import {
  HISTORY_DEPTH,
  QUEUE_LIMIT,
  advance,
  avoid,
  clearAvoids,
  connected,
  connecting,
  disconnected,
  expireAvoids,
  lagged,
  liveReducer,
  received,
  replay,
  selectAvoidedCount,
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
  selectQueueDepth,
  selectSelection,
  selectSince,
  toggleHoldSystem,
  toggleHoldTalkgroup,
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

/** Play `call` and let it arrive as the live feed would. */
const arrive = (...calls: Call[]) => calls.map((one) => received({ call: one }))

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
     *  can backfill what it missed (ADR-0004 catch-up). */
    it('tracks the highest Call id it has seen', () => {
      const state = reduce(...arrive(call(7), call(9), call(8)))

      expect(selectSince(rootState(state))).toBe(9)
    })

    it('counts Calls the server says it dropped, so the UI can admit the gap', () => {
      const state = reduce(lagged(12), lagged(3))

      expect(selectMissed(rootState(state))).toBe(15)
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
      expect(state.queue.at(-1)).toEqual(flood.at(-1))
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
  })

  /** ADR-0004: catch-up delivery is *at-least-once*, so a Call ingested in the
   *  window between connect and the backfill query arrives twice. Hearing it
   *  twice is what the client dedups away. */
  describe('a Call that arrives twice', () => {
    it('is played once', () => {
      const state = reduce(...arrive(call(1), call(2)), received({ call: call(2) }))

      expect(selectQueueDepth(rootState(state))).toBe(1)
      expect(selectLiveCall(rootState(state))).toEqual(call(1))
    })

    /** Concurrent ingests can broadcast out of id order (ADR-0004), so ids are
     *  matched as a set — a high-water mark would drop the late arrival. */
    it('does not drop a Call that simply arrived out of order', () => {
      const state = reduce(...arrive(call(9), call(4)))

      expect(selectQueueDepth(rootState(state))).toBe(1)
      expect(state.queue[0]).toEqual(call(4))
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
      expect(state.queue[0]).toEqual(call(3, 11, 200))
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
  it('goes quiet when the listener switches to playback mode', () => {
    const state = reduce(...arrive(call(1), call(2)), enterPlaybackMode())

    expect(selectLiveCall(rootState(state))).toBeNull()
    expect(selectQueueDepth(rootState(state))).toBe(0)
    // The catch-up cursor survives, so returning to the feed doesn't refetch
    // the world.
    expect(selectSince(rootState(state))).toBe(2)
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
      received({ call: patched }),
    )

    expect(selectLiveCall(rootState(state))).toEqual(patched)
  })
})

describe('the selection as the panel draws it', () => {
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
