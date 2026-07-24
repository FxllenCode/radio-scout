import { render } from '@testing-library/react'
import { describe, expect, it } from 'vitest'

import { StatusLed } from './StatusLed'

describe('StatusLed', () => {
  it('is decorative, sized, and animates when pulsing', () => {
    const { container } = render(<StatusLed color="green" size={8} pulse />)
    const dot = container.querySelector('span')
    expect(dot).not.toBeNull()
    expect(dot).toHaveAttribute('aria-hidden')
    expect(dot!.className).toContain('animate-pulse')
    expect(dot!.style.width).toBe('8px')
    expect(dot!.style.height).toBe('8px')
  })

  it('omits the pulse animation and defaults its size when steady', () => {
    const { container } = render(<StatusLed color="red" />)
    const dot = container.querySelector('span')!
    expect(dot.className).not.toContain('animate-pulse')
    expect(dot.style.width).toBe('12px') // default size
  })
})
