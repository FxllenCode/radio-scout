/**
 * **When the county gets busy** (#62) — hourly buckets folded into a week.
 *
 * The server answers with Calls per hour across a stretch of time; this folds
 * those into a seven-by-twenty-four grid, so "Tuesday mornings" is a row of
 * cells rather than a shape a Listener has to find in a long ribbon.
 *
 * # The fold happens here because only the browser knows what an hour is
 *
 * A heatmap of *hour of day* is a question about the Listener's own clock, and
 * the server has no idea what that is. Grouping by `strftime('%H')` or
 * `date_trunc('hour')` would have put the answer in UTC — and would have been a
 * dialect divergence besides (ADR-0003). So the server buckets by plain integer
 * arithmetic from an origin, and the only thing that ever converts an instant
 * into an hour is `Date`, in the browser holding the Listener's timezone.
 *
 * Daylight saving is why an *hourly* bucket is the unit rather than anything
 * finer or coarser: a shift moves the clock by a whole hour, so every bucket
 * still begins on a local hour boundary either side of one.
 *
 * # And why the window is bounded
 *
 * A year of hourly buckets is 8,760 numbers, past the cap the server will
 * serialize — and past it the server *widens* the buckets, which would leave
 * this folding three-hour totals into whichever hour they happened to start in
 * and drawing a confidently wrong picture. [`heatmapWindow`] asks for a stretch
 * that fits, so the answer is always exact.
 */
import type { Series } from '@/types'

import { bucketStartMs } from './density'

/** One hour, the only bucket width this fold is exact at. */
export const HOUR_MS = 3_600_000

/**
 * How much history one heatmap covers.
 *
 * Four weeks: enough that a weekly rhythm is four samples deep rather than one,
 * and 672 hourly buckets — comfortably inside the server's own bound, so what
 * comes back is never widened underneath the fold.
 */
export const HEATMAP_DAYS = 28

/** Rows, starting Monday — which groups the weekend, and the weekend is the
 *  shape somebody opens this looking for. */
export const DAY_LABELS = ['Mon', 'Tue', 'Wed', 'Thu', 'Fri', 'Sat', 'Sun']

/** A week of hours, and the busiest cell in it. */
export interface Heatmap {
  /** `cells[day][hour]`, day 0 = Monday, hour 0 = local midnight. */
  cells: number[][]
  /** The most Calls any one cell holds — what the shading is relative to. */
  busiest: number
}

/**
 * The range an hourly heatmap should ask for, given what the ribbon found.
 *
 * The most recent [`HEATMAP_DAYS`] of the search's own extent, or the whole of
 * it when that is shorter. `before` is inclusive, which is what the archive's
 * own `before` filter means, so this is handed straight to the request.
 */
export function heatmapWindow(series: Series): { after: number; before: number } {
  const before = series.toMs - 1
  const after = Math.max(series.fromMs, before - HEATMAP_DAYS * 24 * HOUR_MS + 1)
  return { after, before }
}

/**
 * Fold a series of hourly buckets into a week.
 *
 * A bucket is placed by the local day and hour it *begins* in, which is exact
 * at hourly grain and is why [`heatmapWindow`] keeps the request inside the
 * server's bound. A coarser series still folds — it is simply attributed to the
 * hour each bucket starts in, which is the honest reading of a number that
 * covers more than one.
 */
export function heatmapOf(series: Series): Heatmap {
  const cells = DAY_LABELS.map(() => new Array<number>(24).fill(0))

  for (const [index, calls] of series.values.entries()) {
    if (calls <= 0) continue
    const at = new Date(bucketStartMs(series, index))
    cells[mondayFirst(at.getDay())][at.getHours()] += calls
  }

  return {
    cells,
    busiest: Math.max(...cells.flat(), 0),
  }
}

/** `Date.getDay()` is Sunday-first; the grid is Monday-first. */
const mondayFirst = (day: number) => (day + 6) % 7
