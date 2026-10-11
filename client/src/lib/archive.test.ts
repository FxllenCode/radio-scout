import { describe, expect, it } from 'vitest'

import {
  dateTimeLocalToMs,
  downloadUrl,
  msToDateTimeLocal,
  formatCallTime,
  formatClock,
  formatDuration,
  formatRowTime,
  lateness,
  pageSummary,
  searchParams,
  starsKept,
} from './archive'

describe('searchParams', () => {
  it('serializes set filters in a stable order', () => {
    expect(
      searchParams({ system: 100, talkgroup: 54241, sort: 'oldest', limit: 50 }),
    ).toBe('limit=50&sort=oldest&system=100&talkgroup=54241')
  })

  it('drops unset and blank filters rather than sending them', () => {
    expect(
      searchParams({
        system: undefined,
        group: '',
        tag: 'Fire',
        offset: 0,
      }),
    ).toBe('offset=0&tag=Fire')
  })

  it('escapes values that need it', () => {
    expect(searchParams({ tag: 'Fire Dispatch' })).toBe('tag=Fire+Dispatch')
    expect(searchParams({ group: 'Law & Order' })).toBe('group=Law+%26+Order')
  })

  it('is empty for no filters at all', () => {
    expect(searchParams({})).toBe('')
  })
})

describe('downloadUrl', () => {
  it('points at the per-call download endpoint', () => {
    expect(downloadUrl(42)).toBe('/api/call/42/download')
  })
})

describe('dateTimeLocalToMs', () => {
  it('reads a datetime-local value as local time', () => {
    // No timezone suffix -> the listener's own timezone, matching the input.
    expect(dateTimeLocalToMs('2026-07-25T14:30')).toBe(
      Date.parse('2026-07-25T14:30'),
    )
  })

  it('treats blank and unparseable values as no bound', () => {
    expect(dateTimeLocalToMs('')).toBeUndefined()
    expect(dateTimeLocalToMs('   ')).toBeUndefined()
    expect(dateTimeLocalToMs('not-a-date')).toBeUndefined()
  })
})

/** The other direction, which is what makes the date inputs *controlled* (#61):
 *  a bound set by a preset, or restored from a link, has to be able to reach
 *  the input it belongs in. */
describe('msToDateTimeLocal', () => {
  it('writes an instant in the shape the input takes', () => {
    expect(msToDateTimeLocal(Date.parse('2026-07-25T14:30'))).toBe(
      '2026-07-25T14:30',
    )
    // Single-digit parts padded, or the input rejects the value outright.
    expect(msToDateTimeLocal(Date.parse('2026-01-02T03:04'))).toBe(
      '2026-01-02T03:04',
    )
  })

  it('is empty for no bound at all, which is what clears the input', () => {
    expect(msToDateTimeLocal(undefined)).toBe('')
    expect(msToDateTimeLocal(Number.NaN)).toBe('')
  })

  it('round-trips a bound a listener could have typed', () => {
    const typed = '2026-07-25T14:30'
    expect(msToDateTimeLocal(dateTimeLocalToMs(typed))).toBe(typed)
  })

  /** The control has no seconds field, so a bound carrying them shows the
   *  minute it falls in. Truncating rather than rounding: a `before` of
   *  23:59:59.999 is the end of that day, and rounding it up would put the
   *  input a whole day past the search it is displaying. */
  it('truncates a bound finer than the control can show', () => {
    expect(msToDateTimeLocal(Date.parse('2026-07-25T14:30:59'))).toBe(
      '2026-07-25T14:30',
    )
  })
})

describe('formatCallTime', () => {
  it('formats in the listener timezone with fixed-width fields', () => {
    expect(formatCallTime(Date.parse('2026-07-25T14:32:05'), '24h')).toBe(
      '2026-07-25 14:32:05',
    )
    // Single-digit parts are padded so a list of calls stays aligned.
    expect(formatCallTime(Date.parse('2026-01-02T03:04:05'), '24h')).toBe(
      '2026-01-02 03:04:05',
    )
  })

  /** The Operator's twelve-hour clock (#77), still fixed-width — midnight and
   *  noon are the two hours a naive `% 12` gets wrong. */
  it.each([
    ['2026-07-25T14:32:05', '2026-07-25 02:32:05 PM'],
    ['2026-07-25T00:15:00', '2026-07-25 12:15:00 AM'],
    ['2026-07-25T12:00:00', '2026-07-25 12:00:00 PM'],
    ['2026-07-25T11:59:59', '2026-07-25 11:59:59 AM'],
    ['2026-07-25T23:00:00', '2026-07-25 11:00:00 PM'],
  ])('writes %s on a twelve-hour clock as %s', (at, written) => {
    expect(formatCallTime(Date.parse(at), '12h')).toBe(written)
  })

  it('shows a dash when a call has no time', () => {
    expect(formatCallTime(undefined, '24h')).toBe('—')
    expect(formatCallTime(Number.NaN, '12h')).toBe('—')
  })
})

/** A row's time (#78): the date only when it says something. A full stamp on
 *  every row cost a phone 125 pixels of the Talkgroup's name. */
describe('formatRowTime', () => {
  const now = Date.parse('2026-07-25T16:00:00')

  it.each([
    ['earlier today', '2026-07-25T14:32:05', '24h', '14:32:05'],
    ['earlier today, twelve-hour', '2026-07-25T14:32:05', '12h', '02:32:05 PM'],
    ['the first second of today', '2026-07-25T00:00:00', '24h', '00:00:00'],
    ['the last second of yesterday', '2026-07-24T23:59:59', '24h', '2026-07-24 23:59:59'],
    ['last year, same date', '2025-07-25T14:32:05', '24h', '2025-07-25 14:32:05'],
    ['yesterday, twelve-hour', '2026-07-24T09:05:00', '12h', '2026-07-24 09:05:00 AM'],
  ] as const)('writes a call from %s as %s', (_case, at, clock, written) => {
    expect(formatRowTime(Date.parse(at), clock, now)).toBe(written)
  })

  it('shows a dash when a call has no time', () => {
    expect(formatRowTime(undefined, '24h', now)).toBe('—')
    expect(formatRowTime(Number.NaN, '12h', now)).toBe('—')
  })
})

/** How late a Call is being heard (#78): a backlog, a Backfill, Catch-up and a
 *  Delay all play Calls minutes after the fact, and the display must not pass
 *  one off as happening now. */
describe('lateness', () => {
  const now = Date.parse('2026-07-25T16:00:00')
  const ago = (seconds: number) => now - seconds * 1000

  it.each([
    ['ended a moment ago', ago(5), undefined, null],
    ['ended just inside the grace', ago(59), undefined, null],
    ['ended a minute ago', ago(60), undefined, '1 min ago'],
    ['ended four minutes ago', ago(4 * 60 + 30), undefined, '4 min ago'],
    ['ended an hour ago', ago(60 * 60), undefined, '1 h ago'],
    ['ended an hour and twelve minutes ago', ago(72 * 60), undefined, '1 h 12 min ago'],
    ['ended a day ago', ago(24 * 3600), undefined, '1 d ago'],
    ['ended three days and change ago', ago(75 * 3600), undefined, '3 d ago'],
    // Measured from when it *ended*: a forty-second dispatch uploaded the
    // moment it unkeyed started forty seconds ago and is not late at all.
    ['a long call that just ended', ago(45), 40_000, null],
    ['a long call that ended two minutes ago', ago(160), 40_000, '2 min ago'],
    // A clock running ahead of the recorder's is not a call from the future.
    ['stamped in the future', ago(-30), undefined, null],
  ] as const)('a call that %s reads %s', (_case, timestamp, durationMs, said) => {
    expect(lateness({ timestamp, durationMs }, now)).toBe(said)
  })

  it('says nothing about a call with no time', () => {
    expect(lateness({ timestamp: undefined }, now)).toBeNull()
  })
})

describe('pageSummary', () => {
  it('counts from one and reports the true total', () => {
    expect(pageSummary(0, 100, 421)).toBe('1–100 of 421')
    expect(pageSummary(400, 21, 421)).toBe('401–421 of 421')
  })

  it('says so when there is nothing to show', () => {
    expect(pageSummary(0, 0, 0)).toBe('No calls')
    expect(pageSummary(500, 0, 421)).toBe('No calls')
  })
})

describe('formatDuration', () => {
  it('reads a transmission as a scanner operator says one', () => {
    // Under a minute is bare seconds — a call is almost always this.
    expect(formatDuration(900)).toBe('0.9s')
    expect(formatDuration(8250)).toBe('8.3s')
    expect(formatDuration(59_900)).toBe('59.9s')
    // Past a minute it becomes m:ss, because "94.2s" is not a length anyone
    // reads at a glance.
    expect(formatDuration(60_000)).toBe('1:00')
    expect(formatDuration(94_200)).toBe('1:34')
    expect(formatDuration(3_600_000)).toBe('60:00')
  })

  it('shows a dash for a Call whose length was never measured', () => {
    // Every Call ingested before #42, and anything whose container header could
    // not be read. Absent is not zero, and must not read as a kerchunk.
    expect(formatDuration(undefined)).toBe('—')
  })

  it('shows a dash rather than nonsense for an impossible value', () => {
    expect(formatDuration(Number.NaN)).toBe('—')
    expect(formatDuration(-1)).toBe('—')
  })
})

/** What a Star is worth here, said out loud (#66, spec US 37). */
describe('starsKept', () => {
  it.each([
    [{ kept: false, keptDays: 0 }, 'Stars do not outlast the retention window here.'],
    // `0` is "for good" in every window of `[retention]`, and "kept for 0 days"
    // would read as the exact opposite of what the Operator configured.
    [{ kept: true, keptDays: 0 }, 'Starred calls are kept indefinitely.'],
    [{ kept: true, keptDays: 90 }, 'Starred calls are kept for 90 days.'],
    // A policy switched off outranks whatever number is left beside it, so an
    // Operator who turned it off is not told a Call survives for a quarter.
    [{ kept: false, keptDays: 90 }, 'Stars do not outlast the retention window here.'],
  ])('reads %j as its sentence', (offer, sentence) => {
    expect(starsKept(offer)).toBe(sentence)
  })
})

/** The wall clock on the Live screen (#78) — rdio's, and a lost affordance:
 *  a scanner on a shelf is also the clock in the room. Minutes, on the
 *  Operator's clock. */
describe('formatClock', () => {
  it.each([
    ['2026-07-25T14:32:05', '24h', '14:32'],
    ['2026-07-25T04:05:59', '24h', '04:05'],
    ['2026-07-25T14:32:05', '12h', '02:32 PM'],
    ['2026-07-25T00:15:00', '12h', '12:15 AM'],
    ['2026-07-25T12:00:00', '12h', '12:00 PM'],
  ] as const)('reads %s on a %s clock as %s', (at, clock, written) => {
    expect(formatClock(Date.parse(at), clock)).toBe(written)
  })
})
