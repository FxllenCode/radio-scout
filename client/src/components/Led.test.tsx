import { render } from '@testing-library/react'
import { describe, expect, it } from 'vitest'

import { ChannelLed, StatusLamp } from './Led'

describe('ChannelLed', () => {
  it('is decorative, sized, and animates when pulsing', () => {
    const { container } = render(<ChannelLed color="green" size={8} pulse />)
    const dot = container.querySelector('span')
    expect(dot).not.toBeNull()
    expect(dot).toHaveAttribute('aria-hidden')
    expect(dot!.className).toContain('animate-pulse')
    expect(dot!.style.width).toBe('8px')
    expect(dot!.style.height).toBe('8px')
  })

  it('omits the pulse animation and defaults its size when steady', () => {
    const { container } = render(<ChannelLed color="red" />)
    const dot = container.querySelector('span')!
    expect(dot.className).not.toContain('animate-pulse')
    expect(dot.style.width).toBe('12px') // default size
  })
})

/** A status is never mistaken for a channel (#78): same light, other shape. */
describe('StatusLamp', () => {
  it('is square where a channel LED is round', () => {
    const lamp = render(<StatusLamp color="red" pulse />).container.querySelector('span')!
    const led = render(<ChannelLed color="red" />).container.querySelector('span')!

    expect(lamp).toHaveAttribute('data-shape', 'square')
    expect(lamp.className).not.toContain('rounded-full')
    expect(lamp).toHaveAttribute('aria-hidden')
    expect(lamp.className).toContain('animate-pulse')
    expect(lamp.style.width).toBe('8px') // default size
    expect(led).toHaveAttribute('data-shape', 'round')
    expect(led.className).toContain('rounded-full')
  })
})
