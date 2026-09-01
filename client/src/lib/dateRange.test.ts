import { describe, expect, it } from 'vitest'

import { PRESETS, rangeOf, type PresetId } from './dateRange'

/** A Tuesday afternoon, in whatever timezone the suite is running in — which is
 *  the Listener's, and the only one these presets are about. */
const NOW = Date.parse('2026-09-01T14:32:05')

const at = (iso: string) => Date.parse(iso)

describe('what a preset covers', () => {
  it('rolls the last hour back from now', () => {
    expect(rangeOf('last-hour', NOW)).toEqual({
      after: at('2026-09-01T13:32:05'),
      before: NOW,
    })
  })

  it('rolls the last seven days back from now', () => {
    expect(rangeOf('last-7-days', NOW)).toEqual({
      after: at('2026-08-25T14:32:05'),
      before: NOW,
    })
  })

  it('reads today as the whole calendar day, not as the hours so far', () => {
    expect(rangeOf('today', NOW)).toEqual({
      after: at('2026-09-01T00:00:00'),
      // Inclusive, per `SearchQuery.before` — so one millisecond short of
      // tomorrow rather than tomorrow itself.
      before: at('2026-09-02T00:00:00') - 1,
    })
  })

  it('reads yesterday as the calendar day before it', () => {
    expect(rangeOf('yesterday', NOW)).toEqual({
      after: at('2026-08-31T00:00:00'),
      before: at('2026-09-01T00:00:00') - 1,
    })
  })

  /** `before` is an *inclusive* bound on the wire, so two adjacent calendar
   *  presets that shared an instant would both match a Call struck at midnight
   *  — the one moment a scanner archive is guaranteed to have traffic on. */
  it('leaves no instant in both today and yesterday', () => {
    expect(rangeOf('yesterday', NOW).before).toBe(rangeOf('today', NOW).after - 1)
  })
})

/**
 * The calendar presets are computed from local date parts rather than by
 * subtracting 86,400,000 milliseconds, because a day is not always that long: on
 * the two days a year a timezone shifts, "yesterday" arrived at by arithmetic
 * lands an hour into the day before or an hour short of it.
 */
describe('a day that is not twenty-four hours long', () => {
  it.each(['today', 'yesterday'] as const)(
    '%s starts at a local midnight',
    (preset) => {
      const start = new Date(rangeOf(preset, NOW).after)
      expect([start.getHours(), start.getMinutes(), start.getSeconds()]).toEqual([
        0, 0, 0,
      ])
    },
  )

  it('ends yesterday where today begins, whatever happened overnight', () => {
    // Across a US fall-back Sunday (2026-11-01), where the local day is 25
    // hours long. Asserted as adjacency rather than as a length, so this holds
    // wherever the suite runs — including timezones with no shift at all.
    const overnight = Date.parse('2026-11-02T09:00:00')
    const yesterday = rangeOf('yesterday', overnight)
    const today = rangeOf('today', overnight)
    expect(yesterday.before + 1).toBe(today.after)
    expect(new Date(yesterday.after).getDate()).toBe(1)
  })
})

describe('the presets offered', () => {
  it('is the set the ticket names, in the order a listener reaches for them', () => {
    expect(PRESETS.map((preset) => preset.id)).toEqual([
      'last-hour',
      'today',
      'yesterday',
      'last-7-days',
    ] satisfies PresetId[])
  })

  it('names each one for a control a thumb can read', () => {
    expect(PRESETS.map((preset) => preset.label)).toEqual([
      'Last hour',
      'Today',
      'Yesterday',
      'Last 7 days',
    ])
  })
})
