import { describe, expect, it } from 'vitest'

import { WAVEFORM_BARS, barsLit, waveformBars } from './waveform'

describe('waveform bars', () => {
  it('is the same shape every time a Call is drawn', () => {
    const first = waveformBars(4242)

    expect(waveformBars(4242)).toEqual(first)
    expect(first).toHaveLength(WAVEFORM_BARS)
  })

  it('gives different Calls different shapes', () => {
    expect(waveformBars(1)).not.toEqual(waveformBars(2))
  })

  it('stays within the height the display can draw', () => {
    for (const seed of [0, 1, 7, 4242, 999999]) {
      for (const bar of waveformBars(seed)) {
        expect(bar).toBeGreaterThan(0)
        expect(bar).toBeLessThanOrEqual(1)
      }
    }
  })

  /** A transmission opens and closes quieter than it runs, so the envelope
   *  tapers — otherwise the bars read as a solid block rather than as speech. */
  it('tapers at both ends', () => {
    const bars = waveformBars(4242)
    const edges = [...bars.slice(0, 4), ...bars.slice(-4)]
    const middle = bars.slice(WAVEFORM_BARS / 2 - 4, WAVEFORM_BARS / 2 + 4)

    const mean = (values: number[]) =>
      values.reduce((total, value) => total + value, 0) / values.length
    expect(mean(edges)).toBeLessThan(mean(middle))
  })

  describe('the played portion', () => {
    it.each([
      ['nothing at the start', 0, 0],
      ['half way', 0.5, WAVEFORM_BARS / 2],
      ['all of it at the end', 1, WAVEFORM_BARS],
    ])('lights %s', (_case, progress, expected) => {
      expect(barsLit(progress, WAVEFORM_BARS)).toBe(expected)
    })

    /** Before `loadedmetadata` a duration is `NaN` and progress is meaningless;
     *  a seek past the end or a stale `currentTime` can overshoot. */
    it.each([Number.NaN, -1, 4])('survives a nonsense progress of %s', (progress) => {
      const lit = barsLit(progress, WAVEFORM_BARS)

      expect(lit).toBeGreaterThanOrEqual(0)
      expect(lit).toBeLessThanOrEqual(WAVEFORM_BARS)
    })
  })
})
