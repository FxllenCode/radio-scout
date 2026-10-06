import { describe, expect, it } from 'vitest'

import { EVERYTHING, type Selection } from '@/lib/selection'

import { stationQuery } from './station'

/** Talkgroup 2 on System 100, and nothing else. */
const ONE_CHANNEL: Selection = { all: false, sel: { '100': { '2': true } } }

describe('stationQuery', () => {
  it('carries the Selection in the spelling a share link uses', () => {
    expect(stationQuery(ONE_CHANNEL)).toBe('sel=0_100.2')
    expect(stationQuery(EVERYTHING)).toBe('sel=1')
  })

  it('carries the grant this browser holds, so a gated channel plays on the speaker too', () => {
    expect(stationQuery(ONE_CHANNEL, 'rsg_abc123')).toBe('sel=0_100.2&grant=rsg_abc123')
  })

  it('carries no grant when this browser holds none', () => {
    expect(stationQuery(ONE_CHANNEL, undefined)).toBe('sel=0_100.2')
    expect(stationQuery(ONE_CHANNEL, '')).toBe('sel=0_100.2')
  })
})
