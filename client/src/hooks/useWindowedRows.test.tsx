import { act, render, renderHook, screen, waitFor } from '@testing-library/react'
import { afterEach, describe, expect, it, vi } from 'vitest'

import { OVERSCAN, WHOLE_BELOW, type WindowView } from '@/lib/window'

import { useWindowedRows } from './useWindowedRows'

const ROW = 44
const COUNT = 400

/** A list that reports the window it was given, and counts how often it was
 *  redrawn — the two things this hook is answerable for. `nudge` is anything
 *  else on the page changing: it re-renders the list without touching its row
 *  count and without a scroll. */
function Windowed({
  count = COUNT,
  attach = true,
  nudge,
}: {
  count?: number
  attach?: boolean
  nudge?: unknown
}) {
  const { ref, view } = useWindowedRows(count, ROW)
  drawn += 1
  return (
    <ul
      ref={attach ? ref : undefined}
      data-testid="list"
      data-nudge={String(nudge)}
      data-view={JSON.stringify(view)}
    />
  )
}

let drawn = 0

const view = (): WindowView => JSON.parse(screen.getByTestId('list').dataset.view!)

/** jsdom lays nothing out, so where the list sits is something a test states. */
function sitting(top: number) {
  return vi.spyOn(Element.prototype, 'getBoundingClientRect').mockReturnValue({
    top,
  } as DOMRect)
}

/** Scroll, and let the animation frame the hook coalesced into land. */
async function scroll(times = 1) {
  await act(async () => {
    for (let index = 0; index < times; index += 1) {
      window.dispatchEvent(new Event('scroll'))
    }
    await new Promise((resolve) => requestAnimationFrame(resolve))
  })
}

afterEach(() => {
  vi.restoreAllMocks()
  drawn = 0
})

describe('useWindowedRows (#57)', () => {
  it('measures the list against the viewport on the way in', () => {
    sitting(0)
    render(<Windowed />)

    // jsdom's window is 768 tall: seventeen and a half rows, plus the overscan.
    expect(view()).toEqual({
      start: 0,
      end: Math.ceil(768 / ROW) + OVERSCAN,
      padTop: 0,
      padBottom: (COUNT - Math.ceil(768 / ROW) - OVERSCAN) * ROW,
    })
  })

  it('follows the list up the viewport as the page scrolls', async () => {
    sitting(0)
    render(<Windowed />)

    sitting(-100 * ROW)
    await scroll()

    expect(view().start).toBe(100 - OVERSCAN)
  })

  /**
   * A scroll event lands per frame. Redrawing on each one would spend a phone's
   * frame budget on a list that has not changed — so a scroll that does not
   * move the drawn range does not redraw.
   */
  it('redraws only when the drawn rows actually change', async () => {
    sitting(0)
    render(<Windowed />)
    await scroll()
    const settled = drawn

    sitting(-1)
    await scroll()
    expect(drawn).toBe(settled)

    sitting(-100 * ROW)
    await scroll()
    expect(drawn).toBeGreaterThan(settled)
  })

  /** Several scroll events inside one frame are one measurement: reading
   *  layout is the expensive half, and doing it twice cannot answer twice.
   *  Measured against a scroll that moves nothing, so the count is the frame's
   *  and not a re-render's. */
  it('measures once per frame however many scroll events land', async () => {
    const measure = sitting(0)
    render(<Windowed />)
    measure.mockClear()

    await scroll(5)

    expect(measure).toHaveBeenCalledTimes(1)
  })

  /**
   * A list moves without being scrolled: a System folds away above it, a pinned
   * section appears, a filter empties the section over it. None of that is a
   * scroll event and none of it changes this list's row count — so a window
   * measured only on scroll would go on drawing the rows that *used* to be on
   * screen, and a section below a fold would show nothing but padding until the
   * Listener happened to scroll.
   */
  it('re-measures when the list moves without being scrolled', () => {
    sitting(0)
    const { rerender } = render(<Windowed nudge="before" />)
    expect(view().start).toBe(0)

    sitting(-100 * ROW)
    rerender(<Windowed nudge="after" />)

    expect(view().start).toBe(100 - OVERSCAN)
  })

  it('stops measuring once the list is gone', async () => {
    sitting(0)
    const cancel = vi.spyOn(globalThis, 'cancelAnimationFrame')
    const { unmount } = render(<Windowed />)
    sitting(-100 * ROW)
    window.dispatchEvent(new Event('scroll'))

    unmount()

    expect(cancel).toHaveBeenCalled()
    await waitFor(() => expect(screen.queryByTestId('list')).toBeNull())
  })

  /** A list too short to be worth windowing is whole, so nothing about it
   *  depends on having been measured at all. */
  it('draws a short list whole', () => {
    sitting(0)
    render(<Windowed count={WHOLE_BELOW} />)

    expect(view()).toEqual({ start: 0, end: WHOLE_BELOW, padTop: 0, padBottom: 0 })
  })

  /** The ref is the hook's only way to find the list. Handed to nothing, it
   *  measures nothing and leaves the window where it was, rather than throwing
   *  under a Listener who is only trying to pick a Talkgroup. */
  it('does nothing at all when the ref reaches no list', async () => {
    sitting(-100 * ROW)
    const { result } = renderHook(() => useWindowedRows(COUNT, ROW))
    const before = result.current.view

    await scroll()

    expect(result.current.view).toEqual(before)
    expect(before.start).toBe(0)
  })
})
