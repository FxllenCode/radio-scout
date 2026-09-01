import { describe, expect, it } from 'vitest'

import {
  DAY_LABELS,
  HEATMAP_DAYS,
  HOUR_MS,
  heatmapOf,
  heatmapWindow,
  isHourly,
} from './heatmap'
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
    const from = localHour(2026, 8, 17, 6)
    const window = heatmapWindow({
      fromMs: from,
      toMs: from + 48 * HOUR_MS,
      bucketMs: HOUR_MS,
      values: [],
    })

    expect(window).toEqual({ after: from, before: from + 48 * HOUR_MS - 1 })
  })

  /**
   * **The origin is snapped to a local hour**, or the whole grid is a lie.
   *
   * The server buckets from the origin it is handed, and the extent a ribbon
   * reports is the oldest matching Call's exact millisecond — so an unsnapped
   * request produces cells holding 09:37–10:37 under a column labelled 09:00,
   * shifted by an arbitrary offset nothing on screen could reveal. This is the
   * common case, not an edge one: it is every undated search.
   */
  it('starts on a local hour, whatever instant the archive begins at', () => {
    const ragged = localHour(2026, 8, 17, 9) + 37 * 60_000 + 4_211

    const window = heatmapWindow({
      fromMs: ragged,
      toMs: ragged + 12 * HOUR_MS,
      bucketMs: HOUR_MS,
      values: [],
    })

    expect(window.after).toBe(localHour(2026, 8, 17, 9))
    expect(new Date(window.after).getMinutes()).toBe(0)
    expect(new Date(window.after).getSeconds()).toBe(0)
    expect(window.after).toBeLessThanOrEqual(ragged)
  })

  /** The same for the four-week window, which is derived from the other end. */
  it('snaps the four-week window too', () => {
    const end = localHour(2026, 8, 17, 9) + 41 * 60_000
    const window = heatmapWindow({
      fromMs: 0,
      toMs: end,
      bucketMs: HOUR_MS,
      values: [],
    })

    expect(new Date(window.after).getMinutes()).toBe(0)
  })
})

describe('isHourly (#62)', () => {
  /** The fold is only exact at hourly grain, and the server is entitled to
   *  answer coarser than it was asked — so the assumption is checked rather
   *  than trusted. */
  it('tells an hourly answer from a widened one', () => {
    expect(isHourly(hourly(0, [1, 2]))).toBe(true)
    expect(isHourly({ ...hourly(0, [1, 2]), bucketMs: 3 * HOUR_MS })).toBe(false)
  })
})
