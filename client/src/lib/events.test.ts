import { describe, expect, it } from 'vitest'

import {
  eventExportUrl,
  eventSummary,
  formatBytes,
  freezeNotice,
} from './events'
import type { FreezeReport } from '@/types'

const nothing: FreezeReport = {
  frozen: 0,
  alreadyHeld: 0,
  missing: 0,
  unreadable: 0,
}

describe('freezeNotice', () => {
  /** **Silence is the success case.** A notice that appeared every time would be
   *  furniture, and an Operator would stop reading the one that mattered. */
  it('says nothing when every call was frozen', () => {
    expect(freezeNotice({ ...nothing, frozen: 4 })).toBeNull()
  })

  /** A Call that aged out while they were reading the page is normal, and is
   *  still worth saying: the incident they kept is short one transmission. */
  it('names the calls that were already held', () => {
    expect(freezeNotice({ ...nothing, frozen: 2, alreadyHeld: 1 })).toBe(
      '2 calls added. 1 call was already in this event.',
    )
  })

  it('names the calls that have gone', () => {
    expect(freezeNotice({ ...nothing, frozen: 1, missing: 3 })).toBe(
      '1 call added. 3 calls are no longer in the archive.',
    )
  })

  /** **The one that means the store is unwell**, and the one an Operator acts
   *  on — they can add those Calls again once it is back, and the server wrote
   *  no row for them precisely so that works. */
  it('names the calls whose audio would not read', () => {
    expect(freezeNotice({ ...nothing, unreadable: 2 })).toBe(
      '0 calls added. 2 calls could not be read from storage.',
    )
  })

  it('joins every kind of trouble into one sentence', () => {
    expect(
      freezeNotice({ frozen: 1, alreadyHeld: 1, missing: 1, unreadable: 1 }),
    ).toBe(
      '1 call added. 1 call was already in this event; 1 call is no longer in the archive; 1 call could not be read from storage.',
    )
  })
})

describe('formatBytes', () => {
  /** Decimal units, because that is what a disk is sold in and what
   *  `[retention] max_size_gb` means — the number this is compared against. */
  it.each([
    [0, '0 B'],
    [512, '512 B'],
    [1_000, '1.0 kB'],
    [1_500, '1.5 kB'],
    [1_430_000_000, '1.4 GB'],
    [2_000_000_000_000, '2.0 TB'],
    // Past the largest unit it keeps counting in it rather than inventing one.
    [5_000_000_000_000_000, '5000.0 TB'],
  ])('renders %i as %s', (bytes, expected) => {
    expect(formatBytes(bytes)).toBe(expected)
  })

  /** A number that is not one is a dash rather than `NaN B`, which is what a
   *  count missing from an older server's document would otherwise render as. */
  it.each([[Number.NaN], [-1], [Number.POSITIVE_INFINITY]])(
    'renders %s as a dash',
    (bytes) => {
      expect(formatBytes(bytes)).toBe('—')
    },
  )
})

describe('eventSummary', () => {
  it('counts the calls and what they cost', () => {
    expect(eventSummary(12, 4_200_000)).toBe('12 calls · 4.2 MB kept')
  })

  it('does not pluralise one call', () => {
    expect(eventSummary(1, 900)).toBe('1 call · 900 B kept')
  })
})

describe('eventExportUrl', () => {
  it('names the event and the format', () => {
    expect(eventExportUrl(7, 'zip')).toBe(
      '/api/admin/events/7/export?format=zip',
    )
    expect(eventExportUrl(7, 'wav')).toBe(
      '/api/admin/events/7/export?format=wav',
    )
  })
})
