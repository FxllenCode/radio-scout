import { readFileSync } from 'node:fs'
import { resolve } from 'node:path'
import { describe, expect, it } from 'vitest'

import type { Call } from '@/types'

import { LED_HEX, LED_ORDER, ledForCall, ledForTalkgroup, ledVar } from './led'

const call = (over: Partial<Call> = {}): Call => ({
  id: 1,
  systemRef: 11,
  talkgroupRef: 54241,
  audioUrl: '/api/call/1/audio',
  ...over,
})

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

/** The operator's curated color (set by CSV import, #18) is the whole point of
 *  US 37's `led` column — but an uncurated archive still has to read at a
 *  glance, so the deterministic color remains the floor. */
describe('ledForCall', () => {
  it('prefers the curated color the operator imported', () => {
    // Deliberately a color the fallback would not have picked, so this can't
    // pass by coincidence.
    const fallback = ledForTalkgroup(11, 54241)
    const curated = LED_ORDER.find((c) => c !== fallback)!

    expect(ledForCall(call({ led: curated }))).toBe(curated)
  })

  it('falls back to the deterministic color when nothing is curated', () => {
    expect(ledForCall(call())).toBe(ledForTalkgroup(11, 54241))
  })

  it('accepts every palette color', () => {
    for (const color of LED_ORDER) {
      expect(ledForCall(call({ led: color }))).toBe(color)
    }
  })

  it.each(['purple', '#ff0000', '', '   ', 'led-red'])(
    'falls back rather than trusting an off-palette %o',
    (led) => {
      // The server validates on import, so this is a hand-edited or
      // pre-validation database row — it must not paint an undefined color.
      expect(ledForCall(call({ led }))).toBe(ledForTalkgroup(11, 54241))
    },
  )

  it.each(['RED', 'Red', '  red  '])(
    'is forgiving about the casing and spacing of a stored %o',
    (led) => {
      expect(ledForCall(call({ led }))).toBe('red')
    },
  )

  it('colors two talkgroups differently when curation says so', () => {
    const a = call({ talkgroupRef: 1, led: 'red' })
    const b = call({ talkgroupRef: 2, led: 'blue' })
    expect(ledForCall(a)).toBe('red')
    expect(ledForCall(b)).toBe('blue')
  })
})
