/**
 * Which rows of a long list to draw, measured against the page (#57).
 *
 * The adapter half of `lib/window`: it measures where the list sits in the
 * viewport and hands the numbers to [`windowOf`], which decides. Nothing here
 * knows what a row is or what a panel is — a list ref, a count and a row
 * height, and a range back.
 *
 * # Why the page and not a container
 *
 * The Talkgroups panel scrolls with the page. A scroll container of its own
 * would cost iOS its momentum chaining, its pull-to-refresh and the URL-bar
 * collapse, and would put the sticky controls in a different coordinate space
 * than the rest of the app's chrome. So the list is measured where it is:
 * `getBoundingClientRect().top` is how far its first row sits below the top of
 * the viewport, and the negative of that is how far it has scrolled past.
 *
 * # Why the state is the range and not the scroll position
 *
 * A scroll event lands per frame. Keeping the pixel offset in state would
 * re-render the list sixty times a second on a flick; keeping the *range* means
 * a render only when the drawn rows actually change — once per overscan's worth
 * of scrolling. The measurement itself is coalesced into an animation frame,
 * which is the only rate at which its answer could matter.
 *
 * # Why it re-measures after every render, not only on scroll
 *
 * A list moves up and down the page without anybody scrolling it: a System
 * folding away above it, a pinned section appearing, the filter emptying a
 * section. None of those is a scroll event and none of them changes this list's
 * own row count, so a window measured only on scroll would keep drawing the
 * rows that used to be on screen — a section showing nothing but padding until
 * the Listener happens to scroll. The extra cost is one `getBoundingClientRect`
 * per render of the list, and the measurement settles in one pass because a
 * window that has not moved sets no state.
 */
import { useCallback, useLayoutEffect, useRef, useState, type RefObject } from 'react'

import { windowOf, type WindowView } from '@/lib/window'

export function useWindowedRows(
  count: number,
  rowHeight: number,
): { ref: RefObject<HTMLUListElement | null>; view: WindowView } {
  const ref = useRef<HTMLUListElement>(null)
  // Before the first measurement the viewport is unknown, which draws the
  // overscan and nothing else — for the one frame between mount and layout.
  // A short list is whole either way, so nothing below the threshold ever
  // sees it.
  const [view, setView] = useState<WindowView>(() =>
    windowOf({ count, rowHeight, scrolled: 0, viewport: 0 }),
  )

  const measure = useCallback(() => {
    const list = ref.current
    if (!list) return
    const next = windowOf({
      count,
      rowHeight,
      scrolled: -list.getBoundingClientRect().top,
      viewport: window.innerHeight,
    })
    setView((drawn) => (same(drawn, next) ? drawn : next))
  }, [count, rowHeight])

  // After every render, deliberately — see the note above on lists that move
  // without being scrolled.
  useLayoutEffect(measure)

  useLayoutEffect(() => {
    let frame = 0
    const onScroll = () => {
      frame ||= requestAnimationFrame(() => {
        frame = 0
        measure()
      })
    }

    window.addEventListener('scroll', onScroll, { passive: true })
    window.addEventListener('resize', onScroll)
    return () => {
      if (frame) cancelAnimationFrame(frame)
      window.removeEventListener('scroll', onScroll)
      window.removeEventListener('resize', onScroll)
    }
  }, [measure])

  return { ref, view }
}

const same = (a: WindowView, b: WindowView) =>
  a.start === b.start &&
  a.end === b.end &&
  a.padTop === b.padTop &&
  a.padBottom === b.padBottom
