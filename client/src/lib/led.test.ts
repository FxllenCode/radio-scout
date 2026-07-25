import { readFileSync } from 'node:fs'
import { resolve } from 'node:path'
import { describe, expect, it } from 'vitest'

import { LED_HEX, LED_ORDER, ledForTalkgroup, ledVar } from './led'

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

  /** Lock-screen artwork is a PNG, so it needs the literal hex a CSS variable
   *  can't give it (#14). Two homes for one palette is a drift risk, so this
   *  pins the TS copy to the stylesheet the UI actually renders. */
  it('keeps its hex values in step with the CSS tokens', () => {
    // Vitest runs from the client root, where the stylesheet lives under src/.
    const css = readFileSync(resolve('src/index.css'), 'utf8')

    for (const color of LED_ORDER) {
      expect(css).toContain(`--color-led-${color}: ${LED_HEX[color]};`)
    }
    expect(Object.keys(LED_HEX).sort()).toEqual([...LED_ORDER].sort())
  })

  it('spreads talkgroups across more than one color', () => {
    const seen = new Set(
      Array.from({ length: 40 }, (_, i) => ledForTalkgroup(1, i)),
    )
    expect(seen.size).toBeGreaterThan(1)
  })
})
