import { describe, expect, it } from 'vitest'

import type { Call } from '@/types'

import {
  FIFO,
  enqueue,
  retain,
  takeNext,
  type PriorityOf,
  type QueuePolicy,
} from './queue'

/** A Call whose id is also its arrival order — which is what every assertion
 *  below reads staleness off, since a queued Call carries no arrival stamp of
 *  its own. */
function call(id: number, talkgroupRef = 100): Call {
  return {
    id,
    systemRef: 11,
    talkgroupRef,
    talkgroupLabel: `Talkgroup ${talkgroupRef}`,
    audioUrl: `/api/call/${id}/audio`,
  }
}

/** A Priority per arrival: `levels[0]` is the first Call's, and anything that
 *  arrived beyond the table is routine. */
const byArrival =
  (levels: readonly number[]): PriorityOf =>
  (one) =>
    levels[one.id - 1] ?? 0

/** Ids, which is all these assertions ever compare — a queue is a sequence of
 *  Calls, and which Calls in which order is the whole of it. */
const ids = (calls: readonly Call[]): number[] => calls.map((one) => one.id)

/**
 * Feed `count` Calls through [`enqueue`] in arrival order.
 *
 * The queue is built the way the slice builds it — one arrival at a time —
 * rather than assembled and then sorted, because the cap decides *per arrival*
 * and a queue assembled whole would never make that decision.
 */
function arrive(policy: QueuePolicy, count: number) {
  let queue: Call[] = []
  const dropped: Call[] = []
  for (let id = 1; id <= count; id += 1) {
    const after = enqueue(queue, call(id), policy)
    queue = after.queue
    dropped.push(...after.dropped)
  }
  return { queue, dropped }
}

/**
 * The play order the policy describes, worked out independently of the module:
 * highest **Priority** first, and within one Priority the stalest first.
 *
 * A whole oracle rather than a spot check — where nothing is dropped, this is
 * exactly what the queue must be.
 */
function playOrder(calls: readonly Call[], priorityOf: PriorityOf): Call[] {
  return [...calls].sort((a, b) => priorityOf(b) - priorityOf(a) || a.id - b.id)
}

/**
 * Every assignment of `values` across `length` slots — the whole space, not a
 * sample of it.
 *
 * This is the client's answer to the backend's `proptest` (ADR-0010 names no
 * property library for the frontend): the queue's input space is small enough
 * to enumerate *completely*, which is stronger than sampling it and has no seed
 * to report when it fails. The large-N behaviour the enumeration can't reach is
 * covered by the explicit cases beside it.
 */
function every<T>(values: readonly T[], length: number): T[][] {
  return length === 0
    ? [[]]
    : every(values, length - 1).flatMap((rest) => values.map((one) => [...rest, one]))
}

/**
 * Three levels over four arrivals: 81 queues, each of which is every
 * interleaving of routine and Priority traffic that four Calls can make.
 *
 * Fed through `it.each` rather than looped inside one `it`, so a failure names
 * the assignment that broke — which is the counterexample a property library
 * would have shrunk to, and the reason ADR-0010 asks for case *tables*.
 */
const SPACE = every([0, 1, 2], 4).map((levels) => [levels] as const)

describe('the queue plays in Priority order, stalest first (#95, spec US 27)', () => {
  it('is arrival order when nothing has Priority', () => {
    const { queue } = arrive({ limit: 10 }, 4)

    expect(ids(queue)).toEqual([1, 2, 3, 4])
  })

  /** CONTEXT.md **Priority**: "makes its calls jump the listening queue instead
   *  of waiting their turn". */
  it('puts an arriving Priority Call ahead of the routine traffic waiting', () => {
    const { queue } = arrive({ limit: 10, priorityOf: byArrival([0, 0, 1]) }, 3)

    expect(ids(queue)).toEqual([3, 1, 2])
  })

  /** Priority is queue order, not a scheduler: two Priority Calls are still
   *  first-come-first-served with respect to each other. */
  it('keeps arrival order among Calls of equal Priority', () => {
    const { queue } = arrive({ limit: 10, priorityOf: byArrival([0, 1, 0, 1]) }, 4)

    expect(ids(queue)).toEqual([2, 4, 1, 3])
  })

  it('orders higher Priority ahead of lower, not merely ahead of routine', () => {
    const { queue } = arrive({ limit: 10, priorityOf: byArrival([1, 0, 2]) }, 3)

    expect(ids(queue)).toEqual([3, 1, 2])
  })

  /** The complete oracle: with room for everything, the queue *is* the play
   *  order — for all 81 ways four Calls can be prioritized. */
  it.each(SPACE)('is the play order of everything that arrived: %j', (levels) => {
    const priorityOf = byArrival(levels)
    const { queue, dropped } = arrive({ limit: 4, priorityOf }, 4)

    expect(dropped).toEqual([])
    expect(ids(queue)).toEqual(ids(playOrder(queue, priorityOf)))
    expect(ids(queue).slice().sort()).toEqual([1, 2, 3, 4])
  })
})

describe('the cap truncates in the same order it plays (CONTEXT.md)', () => {
  /** Today's rule, and it must not move: with no Priority anywhere the queue is
   *  one band, so "the stalest of the lowest band" is "the stalest". */
  it('drops the stalest when nothing has Priority, exactly as it always did', () => {
    const { queue, dropped } = arrive({ limit: 3 }, 6)

    expect(ids(queue)).toEqual([4, 5, 6])
    expect(ids(dropped)).toEqual([1, 2, 3])
  })

  /**
   * The reason this ticket exists. Dropping the stalest was right while order
   * was arrival order; once Priority exists it discards the one Talkgroup the
   * Listener said mattered while routine chatter plays on.
   */
  it('drops routine traffic before Priority traffic, however stale the Priority', () => {
    // Call 1 is the stalest thing in the queue *and* the only Priority Call.
    const { queue, dropped } = arrive({ limit: 2, priorityOf: byArrival([1]) }, 4)

    expect(ids(queue)).toEqual([1, 4])
    expect(ids(dropped)).toEqual([2, 3])
  })

  it('drops the stalest within the band it is dropping from', () => {
    const { queue, dropped } = arrive(
      { limit: 3, priorityOf: byArrival([0, 1, 0, 1, 0]) },
      5,
    )

    // Priority: 2, 4. Routine: 1, 3, 5 — and 1 is the stalest of them.
    expect(ids(queue)).toEqual([2, 4, 5])
    expect(ids(dropped)).toEqual([1, 3])
  })

  /** A queue already full of Priority traffic does not make room for routine
   *  traffic by evicting the Priority — the arriving Call is what goes, and it
   *  is admitted as missed rather than vanishing. */
  it('drops the arriving Call when it is the lowest Priority thing there is', () => {
    const { queue, dropped } = arrive(
      { limit: 2, priorityOf: byArrival([1, 1, 0]) },
      3,
    )

    expect(ids(queue)).toEqual([1, 2])
    expect(ids(dropped)).toEqual([3])
  })

  it('reaches into the next band up once the lowest is exhausted', () => {
    const { queue, dropped } = arrive(
      { limit: 1, priorityOf: byArrival([2, 0, 1]) },
      3,
    )

    // Routine (2) goes first, then the middle band (3), leaving the top.
    expect(ids(queue)).toEqual([1])
    expect(ids(dropped)).toEqual([2, 3])
  })

  it('reports what it dropped, so nothing is lost silently', () => {
    const { dropped } = arrive({ limit: 1 }, 3)

    // The Calls themselves, not a count: the display admits them, and the
    // queue sheet (#58) has something to name.
    expect(dropped).toEqual([call(1), call(2)])
  })

  /**
   * The cap's rules, over the whole space at a limit that forces two drops on
   * every one of the 81 shapes.
   *
   * Invariants rather than an oracle, because the cap decides per arrival and a
   * queue computed whole would not reproduce the sequence of decisions.
   */
  it.each(SPACE)('never drops a Call that outranks one it kept: %j', (levels) => {
    const priorityOf = byArrival(levels)
    const { queue, dropped } = arrive({ limit: 2, priorityOf }, 4)

    expect(queue).toHaveLength(2)
    expect(dropped).toHaveLength(2)
    // Conservation: everything that arrived is in exactly one of the two.
    expect([...ids(queue), ...ids(dropped)].sort()).toEqual([1, 2, 3, 4])
    // Play order holds after truncation, not merely before it.
    expect(ids(queue)).toEqual(ids(playOrder(queue, priorityOf)))

    for (const gone of dropped) {
      for (const kept of queue) {
        // Lowest Priority first...
        expect(priorityOf(gone)).toBeLessThanOrEqual(priorityOf(kept))
        // ...then stalest: a kept Call of the same Priority arrived later.
        if (priorityOf(gone) === priorityOf(kept)) {
          expect(gone.id).toBeLessThan(kept.id)
        }
      }
    }
  })

  /** The enumeration reaches four Calls; the real ceiling is a hundred. */
  it('holds the queue at the limit however far behind the Listener falls', () => {
    const { queue, dropped } = arrive({ limit: 100 }, 250)

    expect(queue).toHaveLength(100)
    expect(dropped).toHaveLength(150)
    expect(ids(queue)[0]).toBe(151)
  })

  it('drops nothing while there is room', () => {
    const { queue, dropped } = arrive({ limit: 4 }, 4)

    expect(ids(queue)).toEqual([1, 2, 3, 4])
    expect(dropped).toEqual([])
  })
})

describe('taking the next Call', () => {
  /** The array *is* play order, so what plays next is the head — one statement
   *  rather than a rule the caller has to know. */
  it('takes the head and leaves the rest', () => {
    const { queue } = arrive({ limit: 10, priorityOf: byArrival([0, 1]) }, 3)
    const taken = takeNext(queue)

    expect(taken.next).toEqual(call(2))
    expect(ids(taken.queue)).toEqual([1, 3])
  })

  it('falls quiet on an empty queue', () => {
    const taken = takeNext([])

    expect(taken.next).toBeNull()
    expect(taken.queue).toEqual([])
  })

  /** Taking every Call out one at a time is the play order, for all 81 shapes —
   *  which is what makes "the queue plays in this order" a fact about playing
   *  rather than about the array's shape. */
  it.each(SPACE)('plays out in Priority order: %j', (levels) => {
    const priorityOf = byArrival(levels)
    let { queue } = arrive({ limit: 4, priorityOf }, 4)

    const played: Call[] = []
    for (let taken = takeNext(queue); taken.next; taken = takeNext(queue)) {
      played.push(taken.next)
      queue = taken.queue
    }

    expect(ids(played)).toEqual(ids(playOrder(played, priorityOf)))
    expect(played).toHaveLength(4)
  })
})

describe('retaining what the Selection still wants', () => {
  it('keeps what is wanted, in the order it was going to play', () => {
    const { queue } = arrive({ limit: 10, priorityOf: byArrival([0, 1, 0]) }, 3)

    expect(ids(retain(queue, (one) => one.id !== 1))).toEqual([2, 3])
  })

  /**
   * CONTEXT.md **Priority**: "Queue order, not selection — a priority talkgroup
   * still has to be selected to be heard."
   *
   * So retention is Priority-blind by construction: a Priority Call the
   * Listener has deselected, held away from, or avoided leaves the queue like
   * any other. The slice's other half of this — that an unwanted Call never
   * reaches [`enqueue`] at all — is `live.test.ts`'s.
   */
  it('drops a Priority Call the Listener no longer wants', () => {
    const { queue } = arrive({ limit: 10, priorityOf: byArrival([0, 9]) }, 2)
    expect(ids(queue)).toEqual([2, 1])

    expect(ids(retain(queue, (one) => one.id !== 2))).toEqual([1])
  })

  it('keeps everything when everything is still wanted', () => {
    const { queue } = arrive({ limit: 10 }, 3)

    expect(retain(queue, () => true)).toEqual(queue)
  })

  it('empties when nothing is', () => {
    const { queue } = arrive({ limit: 10 }, 3)

    expect(retain(queue, () => false)).toEqual([])
  })

  /**
   * A purge leaves a queue that is still in play order and still Priority-blind
   * about *what* it dropped — across every shape, and against every one of the
   * 16 subsets of four Calls the Listener might still want.
   *
   * The sweep the other three operations get: a **Hold**, an **Avoid** and a
   * Selection change all arrive here, and each can take any subset.
   */
  it.each(SPACE)('purges any subset without disturbing play order: %j', (levels) => {
    const priorityOf = byArrival(levels)
    const { queue } = arrive({ limit: 4, priorityOf }, 4)

    for (let wanted = 0; wanted < 16; wanted += 1) {
      // Bit `n` of `wanted` is "the Listener still wants the Call with id n+1".
      const keeps = (one: Call) => (wanted & (1 << (one.id - 1))) !== 0
      const left = retain(queue, keeps)

      // Exactly what was wanted, and nothing a Priority could smuggle back in.
      expect(ids(left).sort()).toEqual(ids(queue.filter(keeps)).sort())
      // Still in play order, so the head is still what plays next.
      expect(ids(left)).toEqual(ids(playOrder(left, priorityOf)))
    }
  })
})

describe('the FIFO policy', () => {
  /** The default, and the one production runs on until #58 gives a Listener a
   *  way to mark a Talkgroup Priority. */
  it('ranks every Call the same, so arrival order is the whole order', () => {
    expect(FIFO(call(1))).toBe(FIFO(call(2, 999)))
  })

  /** Omitting `priorityOf` must mean FIFO and not something subtly else — every
   *  Call production queues today goes down this path. */
  it('is what a policy with no Priority means', () => {
    const levels = [0, 0, 0, 0, 0]

    expect(ids(arrive({ limit: 3 }, 5).queue)).toEqual(
      ids(arrive({ limit: 3, priorityOf: byArrival(levels) }, 5).queue),
    )
  })
})
