import { describe, expect, it } from 'vitest'

import {
  callCategory,
  formatFrequency,
  siteName,
  systemName,
  talkgroupName,
  unitHref,
  unitName,
} from './call'

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

describe('siteName', () => {
  it('names the tower a Call was heard on (#42, spec US 11)', () => {
    // A recorder sends a bare number and nothing else, so that is what a
    // listener gets — enough to tell one tower from another, which is the whole
    // of what makes simulcast coverage legible.
    expect(siteName({ ...call, siteRef: 3 })).toBe('Site 3')
  })

  it('says nothing at all on a single-site system', () => {
    // Most systems have one site and no recorder mentions it. A "Site —" on
    // every row would be clutter bought for nothing.
    expect(siteName(call)).toBeUndefined()
  })

  it('prefers the tower’s real name once mining has found one (#48)', () => {
    // "Site 1" tells a listener two towers apart; "Downtown" tells them which
    // half of the county is talking. SDRTrunk buries the name in the ID3 of
    // every MP3 it uploads, and mining is what digs it out.
    expect(siteName({ ...call, siteRef: 1, siteLabel: 'Downtown' })).toBe('Downtown')
  })

  it('names a tower whose Ref this instance minted itself', () => {
    // A mined Site has no Ref from the radio network — SDRTrunk sends none —
    // so the server assigns the lowest free one purely to have an identity. It
    // is not a number worth showing anybody, and the name is the whole point.
    expect(siteName({ ...call, siteLabel: 'North Ridge' })).toBe('North Ridge')
  })
})

describe('naming the radio that keyed (#47, spec US 42)', () => {
  it('prefers the name somebody gave it', () => {
    expect(unitName({ ...call, unitRef: 1200, unitLabel: 'Engine 1' })).toBe(
      'Engine 1',
    )
  })

  /** A bare Ref rather than a placeholder: the same number on three Calls says
   *  they are the same radio, which is the whole thing rdio-scanner throws away
   *  by never showing units at all. */
  it('falls back to the Ref, which is still an identity', () => {
    expect(unitName({ ...call, unitRef: 1200 })).toBe('1200')
  })

  it('is nothing at all when no radio was heard', () => {
    expect(unitName(call)).toBeUndefined()
    expect(unitHref(call)).toBeUndefined()
  })

  /** A Ref is unique only within its System (CONTEXT.md), so both are in the
   *  path — exactly as `GET /api/unit/{systemRef}/{ref}` takes them. */
  it('links to that radio within its own System', () => {
    expect(unitHref({ ...call, unitRef: 1200 })).toBe('/unit/11/1200')
  })
})
