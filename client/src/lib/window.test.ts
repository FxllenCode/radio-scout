import { describe, expect, it } from 'vitest'

import { WHOLE_BELOW, windowOf, type WindowInput } from './window'

/** A county System, at the scale #57 exists for, scrolled to the top. */
const COUNTY: WindowInput = {
  count: 400,
  rowHeight: 44,
  scrolled: 0,
  viewport: 660,
  overscan: 4,
}

const view = (over: Partial<WindowInput> = {}) => windowOf({ ...COUNTY, ...over })

describe('windowOf (#57)', () => {
  it('draws the rows on screen and a few either side, not 400', () => {
    expect(view()).toEqual({
      start: 0,
      // 660px of viewport is 15 rows, plus the overscan below.
      end: 19,
      padTop: 0,
      padBottom: (400 - 19) * 44,
    })
  })

  it('follows the scroll down the list', () => {
    expect(view({ scrolled: 100 * 44 })).toEqual({
      start: 96,
      end: 119,
      padTop: 96 * 44,
      padBottom: (400 - 119) * 44,
    })
  })

  /** The list is still below the fold: nothing has scrolled past the top, so
   *  the first row is the first row — never a negative index. */
  it('clamps at the top while the list is still below the fold', () => {
    expect(view({ scrolled: -2_000 })).toMatchObject({ start: 0, padTop: 0 })
  })

  /** Scrolled clean past: every row is above the viewport, so the section is
   *  all padding and no rows — which is what keeps a long panel cheap once the
   *  Listener is reading the System below it. */
  it('draws nothing once the whole list is above the viewport', () => {
    expect(view({ scrolled: 400 * 44 + 5_000 })).toEqual({
      start: 400,
      end: 400,
      padTop: 400 * 44,
      padBottom: 0,
    })
  })

  /**
   * The list occupies exactly the same height whether or not it is windowed.
   * Without it the page's scroll height changes as the Listener scrolls, which
   * a browser answers by moving the scroll position — the jitter that makes a
   * hand-rolled window feel broken.
   */
  it('always stands as tall as the whole list', () => {
    for (const scrolled of [-500, 0, 1, 43, 44, 4_000, 17_600, 99_999]) {
      for (const count of [0, 1, WHOLE_BELOW, 400]) {
        const { start, end, padTop, padBottom } = view({ scrolled, count })
        expect(padTop + (end - start) * 44 + padBottom).toBe(count * 44)
        expect(start).toBeLessThanOrEqual(end)
      }
    }
  })

  /** Windowing costs a scroll listener, a measurement and rows that appear as
   *  you reach them. A panel of eight is not worth any of it. */
  it('draws a short list whole, however small the viewport', () => {
    expect(view({ count: WHOLE_BELOW, viewport: 20, scrolled: 900 })).toEqual({
      start: 0,
      end: WHOLE_BELOW,
      padTop: 0,
      padBottom: 0,
    })
  })

  it('windows the first list too long to draw whole', () => {
    expect(view({ count: WHOLE_BELOW + 1, viewport: 20, scrolled: 0 }).end).toBeLessThan(
      WHOLE_BELOW,
    )
  })

  it('has nothing to draw for an empty list', () => {
    expect(view({ count: 0 })).toEqual({ start: 0, end: 0, padTop: 0, padBottom: 0 })
  })
})
