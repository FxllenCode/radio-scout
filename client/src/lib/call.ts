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
  return [call.talkgroupTag, call.talkgroupGroup, siteName(call)]
    .filter(Boolean)
    .join(' · ')
}

/** The tower a Call was heard on, for multi-site systems (#42, spec US 11).
 *
 *  `undefined` when no recorder named one — which is every Call on a
 *  single-site system, and every Call from a recorder that doesn't send a
 *  `site`. A placeholder on those rows would be clutter bought for nothing, so
 *  the line simply doesn't carry the field.
 *
 *  A bare number is all a recorder sends (rdio's `site` is an integer), and it
 *  is enough: telling tower 3 from tower 7 is the whole of what makes simulcast
 *  coverage legible. #48 backfills real site names from SDRTrunk's ID3 tags. */
export function siteName(call: Call): string | undefined {
  return call.siteRef === undefined ? undefined : `Site ${call.siteRef}`
}

/** Hertz as the megahertz a scanner display shows. The six decimals are what
 *  tell adjacent channels apart. */
export function formatFrequency(hertz: number | undefined): string {
  if (hertz === undefined) return '—'
  return (hertz / 1_000_000).toFixed(6)
}
