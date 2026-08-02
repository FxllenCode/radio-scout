import { describe, expect, it } from 'vitest'

import type { Catalog } from '@/types'

import {
  categoryViews,
  EVERYTHING,
  restrictToSystem,
  restrictToTalkgroup,
  isSelected,
  setEverything,
  setSystem,
  setTalkgroups,
  stateOf,
  summarize,
  talkgroupsOf,
  type Selection,
} from './selection'

describe('isSelected', () => {
  /** The same resolution order the server applies (`Subscription::selects` in
   *  src/live.rs) — the client filters its queue with the very matrix it sent,
   *  so the two can't drift. */
  it('starts with everything on', () => {
    expect(isSelected(EVERYTHING, 11, 100)).toBe(true)
  })

  it('reads the global default when nothing is said about the Talkgroup', () => {
    const selection: Selection = { all: false, sel: {} }

    expect(isSelected(selection, 11, 100)).toBe(false)
  })

  it("prefers a System's wildcard over the global default", () => {
    const selection: Selection = { all: false, sel: { '11': { '*': true } } }

    expect(isSelected(selection, 11, 100)).toBe(true)
    expect(isSelected(selection, 12, 100)).toBe(false)
  })

  it('prefers an explicit Talkgroup over its System wildcard', () => {
    const selection: Selection = {
      all: true,
      sel: { '11': { '*': true, '100': false } },
    }

    expect(isSelected(selection, 11, 100)).toBe(false)
    expect(isSelected(selection, 11, 101)).toBe(true)
  })
})

/** The stored shape is the wire shape, so "what did we store" is a real
 *  assertion: a selection that accumulated redundant entries would be sent to
 *  the server on every change and persisted forever. */
describe('changing the selection', () => {
  const tg = (systemRef: number, talkgroupRef: number) => ({
    systemRef,
    talkgroupRef,
  })

  it('records turning one Talkgroup off as a single exception', () => {
    expect(setTalkgroups(EVERYTHING, [tg(11, 100)], false)).toEqual({
      all: true,
      sel: { '11': { '100': false } },
    })
  })

  it('drops the exception when the Talkgroup agrees with the default again', () => {
    const off = setTalkgroups(EVERYTHING, [tg(11, 100)], false)

    expect(setTalkgroups(off, [tg(11, 100)], true)).toEqual(EVERYTHING)
  })

  it('changes several Talkgroups at once — what a category toggle does', () => {
    const selection = setTalkgroups(
      EVERYTHING,
      [tg(11, 100), tg(11, 101), tg(12, 100)],
      false,
    )

    expect(selection).toEqual({
      all: true,
      sel: { '11': { '100': false, '101': false }, '12': { '100': false } },
    })
  })

  it('leaves the selection alone when it changes nothing', () => {
    expect(setTalkgroups(EVERYTHING, [], false)).toEqual(EVERYTHING)
  })

  it('turns a whole System off with one wildcard, not one entry per Talkgroup', () => {
    const selection = setSystem(EVERYTHING, 11, false)

    expect(selection).toEqual({ all: true, sel: { '11': { '*': false } } })
    expect(isSelected(selection, 11, 999)).toBe(false)
    expect(isSelected(selection, 12, 999)).toBe(true)
  })

  it('forgets a System’s Talkgroup exceptions when the whole System is set', () => {
    const mixed = setTalkgroups(EVERYTHING, [tg(11, 100)], false)

    expect(setSystem(mixed, 11, true)).toEqual(EVERYTHING)
  })

  it('keeps a Talkgroup exception under a System wildcard', () => {
    const nothing = setEverything(false)
    const selection = setTalkgroups(nothing, [tg(11, 100)], true)

    expect(selection).toEqual({ all: false, sel: { '11': { '100': true } } })
    expect(isSelected(selection, 11, 100)).toBe(true)
    expect(isSelected(selection, 11, 101)).toBe(false)
  })

  it('drops an exception that agrees with its System wildcard', () => {
    const held = setSystem(setEverything(false), 11, true)
    const selection = setTalkgroups(held, [tg(11, 100)], true)

    expect(selection).toEqual({ all: false, sel: { '11': { '*': true } } })
  })

  it('wipes every exception when everything is turned on or off', () => {
    const mixed = setTalkgroups(setSystem(EVERYTHING, 12, false), [tg(11, 100)], false)

    expect(setEverything(true)).toEqual({ all: true, sel: {} })
    expect(setEverything(false)).toEqual({ all: false, sel: {} })
    expect(mixed).not.toEqual(EVERYTHING)
  })

  it('never mutates the selection it was given', () => {
    const before = structuredClone(EVERYTHING)

    setTalkgroups(EVERYTHING, [tg(11, 100)], false)
    setSystem(EVERYTHING, 11, false)

    expect(EVERYTHING).toEqual(before)
  })
})

/** A two-System catalog, the shape `GET /api/catalog` serves. */
const CATALOG: Catalog = {
  systems: [
    {
      ref: 100,
      label: 'Alpha',
      talkgroups: [
        { ref: 1, label: 'Alpha Fire', tag: 'Fire', groups: ['Emergency'] },
        { ref: 2, label: 'Alpha Law', tag: 'Law', groups: ['Emergency', 'Public'] },
      ],
    },
    {
      ref: 200,
      label: 'Beta',
      talkgroups: [
        { ref: 1, label: 'Beta Dispatch', tag: 'Fire', groups: ['Public'] },
      ],
    },
  ],
}

describe('reading the catalog against a selection', () => {
  it('flattens Talkgroups with the System they belong to', () => {
    expect(talkgroupsOf(CATALOG).map((t) => [t.systemRef, t.talkgroupRef])).toEqual([
      [100, 1],
      [100, 2],
      [200, 1],
    ])
  })

  /** One Ref field, not two spellings of it (#91). A flattened entry used to
   *  carry the catalog's `ref` *and* the key's `talkgroupRef` holding the same
   *  number, so a reader had to know which one this particular call site had
   *  settled on — and two of them could be compared and always agree. */
  it('spells a Talkgroup Ref exactly once', () => {
    expect(talkgroupsOf(CATALOG)[0]).toEqual({
      systemRef: 100,
      systemLabel: 'Alpha',
      talkgroupRef: 1,
      label: 'Alpha Fire',
      tag: 'Fire',
      groups: ['Emergency'],
    })
  })

  it('counts how much of the catalog is selected', () => {
    const off = setTalkgroups(EVERYTHING, [{ systemRef: 100, talkgroupRef: 2 }], false)

    expect(summarize(CATALOG, EVERYTHING)).toEqual({ on: 3, total: 3 })
    expect(summarize(CATALOG, off)).toEqual({ on: 2, total: 3 })
    expect(summarize(CATALOG, setEverything(false))).toEqual({ on: 0, total: 3 })
  })

  /** The three-state category toggle of spec US 20. */
  it('reports a category as all, some, or none', () => {
    const [emergency] = categoryViews(CATALOG, EVERYTHING, 'group')
    expect(emergency).toMatchObject({ label: 'Emergency', state: 'all', on: 2, total: 2 })

    const oneOff = setTalkgroups(EVERYTHING, [{ systemRef: 100, talkgroupRef: 1 }], false)
    expect(categoryViews(CATALOG, oneOff, 'group')[0]).toMatchObject({
      state: 'some',
      on: 1,
    })

    expect(categoryViews(CATALOG, setEverything(false), 'group')[0]).toMatchObject({
      state: 'none',
      on: 0,
    })
  })

  it('spans Systems in one category — the point of a Group', () => {
    const [, publicGroup] = categoryViews(CATALOG, EVERYTHING, 'group')

    expect(publicGroup.label).toBe('Public')
    expect(publicGroup.keys).toEqual([
      { systemRef: 100, talkgroupRef: 2 },
      { systemRef: 200, talkgroupRef: 1 },
    ])
  })

  it('offers Tag categories as well as Group ones, sorted and deduped', () => {
    expect(categoryViews(CATALOG, EVERYTHING, 'tag').map((c) => c.label)).toEqual([
      'Fire',
      'Law',
    ])
    expect(categoryViews(CATALOG, EVERYTHING, 'group').map((c) => c.label)).toEqual([
      'Emergency',
      'Public',
    ])
  })

  /** Auto-populate always assigns a Tag, but CSV import (#18) and a hand-edited
   *  database can leave one without — and an untagged Talkgroup belongs to no
   *  Tag category rather than to an empty-named one. */
  it('leaves an untagged Talkgroup out of the Tag categories', () => {
    const untagged: Catalog = {
      systems: [{ ref: 100, talkgroups: [{ ref: 9, groups: [] }] }],
    }

    expect(categoryViews(untagged, EVERYTHING, 'tag')).toEqual([])
    expect(categoryViews(untagged, EVERYTHING, 'group')).toEqual([])
    expect(summarize(untagged, EVERYTHING)).toEqual({ on: 1, total: 1 })
  })

  it('has no categories to offer for an empty catalog', () => {
    const empty: Catalog = { systems: [] }

    expect(categoryViews(empty, EVERYTHING, 'group')).toEqual([])
    expect(summarize(empty, EVERYTHING)).toEqual({ on: 0, total: 0 })
    expect(stateOf(EVERYTHING, [])).toBe('none')
  })
})

/** Holding narrows the feed to the current System or Talkgroup and then gives
 *  the selection back (CONTEXT.md **Hold**) — so a hold is the selection seen
 *  through a window, not a replacement for it. */
describe('restricting a selection to what is held', () => {
  it('keeps only the held System, still on by default', () => {
    expect(restrictToSystem(EVERYTHING, 11)).toEqual({
      all: false,
      sel: { '11': { '*': true } },
    })
  })

  it('carries the held System’s own exceptions through', () => {
    const selection = setTalkgroups(EVERYTHING, [{ systemRef: 11, talkgroupRef: 100 }], false)

    expect(restrictToSystem(selection, 11)).toEqual({
      all: false,
      sel: { '11': { '*': true, '100': false } },
    })
  })

  it('holds a System that was only partly selected without turning it all on', () => {
    const selection = setTalkgroups(
      setEverything(false),
      [{ systemRef: 11, talkgroupRef: 100 }],
      true,
    )

    expect(restrictToSystem(selection, 11)).toEqual({
      all: false,
      sel: { '11': { '100': true } },
    })
  })

  it('selects nothing when the held System has nothing selected in it', () => {
    expect(restrictToSystem(setEverything(false), 11)).toEqual({ all: false, sel: {} })
  })

  /** A Talkgroup hold is the listener pointing at what is playing, so it wins
   *  over a selection that would have excluded it. */
  it('keeps only the held Talkgroup, whatever the selection said', () => {
    expect(restrictToTalkgroup(11, 100)).toEqual({
      all: false,
      sel: { '11': { '100': true } },
    })
  })
})
