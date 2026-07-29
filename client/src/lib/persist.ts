/**
 * Remembering the listener's **Selection** across reloads (#12, spec US 22),
 * optionally namespaced so one browser can run more than one independent
 * scanner.
 *
 * # Improving on rdio-scanner
 *
 * rdio keeps the same idea (`rdio-scanner-lfm-<id>`, namespaced by `?id=`) and
 * we keep its URL convention so a bookmarked second scanner still works. What
 * changes:
 *
 * - **A stored value we can't read is ignored, never destructive.** rdio calls
 *   `localStorage.clear()` on a version mismatch — wiping every key on the
 *   origin, including ones that aren't its own. Here a bad value costs the
 *   listener their selection and nothing else.
 * - **What is stored is validated**, not merely `JSON.parse`d, so a hand-edited
 *   or half-written value can't put a malformed matrix on the wire.
 * - **Storage being denied is a supported state.** Safari in private mode
 *   throws on `getItem`/`setItem`; the scanner still runs, just unremembered.
 */
import type { Selection } from './selection'

/** Prefix for every key this app owns in local storage. */
const KEY_PREFIX = 'radio-scout:selection'

/** Read a key, treating a browser that denies storage — or has none at all —
 *  as one holding nothing. The guarded pair every module here reaches for; see
 *  the module note on storage being a supported *absence*, not an error. */
export function readStored(
  storage: Storage | undefined,
  key: string,
): string | undefined {
  try {
    return storage?.getItem(key) ?? undefined
  } catch {
    return undefined
  }
}

/** Write a key, ignoring a browser that refuses to remember it. */
export function writeStored(
  storage: Storage | undefined,
  key: string,
  value: string,
): void {
  try {
    storage?.setItem(key, value)
  } catch {
    // Storage denied (private mode, blocked site data). Nothing here is worth
    // interrupting a listener over.
  }
}

/** The default scanner, when the URL names none. */
const DEFAULT_NAMESPACE = 'default'

/** Which scanner this tab is: `?id=<name>`, as rdio-scanner names it. Two tabs
 *  at different `?id=` are two independent scanners in one browser. */
export function namespaceOf(search: string = location.search): string {
  return new URLSearchParams(search).get('id') || DEFAULT_NAMESPACE
}

/** Where a namespace's selection is stored. */
export function selectionKey(namespace: string): string {
  return `${KEY_PREFIX}:${namespace}`
}

/** Where a namespace's feed-off choice is stored (#80).
 *
 *  Its own key rather than a field inside the stored Selection: they are
 *  different things with different lifetimes, and folding the switch into the
 *  matrix would mean an older build reading a shape it validates against and
 *  rejecting the listener's Selection along with it. */
export function feedOffKey(namespace: string): string {
  return `${KEY_PREFIX}:${namespace}:feed-off`
}

/** Whether this browser last left the live feed off, or `undefined` if it has
 *  never said — which is not the same as `false`, and is why the caller decides
 *  the default rather than this. */
export function loadFeedOff(
  storage: Storage,
  namespace: string,
): boolean | undefined {
  const stored = readStored(storage, feedOffKey(namespace))
  if (stored === 'true') return true
  if (stored === 'false') return false
  // Absent, or something we didn't write. Either way we know nothing.
  return undefined
}

/** Remember the feed-off choice for `namespace`. */
export function saveFeedOff(
  storage: Storage,
  namespace: string,
  feedOff: boolean,
): void {
  writeStored(storage, feedOffKey(namespace), String(feedOff))
}

/** The remembered selection, or `undefined` if there isn't a usable one. */
export function loadSelection(
  storage: Storage,
  namespace: string,
): Selection | undefined {
  const stored = readStored(storage, selectionKey(namespace))
  if (stored === undefined) return undefined

  try {
    const parsed: unknown = JSON.parse(stored)
    return isSelection(parsed) ? parsed : undefined
  } catch {
    return undefined
  }
}

/** Remember this selection for `namespace`. */
export function saveSelection(
  storage: Storage,
  namespace: string,
  selection: Selection,
): void {
  writeStored(storage, selectionKey(namespace), JSON.stringify(selection))
}

/** Is this the matrix shape (ADR-0004) rather than whatever else was under our
 *  key? Checked to the leaves: an entry that isn't a boolean would be sent to
 *  the server and resolved as truthy. */
function isSelection(value: unknown): value is Selection {
  if (typeof value !== 'object' || value === null) return false
  const { all, sel } = value as Record<string, unknown>
  if (typeof all !== 'boolean') return false
  if (typeof sel !== 'object' || sel === null || Array.isArray(sel)) return false

  return Object.values(sel).every(
    (talkgroups) =>
      typeof talkgroups === 'object' &&
      talkgroups !== null &&
      !Array.isArray(talkgroups) &&
      Object.values(talkgroups).every((on) => typeof on === 'boolean'),
  )
}
