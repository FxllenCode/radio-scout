/** Reading the two comma-separated fields the curation forms use (#49).
 *
 *  Both are here rather than in the screens that render them because both are
 *  the client half of a rule the *server* also enforces, and the two agreeing is
 *  what makes the field safe to type into. They are also the only logic on those
 *  screens worth testing without a DOM. */

/** A comma-separated list as the set of names it means.
 *
 *  An empty box is the empty set, which is how a channel loses its last Group:
 *  `groups` **replaces**, the CSV importer's rule (#18), because a set-valued
 *  field with no way to spell "none" could add a Group and never take one away.
 *  A stray comma is not a Group called `""` — the server drops those too, and
 *  dropping them here as well means the count shown before saving is the count
 *  that gets saved. */
export function splitList(raw: string): string[] {
  return raw
    .split(',')
    .map((entry) => entry.trim())
    .filter((entry) => entry !== '')
}

/** A comma-separated list of Refs, or `undefined` if any entry is not one.
 *
 *  **The one field where dropping a typo would be silent.** rdio-scanner stores
 *  a System's blacklist as free text and ignores whatever will not parse, so a
 *  mistyped ref refuses nothing and says nothing — an Operator believes a
 *  channel is blacklisted and it is being ingested. Refusing here puts the
 *  message under the input, before the request is even made. */
export function parseRefs(raw: string): number[] | undefined {
  const refs = splitList(raw).map(Number)
  return refs.some((ref) => !Number.isInteger(ref)) ? undefined : refs
}

/**
 * A nullable boolean as a three-state `<select>` value (#20, #68).
 *
 * `''` is **inherit** — follow whatever is above this row — and it is the state
 * a plain checkbox cannot spell, which is why both settings that have it are
 * selects. Two forms use it now (a System's enhancement, a Talkgroup's access),
 * so it is written once rather than twice with the same three lines.
 */
export function inherit(value: boolean | null | undefined): string {
  if (value === true) return 'on'
  if (value === false) return 'off'
  return ''
}

/** Which of the three things a retention window is currently saying (#69). */
export type KeepMode = 'inherit' | 'days' | 'forever'

/** The largest window the server will take — `[retention] days` and both
 *  overrides are `u32`, so a bigger number is a `422` with nothing an Operator
 *  can read. Refusing it here is `is_postable_url`'s rule (#54): a form must not
 *  accept what the server refuses, or the value goes up and comes straight back
 *  after the box has cleared. */
const MAX_DAYS = 0xffffffff

/** What a stored window means, as the mode a form shows it in.
 *
 *  Three options rather than one number box, because `0` means **forever** on
 *  the wire and a number box makes that a trap: an Operator typing `0` into
 *  "days to keep" means *delete these*, which is the exact opposite. The magic
 *  value stays in the protocol, where `[retention] days` already spends it, and
 *  the screen says what it means. */
export function keepMode(days: number | null | undefined): KeepMode {
  if (days === null || days === undefined) return 'inherit'
  return days === 0 ? 'forever' : 'days'
}

/** What a form sends for a retention window, or `undefined` when the box does
 *  not hold one.
 *
 *  `undefined` is *refuse to submit*, [`parseRefs`]' rule — not "leave it
 *  alone", which is a thing only an absent key can say and a thing this control
 *  never means: it is rendered from the row it is editing, so it always has an
 *  answer. A blank box under **days** has not given one, and `0` there is not
 *  one either: that is what the *forever* option is for, and reading it as one
 *  would make two options mean the same thing while looking like they do not. */
export function keepWindow(
  mode: KeepMode,
  raw: string,
): number | null | undefined {
  if (mode === 'inherit') return null
  if (mode === 'forever') return 0
  const days = Number(raw.trim())
  const given = raw.trim() !== '' && Number.isInteger(days)
  return given && days > 0 && days <= MAX_DAYS ? days : undefined
}
