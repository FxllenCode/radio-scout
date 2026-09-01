import { describe, expect, it } from 'vitest'

import { DAY_LABELS, HEATMAP_DAYS, HOUR_MS, heatmapOf, heatmapWindow } from './heatmap'
import type { Series } from '@/types'

/** A local instant, so every assertion below is about the Listener's own clock
 *  rather than about whichever timezone the run happens to be in. */
const localHour = (
  year: number,
  month: number,
  day: number,
  hour: number,
): number => new Date(year, month - 1, day, hour).getTime()

/** `hours` consecutive hourly buckets from `fromMs`. */
const hourly = (fromMs: number, values: number[]): Series => ({
  fromMs,
  toMs: fromMs + values.length * HOUR_MS,
  bucketMs: HOUR_MS,
  values,
})

describe('heatmapOf (#62)', () => {
  /** 2026-08-17 is a Monday. */
  it('places a bucket on the local day and hour it begins in', () => {
    const { cells, busiest } = heatmapOf(
      // Monday 09:00, 10:00, 11:00.
      hourly(localHour(2026, 8, 17, 9), [4, 0, 7]),
    )

    expect(cells[0][9]).toBe(4)
    expect(cells[0][10]).toBe(0)
    expect(cells[0][11]).toBe(7)
    expect(busiest).toBe(7)
  })

  /** The whole point: the same hour on two different weeks is one cell, which
   *  is what turns four weeks of traffic into a rhythm. */
  it('adds the same hour of two weeks into one cell', () => {
    const first = heatmapOf(hourly(localHour(2026, 8, 18, 14), [3]))
    const both = heatmapOf({
      ...hourly(localHour(2026, 8, 18, 14), []),
      // Tuesday 14:00, then exactly a week of hours later.
      values: [3, ...new Array(7 * 24 - 1).fill(0), 5],
      toMs: localHour(2026, 8, 18, 14) + 7 * 24 * HOUR_MS + HOUR_MS,
    })

    // Tuesday is row 1 when the week starts on Monday.
    expect(first.cells[1][14]).toBe(3)
    expect(both.cells[1][14]).toBe(8)
  })

  it('starts the week on Monday and ends it on Sunday', () => {
    // 2026-08-23 is a Sunday.
    const sunday = heatmapOf(hourly(localHour(2026, 8, 23, 2), [6]))

    expect(DAY_LABELS).toHaveLength(7)
    expect(DAY_LABELS[6]).toBe('Sun')
    expect(sunday.cells[6][2]).toBe(6)
    expect(sunday.cells[0].every((calls) => calls === 0)).toBe(true)
  })

  it('is a full week of hours whatever it was given', () => {
    const { cells, busiest } = heatmapOf(hourly(localHour(2026, 8, 17, 0), []))

    expect(cells).toHaveLength(7)
    expect(cells.every((day) => day.length === 24)).toBe(true)
    expect(busiest).toBe(0)
  })
})

describe('heatmapWindow (#62)', () => {
  /**
   * The bound is what keeps the fold exact: past it the server widens the
   * buckets, and this would then be attributing three hours of traffic to
   * whichever hour they started in — a confidently wrong picture with nothing
   * on screen to say so.
   */
  it('asks for the most recent four weeks of a long archive', () => {
    const year = 365 * 24 * HOUR_MS
    const window = heatmapWindow({
      fromMs: 0,
      toMs: year,
      bucketMs: HOUR_MS,
      values: [],
    })

    expect(window.before).toBe(year - 1)
    expect(window.before - window.after + 1).toBe(HEATMAP_DAYS * 24 * HOUR_MS)
    // ...and that is a request the server will answer at hourly grain.
    expect((window.before - window.after + 1) / HOUR_MS).toBeLessThanOrEqual(1024)
  })

  it('asks for the whole of a shorter one', () => {
    const window = heatmapWindow({
      fromMs: 5_000,
      toMs: 5_000 + 48 * HOUR_MS,
      bucketMs: HOUR_MS,
      values: [],
    })

    expect(window).toEqual({ after: 5_000, before: 5_000 + 48 * HOUR_MS - 1 })
  })
})
