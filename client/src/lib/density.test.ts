import { describe, expect, it } from 'vitest'

import {
  barHeights,
  bucketAt,
  bucketOfOffset,
  bucketStartMs,
  offsetOfBucket,
  totalOf,
} from './density'
import type { Series } from '@/types'

/** Four hours, one bucket an hour, busiest in the middle. */
const SERIES: Series = {
  fromMs: 1_000,
  toMs: 1_000 + 4 * 3_600_000,
  bucketMs: 3_600_000,
  values: [2, 10, 0, 3],
}

describe('barHeights (#62)', () => {
  it('draws the busiest bucket full height', () => {
    expect(barHeights([1, 4, 2])).toEqual([0.25, 1, 0.5])
  })

  /** A bucket with one Call beside one with four hundred is a quarter of a
   *  percent tall, which is indistinguishable from nothing — so the ribbon
   *  would claim there was no traffic in a stretch that had some. */
  it('never draws a bucket that has traffic as empty', () => {
    const [quiet, busy] = barHeights([1, 400])

    expect(quiet).toBeGreaterThan(0.05)
    expect(busy).toBe(1)
  })

  it('draws an empty bucket as empty, and an empty series as flat', () => {
    expect(barHeights([0, 5])[0]).toBe(0)
    expect(barHeights([0, 0, 0])).toEqual([0, 0, 0])
    expect(barHeights([])).toEqual([])
  })
})

describe('offsetOfBucket (#62, spec US 35)', () => {
  /** Newest-first, so everything *later* in time is *earlier* in the results.
   *  The last bucket is therefore page one. */
  it('walks backwards through the buckets when the results are newest first', () => {
    // Bucket 3 is the newest: nothing is ahead of it.
    expect(offsetOfBucket(SERIES.values, 3, 'newest', 5)).toBe(0)
    // Bucket 2 has the 3 newer Calls ahead of it -> page one still.
    expect(offsetOfBucket(SERIES.values, 2, 'newest', 5)).toBe(0)
    // Bucket 1 has 3 ahead of it too (bucket 2 is empty).
    expect(offsetOfBucket(SERIES.values, 1, 'newest', 5)).toBe(0)
    // Bucket 0 has 13 ahead of it -> the third page of five.
    expect(offsetOfBucket(SERIES.values, 0, 'newest', 5)).toBe(10)
  })

  it('walks forwards when the results are oldest first', () => {
    expect(offsetOfBucket(SERIES.values, 0, 'oldest', 5)).toBe(0)
    expect(offsetOfBucket(SERIES.values, 1, 'oldest', 5)).toBe(0)
    expect(offsetOfBucket(SERIES.values, 3, 'oldest', 5)).toBe(10)
  })

  /** The window is a page, so the offset always names one — a Listener lands on
   *  the page holding that instant rather than on a half-page starting inside
   *  it. */
  it('always names a page boundary', () => {
    for (let index = 0; index < SERIES.values.length; index += 1) {
      expect(offsetOfBucket(SERIES.values, index, 'newest', 5) % 5).toBe(0)
    }
  })

  it('clamps a bucket off either end rather than answering nonsense', () => {
    expect(offsetOfBucket(SERIES.values, -3, 'oldest', 5)).toBe(0)
    expect(offsetOfBucket(SERIES.values, 99, 'oldest', 5)).toBe(10)
    expect(offsetOfBucket([], 0, 'oldest', 5)).toBe(0)
  })

  /** A zero page size is a caller's mistake and must not divide by zero. */
  it('survives a nonsense page size', () => {
    expect(offsetOfBucket(SERIES.values, 0, 'newest', 0)).toBe(13)
  })
})

describe('bucketOfOffset (#62)', () => {
  it('says which bucket the page on screen is in', () => {
    // Newest first: rows 0..2 are bucket 3, rows 3..12 are bucket 1, then 0.
    expect(bucketOfOffset(SERIES.values, 0, 'newest')).toBe(3)
    expect(bucketOfOffset(SERIES.values, 3, 'newest')).toBe(1)
    expect(bucketOfOffset(SERIES.values, 13, 'newest')).toBe(0)
  })

  /** Off the end is the last bucket that has anything in it, because that is
   *  where a Listener who paged past the end actually is. */
  it('answers with the last bucket that has traffic when the offset runs off the end', () => {
    expect(bucketOfOffset(SERIES.values, 999, 'newest')).toBe(0)
    expect(bucketOfOffset(SERIES.values, 999, 'oldest')).toBe(3)
    expect(bucketOfOffset([0, 0], 4, 'newest')).toBe(1)
    expect(bucketOfOffset([], 4, 'newest')).toBe(0)
  })

  /** An unspelled ordering is newest-first, because that is what a bare
   *  `/search` means — and the default is here rather than at each call site so
   *  two of them cannot disagree. */
  it('reads an absent ordering as newest first', () => {
    expect(bucketOfOffset(SERIES.values, 0, undefined)).toBe(
      bucketOfOffset(SERIES.values, 0, 'newest'),
    )
    expect(offsetOfBucket(SERIES.values, 0, undefined, 5)).toBe(
      offsetOfBucket(SERIES.values, 0, 'newest', 5),
    )
  })

  /**
   * **The round trip.** The marker on the ribbon and the page it points at have
   * to be the same place, or scrubbing would drift — the marker landing one
   * bucket away from the traffic the Listener is now reading, a little further
   * every drag.
   *
   * Exhaustive over every non-empty bucket of a shape with gaps, ties and both
   * orderings, because that is small enough to cover completely (`lib/queue`'s
   * argument) and the failure is a single off-by-one nobody would pick out of a
   * sampled run.
   */
  it('lands on the bucket it was asked for, both orderings', () => {
    const values = [4, 0, 7, 1, 0, 9]

    for (const ordering of ['newest', 'oldest'] as const) {
      for (const [index, calls] of values.entries()) {
        if (calls === 0) continue
        const offset = offsetOfBucket(values, index, ordering, 1)

        expect({ ordering, index, at: bucketOfOffset(values, offset, ordering) })
          .toEqual({ ordering, index, at: index })
      }
    }
  })
})

describe('bucketStartMs and bucketAt (#62)', () => {
  it('places a bucket at its own instant', () => {
    expect(bucketStartMs(SERIES, 0)).toBe(1_000)
    expect(bucketStartMs(SERIES, 2)).toBe(1_000 + 2 * 3_600_000)
  })

  it('reads a pointer across the ribbon as a bucket', () => {
    expect(bucketAt(4, 0)).toBe(0)
    expect(bucketAt(4, 0.5)).toBe(2)
    expect(bucketAt(4, 0.99)).toBe(3)
  })

  /** A drag that leaves the element still means the end it left by — a
   *  scrubber that stopped responding when a thumb slipped off would read as
   *  broken. */
  it('clamps a pointer that has left the ribbon', () => {
    expect(bucketAt(4, -0.4)).toBe(0)
    expect(bucketAt(4, 1.8)).toBe(3)
    expect(bucketAt(0, 0.5)).toBe(0)
  })
})

describe('totalOf (#62)', () => {
  it('adds the bars up, which is the search`s own total', () => {
    expect(totalOf(SERIES)).toBe(15)
    expect(totalOf({ ...SERIES, values: [] })).toBe(0)
  })
})
