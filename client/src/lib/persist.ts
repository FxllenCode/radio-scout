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
import type { PanelMemory, PanelSort } from './panel'
import type { Avoids, Hold, Selection } from './selection'

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

/** Where a namespace's **Avoid** deadlines are stored (#91).
 *
 *  Its own key for the reason [`feedOffKey`] has one, and for a second: an
 *  Avoid list is the shortest-lived of the three, and a value an older build
 *  cannot read must cost the Listener that list alone — never their
 *  Selection. */
export function avoidsKey(namespace: string): string {
  return `${KEY_PREFIX}:${namespace}:avoids`
}

/** Where a namespace's **Hold** is stored (#91). */
export function holdKey(namespace: string): string {
  return `${KEY_PREFIX}:${namespace}:hold`
}

/** Where a namespace's **Priority** Talkgroups are stored (#58, spec US 27).
 *
 *  Its own key, for [`avoidsKey`]'s reason: a list an older build cannot read
 *  must cost the Listener that list alone and never their Selection. */
export function priorityKey(namespace: string): string {
  return `${KEY_PREFIX}:${namespace}:priority`
}

/**
 * The Talkgroups this browser last had marked **Priority**, or `undefined` if
 * there isn't a usable list.
 *
 * Checked to the leaves like every other stored value here, and the leaves are
 * the whole of it: a key that is not a `systemRef:talkgroupRef` pair could
 * never match a Call, so it would sit in storage forever ordering nothing —
 * and unlike a bad **Pin**, which merely fails to draw a row, this one decides
 * what plays next.
 */
export function loadPriority(
  storage: Storage,
  namespace: string,
): string[] | undefined {
  const stored = parseStored(storage, priorityKey(namespace))
  return isKeyList(stored) ? stored : undefined
}

/** Remember the Priority Talkgroups for `namespace`. */
export function savePriority(
  storage: Storage,
  namespace: string,
  priority: readonly string[],
): void {
  writeStored(storage, priorityKey(namespace), JSON.stringify(priority))
}

/** Where a namespace's panel arrangement is stored (#57) — its **Pin**s, the
 *  Systems it has folded away, and its sort.
 *
 *  One key for the three, unlike the Selection and the Avoids, because they are
 *  one screen's memory of itself with one lifetime; and read field by field
 *  below, so a value we cannot make sense of costs only the field it is in. */
export function panelKey(namespace: string): string {
  return `${KEY_PREFIX}:${namespace}:panel`
}

/**
 * How this browser last had the Talkgroups panel arranged — as much of it as
 * can be believed.
 *
 * Partial rather than whole, and guarded per field: a stored sort naming an
 * order this build no longer has must not also cost the Listener the six Pins
 * they picked out of four hundred Talkgroups. What is missing falls back to the
 * default, which is what a Listener who has never arranged anything gets.
 */
export function loadPanel(storage: Storage, namespace: string): Partial<PanelMemory> {
  const stored = parseStored(storage, panelKey(namespace))
  if (!isRecord(stored)) return {}

  return {
    ...(isSort(stored.sort) ? { sort: stored.sort } : {}),
    ...(isKeyList(stored.pinned) ? { pinned: stored.pinned } : {}),
    ...(isExpanded(stored.expanded) ? { expanded: stored.expanded } : {}),
  }
}

/** Remember how the panel is arranged for `namespace`. */
export function savePanel(
  storage: Storage,
  namespace: string,
  panel: PanelMemory,
): void {
  writeStored(storage, panelKey(namespace), JSON.stringify(panel))
}

const SORTS: readonly PanelSort[] = ['name', 'active']

const isSort = (value: unknown): value is PanelSort =>
  SORTS.includes(value as PanelSort)

/** Every **Pin** names a Talkgroup, or the panel would hold up a row it cannot
 *  draw — [`isAvoids`]'s lesson, one key at a time. */
const isKeyList = (value: unknown): value is string[] =>
  Array.isArray(value) &&
  value.every((key) => typeof key === 'string' && TALKGROUP_KEY.test(key))

/** A System is named by its Ref, so a key that is not a number could never
 *  match one and would sit in storage forever saying nothing. */
const isExpanded = (value: unknown): value is Record<number, boolean> =>
  isRecord(value) &&
  Object.entries(value).every(
    ([systemRef, shown]) => /^\d+$/.test(systemRef) && typeof shown === 'boolean',
  )

const isRecord = (value: unknown): value is Record<string, unknown> =>
  typeof value === 'object' && value !== null && !Array.isArray(value)

/**
 * The **Avoids** this browser remembered, with everything already lapsed at
 * `now` dropped.
 *
 * The whole reason an Avoid is stored as a *deadline* (#91): the moment
 * survives persistence where a running timer cannot, so "twenty minutes left"
 * is a subtraction on the way back in, and an hour that ran out while the tab
 * was closed is simply not in force. Nothing had to be running to notice.
 */
export function loadAvoids(
  storage: Storage,
  namespace: string,
  now: number,
): Avoids | undefined {
  const stored = parseStored(storage, avoidsKey(namespace))
  if (!isAvoids(stored)) return undefined

  return Object.fromEntries(
    Object.entries(stored).filter(([, until]) => until === 0 || until > now),
  )
}

/** Remember these Avoids for `namespace`. */
export function saveAvoids(
  storage: Storage,
  namespace: string,
  avoided: Avoids,
): void {
  writeStored(storage, avoidsKey(namespace), JSON.stringify(avoided))
}

/** The **Hold** this browser was left holding, or `undefined` if there isn't a
 *  usable one. `null` is a Hold that was deliberately released, which is a
 *  different thing from never having said. */
export function loadHold(storage: Storage, namespace: string): Hold | null | undefined {
  const stored = parseStored(storage, holdKey(namespace))
  if (stored === null) return null
  return isHold(stored) ? stored : undefined
}

/** Remember this Hold for `namespace`. */
export function saveHold(
  storage: Storage,
  namespace: string,
  hold: Hold | null,
): void {
  writeStored(storage, holdKey(namespace), JSON.stringify(hold))
}

/** A stored key read back as whatever JSON it holds — `undefined` for absent,
 *  and for the half-written value `JSON.parse` refuses. */
function parseStored(storage: Storage, key: string): unknown {
  const stored = readStored(storage, key)
  if (stored === undefined) return undefined
  try {
    return JSON.parse(stored)
  } catch {
    return undefined
  }
}

/** What an [`talkgroupKey`] looks like: two Refs and the colon between them. The
 *  same key an **Avoid** and a **Pin** both name a Talkgroup by. */
const TALKGROUP_KEY = /^\d+:\d+$/

/**
 * Is this a deadline map rather than whatever else was under our key?
 *
 * Checked to the leaves, like [`isSelection`], and **the keys are half of the
 * leaves**: a deadline that is not a number would be compared to the clock and
 * silence a Talkgroup forever, and a key that is not a `systemRef:talkgroupRef`
 * pair parses to `NaN` on both halves and reaches the wire as
 * `sel: { NaN: { NaN: false } }` — on every `sub` frame, permanently if its
 * deadline is `0`, and invisible in the panel because no row can be keyed by it.
 */
function isAvoids(value: unknown): value is Avoids {
  if (!isRecord(value)) return false
  return Object.entries(value).every(
    ([key, until]) => TALKGROUP_KEY.test(key) && typeof until === 'number',
  )
}

function isHold(value: unknown): value is Hold {
  if (typeof value !== 'object' || value === null || Array.isArray(value)) return false
  const { systemRef, talkgroupRef } = value as Record<string, unknown>
  return (
    typeof systemRef === 'number' &&
    (talkgroupRef === null || typeof talkgroupRef === 'number')
  )
}

/** The remembered selection, or `undefined` if there isn't a usable one. */
export function loadSelection(
  storage: Storage,
  namespace: string,
): Selection | undefined {
  const stored = parseStored(storage, selectionKey(namespace))
  return isSelection(stored) ? stored : undefined
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
