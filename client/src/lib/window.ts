/**
 * Windowed rendering: which rows of a long list are worth drawing (#57).
 *
 * A county System is 400+ Talkgroups, and the Talkgroups panel has to scroll
 * smoothly *while audio plays* on a phone. Every row in the DOM is a button,
 * an LED, a label and two counters; four hundred of them are a scroll a
 * Pi-class device cannot keep up with, and React has to reconcile all of them
 * every time the **Selection** changes.
 *
 * So the list draws the rows on screen, a few either side, and stands in for
 * the rest with padding. This is the whole of the rule, as a value: the screen
 * hands it a scroll position and gets back a range and two paddings. Nothing
 * here measures anything, which is what makes every edge — the list still
 * below the fold, the list scrolled clean past, the empty list — a case in a
 * table rather than a thing to be reproduced by scrolling a browser.
 *
 * # Why the height is preserved exactly
 *
 * `padTop + drawn + padBottom` is always the whole list's height. A windowed
 * list that stands shorter than it is changes the page's scroll height as the
 * Listener scrolls it, and the browser answers by moving the scroll position —
 * which is the jitter that makes hand-rolled windowing feel broken. The
 * invariant is asserted rather than commented.
 *
 * # Why it windows against the page
 *
 * The panel scrolls with the page rather than inside a container of its own.
 * A nested scroller on iOS gives up momentum chaining, pull-to-refresh and the
 * URL-bar collapse, and sticky headers inside one behave differently again —
 * so the list measures where it sits in the viewport instead
 * ([`useWindowedRows`]), and the caller passes the result here.
 */

/** Rows shorter than this are drawn whole. Windowing costs a scroll listener,
 *  a measurement per frame and rows that materialize as you reach them; a
 *  panel of eight Talkgroups is not worth any of it, and neither is a filtered
 *  view that found three. */
export const WHOLE_BELOW = 60

/** How many rows above and below the viewport are drawn anyway, so a fast
 *  flick has something to show before the next measurement lands. */
export const OVERSCAN = 6

export interface WindowInput {
  count: number
  /** Every row is the same height, set from one constant so the number here
   *  and the number the browser lays out cannot drift. */
  rowHeight: number
  /** How far the list's first row has scrolled *past* the top of the viewport.
   *  Negative while the list has not reached it yet. */
  scrolled: number
  /** How much of the viewport the list could occupy. */
  viewport: number
  overscan?: number
}

/** The rows to draw, and what stands in for the ones that aren't. */
export interface WindowView {
  /** First row to draw. */
  start: number
  /** One past the last — `rows.slice(start, end)`. */
  end: number
  padTop: number
  padBottom: number
}

export function windowOf({
  count,
  rowHeight,
  scrolled,
  viewport,
  overscan = OVERSCAN,
}: WindowInput): WindowView {
  if (count <= WHOLE_BELOW) {
    return { start: 0, end: count, padTop: 0, padBottom: 0 }
  }

  const start = clamp(Math.floor(scrolled / rowHeight) - overscan, 0, count)
  const end = clamp(Math.ceil((scrolled + viewport) / rowHeight) + overscan, start, count)

  return {
    start,
    end,
    padTop: start * rowHeight,
    padBottom: (count - end) * rowHeight,
  }
}

const clamp = (value: number, low: number, high: number) =>
  Math.min(Math.max(value, low), high)
