/** The LED palette (docs/design/brief.md): the per-system/per-talkgroup color
 *  is the only *meaningful* color in the UI — it tells you which service is
 *  talking. Values reference the CSS tokens defined in index.css so they stay
 *  in one place and can be themed. */

export const LED_COLORS = {
  blue: 'var(--color-led-blue)',
  cyan: 'var(--color-led-cyan)',
  green: 'var(--color-led-green)',
  magenta: 'var(--color-led-magenta)',
  orange: 'var(--color-led-orange)',
  red: 'var(--color-led-red)',
  white: 'var(--color-led-white)',
  yellow: 'var(--color-led-yellow)',
} as const

export type LedColor = keyof typeof LED_COLORS

/** The same palette as literal hex, for the places a CSS variable can't reach —
 *  today the lock-screen artwork PNG (#14), which is encoded, not styled.
 *  `led.test.ts` pins these to the `--color-led-*` tokens in `index.css`. */
export const LED_HEX: Record<LedColor, string> = {
  blue: '#5b9bff',
  cyan: '#3dd6e8',
  green: '#3ddc84',
  magenta: '#e05cff',
  orange: '#ffb020',
  red: '#ff5c5c',
  white: '#fafafa',
  yellow: '#eac54f',
}

/** Stable ordering, matching the palette carried over from rdio-scanner. */
export const LED_ORDER: readonly LedColor[] = [
  'blue',
  'cyan',
  'green',
  'magenta',
  'orange',
  'red',
  'white',
  'yellow',
]

/** The CSS value for a LED color, for use in inline `style` (dynamic per call). */
export function ledVar(color: LedColor): string {
  return LED_COLORS[color]
}

/** The color a talkgroup gets when nobody has curated one. Deterministic, so a
 *  talkgroup keeps the same color across sessions and devices, and an archive
 *  that has never been curated still reads at a glance. */
export function ledForTalkgroup(systemRef: number, talkgroupRef: number): LedColor {
  const index = Math.abs(systemRef * 31 + talkgroupRef) % LED_ORDER.length
  return LED_ORDER[index]
}

/** The color to paint a Call's LED: the operator's curated choice (set by
 *  Talkgroup CSV import, #18) when there is one, else the deterministic
 *  fallback above.
 *
 *  The server validates `led` against this same palette on import, so an
 *  off-palette value here means a hand-edited or pre-#18 database row — fall
 *  back rather than emit `var(--color-led-purple)`, which resolves to nothing
 *  and renders an invisible LED. */
export function ledForCall(call: {
  systemRef: number
  talkgroupRef: number
  led?: string
}): LedColor {
  const curated = call.led?.trim().toLowerCase()
  if (curated && curated in LED_COLORS) {
    return curated as LedColor
  }
  return ledForTalkgroup(call.systemRef, call.talkgroupRef)
}
