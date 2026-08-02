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
 */
import {
  avoidKey,
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
   *  number a Talkgroup `1`, which a Ref alone would collide on. */
  key: string
  /** What to call it: its label, its name, or its Ref. */
  label: string
  talkgroup: CatalogEntry
  /** Will the Listener hear it? The Avoids are already laid into the Selection
   *  this was drawn from, so a silenced Talkgroup reads off here exactly as it
   *  does in the counts above. */
  selected: boolean
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
  systems: PanelSystem[]
  /** The filter matched nothing, so the panel says so instead of drawing a
   *  wall of empty sections. */
  empty: boolean
}

export interface PanelInput {
  catalog: Catalog
  /** The **Selection** with the Avoids already laid over it — what the Listener
   *  will actually hear (`selectAudibleSelection`). */
  selection: Selection
  /** The deadlines, for the badge each avoided row carries. */
  avoided: Avoids
  filter: string
}

export function panelOf({ catalog, selection, avoided, filter }: PanelInput): Panel {
  const matches = matching(talkgroupsOf(catalog), filter)
  // Whitespace is not a filter: a Listener who typed a space and deleted the
  // word must get the wildcard back, or a System's "All off" would quietly stop
  // covering the Talkgroups they have never heard of.
  const filtered = filter.trim().length > 0

  const systems = catalog.systems.flatMap((system) => {
    const rows = matches.filter((entry) => entry.systemRef === system.ref)
    if (rows.length === 0) return []
    const on = countOn(selection, rows)
    const allOn = stateOf(selection, rows) === 'all'

    return [
      {
        key: system.ref,
        label: system.label ?? `System ${system.ref}`,
        on,
        total: rows.length,
        allOn,
        // The control acts on what it is next to. Unfiltered that is the System
        // itself, as one wildcard; filtered it is the rows on screen and
        // nothing else, so "All off" on a System showing one row does not
        // silence the thirty-nine it is hiding.
        all: filtered
          ? { keys: keysOf(rows), on: !allOn }
          : { systemRef: system.ref, on: !allOn },
        rows: rows.map((entry) => rowOf(entry, selection, avoided)),
      },
    ]
  })

  return {
    ...summarize(catalog, selection),
    categories: categoriesOf(catalog, selection),
    systems,
    empty: matches.length === 0,
  }
}

function rowOf(
  talkgroup: CatalogEntry,
  selection: Selection,
  avoided: Avoids,
): PanelRow {
  const { systemRef, talkgroupRef } = talkgroup
  const key = avoidKey(systemRef, talkgroupRef)
  const selected = isSelected(selection, systemRef, talkgroupRef)

  return {
    key,
    label: talkgroup.label ?? talkgroup.name ?? `Talkgroup ${talkgroupRef}`,
    talkgroup,
    selected,
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
