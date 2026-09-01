import { describe, expect, it } from 'vitest'

import { LISTENER_RANGES, listenerQuery, peakOf } from './listenerChart'
import type { Series } from '@/types'

const HOUR = 3_600_000

const series = (values: number[], bucketMs = HOUR): Series => ({
  fromMs: 1_000,
  toMs: 1_000 + values.length * bucketMs,
  bucketMs,
  values,
})

describe('listenerQuery (#62, spec US 41)', () => {
  it('asks for the range ending now, at the range`s own grain', () => {
    const day = LISTENER_RANGES[0]

    expect(listenerQuery(day, 1_000_000_000)).toEqual({
      after: 1_000_000_000 - 24 * HOUR + 1,
      before: 1_000_000_000,
      bucketMs: HOUR,
    })
  })

  /** Every range has to come back at the width it asked for, or the labels
   *  would be describing buckets the server had widened underneath them. */
  it('never asks for more buckets than the server will serialize', () => {
    for (const range of LISTENER_RANGES) {
      expect(range.spanMs / range.bucketMs).toBeLessThanOrEqual(1024)
      expect(range.spanMs / range.bucketMs).toBeGreaterThan(6)
    }
  })
})

describe('peakOf (#62, spec US 41)', () => {
  it('finds the busiest bucket and says when it began', () => {
    expect(peakOf(series([1, 4, 2]))).toEqual({
      listeners: 4,
      atMs: 1_000 + HOUR,
    })
  })

  /**
   * Nobody-ever is not a peak of zero. An Instance nobody has found yet has no
   * headline to report, where "0 listeners at 03:00" reads as though somebody
   * looked and counted.
   */
  it('has no answer when nobody was ever on', () => {
    expect(peakOf(series([0, 0, 0]))).toBeUndefined()
    expect(peakOf(series([]))).toBeUndefined()
  })

  /** The earlier of two equal peaks, so re-reading the page a minute later does
   *  not move its own headline between two ties. */
  it('keeps the earliest of two equal peaks', () => {
    expect(peakOf(series([3, 1, 3]))).toEqual({ listeners: 3, atMs: 1_000 })
  })
})
