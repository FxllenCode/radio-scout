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
