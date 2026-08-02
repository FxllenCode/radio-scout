import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

import type { Call } from '@/types'

import { avoid, clearAvoids, received, selectAvoids, selectLiveMatrix } from './live'
import { makeStore, type AppStore } from './store'

const call = (talkgroupRef: number): Call => ({
  id: talkgroupRef,
  systemRef: 11,
  talkgroupRef,
  audioUrl: `/api/call/${talkgroupRef}/audio`,
})

/** A store that remembers nothing, so no test inherits another's Avoids. */
const scanner = (): AppStore => makeStore({ storage: undefined })

/** Avoid the Talkgroup `talkgroupRef` is on until `until`. */
function avoidUntil(store: AppStore, talkgroupRef: number, until: number) {
  store.dispatch(received(call(talkgroupRef), 1))
  store.dispatch(avoid({ until }))
}

/** What the server is being told to filter out — the exceptions the Avoids
 *  put on the subscription matrix. */
const filteredOut = (store: AppStore) => selectLiveMatrix(store.getState()).sel

describe('an Avoid lapsing on its own (#91, spec US 14)', () => {
  beforeEach(() => {
    vi.useFakeTimers()
    vi.setSystemTime(0)
  })
  afterEach(() => vi.useRealTimers())

  /** The store keeps its own clock for this, rather than the live-feed socket
   *  component sweeping every five seconds: an Avoid is a **deadline**, so
   *  there is exactly one moment worth waking for and it is known the moment
   *  the Listener sets it. */
  it('lets a timed Avoid back in at its deadline, and not before', async () => {
    const store = scanner()
    avoidUntil(store, 100, 30 * 60_000)

    await vi.advanceTimersByTimeAsync(30 * 60_000 - 1)
    expect(filteredOut(store)).toEqual({ '11': { '100': false } })

    await vi.advanceTimersByTimeAsync(1)
    expect(selectLiveMatrix(store.getState())).toEqual({ all: true, sel: {} })
  })

  /** Spec US 14's timed mode is the *optional* one: an indefinite Avoid has no
   *  moment to wake for, and must not be let back in by one. */
  it('never lets an indefinite Avoid lapse', async () => {
    const store = scanner()
    avoidUntil(store, 100, 0)

    await vi.advanceTimersByTimeAsync(24 * 60 * 60_000)

    expect(filteredOut(store)).toEqual({ '11': { '100': false } })
  })

  /** One timer, rescheduled — not one per Avoid. Each lapses at its own moment
   *  and the next is scheduled behind it. */
  it('lets several Avoids lapse in the order their deadlines fall', async () => {
    const store = scanner()
    avoidUntil(store, 100, 60 * 60_000)
    avoidUntil(store, 200, 30 * 60_000)
    avoidUntil(store, 300, 0)

    await vi.advanceTimersByTimeAsync(30 * 60_000)
    expect(selectAvoids(store.getState())).toEqual({ '11:100': 3_600_000, '11:300': 0 })

    await vi.advanceTimersByTimeAsync(30 * 60_000)
    expect(selectAvoids(store.getState())).toEqual({ '11:300': 0 })
  })

  /**
   * The belt to the clock's braces.
   *
   * A browser throttles a backgrounded tab's timers — heavily, on a phone — so
   * the clock above can wake long after its moment. The deadline is therefore
   * compared again wherever audibility is actually *asked*: a Call arriving
   * judges itself against the moment it arrived, not against whenever the map
   * was last swept. This is the half a timestamp buys that an interval could
   * not.
   */
  it('hears a Call whose Avoid is up even if the clock has not woken', () => {
    const store = scanner()
    avoidUntil(store, 100, 30 * 60_000)
    vi.setSystemTime(31 * 60_000)

    store.dispatch(received({ ...call(100), id: 999 }, 2))

    expect(store.getState().live.current?.id).toBe(999)
    // ...and the server is told, so it resumes sending the Talkgroup rather
    // than filtering out everything after this one.
    expect(selectLiveMatrix(store.getState())).toEqual({ all: true, sel: {} })
  })

  /** Nothing left to wake for. A timer still pending against a released Avoid
   *  would re-open a Talkgroup the Listener had since silenced again. */
  it('stops waiting once the Listener releases the Avoid themselves', async () => {
    const store = scanner()
    avoidUntil(store, 100, 30 * 60_000)
    store.dispatch(clearAvoids())
    avoidUntil(store, 200, 0)

    await vi.advanceTimersByTimeAsync(60 * 60_000)

    expect(selectAvoids(store.getState())).toEqual({ '11:200': 0 })
  })
})
