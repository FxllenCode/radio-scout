/**
 * The Talkgroups panel, derived once (#91, spec US 19–22).
 *
 * One function takes the catalog, the audible **Selection**, the **Avoids** and
 * the filter text, and returns the panel *as drawn*: the summary counts, the
 * Group and Tag category rows, and the System sections with every row already
 * carrying its key, its selected state, its Avoid deadline and the action a tap
 * means. The screen then renders it and dispatches what it is handed — it works
 * nothing out.
 *
 * # Why once
 *
 * A county system is 400+ Talkgroups, and #57 asks that they scroll smoothly
 * *while audio plays*. The panel used to derive itself four times per render —
 * a flatten for the filter, a flatten per category kind, a count per System —
 * and each row then read the store again for its own Avoid. Playback progress
 * dispatches several times a second, so every one of those ran several times a
 * second precisely because a Call was playing. Memoized on stable inputs (the
 * selectors that feed it hold their identity now), this runs when the catalog,
 * the Selection, the Avoids or the filter actually change, and not otherwise.
 *
 * # Why the tap is a value
 *
 * "The control acts on what it is next to" was a rule written inside an
 * `onClick`, so the only way to assert it was to type into a search box and
 * click — which is why its *unfiltered* half had never been asserted at all.
 * As a [`Choice`] it is a table test.
 *
 * # Why there is no clock here
 *
 * An **Avoid** is a deadline, and #91 puts the comparison "wherever audibility
 * is asked". This is deliberately not one of those places: the *store* is the
 * authority on which Avoids are in force, and it keeps that true three ways —
 * its own clock fires at the earliest deadline (`store/avoids`), every arriving
 * Call prunes against the moment it arrived, and a reload drops what lapsed
 * while the tab was closed (`lib/persist`). So the map handed here is already
 * current, and the Selection handed with it was silenced by that same map,
 * which is what makes a row unable to badge a deadline while still reading on.
 *
 * The one window that leaves is a browser throttling a *hidden* tab's timers —
 * and showing this panel means showing the tab, which un-throttles them. Taking
 * a `now` here would close a gap about a frame wide and cost the panel its
 * single source of truth.
 *
 * The same rule is what keeps the **most-active sort** ordering by a *count*,
 * tie-broken by a stored *instant*, and never by an **age** (#57): a count and
 * an instant are facts the server measured, where an age is a subtraction from
 * a clock — and a list that re-sorted itself under the Listener's thumb every
 * thirty seconds is not a list. [`Panel.ticking`] is the other half: it says
 * whether anything *drawn* is such a subtraction, so a panel of switches and
 * counts keeps no clock at all.
 */
import { ledForCall, type LedColor } from './led'
import {
  talkgroupKey,
  categoryViews,
  countOn,
  isSelected,
  stateOf,
  summarize,
  talkgroupsOf,
  type Avoids,
  type CatalogEntry,
  type CategoryView,
  type Selection,
  type TalkgroupKey,
} from './selection'
import type { Catalog } from '@/types'

/**
 * How many rows a System may hold before it starts folded away (#57).
 *
 * Per System rather than per catalog: collapsing a five-row System because the
 * county beside it has four hundred costs a tap and saves nothing. Above this,
 * the section header — its counts and its All on/off — is what an unscrolled
 * Listener sees, which is the whole of what "reachable without scrolling"
 * means at county scale.
 */
export const COLLAPSE_ABOVE = 50

/**
 * How the rows inside a section are ordered.
 *
 * `name` is the catalog's own order, which the server has already sorted by
 * what a Listener reads (label, then Ref). `active` is the busiest first — the
 * answer to "what is happening right now" that a 400-row alphabetical list
 * cannot give.
 */
export type PanelSort = 'name' | 'active'

/**
 * How the panel is *arranged*, and the whole of what is remembered about it
 * (`lib/persist`, `store/panel`).
 *
 * It lives here, beside the function that reads it, for the reason #91 moved
 * **Hold** and **Avoids** down into `lib/selection`: `lib/` depends on nothing
 * in `store/`, and the persistence layer needs to name this shape.
 */
export interface PanelMemory {
  sort: PanelSort
  /** The **Pin**ned Talkgroups, by row key. A key the catalog has nothing for
   *  simply draws nothing — a Talkgroup an Operator deleted must not become an
   *  unnameable row. */
  pinned: string[]
  /** What the Listener has said about each System's section, by System Ref.
   *  Absent means they have not said, and [`COLLAPSE_ABOVE`] decides — so a
   *  System they opened stays open across a reload, which a list of what is
   *  *shut* could not express. */
  expanded: Record<number, boolean>
}

/**
 * What a tap on one of the panel's controls means.
 *
 * Two shapes, because there are two things a control can act on and the
 * difference matters (spec US 21): a **wildcard** over a whole System, which
 * covers the Talkgroups this browser has never heard of, or exactly these
 * **keys** and nothing else. They are deliberately the two live-slice action
 * payloads, so dispatching one is a choice between two action creators and
 * never a rebuild.
 */
export type Choice =
  | { systemRef: number; on: boolean }
  | { keys: TalkgroupKey[]; on: boolean }

/** One Talkgroup row, as drawn. */
export interface PanelRow {
  /** Stable across renders and unique across Systems — two Systems routinely
   *  number a Talkgroup `1`, which a Ref alone would collide on. It is also how
   *  a **Pin** names the row it holds up. */
  key: string
  /** What to call it: its label, its name, or its Ref. */
  label: string
  /** The TGID, drawn at the end of the row. */
  talkgroupRef: number
  /** Which System it belongs to — drawn on a **Pin**ned row, which has been
   *  lifted out of its section and would otherwise be a name with no home. */
  systemLabel: string
  /** The LED colour, resolved here rather than per row per render — the last
   *  derivation the panel did not own (#91), folded in now that the list is
   *  windowed and a row is drawn while a Listener flicks past it. */
  led: LedColor
  /** Will the Listener hear it? The Avoids are already laid into the Selection
   *  this was drawn from, so a silenced Talkgroup reads off here exactly as it
   *  does in the counts above. */
  selected: boolean
  /** Held at the top of the panel (spec US 29). Marked on both copies — the
   *  one in the pinned section and the one still in its System. */
  pinned: boolean
  /** Calls in the catalog's activity window, `0` for a Talkgroup it did not
   *  hear. What the most-active sort orders by. */
  recentCalls: number
  /** The newest of those Calls, absent when there were none — what the
   *  last-heard age on the row is a subtraction from. */
  lastCallAtMs?: number
  /** When the **Avoid** on it lapses — `0` for "until the Listener says
   *  otherwise", absent when it is not avoided. The deadline itself, because
   *  the countdown beside it is a subtraction from the clock. */
  avoidedUntil?: number
  choice: Choice
}

/** One System's section: its rows, and what its All on/off acts on. */
export interface PanelSystem {
  key: number
  label: string
  /** How many of the rows *shown* are on, out of how many are shown. */
  on: number
  total: number
  allOn: boolean
  all: Choice
  /** Folded away: the header, its counts and its All on/off are drawn, and its
   *  rows are not. Its counts are honest either way, which is what makes
   *  collapsing different from hiding. */
  collapsed: boolean
  rows: PanelRow[]
}

/** A category chip: [`CategoryView`] plus what tapping it means. */
export interface PanelCategory extends CategoryView {
  choice: Choice
}

/** The Group rows and the Tag rows, each under the heading they are drawn
 *  with. A kind the catalog has nothing of is absent rather than empty. */
export interface PanelCategories {
  heading: string
  categories: PanelCategory[]
}

/** The panel, as drawn. */
export interface Panel {
  /** `on` of `total` across the whole catalog — never only the filtered view,
   *  which would claim the Listener had deselected what is merely hidden. */
  on: number
  total: number
  categories: PanelCategories[]
  /** The **Pin**ned rows, above every System and across all of them (spec
   *  US 29) — empty when nothing is pinned. */
  pinned: PanelRow[]
  systems: PanelSystem[]
  /** Something *drawn* is measured from the clock — a timed **Avoid**'s
   *  countdown, or a last-heard age — so the screen knows whether to keep one
   *  running. A panel of switches and counts needs no clock at all, and a
   *  parked phone should not redraw twice a minute for nothing. */
  ticking: boolean
  /** The filter matched nothing, so the panel says so instead of drawing a
   *  wall of empty sections. */
  empty: boolean
}

export interface PanelInput extends PanelMemory {
  catalog: Catalog
  /** The **Selection** with the Avoids already laid over it — what the Listener
   *  will actually hear (`selectAudibleSelection`). */
  selection: Selection
  /** The deadlines, for the badge each avoided row carries. */
  avoided: Avoids
  filter: string
}

export function panelOf({
  catalog,
  selection,
  avoided,
  filter,
  pinned,
  expanded,
  sort,
}: PanelInput): Panel {
  const matches = matching(talkgroupsOf(catalog), filter)
  // Whitespace is not a filter: a Listener who typed a space and deleted the
  // word must get the wildcard back, or a System's "All off" would quietly stop
  // covering the Talkgroups they have never heard of.
  const filtered = filter.trim().length > 0
  const held = new Set(pinned)
  const drawRow = (entry: CatalogEntry) => rowOf(entry, selection, avoided, held)

  const systems = catalog.systems.flatMap((system) => {
    const entries = matches.filter((entry) => entry.systemRef === system.ref)
    if (entries.length === 0) return []
    const on = countOn(selection, entries)
    const allOn = stateOf(selection, entries) === 'all'
    // Filtering is asking to see what matched, so a fold gives way to it —
    // otherwise a search would be answered with a shut box.
    const collapsed =
      !filtered && !(expanded[system.ref] ?? entries.length <= COLLAPSE_ABOVE)

    return [
      {
        key: system.ref,
        label: system.label ?? `System ${system.ref}`,
        on,
        total: entries.length,
        allOn,
        collapsed,
        // The control acts on what it is next to. Unfiltered that is the System
        // itself, as one wildcard; filtered it is the rows on screen and
        // nothing else, so "All off" on a System showing one row does not
        // silence the thirty-nine it is hiding.
        all: filtered
          ? { keys: keysOf(entries), on: !allOn }
          : { systemRef: system.ref, on: !allOn },
        // A folded System draws no rows, so it builds none: four hundred of
        // them, each resolving an LED, is exactly the work a county-scale panel
        // is folded to avoid.
        rows: collapsed ? [] : ordered(entries, sort).map(drawRow),
      },
    ]
  })

  // Across every System, because a daily channel in the third one is exactly
  // what spec US 29 is about — floating it to the top of a section that is
  // itself two scrolls down would not be a Pin.
  const pinnedRows = ordered(
    matches.filter((entry) => held.has(talkgroupKey(entry.systemRef, entry.talkgroupRef))),
    sort,
  ).map(drawRow)

  return {
    ...summarize(catalog, selection),
    categories: categoriesOf(catalog, selection),
    pinned: pinnedRows,
    systems,
    ticking: [pinnedRows, ...systems.map((system) => system.rows)]
      .flat()
      .some((row) => ticks(row, sort)),
    empty: matches.length === 0,
  }
}

/** Is this row's own display a subtraction from the clock? A timed **Avoid**
 *  counts down; a last-heard age climbs. A count does neither, which is why the
 *  most-active sort needs no clock at all. */
const ticks = (row: PanelRow, sort: PanelSort): boolean =>
  (row.avoidedUntil ?? 0) > 0 || (sort === 'name' && row.lastCallAtMs !== undefined)

/**
 * The rows of a section, in the order they are drawn.
 *
 * `name` is the order they arrived in — the server sorted the catalog by what
 * a Listener reads, and re-sorting it here would be a second opinion. `active`
 * is the busiest first, then whoever spoke most recently of the ones that tie,
 * and catalog order under that: a stable sort, so two silent channels keep the
 * order they would have had.
 */
function ordered(entries: CatalogEntry[], sort: PanelSort): CatalogEntry[] {
  if (sort === 'name') return entries
  return [...entries].sort(
    (a, b) =>
      (b.recentCalls ?? 0) - (a.recentCalls ?? 0) ||
      (b.lastCallAtMs ?? 0) - (a.lastCallAtMs ?? 0),
  )
}

function rowOf(
  talkgroup: CatalogEntry,
  selection: Selection,
  avoided: Avoids,
  held: Set<string>,
): PanelRow {
  const { systemRef, talkgroupRef } = talkgroup
  const key = talkgroupKey(systemRef, talkgroupRef)
  const selected = isSelected(selection, systemRef, talkgroupRef)

  return {
    key,
    label: talkgroup.label ?? talkgroup.name ?? `Talkgroup ${talkgroupRef}`,
    talkgroupRef,
    systemLabel: talkgroup.systemLabel ?? `System ${systemRef}`,
    led: ledForCall({ systemRef, talkgroupRef, led: talkgroup.led }),
    selected,
    pinned: held.has(key),
    recentCalls: talkgroup.recentCalls ?? 0,
    ...(talkgroup.lastCallAtMs === undefined
      ? {}
      : { lastCallAtMs: talkgroup.lastCallAtMs }),
    ...(key in avoided ? { avoidedUntil: avoided[key] } : {}),
    choice: { keys: [{ systemRef, talkgroupRef }], on: !selected },
  }
}

/** The Group rows and the Tag rows. A category that is fully on turns off;
 *  anything else — none of it, or some of it — turns on. */
function categoriesOf(catalog: Catalog, selection: Selection): PanelCategories[] {
  return (
    [
      { heading: 'Groups', kind: 'group' },
      { heading: 'Tags', kind: 'tag' },
    ] as const
  ).flatMap(({ heading, kind }) => {
    const views = categoryViews(catalog, selection, kind)
    if (views.length === 0) return []
    return [
      {
        heading,
        categories: views.map((view) => ({
          ...view,
          choice: { keys: view.keys, on: view.state !== 'all' },
        })),
      },
    ]
  })
}

/**
 * How long ago a Talkgroup last spoke, as short as a 400-row list can afford.
 *
 * Bounded above by the catalog's activity window, so this never has to say
 * "3 days" — a Talkgroup quieter than the window carries no instant at all and
 * its row simply shows nothing. Anything under a minute is "now", because a
 * row that flickers between `0m` and `1m` is noise, and a clock a second ahead
 * of the server's would otherwise read as a negative age.
 *
 * Deliberately not shared with [`windowLabel`] below or with the Avoid badge's
 * countdown, near-identical though the three look: this floors *elapsed* time
 * and names its own floor, the countdown ceils time *remaining* (a minute left
 * must never read as none), and a window label rounds a configured constant.
 * One function over the three would take a rounding mode and a vocabulary,
 * which is three functions wearing a coat.
 */
export function lastHeard(now: number, at: number): string {
  const minutes = Math.floor((now - at) / 60_000)
  if (minutes < 1) return 'now'
  if (minutes < 60) return `${minutes}m`
  return `${Math.floor(minutes / 60)}h`
}

/**
 * The catalog's activity window, in the words the sort control shows.
 *
 * From the server's own number rather than a constant spelled again here, so
 * "most active (24h)" cannot come to describe a window that has moved.
 */
export function windowLabel(ms: number): string {
  const minutes = Math.round(ms / 60_000)
  return minutes < 60 ? `${minutes}m` : `${Math.round(minutes / 60)}h`
}

const keysOf = (entries: CatalogEntry[]): TalkgroupKey[] =>
  entries.map(({ systemRef, talkgroupRef }) => ({ systemRef, talkgroupRef }))

/** Talkgroups whose label, name, tag, group or TGID contains `filter`. */
function matching(talkgroups: CatalogEntry[], filter: string): CatalogEntry[] {
  const needle = filter.trim().toLowerCase()
  if (!needle) return talkgroups
  return talkgroups.filter((talkgroup) =>
    [
      talkgroup.label,
      talkgroup.name,
      talkgroup.tag,
      ...talkgroup.groups,
      String(talkgroup.talkgroupRef),
    ].some((field) => field?.toLowerCase().includes(needle)),
  )
}
