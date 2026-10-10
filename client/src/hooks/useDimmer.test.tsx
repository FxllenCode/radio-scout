import { act, fireEvent, render, screen } from '@testing-library/react'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

import { useDimmer } from './useDimmer'

/** A display that is lit while `lit`, and dims after `secs` of not being. */
function Display({ lit, secs }: { lit: boolean; secs: number }) {
  const { dimmed, wake } = useDimmer(lit, secs)
  return (
    <button type="button" data-dimmed={dimmed} onPointerDown={wake}>
      display
    </button>
  )
}

const display = () => screen.getByRole('button', { name: 'display' })
const passes = (ms: number) => act(() => void vi.advanceTimersByTime(ms))

beforeEach(() => {
  vi.useFakeTimers()
})

afterEach(() => {
  vi.useRealTimers()
})

/** The Operator's display dimmer (#77, spec US 57) — rdio's `dimmerDelay`. */
describe('the display dimmer', () => {
  it('dims after the quiet it was given, and not a moment before', () => {
    render(<Display lit={false} secs={5} />)

    passes(4_999)
    expect(display()).toHaveAttribute('data-dimmed', 'false')
    passes(1)
    expect(display()).toHaveAttribute('data-dimmed', 'true')
  })

  it('lights for a Call on the air, and counts the quiet again from its end', () => {
    const { rerender } = render(<Display lit={false} secs={5} />)
    passes(5_000)

    rerender(<Display lit secs={5} />)
    expect(display()).toHaveAttribute('data-dimmed', 'false')
    passes(60_000)
    expect(display()).toHaveAttribute('data-dimmed', 'false')

    rerender(<Display lit={false} secs={5} />)
    passes(4_000)
    expect(display()).toHaveAttribute('data-dimmed', 'false')
    passes(1_000)
    expect(display()).toHaveAttribute('data-dimmed', 'true')
  })

  /** A touch is a Listener looking at it — rdio wakes its display on any
   *  interaction too. */
  it('wakes when touched, and dims again after the same quiet', () => {
    render(<Display lit={false} secs={5} />)
    passes(5_000)

    fireEvent.pointerDown(display())
    expect(display()).toHaveAttribute('data-dimmed', 'false')
    passes(5_000)
    expect(display()).toHaveAttribute('data-dimmed', 'true')
  })

  /** `0` is the Operator saying never — which is what ships. */
  it('never dims at zero', () => {
    render(<Display lit={false} secs={0} />)

    passes(3_600_000)
    expect(display()).toHaveAttribute('data-dimmed', 'false')
  })
})
