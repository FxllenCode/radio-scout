import { describe, expect, it } from 'vitest'

import type { Call } from '@/types'

import {
  FIFO,
  callsOf,
  enqueue,
  newest,
  priorityFrom,
  reorder,
  retain,
  takeNext,
  withdraw,
  type PriorityOf,
  type Queued,
  type QueuePolicy,
} from './queue'

/** A Call whose id is also its arrival ordinal — every `arrive` below stamps
 *  it as such, so an assertion can read staleness straight off an id. */
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

/** The same, over queue entries. */
const queuedIds = (queue: readonly Queued[]): number[] => ids(callsOf(queue))

/**
 * Feed `count` Calls through [`enqueue`] in arrival order.
 *
 * The queue is built the way the slice builds it — one arrival at a time —
 * rather than assembled and then sorted, because the cap decides *per arrival*
 * and a queue assembled whole would never make that decision.
 */
function arrive(policy: QueuePolicy, count: number) {
  let queue: Queued[] = []
  const dropped: Call[] = []
  for (let id = 1; id <= count; id += 1) {
    // The ordinal is the id, so every assertion below reads arrival order off
    // the numbers it is already comparing.
    const after = enqueue(queue, call(id), id, policy)
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

    expect(queuedIds(queue)).toEqual([1, 2, 3, 4])
  })

  /** CONTEXT.md **Priority**: "makes its calls jump the listening queue instead
   *  of waiting their turn". */
  it('puts an arriving Priority Call ahead of the routine traffic waiting', () => {
    const { queue } = arrive({ limit: 10, priorityOf: byArrival([0, 0, 1]) }, 3)

    expect(queuedIds(queue)).toEqual([3, 1, 2])
  })

  /** Priority is queue order, not a scheduler: two Priority Calls are still
   *  first-come-first-served with respect to each other. */
  it('keeps arrival order among Calls of equal Priority', () => {
    const { queue } = arrive({ limit: 10, priorityOf: byArrival([0, 1, 0, 1]) }, 4)

    expect(queuedIds(queue)).toEqual([2, 4, 1, 3])
  })

  it('orders higher Priority ahead of lower, not merely ahead of routine', () => {
    const { queue } = arrive({ limit: 10, priorityOf: byArrival([1, 0, 2]) }, 3)

    expect(queuedIds(queue)).toEqual([3, 1, 2])
  })

  /** The complete oracle: with room for everything, the queue *is* the play
   *  order — for all 81 ways four Calls can be prioritized. */
  it.each(SPACE)('is the play order of everything that arrived: %j', (levels) => {
    const priorityOf = byArrival(levels)
    const { queue, dropped } = arrive({ limit: 4, priorityOf }, 4)

    expect(dropped).toEqual([])
    expect(queuedIds(queue)).toEqual(ids(playOrder(callsOf(queue), priorityOf)))
    expect(queuedIds(queue).slice().sort()).toEqual([1, 2, 3, 4])
  })
})

describe('the cap truncates in the same order it plays (CONTEXT.md)', () => {
  /** Today's rule, and it must not move: with no Priority anywhere the queue is
   *  one band, so "the stalest of the lowest band" is "the stalest". */
  it('drops the stalest when nothing has Priority, exactly as it always did', () => {
    const { queue, dropped } = arrive({ limit: 3 }, 6)

    expect(queuedIds(queue)).toEqual([4, 5, 6])
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

    expect(queuedIds(queue)).toEqual([1, 4])
    expect(ids(dropped)).toEqual([2, 3])
  })

  it('drops the stalest within the band it is dropping from', () => {
    const { queue, dropped } = arrive(
      { limit: 3, priorityOf: byArrival([0, 1, 0, 1, 0]) },
      5,
    )

    // Priority: 2, 4. Routine: 1, 3, 5 — and 1 is the stalest of them.
    expect(queuedIds(queue)).toEqual([2, 4, 5])
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

    expect(queuedIds(queue)).toEqual([1, 2])
    expect(ids(dropped)).toEqual([3])
  })

  it('reaches into the next band up once the lowest is exhausted', () => {
    const { queue, dropped } = arrive(
      { limit: 1, priorityOf: byArrival([2, 0, 1]) },
      3,
    )

    // Routine (2) goes first, then the middle band (3), leaving the top.
    expect(queuedIds(queue)).toEqual([1])
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
    expect([...queuedIds(queue), ...ids(dropped)].sort()).toEqual([1, 2, 3, 4])
    // Play order holds after truncation, not merely before it.
    expect(queuedIds(queue)).toEqual(ids(playOrder(callsOf(queue), priorityOf)))

    for (const gone of dropped) {
      for (const kept of callsOf(queue)) {
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
    expect(queuedIds(queue)[0]).toBe(151)
  })

  it('drops nothing while there is room', () => {
    const { queue, dropped } = arrive({ limit: 4 }, 4)

    expect(queuedIds(queue)).toEqual([1, 2, 3, 4])
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
    expect(queuedIds(taken.queue)).toEqual([1, 3])
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

    expect(queuedIds(retain(queue, (one) => one.id !== 1))).toEqual([2, 3])
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
    expect(queuedIds(queue)).toEqual([2, 1])

    expect(queuedIds(retain(queue, (one) => one.id !== 2))).toEqual([1])
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
      expect(queuedIds(left).sort()).toEqual(ids(callsOf(queue).filter(keeps)).sort())
      // Still in play order, so the head is still what plays next.
      expect(queuedIds(left)).toEqual(ids(playOrder(callsOf(left), priorityOf)))
    }
  })
})

describe('the FIFO policy', () => {
  /** The default, and what a Listener who has marked nothing runs on. */
  it('ranks every Call the same, so arrival order is the whole order', () => {
    expect(FIFO(call(1))).toBe(FIFO(call(2, 999)))
  })

  /** Omitting `priorityOf` must mean FIFO and not something subtly else — every
   *  Call production queues today goes down this path. */
  it('is what a policy with no Priority means', () => {
    const levels = [0, 0, 0, 0, 0]

    expect(queuedIds(arrive({ limit: 3 }, 5).queue)).toEqual(
      queuedIds(arrive({ limit: 3, priorityOf: byArrival(levels) }, 5).queue),
    )
  })
})

describe('re-ordering a standing queue when the Priorities change (#58)', () => {
  /**
   * The whole reason this operation exists. #95 kept play order *on insert*,
   * which holds only while `priorityOf` answers the same way it did when each
   * waiting Call arrived — and #58 is what puts a Priority toggle in front of a
   * Listener, whose finger reaches for it precisely when forty Calls are
   * already waiting.
   */
  it('lifts a promoted Call past the routine traffic already waiting', () => {
    const { queue } = arrive({ limit: 10 }, 4)

    expect(queuedIds(reorder(queue, byArrival([0, 0, 1, 0])))).toEqual([3, 1, 2, 4])
  })

  /**
   * The direction that needed the arrival stamp. A demoted Call has to fall
   * back among Calls it once outranked, in *arrival* order — and after the
   * demotion those Calls are all tied, so nothing about the array's shape can
   * say which of them spoke first. Position could only ever put the demoted
   * Call back where its old Priority had lifted it to.
   */
  it('drops a demoted Call back to where it arrived, not to where it sat', () => {
    // 3 outranks everything, so the queue stands [3, 1, 2, 4].
    const { queue } = arrive({ limit: 10, priorityOf: byArrival([0, 0, 1, 0]) }, 4)
    expect(queuedIds(queue)).toEqual([3, 1, 2, 4])

    // Priority off. 3 arrived third and belongs third — not first, which is
    // where a stable re-sort of the array as it stands would have left it.
    expect(queuedIds(reorder(queue, FIFO))).toEqual([1, 2, 3, 4])
  })

  /** The complete oracle: from any of the 81 shapes, to any of the 81 shapes,
   *  the answer is the play order of what is in hand. Nothing is added, nothing
   *  is lost, and the head is what plays next. */
  it.each(SPACE)('is the play order under the new Priorities: %j', (before) => {
    const { queue } = arrive({ limit: 4, priorityOf: byArrival(before) }, 4)

    for (const after of every([0, 1, 2], 4)) {
      const priorityOf = byArrival(after)
      const moved = reorder(queue, priorityOf)

      expect(queuedIds(moved)).toEqual(ids(playOrder(callsOf(moved), priorityOf)))
      expect(queuedIds(moved).slice().sort()).toEqual([1, 2, 3, 4])
    }
  })

  it('leaves an empty queue empty', () => {
    expect(reorder([], FIFO)).toEqual([])
  })

  /** Re-ordering is not a re-cap: it moves Calls, it never gives one up. */
  it('never drops anything, however deep the queue', () => {
    const { queue } = arrive({ limit: 100 }, 100)

    expect(reorder(queue, byArrival([0]))).toHaveLength(100)
  })
})

describe('the newest Call waiting (#58 jump-to-newest)', () => {
  /**
   * *Newest* is the last to arrive, which under Priority is emphatically not
   * the tail: the tail is the lowest-ranked Call there is. Jumping to the tail
   * would hand a Listener the stalest routine chatter in the queue and call it
   * catching up.
   */
  it('is the last Call to have arrived, not the last in play order', () => {
    const { queue } = arrive({ limit: 10, priorityOf: byArrival([0, 0, 1]) }, 3)
    expect(queuedIds(queue)).toEqual([3, 1, 2])

    expect(newest(queue)?.id).toBe(3)
  })

  it('is the tail when nothing has Priority, which is the ordinary case', () => {
    expect(newest(arrive({ limit: 10 }, 4).queue)?.id).toBe(4)
  })

  it('is nothing at all when nothing is waiting', () => {
    expect(newest([])).toBeUndefined()
  })
})

describe('taking one named Call out of the queue (#58 play-now / drop)', () => {
  /** One operation for both, because they differ only in what the caller does
   *  with what comes back — which is the slice's decision, not this module's. */
  it('hands back the Call and the queue without it, in play order', () => {
    const { queue } = arrive({ limit: 10, priorityOf: byArrival([0, 1, 0]) }, 3)
    expect(queuedIds(queue)).toEqual([2, 1, 3])

    const taken = withdraw(queue, 1)

    expect(taken.call).toEqual(call(1))
    expect(queuedIds(taken.queue)).toEqual([2, 3])
  })

  /**
   * The moving-target case, and why this takes an **id** rather than an index.
   * The sheet a Listener is reading re-orders under them — a Priority Call
   * arriving jumps to the head, and every index below it moves by one. An index
   * would drop whichever Call had slid into that slot.
   */
  it('takes the Call that was named even after the queue re-ordered under it', () => {
    const { queue } = arrive({ limit: 10 }, 3)
    // A Priority Call arrives and takes the head: every index shifts.
    const after = enqueue(queue, call(4), 4, { limit: 10, priorityOf: byArrival([0, 0, 0, 1]) })
    expect(queuedIds(after.queue)).toEqual([4, 1, 2, 3])

    expect(withdraw(after.queue, 2).call).toEqual(call(2))
  })

  /** A Call that has already played, or that a purge took, is not there to
   *  take — and saying so is what lets the slice do nothing rather than guess. */
  it('hands back nothing for a Call the queue does not hold', () => {
    const { queue } = arrive({ limit: 10 }, 2)
    const taken = withdraw(queue, 99)

    expect(taken.call).toBeNull()
    expect(queuedIds(taken.queue)).toEqual([1, 2])
  })
})

describe('what a Listener marking Talkgroups Priority means (#58, spec US 27)', () => {
  const dispatch = { systemRef: 11, talkgroupRef: 100 }

  it('outranks a Talkgroup nobody marked', () => {
    const priorityOf = priorityFrom(['11:100'])

    expect(priorityOf(call(1, 100))).toBeGreaterThan(priorityOf(call(2, 200)))
  })

  it('ranks everything the same when nothing is marked, which is FIFO', () => {
    const priorityOf = priorityFrom([])

    expect(priorityOf(call(1, 100))).toBe(priorityOf(call(2, 200)))
  })

  /** A Ref is unique only within a System (#47's `UnitScope` lesson, one layer
   *  up): Talkgroup 100 on two Systems is two channels, and marking one must
   *  not promote the other. */
  it('is scoped to the System, because a Ref is unique only within one', () => {
    const priorityOf = priorityFrom([`${dispatch.systemRef}:100`])
    const elsewhere = { ...call(2, 100), systemRef: 12 }

    expect(priorityOf(elsewhere)).toBe(0)
  })

  /**
   * A **Patch** reaches the channel it was patched onto, and Priority follows
   * it — the same rule `wants` applies to whether a Call is heard at all, and
   * `Selection::reaches_channels` applies on the server. A Call that reaches a
   * Listener's dispatch channel *because* it was patched there is the one they
   * least want waiting behind tactical chatter.
   */
  it('promotes a patched Call that reaches a Priority Talkgroup', () => {
    const priorityOf = priorityFrom(['11:100'])
    const patched = { ...call(1, 200), patches: [100] }

    expect(priorityOf(patched)).toBeGreaterThan(0)
  })

  it('leaves a patched Call alone when none of its channels is marked', () => {
    const priorityOf = priorityFrom(['11:100'])

    expect(priorityOf({ ...call(1, 200), patches: [300] })).toBe(0)
  })
})
