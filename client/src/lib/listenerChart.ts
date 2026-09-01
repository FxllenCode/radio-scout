/**
 * The operator's listener chart (#62, spec US 41) — what it asks for, and what
 * it says about the answer.
 *
 * Pure, clock as a parameter (`lib/dateRange`'s rule): every range and every
 * reading is a value a test constructs.
 *
 * There is deliberately nothing here about *who* was listening, because there
 * is nothing on the wire about it. `listener_samples` is a time and a count
 * (ADR-0011 rule 5), so the most this can ever answer is "how many, and when" —
 * which is exactly the question US 41 asks.
 */
import type { Series } from '@/types'

const HOUR_MS = 3_600_000
const DAY_MS = 24 * HOUR_MS

/** How far back a chart looks, and how finely. */
export interface ListenerRange {
  id: 'day' | 'week' | 'month'
  label: string
  spanMs: number
  /** The bucket width, chosen so each range is a readable number of bars rather
   *  than the server's default hundred-and-twenty of arbitrary width. */
  bucketMs: number
}

/**
 * The three ranges offered.
 *
 * Each one's bucket is the unit its question is asked in: "was anybody on last
 * night" is an hour, "is Saturday busier than Tuesday" is a few hours, and "is
 * this month busier than last" is a day. None exceeds the server's bound, so
 * none comes back at a width the labels would then be lying about.
 */
export const LISTENER_RANGES: readonly ListenerRange[] = [
  { id: 'day', label: '24 hours', spanMs: DAY_MS, bucketMs: HOUR_MS },
  { id: 'week', label: '7 days', spanMs: 7 * DAY_MS, bucketMs: 3 * HOUR_MS },
  { id: 'month', label: '30 days', spanMs: 30 * DAY_MS, bucketMs: DAY_MS },
]

/** What to ask the server for, given the range and the moment it was asked. */
export function listenerQuery(range: ListenerRange, now: number) {
  return {
    // Inclusive at both ends, which is what the archive's own `before` means.
    after: now - range.spanMs + 1,
    before: now,
    bucketMs: range.bucketMs,
  }
}

/** The busiest bucket, and when it began. */
export interface Peak {
  listeners: number
  atMs: number
}

/**
 * The most Listeners this chart ever saw at once, and when — spec US 41's
 * headline, said in a sentence rather than left for an Operator to find by
 * squinting at bars.
 *
 * `undefined` when nobody was ever on, which is a different fact from a peak of
 * zero and reads differently on screen: an Instance nobody has found yet has no
 * peak, where "0 listeners at 3am" implies somebody looked.
 *
 * The **earliest** of two equal peaks wins, so a chart that is re-read a minute
 * later does not move its own headline between two ties.
 */
export function peakOf(series: Series): Peak | undefined {
  let best: Peak | undefined
  for (const [bucket, listeners] of series.values.entries()) {
    if (listeners <= 0) continue
    if (best === undefined || listeners > best.listeners) {
      best = { listeners, atMs: series.fromMs + bucket * series.bucketMs }
    }
  }
  return best
}
