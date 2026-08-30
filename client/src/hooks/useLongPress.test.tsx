import { fireEvent, render, screen } from '@testing-library/react'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

import { LONG_PRESS_MS, useLongPress } from './useLongPress'

const tapped: string[] = []
const held: string[] = []

/**
 * A list whose rows can change under the finger — which is the whole hazard.
 * `rows` is what it draws now; a re-render with a different list is a Call
 * arriving while somebody is holding a row down.
 */
function Rows({ rows }: { rows: string[] }) {
  const press = useLongPress<string>({
    onTap: (row) => tapped.push(row),
    onHold: (row) => held.push(row),
  })

  return (
    <ul>
      {rows.map((row) => (
        <li key={row}>
          <button type="button" {...press(row)}>
            {row}
          </button>
        </li>
      ))}
    </ul>
  )
}

const row = (name: string) => screen.getByRole('button', { name })

beforeEach(() => {
  tapped.length = 0
  held.length = 0
  vi.useFakeTimers()
})

afterEach(() => vi.useRealTimers())

describe('a long press (#58)', () => {
  it('taps when the finger comes up before the press is long', () => {
    render(<Rows rows={['a']} />)

    fireEvent.pointerDown(row('a'))
    vi.advanceTimersByTime(LONG_PRESS_MS - 1)
    fireEvent.pointerUp(row('a'))
    fireEvent.click(row('a'))

    expect(tapped).toEqual(['a'])
    expect(held).toEqual([])
  })

  it('holds when the finger stays down', () => {
    render(<Rows rows={['a']} />)

    fireEvent.pointerDown(row('a'))
    vi.advanceTimersByTime(LONG_PRESS_MS)

    expect(held).toEqual(['a'])
  })

  /** A press that opened a menu must not also do what a tap does — otherwise
   *  holding a session-log row to reach *Avoid* would replay the Call first. */
  it('does not also tap the click that ends a long press', () => {
    render(<Rows rows={['a']} />)

    fireEvent.pointerDown(row('a'))
    vi.advanceTimersByTime(LONG_PRESS_MS)
    fireEvent.pointerUp(row('a'))
    fireEvent.click(row('a'))

    expect(held).toEqual(['a'])
    expect(tapped).toEqual([])
  })

  /** ...and the suppression lasts exactly one click. */
  it('taps again on the next press', () => {
    render(<Rows rows={['a']} />)

    fireEvent.pointerDown(row('a'))
    vi.advanceTimersByTime(LONG_PRESS_MS)
    fireEvent.pointerUp(row('a'))
    fireEvent.click(row('a'))

    fireEvent.pointerDown(row('a'))
    fireEvent.pointerUp(row('a'))
    fireEvent.click(row('a'))

    expect(tapped).toEqual(['a'])
  })

  /**
   * #58's fifth criterion, and the reason the subject is captured at
   * `pointerdown` rather than read when the timer fires. A live list re-renders
   * under the finger — the session log grows at the top, the queue re-orders as
   * a **Priority** Call arrives — and a press must be about the Call it started
   * on or it is a mis-tap with a menu attached.
   */
  it('acts on the row the finger went down on, however the list moved', () => {
    const { rerender } = render(<Rows rows={['a', 'b']} />)

    fireEvent.pointerDown(row('a'))
    // A Call arrives: the list is rebuilt and every row's handlers are rebound.
    rerender(<Rows rows={['new', 'a', 'b']} />)
    vi.advanceTimersByTime(LONG_PRESS_MS)

    expect(held).toEqual(['a'])
  })

  it.each([
    ['the finger is lifted', 'pointerUp'],
    ['the finger slides off the row', 'pointerLeave'],
    ['the browser takes the gesture away', 'pointerCancel'],
  ] as const)('is called off when %s', (_when, event) => {
    render(<Rows rows={['a']} />)

    fireEvent.pointerDown(row('a'))
    fireEvent[event](row('a'))
    vi.advanceTimersByTime(LONG_PRESS_MS * 2)

    expect(held).toEqual([])
  })

  /** A press on a second row while one is pending is one gesture replacing
   *  another, not two timers racing. */
  it('starts over rather than stacking when a second press begins', () => {
    render(<Rows rows={['a', 'b']} />)

    fireEvent.pointerDown(row('a'))
    vi.advanceTimersByTime(LONG_PRESS_MS - 1)
    fireEvent.pointerDown(row('b'))
    vi.advanceTimersByTime(LONG_PRESS_MS)

    expect(held).toEqual(['b'])
  })

  /** The platform's own long-press menu would cover ours, and on iOS it also
   *  starts a text selection over the row. */
  it('keeps the browser from opening its own menu', () => {
    render(<Rows rows={['a']} />)

    expect(fireEvent.contextMenu(row('a'))).toBe(false)
  })

  /** A row unmounted mid-press — the queue sheet closing, the Call playing —
   *  must not fire into a component that is gone. */
  it('drops a pending press when the row goes away', () => {
    const { unmount } = render(<Rows rows={['a']} />)

    fireEvent.pointerDown(row('a'))
    unmount()
    vi.advanceTimersByTime(LONG_PRESS_MS * 2)

    expect(held).toEqual([])
  })
})
