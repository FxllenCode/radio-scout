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
 *  is enough on its own: telling tower 3 from tower 7 is the whole of what
 *  makes simulcast coverage legible. A real name is better, and mining #48
 *  SDRTrunk's ID3 is where one comes from — so the name wins wherever there is
 *  one, and on a mined Site the Ref behind it is this instance's own numbering
 *  rather than anything the radio network assigned. */
export function siteName(call: Call): string | undefined {
  if (call.siteLabel !== undefined) return call.siteLabel
  return call.siteRef === undefined ? undefined : `Site ${call.siteRef}`
}

/** Hertz as the megahertz a scanner display shows. The six decimals are what
 *  tell adjacent channels apart. */
export function formatFrequency(hertz: number | undefined): string {
  if (hertz === undefined) return '—'
  return (hertz / 1_000_000).toFixed(6)
}

/** What a Call's radio reads as on screen (#47, spec US 42): the name somebody
 *  gave it, else the bare Ref — and `undefined` when no radio was heard at all.
 *
 *  A bare Ref rather than a placeholder, because a number is still an identity:
 *  the same 1610092 on three Calls says they are the same radio, which is the
 *  whole thing rdio-scanner throws away by never showing units at all. */
export function unitName(call: Call): string | undefined {
  if (call.unitRef === undefined) return undefined
  return call.unitLabel ?? String(call.unitRef)
}

/** Where that radio's history lives (#47, spec US 44). A Ref is unique only
 *  within its System, so both are in the path — exactly as the API takes them. */
export function unitHref(call: Call): string | undefined {
  if (call.unitRef === undefined) return undefined
  return `/unit/${call.systemRef}/${call.unitRef}`
}
