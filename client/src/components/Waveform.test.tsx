import { render } from '@testing-library/react'
import { describe, expect, it } from 'vitest'

import { WAVEFORM_BARS } from '@/lib/waveform'

import { Waveform } from './Waveform'

/** The bars, in order. They are decorative (`aria-hidden`), so this reaches for
 *  them the way the browser paints them rather than by role. */
function bars(container: HTMLElement): HTMLElement[] {
  return Array.from(container.querySelectorAll('span'))
}

const lit = (bar: HTMLElement) => bar.dataset.lit === 'true'

describe('Waveform', () => {
  it('draws one bar per sample', () => {
    const { container } = render(
      <Waveform seed={1} color="green" progress={0} />,
    )

    expect(bars(container)).toHaveLength(WAVEFORM_BARS)
  })

  it('lights the bars the Call has played through', () => {
    const { container } = render(
      <Waveform seed={1} color="green" progress={0.5} />,
    )

    const drawn = bars(container)
    expect(drawn.filter(lit)).toHaveLength(WAVEFORM_BARS / 2)
    expect(lit(drawn[0])).toBe(true)
    expect(lit(drawn.at(-1)!)).toBe(false)
  })

  it('leaves every bar unlit before the Call starts', () => {
    const { container } = render(
      <Waveform seed={1} color="green" progress={0} />,
    )

    expect(bars(container).filter(lit)).toHaveLength(0)
  })

  /** Paused stills the display rather than blanking it — the Call is still
   *  there, it just isn't moving (spec US 15). */
  it('dims what it has played while paused', () => {
    const playing = render(<Waveform seed={1} color="green" progress={0.5} />)
    const paused = render(
      <Waveform seed={1} color="green" progress={0.5} live={false} />,
    )

    const opacity = (result: ReturnType<typeof render>) =>
      Number(bars(result.container)[0].style.opacity)
    expect(opacity(paused)).toBeLessThan(opacity(playing))
  })
})
