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

/** Placeholder assignment until real per-system/talkgroup LED colors arrive via
 *  CSV import (#18) / curation. Deterministic so a talkgroup keeps its color. */
export function ledForTalkgroup(systemRef: number, talkgroupRef: number): LedColor {
  const index = Math.abs(systemRef * 31 + talkgroupRef) % LED_ORDER.length
  return LED_ORDER[index]
}
