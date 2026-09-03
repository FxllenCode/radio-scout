import { describe, expect, it } from 'vitest'

import type { Catalog } from '@/types'

import { avoidMinutesLeft, avoidedRows } from './avoiding'

const CATALOG: Catalog = {
  activityWindowMs: 86_400_000,
  sharing: true,
  export: { enabled: true, maxCalls: 1000 },
  systems: [
    {
      ref: 11,
      label: 'County',
      talkgroups: [
        { ref: 100, label: 'Fire Dispatch', groups: [] },
        { ref: 200, name: 'TAC 2', groups: [] },
        { ref: 300, groups: [] },
      ],
    },
    { ref: 12, talkgroups: [{ ref: 100, label: 'Alpha', groups: [] }] },
  ],
}

const keys = (rows: ReturnType<typeof avoidedRows>) => rows.map((row) => row.key)

describe('the Avoid sheet (#58, spec US 25)', () => {
  it('has no rows when nothing is silenced', () => {
    expect(avoidedRows({}, CATALOG)).toEqual([])
  })

  it('names each silenced Talkgroup and says when it lapses', () => {
    const [row] = avoidedRows({ '11:100': 0 }, CATALOG)

    expect(row).toEqual({
      key: '11:100',
      label: 'Fire Dispatch',
      systemLabel: 'County',
      talkgroupRef: 100,
      until: 0,
    })
  })

  it('falls back through name, then the Ref', () => {
    const rows = avoidedRows({ '11:200': 0, '11:300': 0 }, CATALOG)

    expect(rows.map((row) => row.label)).toEqual(['TAC 2', 'Talkgroup 300'])
  })

  /**
   * An Avoid outlives the catalog it was placed from: a Talkgroup an Operator
   * renumbered, or a **Profile** restored before the catalog has been fetched.
   * A row that vanished in either case would leave a channel silenced with no
   * way to lift it, since the panel draws the catalog and could not show it
   * either.
   */
  it.each([
    ['the catalog has no such Talkgroup', CATALOG],
    ['the catalog has not arrived yet', undefined],
  ])('still lists an Avoid when %s', (_when, catalog) => {
    const [row] = avoidedRows({ '99:42': 0 }, catalog)

    expect(row).toMatchObject({
      key: '99:42',
      label: 'Talkgroup 42',
      systemLabel: 'System 99',
    })
  })

  /**
   * The map is keyed by string, so its own order puts `11:1000` before
   * `11:99` — a list a Listener reads twice has to read the same way twice, and
   * by something they can see.
   */
  it('orders by System and then by name, never by the key', () => {
    const rows = avoidedRows(
      { '12:100': 0, '11:200': 0, '11:100': 0 },
      CATALOG,
    )

    // County (Fire Dispatch, TAC 2) before System 12 (Alpha).
    expect(keys(rows)).toEqual(['11:100', '11:200', '12:100'])
  })
})

describe('how long a timed Avoid has left', () => {
  const NOW = 1_000_000

  it('is the minutes remaining, rounded up', () => {
    expect(avoidMinutesLeft(NOW, NOW + 30 * 60_000)).toBe(30)
    expect(avoidMinutesLeft(NOW, NOW + 90_000)).toBe(2)
  })

  /** A row reading `0m` beside a channel that is still silent is the countdown
   *  lying about the one thing it is for. */
  it('never reads as none while the Avoid is still in force', () => {
    expect(avoidMinutesLeft(NOW, NOW + 1)).toBe(1)
    expect(avoidMinutesLeft(NOW, NOW)).toBe(1)
  })

  /** A clock a second ahead of the server's must not produce a negative. */
  it('floors at a minute even past the deadline', () => {
    expect(avoidMinutesLeft(NOW, NOW - 60_000)).toBe(1)
  })
})
