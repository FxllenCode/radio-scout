import { describe, expect, it } from 'vitest'

import {
  BAND_HIGH_HZ,
  BAND_LOW_HZ,
  DEFAULT_GAP_MAX_MS,
  DEFAULT_TOLERANCE_PCT,
  MAX_GAP_MS,
  MAX_STEPS,
  MAX_TOLERANCE_PCT,
  MIN_STEP_MS,
  sequenceLine,
  toneProfileLine,
  unusable,
} from './tone'

const quickCall = [
  { hz: 1122.5, minMs: 800 },
  { hz: 1465.6, minMs: 2000 },
]

describe('unusable', () => {
  it('accepts the sequence a station is actually paged with', () => {
    expect(
      unusable(quickCall, DEFAULT_TOLERANCE_PCT, DEFAULT_GAP_MAX_MS),
    ).toBeNull()
  })

  it.each([
    ['a single long group tone', [{ hz: 1050, minMs: 6000 }]],
    [
      'a two-tone page followed by a group tone',
      [...quickCall, { hz: 1050, minMs: 4000 }],
    ],
  ])('accepts %s, which a fixed A/B pair could not spell', (_what, steps) => {
    expect(unusable(steps, DEFAULT_TOLERANCE_PCT, DEFAULT_GAP_MAX_MS)).toBeNull()
  })

  // Every one of these is a profile that would be stored, never fire, and
  // produce no error anywhere — which is the failure this function exists for.
  it.each([
    ['no tones at all', [], DEFAULT_TOLERANCE_PCT, DEFAULT_GAP_MAX_MS, 'at least one tone'],
    [
      'more tones than a sequence has',
      Array.from({ length: MAX_STEPS + 1 }, () => ({ hz: 1000, minMs: 500 })),
      DEFAULT_TOLERANCE_PCT,
      DEFAULT_GAP_MAX_MS,
      'at most',
    ],
    [
      'a tone below the band',
      [{ hz: BAND_LOW_HZ - 1, minMs: 800 }],
      DEFAULT_TOLERANCE_PCT,
      DEFAULT_GAP_MAX_MS,
      'outside',
    ],
    [
      'a tone above the band',
      [{ hz: BAND_HIGH_HZ + 1, minMs: 800 }],
      DEFAULT_TOLERANCE_PCT,
      DEFAULT_GAP_MAX_MS,
      'outside',
    ],
    [
      'a tone too short to measure',
      [{ hz: 1122.5, minMs: MIN_STEP_MS - 1 }],
      DEFAULT_TOLERANCE_PCT,
      DEFAULT_GAP_MAX_MS,
      'held for at least',
    ],
    ['no tolerance at all', quickCall, 0, DEFAULT_GAP_MAX_MS, 'tolerance'],
    [
      'a tolerance wide enough to page the neighbouring station',
      quickCall,
      MAX_TOLERANCE_PCT + 1,
      DEFAULT_GAP_MAX_MS,
      'tolerance',
    ],
    ['a negative gap', quickCall, DEFAULT_TOLERANCE_PCT, -1, 'gap'],
    [
      'a gap so long the tones are not a sequence',
      quickCall,
      DEFAULT_TOLERANCE_PCT,
      MAX_GAP_MS + 1,
      'gap',
    ],
  ])('refuses %s', (_what, steps, tolerance, gap, expected) => {
    expect(unusable(steps, tolerance, gap)).toContain(expected)
  })

  // The boundaries are the numbers a form's own inputs offer, so an Operator
  // choosing the extreme of a control must not be told it is out of range.
  // **Every bound is checked from both sides** — the refusals above sit one step
  // outside, these sit exactly on it — because a ceiling written `>=` where it
  // meant `>` refuses the last legal value, and a table that only ever tests the
  // middle stays green through it.
  it.each([
    [BAND_LOW_HZ],
    [BAND_HIGH_HZ],
  ])('accepts %s Hz, which is the edge of the band', (hz) => {
    expect(
      unusable([{ hz, minMs: MIN_STEP_MS }], MAX_TOLERANCE_PCT, MAX_GAP_MS),
    ).toBeNull()
  })

  it('accepts a sequence of exactly the longest length allowed', () => {
    const steps = Array.from({ length: MAX_STEPS }, () => ({
      hz: 1122.5,
      minMs: MIN_STEP_MS,
    }))

    expect(unusable(steps, DEFAULT_TOLERANCE_PCT, 0)).toBeNull()
  })
})

describe('sequenceLine', () => {
  it('reads the way an Operator says it out loud', () => {
    expect(sequenceLine(quickCall)).toBe(
      '1122.5 Hz for 0.8s → 1465.6 Hz for 2.0s',
    )
  })

  it('says so when there is nothing to page with', () => {
    expect(sequenceLine([])).toBe('no tones')
  })
})

describe('toneProfileLine', () => {
  it('carries the sequence and its slack', () => {
    expect(
      toneProfileLine({
        steps: quickCall,
        tolerancePct: 2,
        disabled: false,
      }),
    ).toBe('1122.5 Hz for 0.8s → 1465.6 Hz for 2.0s · ±2%')
  })

  // A disabled profile is otherwise indistinguishable from one that simply has
  // not been paged, which is the state an Operator most needs to be able to see.
  it('says when a profile is switched off', () => {
    expect(
      toneProfileLine({ steps: quickCall, tolerancePct: 2, disabled: true }),
    ).toContain('disabled')
  })
})
