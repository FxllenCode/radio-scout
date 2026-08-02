import { describe, expect, it } from 'vitest'

import type { Catalog } from '@/types'

import { panelOf } from './panel'
import {
  avoidKey,
  EVERYTHING,
  setEverything,
  setTalkgroups,
  silenced,
} from './selection'

/** Two Systems, four Talkgroups, spanning two Groups and three Tags — enough
 *  for a category to cross a System boundary, which is the case the panel's
 *  category rows exist for. */
const CATALOG: Catalog = {
  systems: [
    {
      ref: 100,
      label: 'Alpha',
      talkgroups: [
        { ref: 1, label: 'Alpha Fire', tag: 'Fire', groups: ['Emergency'] },
        { ref: 2, label: 'Alpha Law', tag: 'Law', groups: ['Emergency', 'Public'] },
        { ref: 3, label: 'Alpha Quiet', tag: 'Ops', groups: ['Public'] },
      ],
    },
    {
      ref: 200,
      label: 'Beta',
      talkgroups: [{ ref: 1, label: 'Beta Dispatch', tag: 'Fire', groups: ['Public'] }],
    },
  ],
}

const draw = (over: Partial<Parameters<typeof panelOf>[0]> = {}) =>
  panelOf({ catalog: CATALOG, selection: EVERYTHING, avoided: {}, filter: '', ...over })

const rowsOf = (panel: ReturnType<typeof panelOf>) =>
  panel.systems.flatMap((system) => system.rows)

describe('the Talkgroups panel, derived once (#91)', () => {
  it('gives a row its key, its state, and the action a tap means', () => {
    const [first] = draw().systems[0].rows

    expect(first).toEqual({
      key: '100:1',
      label: 'Alpha Fire',
      talkgroup: {
        systemRef: 100,
        systemLabel: 'Alpha',
        talkgroupRef: 1,
        label: 'Alpha Fire',
        tag: 'Fire',
        groups: ['Emergency'],
      },
      selected: true,
      // On, so a tap turns it off. The row carries what its own tap means,
      // rather than the screen working it back out from the state it drew.
      choice: { keys: [{ systemRef: 100, talkgroupRef: 1 }], on: false },
    })
  })

  it('turns an unselected row back on when it is tapped', () => {
    const off = setTalkgroups(EVERYTHING, [{ systemRef: 100, talkgroupRef: 1 }], false)

    const [first] = draw({ selection: off }).systems[0].rows

    expect(first.selected).toBe(false)
    expect(first.choice).toEqual({
      keys: [{ systemRef: 100, talkgroupRef: 1 }],
      on: true,
    })
  })

  /** A recorder that sends no labels, or a curated row with them cleared: Refs
   *  stand in, so the panel is still usable. */
  it('names an unlabeled Talkgroup and System by their Refs', () => {
    const panel = draw({
      catalog: { systems: [{ ref: 42, talkgroups: [{ ref: 7, groups: [] }] }] },
    })

    expect(panel.systems[0].label).toBe('System 42')
    expect(panel.systems[0].rows[0].label).toBe('Talkgroup 7')
  })

  /** The panel shows what the Listener will actually *hear*, so an Avoid reads
   *  off in the row — and the deadline rides with it, because that is what the
   *  countdown beside it subtracts from. */
  it('carries an Avoid’s deadline on the row it silences', () => {
    // Both halves come off one Avoid map in the store, so a row cannot badge a
    // deadline while still reading as on: `selectAudibleSelection` is exactly
    // `silenced` over the map passed here.
    const avoided = { [avoidKey(100, 1)]: 30 * 60_000, [avoidKey(100, 2)]: 0 }
    const panel = draw({ selection: silenced(EVERYTHING, avoided), avoided })
    const [fire, law, quiet] = panel.systems[0].rows

    expect(fire).toMatchObject({ selected: false, avoidedUntil: 30 * 60_000 })
    expect(law).toMatchObject({ selected: false, avoidedUntil: 0 })
    expect(quiet.avoidedUntil).toBeUndefined()
    expect(quiet.selected).toBe(true)
  })

  it('leaves out a System the filter has emptied, and says nothing matched', () => {
    const panel = draw({ filter: 'law' })

    expect(panel.systems.map((system) => system.label)).toEqual(['Alpha'])
    expect(rowsOf(panel).map((row) => row.label)).toEqual(['Alpha Law'])
    expect(panel.empty).toBe(false)
    expect(draw({ filter: 'zzz' }).empty).toBe(true)
  })

  it.each([
    ['a label', 'quiet'],
    ['a name', 'alpha'],
    ['a Tag', 'ops'],
    ['a Group', 'public'],
    ['a TGID', '3'],
  ])('finds a Talkgroup by %s', (_what, filter) => {
    expect(rowsOf(draw({ filter })).map((row) => row.label)).toContain('Alpha Quiet')
  })

  describe('the counts', () => {
    it('summarizes the whole catalog, not the filtered view', () => {
      const off = setTalkgroups(EVERYTHING, [{ systemRef: 100, talkgroupRef: 1 }], false)

      expect(draw({ selection: off }).on).toBe(3)
      expect(draw({ selection: off }).total).toBe(4)
      // The filter narrows what is *shown*; it does not claim the Listener
      // deselected everything it is hiding.
      expect(draw({ selection: off, filter: 'law' })).toMatchObject({ on: 3, total: 4 })
    })

    it('counts a System against the rows it is showing', () => {
      const [alpha] = draw({ filter: 'alpha' }).systems
      expect(alpha).toMatchObject({ on: 3, total: 3, allOn: true })

      const [narrowed] = draw({ filter: 'law' }).systems
      expect(narrowed).toMatchObject({ on: 1, total: 1, allOn: true })
    })

    it('reports a System as not all-on when one of its rows is off', () => {
      const off = setTalkgroups(EVERYTHING, [{ systemRef: 100, talkgroupRef: 1 }], false)

      expect(draw({ selection: off }).systems[0]).toMatchObject({
        on: 2,
        total: 3,
        allOn: false,
      })
    })
  })

  describe('the category rows (spec US 20)', () => {
    it('offers Groups and Tags, each with what a tap would flip', () => {
      const panel = draw()

      expect(panel.categories.map((row) => row.heading)).toEqual(['Groups', 'Tags'])
      expect(panel.categories[0].categories.map((one) => one.label)).toEqual([
        'Emergency',
        'Public',
      ])
      expect(panel.categories[1].categories.map((one) => one.label)).toEqual([
        'Fire',
        'Law',
        'Ops',
      ])
    })

    /** A category that is fully on turns off; anything else turns on. That rule
     *  used to live in the screen's `onClick`. */
    it.each([
      ['all on', EVERYTHING, false],
      ['all off', setEverything(false), true],
      [
        'partly on',
        setTalkgroups(EVERYTHING, [{ systemRef: 100, talkgroupRef: 1 }], false),
        true,
      ],
    ])('turns a category that is %s the other way', (_what, selection, on) => {
      const [emergency] = draw({ selection }).categories[0].categories

      expect(emergency.choice).toEqual({
        keys: [
          { systemRef: 100, talkgroupRef: 1 },
          { systemRef: 100, talkgroupRef: 2 },
        ],
        on,
      })
    })

    /** A catalog with nothing to categorize by shows no category rows at all,
     *  rather than an empty heading. */
    it('leaves out a kind the catalog has none of', () => {
      const panel = draw({
        catalog: { systems: [{ ref: 42, talkgroups: [{ ref: 7, groups: [] }] }] },
      })

      expect(panel.categories).toEqual([])
    })
  })

  /**
   * "The control acts on what it is next to" — the rule this was a table test
   * for (#91). It used to be reachable only by typing into the search box and
   * clicking, which is why the *unfiltered* half was never asserted at all.
   *
   * Unfiltered, a System's All on/off is one **wildcard**, which also covers
   * the Talkgroups this browser has never heard of (spec US 21). Filtered, it
   * is the rows on screen and nothing else — "All off" on a System showing one
   * row must not silence the thirty-nine it is hiding.
   */
  describe('a System’s All on / All off', () => {
    it('is a wildcard over the whole System when nothing is filtered', () => {
      const [alpha] = draw().systems

      expect(alpha.all).toEqual({ systemRef: 100, on: false })
    })

    it('is the rows on screen when a filter is showing some of them', () => {
      const [alpha] = draw({ filter: 'law' }).systems

      expect(alpha.all).toEqual({ keys: [{ systemRef: 100, talkgroupRef: 2 }], on: false })
    })

    it('turns a System that is off back on, whichever form it takes', () => {
      const none = setEverything(false)

      expect(draw({ selection: none }).systems[0].all).toEqual({
        systemRef: 100,
        on: true,
      })
      expect(draw({ selection: none, filter: 'law' }).systems[0].all).toEqual({
        keys: [{ systemRef: 100, talkgroupRef: 2 }],
        on: true,
      })
    })

    /** Whitespace is not a filter. A Listener who typed a space and deleted the
     *  word must get the wildcard back, or "All off" would silently stop
     *  covering the Talkgroups they have never heard of. */
    it('is a wildcard again once the filter is blank', () => {
      expect(draw({ filter: '   ' }).systems[0].all).toEqual({
        systemRef: 100,
        on: false,
      })
    })
  })
})
