/**
 * The **Selection** (CONTEXT.md): which Systems and Talkgroups the live feed
 * plays (#12, spec US 19–22).
 *
 * It is stored as the ADR-0004 subscription matrix itself — `all` plus
 * exceptions under `sel[system][talkgroup | "*"]` — rather than as a full
 * `system → talkgroup → bool` map the way rdio-scanner's `livefeedMap` is. Three
 * things fall out of that, and they are the reason for the shape:
 *
 * 1. **What is stored is what goes on the wire.** No rebuild step, and the
 *    client can filter its own queue with exactly the rule the server applies
 *    (`Subscription::selects` in `src/live.rs`), so the two can't drift.
 * 2. **A Talkgroup nobody has decided about inherits the default**, so a
 *    Talkgroup auto-populated (#8) after the selection was saved is heard
 *    rather than silently missing. rdio has to rebuild its map against its
 *    config to answer the same question.
 * 3. **All-on / all-off is one entry, not N**, which keeps the persisted JSON
 *    small and stable on a system with hundreds of Talkgroups.
 *
 * Everything here is pure: the store owns the state, this owns the algebra.
 */
import type { Catalog, CatalogTalkgroup } from '@/types'

import type { Subscription } from './liveFeed'

/** The listener's selection, in the shape the server reads. */
export type Selection = Subscription

/** The Talkgroup key meaning "every Talkgroup in this System" (ADR-0004). */
export const WILDCARD = '*'

/** What a listener starts with: a scanner that plays everything. */
export const EVERYTHING: Selection = { all: true, sel: {} }

/**
 * Is this Talkgroup selected?
 *
 * Most specific wins — an explicit entry, then the System's wildcard, then the
 * global default. That ordering is what lets one Talkgroup be an exception to
 * an otherwise all-on selection.
 */
export function isSelected(
  selection: Selection,
  systemRef: number,
  talkgroupRef: number,
): boolean {
  const system = selection.sel[systemRef]
  const explicit = system?.[talkgroupRef]
  if (explicit !== undefined) return explicit
  return defaultFor(selection, systemRef)
}

/** What a Talkgroup nobody has decided about does, in this System. */
function defaultFor(selection: Selection, systemRef: number): boolean {
  return selection.sel[systemRef]?.[WILDCARD] ?? selection.all
}

/** One Talkgroup, addressed the way the matrix keys it. */
export interface TalkgroupKey {
  systemRef: number
  talkgroupRef: number
}

/** Turn these Talkgroups on or off, leaving everything else as it was — one
 *  tap on a row, or every Talkgroup behind a Group/Tag category chip. */
export function setTalkgroups(
  selection: Selection,
  keys: TalkgroupKey[],
  on: boolean,
): Selection {
  const sel = clone(selection.sel)
  for (const { systemRef, talkgroupRef } of keys) {
    const system = (sel[systemRef] ??= {})
    // An entry that agrees with what the Talkgroup would do anyway is noise: it
    // would ride every wire frame and sit in local storage forever.
    if (on === (system[WILDCARD] ?? selection.all)) {
      delete system[talkgroupRef]
    } else {
      system[talkgroupRef] = on
    }
  }
  return { all: selection.all, sel: prune(sel) }
}

/** Turn a whole System on or off (spec US 21), including the Talkgroups this
 *  browser has never heard of — which is what the wildcard is for. Its own
 *  Talkgroup exceptions are cleared: "all on" that left three off would be a
 *  lie. */
export function setSystem(
  selection: Selection,
  systemRef: number,
  on: boolean,
): Selection {
  const sel = clone(selection.sel)
  sel[systemRef] = on === selection.all ? {} : { [WILDCARD]: on }
  return { all: selection.all, sel: prune(sel) }
}

/** The global all-on / all-off (spec US 21). Every exception goes with it —
 *  the listener just said what they want in full. */
export function setEverything(on: boolean): Selection {
  return { all: on, sel: {} }
}

/**
 * The selection as seen through a **Hold** on one System (CONTEXT.md, spec
 * US 11): everything else is silenced, and inside it the listener's own choices
 * still apply — holding a System does not un-avoid the three Talkgroups in it
 * they turned off.
 *
 * rdio instead *replaces* the map for the duration of the hold (and turns the
 * whole System on if the listener happened to have all of it off), keeping a
 * copy to restore afterwards. Deriving the hold from the selection means there
 * is nothing to restore, so a selection change made while holding survives the
 * release.
 */
export function restrictToSystem(selection: Selection, systemRef: number): Selection {
  const held: Record<string, boolean> = { ...selection.sel[systemRef] }
  // Under a global `all: false` a `'*': false` says nothing, and an empty
  // System entry says nothing either.
  if (defaultFor(selection, systemRef)) held[WILDCARD] = true
  else delete held[WILDCARD]

  return { all: false, sel: prune({ [systemRef]: held }) }
}

/** The selection under a **Hold** on one Talkgroup: that Talkgroup and nothing
 *  else. The listener is pointing at what is playing, so it is on even if the
 *  selection had it off — it reached them via a patch (spec US 18), say. */
export function restrictToTalkgroup(
  systemRef: number,
  talkgroupRef: number,
): Selection {
  return { all: false, sel: { [systemRef]: { [talkgroupRef]: true } } }
}

function clone(sel: Selection['sel']): Selection['sel'] {
  return Object.fromEntries(
    Object.entries(sel).map(([system, talkgroups]) => [system, { ...talkgroups }]),
  )
}

/** Drop Systems left with nothing to say. */
function prune(sel: Selection['sel']): Selection['sel'] {
  return Object.fromEntries(
    Object.entries(sel).filter(([, talkgroups]) => Object.keys(talkgroups).length > 0),
  )
}

// ---------------------------------------------------------------------------
// Reading the catalog against a selection — what the Talkgroups panel draws.
// ---------------------------------------------------------------------------

/** A catalog Talkgroup with the System Ref that completes its key. */
export interface CatalogEntry extends TalkgroupKey, CatalogTalkgroup {
  systemLabel?: string
}

/** Every Talkgroup in the catalog, flattened, in catalog order (which the
 *  server has already sorted for reading). */
export function talkgroupsOf(catalog: Catalog): CatalogEntry[] {
  return catalog.systems.flatMap((system) =>
    system.talkgroups.map((talkgroup) => ({
      ...talkgroup,
      systemRef: system.ref,
      systemLabel: system.label,
      talkgroupRef: talkgroup.ref,
    })),
  )
}

/** How much of a set of Talkgroups is selected — the three states a category
 *  button shows (spec US 20). An empty set is `none`: there is nothing on. */
export type TriState = 'all' | 'some' | 'none'

export function stateOf(selection: Selection, keys: TalkgroupKey[]): TriState {
  const on = countOn(selection, keys)
  if (on === 0) return 'none'
  return on === keys.length ? 'all' : 'some'
}

export function countOn(selection: Selection, keys: TalkgroupKey[]): number {
  return keys.filter((key) => isSelected(selection, key.systemRef, key.talkgroupRef))
    .length
}

/** `on` of `total` Talkgroups selected across the whole catalog. */
export function summarize(
  catalog: Catalog,
  selection: Selection,
): { on: number; total: number } {
  const all = talkgroupsOf(catalog)
  return { on: countOn(selection, all), total: all.length }
}

/** The two kinds of category the panel toggles (CONTEXT.md reserves
 *  "category" for exactly this UI concept spanning Groups and Tags). */
export type CategoryKind = 'group' | 'tag'

/** One category row: what it's called, how much of it is on, and the
 *  Talkgroups a tap would flip. */
export interface CategoryView {
  kind: CategoryKind
  label: string
  state: TriState
  on: number
  total: number
  keys: TalkgroupKey[]
}

/**
 * Every Group (or Tag) category in the catalog, sorted, with its three-state
 * status against this selection.
 *
 * rdio builds category buttons from Groups alone — its `RdioScannerCategoryType.Tag`
 * is reachable in `toggleCategory` but nothing ever constructs one, so the Tag
 * half of the feature is dead. Spec US 20 asks for both, so both are built here.
 */
export function categoryViews(
  catalog: Catalog,
  selection: Selection,
  kind: CategoryKind,
): CategoryView[] {
  const byLabel = new Map<string, TalkgroupKey[]>()
  for (const entry of talkgroupsOf(catalog)) {
    for (const label of labelsOf(entry, kind)) {
      const keys = byLabel.get(label) ?? []
      keys.push({ systemRef: entry.systemRef, talkgroupRef: entry.talkgroupRef })
      byLabel.set(label, keys)
    }
  }

  return [...byLabel.entries()]
    .sort(([a], [b]) => a.localeCompare(b))
    .map(([label, keys]) => ({
      kind,
      label,
      keys,
      state: stateOf(selection, keys),
      on: countOn(selection, keys),
      total: keys.length,
    }))
}

/** A Talkgroup has one Tag and any number of Groups (CONTEXT.md), so category
 *  membership is a list either way. */
function labelsOf(entry: CatalogEntry, kind: CategoryKind): string[] {
  if (kind === 'tag') return entry.tag ? [entry.tag] : []
  return entry.groups
}
