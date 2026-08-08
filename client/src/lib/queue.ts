/**
 * The **listening queue** and its ordering policy (#95).
 *
 * Pure: no React, no store, no clock. A queue in and a queue out, so every rule
 * below is a value a test constructs rather than a slice assembled and driven
 * through a socket.
 *
 * The point of the seam is **Priority** (CONTEXT.md, spec US 27). Ordering used
 * to be spread across three reducers and a `slice(-QUEUE_LIMIT)`: arrival order
 * was the only order, so nothing had to say what "next" meant. Once a Talkgroup
 * can outrank another, "next" and "what the cap drops" become one decision made
 * in one place — and #58 turns Priority on by passing a different
 * [`PriorityOf`], not by editing the reducers again.
 *
 * # What this deliberately is not
 *
 * Not an **Admission** (CONTEXT.md reserves that word for what **Ingest**
 * decided about a Call, #96), which is why the entry point is [`enqueue`]. And
 * not a preemption: a Priority Call jumps the *queue*, never the Call already
 * playing — the glossary calls that out by name as SDRTrunk's stronger notion,
 * which this is not.
 */
import type { Call } from '@/types'

/**
 * How far ahead a Call plays, relative to the ones waiting. Higher goes first;
 * Calls that tie keep arrival order.
 *
 * A level rather than a flag so that "the cap drops **lowest** Priority first"
 * is literally the rule it implements. #58's per-Talkgroup preference is a
 * level of one, and a scheme with tiers costs this module nothing.
 */
export type PriorityOf = (call: Call) => number

/**
 * No Talkgroup outranks another, so arrival order is the whole order.
 *
 * What ships today, and the default of [`QueuePolicy`] — production has no way
 * to mark a Talkgroup Priority until #58, and this is the policy that says so
 * out loud rather than by omission.
 */
export const FIFO: PriorityOf = () => 0

/** How the queue orders itself, and how much of it there may be. */
export interface QueuePolicy {
  /** Ceiling on the queue — `QUEUE_LIMIT` in `store/live.ts`, which is where
   *  the number and the reason for it live. */
  limit: number
  /** Defaults to [`FIFO`]. */
  priorityOf?: PriorityOf
}

/** A queue after a Call joined it, and whatever the cap had to drop to fit. */
export interface Enqueued {
  /** The queue as it now stands, in the order it will play. */
  queue: Call[]
  /**
   * What the cap dropped, in the order it dropped them — lowest Priority
   * first, stalest within a Priority.
   *
   * The Calls rather than a count, because the display owns up to what a
   * Listener did not get rather than hiding it, and a count is the least a
   * caller can make of this.
   */
  dropped: Call[]
}

/** A queue after its next Call came off it. */
export interface Taken {
  /** What plays now, or `null` if the feed falls quiet. */
  next: Call | null
  /** What is still waiting. */
  queue: Call[]
}

/**
 * Put `call` in the queue where the policy says it belongs, and truncate to the
 * limit.
 *
 * The queue is kept **in play order**, so the head is always what plays next
 * and nothing downstream — the `Q` count, the queue sheet, the reducer taking
 * the next Call — has to know the ordering rule to read the queue correctly.
 *
 * A Call goes ahead of the first Call it outranks, and therefore behind every
 * Call that outranks *or ties* it. That tie is what keeps equal-Priority
 * traffic first-come-first-served.
 *
 * # The one precondition, and it is #58's to meet
 *
 * Play order is maintained *on insert*, which holds the array in play order
 * for as long as `priorityOf` answers the same way it did when each waiting
 * Call arrived. **A `priorityOf` that changes under a non-empty queue leaves
 * that queue stale** — a Call already waiting does not jump when the Listener
 * promotes its Talkgroup, and one arriving after can land ahead of a staler
 * equal-Priority peer.
 *
 * Harmless today: production is [`FIFO`] and answers the same way forever. It
 * is #58's to deal with, because #58 is what puts a Priority toggle in front of
 * a Listener — and the honest fix needs something this module deliberately does
 * not have. Re-sorting recovers a *promotion* exactly, but not a demotion:
 * ranking two newly-tied Calls by staleness needs an arrival stamp, and the
 * queue holds bare Calls (no field on one is arrival order — `id` is the
 * server's, which #94 established is not even emission order, and `timestamp`
 * is when the radio keyed up). Giving the queue a stamp changes what
 * `selectQueue` hands `transport.ts`, which is more than this ticket asked for.
 * So: stated here rather than papered over, and left where the toggle is built.
 */
export function enqueue(
  queue: readonly Call[],
  call: Call,
  policy: QueuePolicy,
): Enqueued {
  const priorityOf = policy.priorityOf ?? FIFO
  const priority = priorityOf(call)

  const ahead = queue.findIndex((waiting) => priorityOf(waiting) < priority)
  const joined = [...queue]
  joined.splice(ahead === -1 ? joined.length : ahead, 0, call)

  return cap(joined, policy.limit, priorityOf)
}

/**
 * Truncate to `limit`, dropping in the order CONTEXT.md's **Listening queue**
 * names: lowest Priority first, then stalest.
 *
 * Both halves matter, and the second is the one that was already here: the
 * stalest waiting Call is the most out-of-date audio there is, so a Listener
 * far behind is better served by losing it than by losing what was just said.
 * Dropping it *within the lowest band* is what this ticket adds — the old rule
 * dropped it outright, which discards the one Talkgroup the Listener said
 * mattered while routine chatter plays on.
 *
 * With no Priority anywhere the queue is a single band, so this is exactly the
 * old `slice(-limit)`, which is what keeps today's behaviour still true.
 *
 * Written as "give up the worst one, repeat" rather than as a sort, because
 * [`enqueue`] adds one Call and every other writer shrinks the queue — so the
 * loop below runs **once**, and a sort would spend `n log n` on a Pi-class
 * phone to order ninety-nine Calls it was never going to drop. The loop keeps
 * the contract total anyway: hand it a queue any distance over the limit and it
 * takes them in the same order.
 *
 * Takes `queue` by value and shortens it in place — private, and its one caller
 * hands it an array it has just built and no longer owns, so a second copy per
 * arriving Call would buy nothing.
 */
function cap(queue: Call[], limit: number, priorityOf: PriorityOf): Enqueued {
  const dropped: Call[] = []
  while (queue.length > limit) {
    dropped.push(...queue.splice(worst(queue, priorityOf), 1))
  }
  return { queue, dropped }
}

/**
 * Which waiting Call the cap gives up first: the lowest **Priority** there is,
 * and the stalest of those.
 *
 * "Stalest" is *position*, and that is not a shortcut — a queued Call carries
 * no arrival stamp of its own, so the array's order is the only record of
 * arrival there is. Taking the first of a tie is therefore taking the earliest
 * to arrive, which is what makes the second half of the rule true.
 */
function worst(queue: readonly Call[], priorityOf: PriorityOf): number {
  let at = 0
  let lowest = priorityOf(queue[0])
  for (let index = 1; index < queue.length; index += 1) {
    const priority = priorityOf(queue[index])
    // Strictly lower, so a tie leaves the earlier one standing as the worst.
    if (priority < lowest) {
      at = index
      lowest = priority
    }
  }
  return at
}

/**
 * Take the Call that plays next.
 *
 * The head, because [`enqueue`] keeps the queue in play order — so this is the
 * one statement of "what plays next", rather than a rule every caller repeats.
 */
export function takeNext(queue: readonly Call[]): Taken {
  const [next, ...rest] = queue
  return { next: next ?? null, queue: rest }
}

/**
 * Keep only the Calls the Listener still wants — after a Selection change, a
 * **Hold**, or an **Avoid**.
 *
 * Deliberately Priority-blind, and thin because that is the whole rule: order
 * is a policy, so filtering in place preserves it, and a Priority Call the
 * Listener has deselected leaves like any other. CONTEXT.md: *"Queue order, not
 * selection — a priority talkgroup still has to be selected to be heard."*
 */
export function retain(
  queue: readonly Call[],
  wanted: (call: Call) => boolean,
): Call[] {
  return queue.filter(wanted)
}
