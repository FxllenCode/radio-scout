import { describe, expect, it } from 'vitest'

import {
  dateTimeLocalToMs,
  downloadUrl,
  formatCallTime,
  formatDuration,
  pageSummary,
  searchParams,
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

describe('formatCallTime', () => {
  it('formats in the listener timezone with fixed-width fields', () => {
    expect(formatCallTime(Date.parse('2026-07-25T14:32:05'))).toBe(
      '2026-07-25 14:32:05',
    )
    // Single-digit parts are padded so a list of calls stays aligned.
    expect(formatCallTime(Date.parse('2026-01-02T03:04:05'))).toBe(
      '2026-01-02 03:04:05',
    )
  })

  it('shows a dash when a call has no time', () => {
    expect(formatCallTime(undefined)).toBe('—')
    expect(formatCallTime(Number.NaN)).toBe('—')
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
