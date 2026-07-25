import { describe, expect, it } from 'vitest'

import { callCategory, formatFrequency, systemName, talkgroupName } from './call'

const call = {
  id: 1,
  systemRef: 11,
  talkgroupRef: 54241,
  audioUrl: '/api/call/1/audio',
}

describe('naming a Call', () => {
  it('prefers the labels the recorder sent', () => {
    const labelled = {
      ...call,
      systemLabel: 'Fulton County',
      talkgroupLabel: 'FD Dispatch',
    }

    expect(talkgroupName(labelled)).toBe('FD Dispatch')
    expect(systemName(labelled)).toBe('Fulton County')
  })

  /** Auto-populate (#8) names most Talkgroups, but a recorder that sends no
   *  labels must still produce a display a listener can read. */
  it('falls back to the Refs when it sent none', () => {
    expect(talkgroupName(call)).toBe('Talkgroup 54241')
    expect(systemName(call)).toBe('System 11')
  })

  it('reads the tag and group as one category line', () => {
    expect(
      callCategory({ ...call, talkgroupTag: 'Fire Dispatch', talkgroupGroup: 'Fire' }),
    ).toBe('Fire Dispatch · Fire')
    expect(callCategory({ ...call, talkgroupTag: 'Law' })).toBe('Law')
    expect(callCategory(call)).toBe('')
  })
})

describe('formatting a frequency', () => {
  /** Recorders send hertz; a scanner display reads megahertz, and the six
   *  decimals are what distinguish adjacent channels. */
  it.each([
    [853_412_500, '853.412500'],
    [154_265_000, '154.265000'],
    [0, '0.000000'],
  ])('shows %i Hz as %s MHz', (hertz, shown) => {
    expect(formatFrequency(hertz)).toBe(shown)
  })

  it('shows a dash when the recorder sent none', () => {
    expect(formatFrequency(undefined)).toBe('—')
  })
})
