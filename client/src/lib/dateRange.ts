/**
 * Date-range presets for the archive search (#61, spec US 31).
 *
 * Eight taps on a `datetime-local` pair is the cost of the commonest search
 * there is — "what happened this morning" — so these are the four the ticket
 * names, each one tap.
 *
 * # They resolve to instants, and that is the decision
 *
 * A preset **fills the date inputs** with real moments and then stops existing:
 * what reaches the URL is `after=` and `before=` in milliseconds, never
 * `when=today`. So a link is a *view*, reproducible by whoever opens it and
 * whenever they do — which is what spec US 30 asks a bookmark to be — and the
 * two inputs always show the range actually being searched rather than a range
 * nobody typed.
 *
 * Pure, with the clock as a parameter: `now` is passed in, never read here, so
 * every range is a value a test constructs (`lib/panel`'s rule about ages, for
 * the same reason).
 */

/** The four ranges offered. */
export type PresetId = 'last-hour' | 'today' | 'yesterday' | 'last-7-days'

export interface Preset {
  id: PresetId
  label: string
}

/** In the order a Listener reaches for them: the narrowest first, because "what
 *  was that just now" is the question being asked most often. */
export const PRESETS: readonly Preset[] = [
  { id: 'last-hour', label: 'Last hour' },
  { id: 'today', label: 'Today' },
  { id: 'yesterday', label: 'Yesterday' },
  { id: 'last-7-days', label: 'Last 7 days' },
]

const HOUR_MS = 3_600_000

/** Both ends of a range, in unix milliseconds — `before` **inclusive**, as
 *  `SearchQuery` reads it. */
export interface DateRange {
  after: number
  before: number
}

/**
 * What `preset` covers, given the moment it was tapped.
 *
 * Two kinds, deliberately: **rolling** ones end at `now`, because "the last
 * hour" is measured from the Listener, and **calendar** ones cover a whole
 * local day, because "today" is a day rather than the hours that have gone by —
 * a search of today's traffic made at nine in the morning should not stop
 * matching the moment a Call arrives at ten.
 */
export function rangeOf(preset: PresetId, now: number): DateRange {
  switch (preset) {
    case 'last-hour':
      return { after: now - HOUR_MS, before: now }
    case 'last-7-days':
      return { after: sameTimeDaysAgo(now, 7), before: now }
    case 'today':
      return day(now, 0)
    case 'yesterday':
      return day(now, -1)
  }
}

/**
 * The whole local calendar day `offset` days from the one holding `now`.
 *
 * Built from local date *parts* rather than by subtracting a day's worth of
 * milliseconds: `new Date(y, m, d - 1)` is the previous midnight even when the
 * day between them was 23 or 25 hours long, where arithmetic lands an hour off
 * on the two days a year a timezone shifts.
 */
function day(now: number, offset: number): DateRange {
  const at = new Date(now)
  const start = new Date(at.getFullYear(), at.getMonth(), at.getDate() + offset)
  const next = new Date(at.getFullYear(), at.getMonth(), at.getDate() + offset + 1)
  // One millisecond short of the following midnight, because `before` is an
  // inclusive bound: sharing the instant would put a Call struck at midnight in
  // both this day and the next.
  return { after: start.getTime(), before: next.getTime() - 1 }
}

/** The same clock time, `days` local days earlier — so "the last 7 days" ends
 *  where it began seven days ago rather than an hour either side of it. Walked
 *  across whole local days rather than subtracted, for [`day`]'s reason. */
function sameTimeDaysAgo(now: number, days: number): number {
  const at = new Date(now)
  at.setDate(at.getDate() - days)
  return at.getTime()
}
