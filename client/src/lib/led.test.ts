import { describe, expect, it } from 'vitest'

import { LED_ORDER, ledForTalkgroup, ledVar } from './led'

describe('led palette', () => {
  it('assigns a deterministic, in-palette color per talkgroup', () => {
    const color = ledForTalkgroup(11, 54241)
    expect(ledForTalkgroup(11, 54241)).toBe(color) // stable for the same talkgroup
    expect(LED_ORDER).toContain(color)
  })

  it('maps each color to its CSS token', () => {
    for (const color of LED_ORDER) {
      expect(ledVar(color)).toBe(`var(--color-led-${color})`)
    }
  })

  it('spreads talkgroups across more than one color', () => {
    const seen = new Set(
      Array.from({ length: 40 }, (_, i) => ledForTalkgroup(1, i)),
    )
    expect(seen.size).toBeGreaterThan(1)
  })
})
