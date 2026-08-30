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
 * in one place — and #58 turned Priority on by passing a different
 * [`PriorityOf`] ([`priorityFrom`]), not by editing the reducers again.
 *
 * # The arrival stamp (#58)
 *
 * A queued Call carries an **arrival ordinal**, which #95 left it without and
 * named as the thing #58 would need. It buys exactly two answers that nothing
 * about an array's shape can give:
 *
 * - [`reorder`], when the Listener changes a Priority under a queue that is
 *   already deep. A *promotion* could be recovered from position; a
 *   **demotion** cannot, because the Calls it falls back among are all newly
 *   tied and their arrival order was never written down anywhere else.
 * - [`newest`], which is what jump-to-newest means. Under Priority the tail of
 *   the queue is the *lowest-ranked* Call, not the last to arrive — so jumping
 *   to the tail would hand a Listener the stalest routine chatter there is and
 *   call it catching up.
 *
 * An ordinal rather than a clock, so the module stays pure and a queue is still
 * a value a test constructs. The slice counts.
 *
 * # What this deliberately is not
 *
 * Not an **Admission** (CONTEXT.md reserves that word for what **Ingest**
 * decided about a Call, #96), which is why the entry point is [`enqueue`]. And
 * not a preemption: a Priority Call jumps the *queue*, never the Call already
 * playing — the glossary calls that out by name as SDRTrunk's stronger notion,
 * which this is not.
 */
import { talkgroupKey } from './selection'
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
 * The default of [`QueuePolicy`], and what a Listener who has marked nothing
 * Priority runs on — said out loud rather than by omission. [`priorityFrom`]
 * over an empty set is exactly this.
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

/**
 * A **Call** waiting, and when it arrived.
 *
 * The ordinal is monotonic and means nothing but *order* — it is not a clock,
 * not the Call's `timestamp` (which is when the radio keyed up, and a
 * **Delay**ed Call can be stored early and emitted late), and not its `id`
 * (the server's, which #94 established is not even emission order).
 */
export interface Queued {
  call: Call
  /** Higher is later. Compared only against other entries in the same queue. */
  at: number
}

/** The Calls of a queue, in the order they will play — what a screen draws and
 *  what the page-ahead reads. Written here so no caller has to know that an
 *  entry is a wrapper. */
export const callsOf = (queue: readonly Queued[]): Call[] =>
  queue.map((entry) => entry.call)

/** A queue after a Call joined it, and whatever the cap had to drop to fit. */
export interface Enqueued {
  /** The queue as it now stands, in the order it will play. */
  queue: Queued[]
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
  queue: Queued[]
}

/** A queue after one named Call was taken out of it (#58). */
export interface Withdrawn {
  /** The Call that was named, or `null` if the queue was not holding it. */
  call: Call | null
  /** What is left, in the order it will play. */
  queue: Queued[]
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
 * # The one precondition, and [`reorder`] is what meets it
 *
 * Play order is maintained *on insert*, which holds the array in play order for
 * as long as `priorityOf` answers the same way it did when each waiting Call
 * arrived. A `priorityOf` that changes under a non-empty queue would therefore
 * leave that queue stale — the Listener promotes dispatch and the forty Calls
 * already waiting do not move, which is exactly the moment they reached for it.
 * That is what [`reorder`] is for, and the slice dispatches it on every
 * Priority change.
 */
export function enqueue(
  queue: readonly Queued[],
  call: Call,
  at: number,
  policy: QueuePolicy,
): Enqueued {
  const priorityOf = policy.priorityOf ?? FIFO
  const priority = priorityOf(call)

  const ahead = queue.findIndex((waiting) => priorityOf(waiting.call) < priority)
  const joined = [...queue]
  joined.splice(ahead === -1 ? joined.length : ahead, 0, { call, at })

  return cap(joined, policy.limit, priorityOf)
}

/**
 * The same queue under new Priorities — the answer when a Listener marks a
 * Talkgroup while Calls are already waiting (#58).
 *
 * A total sort by (Priority, then arrival), which is the play order spelled
 * literally. Both halves of the comparison read a *value*: nothing is inferred
 * from where a Call currently sits, which is what makes a **demotion** exact.
 * A stable re-sort of the array as it stands would put a demoted Call back
 * among its new peers in the position its old Priority had lifted it to, ahead
 * of Calls that arrived before it.
 *
 * It never drops: re-ordering cannot make a queue longer, so the cap has
 * nothing to do.
 */
export function reorder(
  queue: readonly Queued[],
  priorityOf: PriorityOf,
): Queued[] {
  return [...queue].sort(
    (a, b) => priorityOf(b.call) - priorityOf(a.call) || a.at - b.at,
  )
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
function cap(queue: Queued[], limit: number, priorityOf: PriorityOf): Enqueued {
  const dropped: Call[] = []
  while (queue.length > limit) {
    dropped.push(...callsOf(queue.splice(worst(queue, priorityOf), 1)))
  }
  return { queue, dropped }
}

/**
 * Which waiting Call the cap gives up first: the lowest **Priority** there is,
 * and the stalest of those.
 *
 * "Stalest" is read off *position* rather than off [`Queued.at`], and the two
 * are the same answer: every operation in this module hands back a queue in
 * play order, so within one Priority band the earlier entry is the staler one.
 * Comparing the stamps here would spell a rule the array already guarantees,
 * and buy a branch nothing could ever take.
 */
function worst(queue: readonly Queued[], priorityOf: PriorityOf): number {
  let at = 0
  let lowest = priorityOf(queue[0].call)
  for (let index = 1; index < queue.length; index += 1) {
    const priority = priorityOf(queue[index].call)
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
export function takeNext(queue: readonly Queued[]): Taken {
  const [next, ...rest] = queue
  return { next: next?.call ?? null, queue: rest }
}

/**
 * The Call that arrived most recently — what *jump to newest* jumps to (#58,
 * spec US 24).
 *
 * Deliberately not the tail. The tail is the lowest-ranked Call in the queue,
 * which under Priority is the routine chatter a Listener is trying to get past;
 * handing them that and calling it catching up would be the same lie the cap's
 * old rule told in the other direction.
 */
export function newest(queue: readonly Queued[]): Call | undefined {
  let latest: Queued | undefined
  for (const entry of queue) {
    if (!latest || entry.at > latest.at) latest = entry
  }
  return latest?.call
}

/**
 * Take one named Call out of the queue (#58).
 *
 * One operation for *play now* and for *drop*, because they differ only in what
 * the caller does with what comes back — and that is the slice's decision (one
 * plays it, the other lets it go), not this module's.
 *
 * By **id**, never by index: the sheet a Listener is reading re-orders under
 * their thumb — an arriving Priority Call takes the head and every index below
 * it moves by one — so an index would act on whichever Call had slid into that
 * slot. A Call the queue is not holding hands back `null` rather than throwing,
 * so a reducer can simply do nothing about a Call that played or was purged
 * between the tap and the dispatch.
 */
export function withdraw(queue: readonly Queued[], id: number): Withdrawn {
  const taken = queue.find((entry) => entry.call.id === id)
  return {
    call: taken?.call ?? null,
    queue: taken ? queue.filter((entry) => entry !== taken) : [...queue],
  }
}

/**
 * What a Listener's marked Talkgroups mean as an ordering (#58, spec US 27).
 *
 * A level of one — marked or not — which is all a per-Talkgroup toggle can say;
 * [`PriorityOf`] is a *level* so that a scheme with tiers costs this module
 * nothing later.
 *
 * A Call is judged by every channel it **reaches**, its own and any it was
 * patched onto — the same rule that decides whether it is heard at all
 * (`wants` in `store/live`, `Selection::reaches_channels` on the server). A
 * Call that reaches a Listener's dispatch channel *because* it was patched
 * there is the one they least want waiting behind tactical chatter.
 *
 * The keys are read into a `Set` once per build rather than per Call: this is
 * asked of every Call in the queue on every re-order, on a Pi-class phone.
 */
export function priorityFrom(keys: readonly string[]): PriorityOf {
  const marked = new Set(keys)
  if (marked.size === 0) return FIFO
  return (call) =>
    [call.talkgroupRef, ...(call.patches ?? [])].some((talkgroupRef) =>
      marked.has(talkgroupKey(call.systemRef, talkgroupRef)),
    )
      ? 1
      : 0
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
  queue: readonly Queued[],
  wanted: (call: Call) => boolean,
): Queued[] {
  return queue.filter((entry) => wanted(entry.call))
}
