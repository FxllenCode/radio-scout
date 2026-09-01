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
 * it when that is shorter.
 *
 * **`after` is snapped back to a local hour, and that is the whole point of
 * this function.** The server buckets from the origin it is *given* — nothing
 * about `[from, from + 3600000)` makes it an hour of anybody's day — and the
 * extent a ribbon reports is the oldest matching Call's exact millisecond. So
 * asking with the extent verbatim produces cells that hold 09:37–10:37 and a
 * grid labelled 09:00, shifted by an arbitrary sub-hour offset that nothing on
 * screen could reveal. Snapping is what makes the label true.
 *
 * Built with `setMinutes(0, 0, 0)` rather than by rounding the instant, because
 * a local hour boundary is not always a whole number of hours from UTC: a
 * half-hour timezone would be thirty minutes out for every cell.
 */
export function heatmapWindow(series: Series): { after: number; before: number } {
  const before = series.toMs - 1
  const from = Math.max(series.fromMs, before - HEATMAP_DAYS * 24 * HOUR_MS + 1)
  return { after: localHourStart(from), before }
}

/** The start of the local hour holding `at` — the origin an hourly bucket has
 *  to run from for "Tuesday 09:00" to mean 09:00. */
function localHourStart(at: number): number {
  const start = new Date(at)
  start.setMinutes(0, 0, 0)
  return start.getTime()
}

/** Whether a series really is hourly, which is what [`heatmapOf`]'s fold needs
 *  to be exact.
 *
 *  It can be false: the server widens a grain that would not fit its own bound
 *  and says so in the answer, so a range wider than [`HEATMAP_DAYS`] — or a
 *  server whose bound is lower than this build assumes — comes back coarser.
 *  Checked rather than assumed, because folding three-hour totals into whichever
 *  hour they began in draws a confidently wrong picture with nothing on screen
 *  to say so. */
export function isHourly(series: Series): boolean {
  return series.bucketMs === HOUR_MS
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
