/** How a Call reads on screen. Shared by the scanner display (#11) and the
 *  archive list (#13) so a Talkgroup is named the same way everywhere. */
import type { Call } from '@/types'

/** Recorders may send no labels at all, so every Call still has to be nameable
 *  from its Refs (CONTEXT.md: a **Ref** is what recorders send). */
export function talkgroupName(call: Call): string {
  return call.talkgroupLabel ?? `Talkgroup ${call.talkgroupRef}`
}

export function systemName(call: Call): string {
  return call.systemLabel ?? `System ${call.systemRef}`
}

/** The Tag · Group line — either, both, or nothing. */
export function callCategory(call: Call): string {
  return [call.talkgroupTag, call.talkgroupGroup].filter(Boolean).join(' · ')
}

/** Hertz as the megahertz a scanner display shows. The six decimals are what
 *  tell adjacent channels apart. */
export function formatFrequency(hertz: number | undefined): string {
  if (hertz === undefined) return '—'
  return (hertz / 1_000_000).toFixed(6)
}
