/**
 * The **density ribbon**'s arithmetic (#62, spec US 34–35).
 *
 * A `Series` is one number per bucket across the search's whole range, answered
 * by `GET /api/calls/activity` under exactly the filters the results came from.
 * Everything a Listener does with it is here and pure — where a bucket is on
 * screen, how far into the results it starts, and which bucket the page they
 * are looking at is inside — so the ribbon component draws and listens and
 * decides nothing.
 *
 * # Jumping is a window move, not a filter change
 *
 * "Time travel doesn't mean paging" (spec US 35) is answered by moving the
 * *window on screen*, which #89 deliberately named apart from the **Run**'s own
 * window: the search does not change, so the Run keeps walking and nothing a
 * Listener had playing is interrupted by their looking somewhere else.
 *
 * That is only affordable because the offset needs **no request to compute**.
 * The bars already say how many Calls are in every bucket, and the ordering
 * says which side of a bucket the earlier results are on — so
 * [`offsetOfBucket`] is a running total, and the page it names is the page that
 * instant is on. Asking the server "how far in is 3am" would have been a second
 * count query per drag frame.
 *
 * # Why the two halves are inverses
 *
 * [`offsetOfBucket`] answers "take me there" and [`bucketOfOffset`] answers
 * "where am I", and they have to agree or the ribbon's own marker would drift
 * away from the page it is pointing at as a Listener scrubs. They are tested as
 * a round trip rather than each against a table of expected numbers.
 */
import type { Series } from '@/types'

/** How many bars a ribbon asks for. A phone is ~360 CSS px wide, so a bar every
 *  three pixels is as fine as a thumb can address; the server bounds it either
 *  way and reports the width it used. */
export const RIBBON_BUCKETS = 120

/** The shortest a non-empty bar is drawn, as a fraction of the tallest.
 *
 *  A bucket holding one Call beside one holding four hundred is 0.25% tall,
 *  which renders as nothing at all — so the ribbon would say "no traffic here"
 *  about a stretch that has some, which is the one thing a density chart must
 *  not do. Zero stays zero. */
const MIN_BAR = 0.12

/** Every bar's height in `[0, 1]`, tallest bucket full. */
export function barHeights(values: number[]): number[] {
  const tallest = Math.max(...values, 0)
  if (tallest <= 0) return values.map(() => 0)
  return values.map((calls) =>
    calls <= 0 ? 0 : Math.max(MIN_BAR, calls / tallest),
  )
}

/** How many Calls the series accounts for — which is the search's own total,
 *  because both come from the same filters. */
export function totalOf(series: Series): number {
  return series.values.reduce((sum, calls) => sum + calls, 0)
}

/**
 * The ordering the results are in, which decides which side of a bucket the
 * earlier rows are on.
 *
 * Optional, and **the default lives here rather than at the call site**: a
 * `SearchQuery` types `sort` as optional even though `readSearchUrl` always
 * spells it, so every caller would otherwise carry its own `?? 'newest'` — one
 * per use, each an untestable branch, and each a chance for one of them to
 * default the other way.
 */
export type Ordering = 'newest' | 'oldest' | undefined

/** Whether the results run newest first — the reading every function here
 *  takes of an [`Ordering`]. */
const newestFirst = (ordering: Ordering) => ordering !== 'oldest'

/**
 * How far into the results the Calls in bucket `index` start, rounded back to
 * the page they are on.
 *
 * Newest-first walks the buckets backwards, so everything *after* a bucket in
 * time comes *before* it in the results; oldest-first is the plain reading.
 * Rounded down to a page boundary because a window is a page: the instant asked
 * for is then somewhere on the page that comes back, which is what "jump to
 * this date" means when the answer is a list.
 */
export function offsetOfBucket(
  values: number[],
  index: number,
  ordering: Ordering,
  pageSize: number,
): number {
  const at = clampIndex(index, values.length)
  const before = newestFirst(ordering)
    ? sum(values.slice(at + 1))
    : sum(values.slice(0, at))
  const page = Math.max(1, Math.trunc(pageSize))
  return Math.floor(before / page) * page
}

/**
 * Which bucket the results at `offset` are in — the ribbon's own marker.
 *
 * The inverse of [`offsetOfBucket`], and inexact in exactly one way that
 * matters: a page spans several buckets, so this answers with the bucket the
 * *first* row of that page falls in. An offset past the end of the results
 * belongs to the last bucket that has any, because that is where the Listener
 * actually is.
 */
export function bucketOfOffset(
  values: number[],
  offset: number,
  ordering: Ordering,
): number {
  const wanted = Math.max(0, Math.trunc(offset))
  const order = newestFirst(ordering)
    ? values.map((_, index) => values.length - 1 - index)
    : values.map((_, index) => index)

  let seen = 0
  let last = order[0] ?? 0
  for (const index of order) {
    if (values[index] <= 0) continue
    last = index
    if (wanted < seen + values[index]) return index
    seen += values[index]
  }
  return last
}

/** The instant bucket `index` begins. */
export function bucketStartMs(series: Series, index: number): number {
  return (
    series.fromMs + clampIndex(index, series.values.length) * series.bucketMs
  )
}

/**
 * Which bucket a pointer `fraction` of the way across the ribbon addresses.
 *
 * Clamped rather than refused: a drag that leaves the element still means
 * something — the end it left by — and a scrubber that stopped responding when
 * a thumb slipped off the top of it would feel broken.
 */
export function bucketAt(count: number, fraction: number): number {
  if (count <= 0) return 0
  return clampIndex(Math.floor(fraction * count), count)
}

const sum = (values: number[]) => values.reduce((total, one) => total + one, 0)

const clampIndex = (index: number, count: number) =>
  Math.min(Math.max(0, Math.trunc(index)), Math.max(0, count - 1))
