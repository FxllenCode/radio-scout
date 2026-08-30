import { describe, expect, it } from 'vitest'

import { countyCatalog } from '@/test/handlers'
import type { Catalog } from '@/types'

/** The window the server counts activity over — 24 hours, as `catalog.rs`
 *  serves it. */
const DAY = 24 * 60 * 60 * 1_000

import { lastHeard, panelOf, windowLabel } from './panel'
import {
  talkgroupKey,
  EVERYTHING,
  setEverything,
  setTalkgroups,
  silenced,
} from './selection'

/** Two Systems, four Talkgroups, spanning two Groups and three Tags — enough
 *  for a category to cross a System boundary, which is the case the panel's
 *  category rows exist for. */
const CATALOG: Catalog = {
  activityWindowMs: DAY,
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
  panelOf({
    catalog: CATALOG,
    selection: EVERYTHING,
    avoided: {},
    priority: [],
    filter: '',
    pinned: [],
    expanded: {},
    sort: 'name',
    ...over,
  })

const rowsOf = (panel: ReturnType<typeof panelOf>) =>
  panel.systems.flatMap((system) => system.rows)

describe('the Talkgroups panel, derived once (#91)', () => {
  it('gives a row its key, its state, and the action a tap means', () => {
    const [first] = draw().systems[0].rows

    expect(first).toEqual({
      key: '100:1',
      label: 'Alpha Fire',
      priority: false,
      talkgroupRef: 1,
      systemLabel: 'Alpha',
      // Resolved here rather than per row per render (#91 left this behind on
      // purpose, for the ticket that windowed the list).
      led: 'red',
      selected: true,
      pinned: false,
      recentCalls: 0,
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
      catalog: {
        activityWindowMs: DAY,
        systems: [{ ref: 42, talkgroups: [{ ref: 7, groups: [] }] }],
      },
    })

    expect(panel.systems[0].label).toBe('System 42')
    expect(panel.systems[0].rows[0].label).toBe('Talkgroup 7')
    expect(panel.systems[0].rows[0].systemLabel).toBe('System 42')
  })

  /** The panel shows what the Listener will actually *hear*, so an Avoid reads
   *  off in the row — and the deadline rides with it, because that is what the
   *  countdown beside it subtracts from. */
  it('carries an Avoid’s deadline on the row it silences', () => {
    // Both halves come off one Avoid map in the store, so a row cannot badge a
    // deadline while still reading as on: `selectAudibleSelection` is exactly
    // `silenced` over the map passed here.
    const avoided = { [talkgroupKey(100, 1)]: 30 * 60_000, [talkgroupKey(100, 2)]: 0 }
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
        catalog: {
        activityWindowMs: DAY,
        systems: [{ ref: 42, talkgroups: [{ ref: 7, groups: [] }] }],
      },
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

/**
 * A county System is 400+ Talkgroups (#57). Everything below is about making
 * that a panel rather than a scroll: what floats to the top, what order the
 * rest is in, and what is folded away until asked for.
 */
describe('the Talkgroups panel at county scale (#57)', () => {
  /** A System with more rows than [`COLLAPSE_ABOVE`], the size the collapse
   *  rule is about — and with activity running backwards against catalog order,
   *  so a most-active sort cannot agree with it by accident. */
  const county = (rows: number) => countyCatalog(rows)

  describe('Pins (spec US 29)', () => {
    /** The point of a Pin: a daily channel in the third System must not live
     *  behind two 400-row scrolls, so pinned rows are their own section above
     *  every System. */
    it('floats pinned Talkgroups to the top, across Systems', () => {
      const panel = draw({ pinned: [talkgroupKey(200, 1), talkgroupKey(100, 2)] })

      expect(panel.pinned.map((row) => row.label)).toEqual([
        'Alpha Law',
        'Beta Dispatch',
      ])
      expect(panel.pinned.map((row) => row.systemLabel)).toEqual(['Alpha', 'Beta'])
    })

    /** A Pin is panel ordering and nothing else (CONTEXT.md), so the row stays
     *  where it was: the System it belongs to still counts it, and scrolling
     *  that System finds no hole where a pinned row used to be. */
    it('leaves the pinned row in its own System too, marked', () => {
      const panel = draw({ pinned: [talkgroupKey(100, 2)] })
      const [fire, law] = panel.systems[0].rows

      expect(law).toMatchObject({ key: '100:2', pinned: true })
      expect(fire.pinned).toBe(false)
      expect(panel.systems[0]).toMatchObject({ on: 3, total: 3 })
      expect(panel.pinned[0].pinned).toBe(true)
    })

    it('has no pinned section when nothing is pinned', () => {
      expect(draw().pinned).toEqual([])
    })

    /** A Talkgroup an Operator deleted, or one pinned under another
     *  **Profile**'s catalog: a stale Pin draws nothing rather than an
     *  unnameable row. */
    it('ignores a Pin on a Talkgroup the catalog does not have', () => {
      expect(draw({ pinned: [talkgroupKey(999, 999)] }).pinned).toEqual([])
    })

    /** The filter narrows the pinned section like everything else — a Listener
     *  searching for "law" is not looking at their pinned Fire channel. */
    it('filters the pinned section with the rest', () => {
      const pinned = [talkgroupKey(100, 1), talkgroupKey(100, 2)]

      expect(draw({ pinned, filter: 'law' }).pinned.map((row) => row.label)).toEqual([
        'Alpha Law',
      ])
    })
  })

  describe('the most-active sort', () => {
    it('leaves the catalog’s own order alone by default', () => {
      expect(draw({ catalog: county(3), sort: 'name' }).systems[0].rows.map((r) => r.label))
        .toEqual(['Channel 001', 'Channel 002', 'Channel 003'])
    })

    /** The fixture's own promise, asserted rather than trusted: if activity ran
     *  *along* catalog order, every assertion below would pass against a sort
     *  that did nothing at all. */
    it('reverses the catalog order of a County, which is the point of it', () => {
      expect(draw({ catalog: county(3), sort: 'active' }).systems[0].rows.map((r) => r.label))
        .toEqual(['Channel 003', 'Channel 002', 'Channel 001'])
    })

    it('puts the busiest Talkgroup first', () => {
      const catalog: Catalog = {
        activityWindowMs: DAY,
        systems: [
          {
            ref: 1,
            label: 'County',
            talkgroups: [
              { ref: 1, label: 'Quiet', groups: [] },
              { ref: 2, label: 'Busy', groups: [], recentCalls: 40, lastCallAtMs: 9 },
              { ref: 3, label: 'Some', groups: [], recentCalls: 4, lastCallAtMs: 99 },
            ],
          },
        ],
      }

      expect(
        draw({ catalog, sort: 'active' }).systems[0].rows.map((row) => row.label),
      ).toEqual(['Busy', 'Some', 'Quiet'])
    })

    /** Two channels that took the same number of Calls are ordered by which
     *  spoke last — the more useful of the two answers, and the one a Listener
     *  scanning for "who is up right now" is really asking for. */
    it('breaks a tie on which spoke most recently, then on catalog order', () => {
      const catalog: Catalog = {
        activityWindowMs: DAY,
        systems: [
          {
            ref: 1,
            label: 'County',
            talkgroups: [
              { ref: 1, label: 'Older', groups: [], recentCalls: 5, lastCallAtMs: 10 },
              { ref: 2, label: 'Newer', groups: [], recentCalls: 5, lastCallAtMs: 20 },
              { ref: 3, label: 'Silent A', groups: [] },
              { ref: 4, label: 'Silent B', groups: [] },
            ],
          },
        ],
      }

      expect(
        draw({ catalog, sort: 'active' }).systems[0].rows.map((row) => row.label),
      ).toEqual(['Newer', 'Older', 'Silent A', 'Silent B'])
    })

    it('sorts the pinned section the same way', () => {
      const catalog = county(3)

      expect(
        draw({ catalog, sort: 'active', pinned: ['1:1', '1:3'] }).pinned.map((r) => r.label),
      ).toEqual(['Channel 003', 'Channel 001'])
      // ...and under catalog order the pin order says nothing, which is why the
      // store keeps the Pins as a set.
      expect(
        draw({ catalog, sort: 'name', pinned: ['1:3', '1:1'] }).pinned.map((r) => r.label),
      ).toEqual(['Channel 001', 'Channel 003'])
    })
  })

  describe('collapsing a System', () => {
    it('leaves a small System open and folds a county-sized one away', () => {
      expect(draw().systems[0].collapsed).toBe(false)
      expect(draw({ catalog: county(400) }).systems[0].collapsed).toBe(true)
    })

    /** The Listener's own choice outranks the size rule, in both directions —
     *  which is why it is stored as what they *said*, not as a list of what is
     *  shut: a System they opened must not re-collapse on the next reload. */
    it.each([
      ['opens a big System the Listener opened', county(400), true, false],
      ['shuts a small System the Listener shut', CATALOG, false, true],
    ])('%s', (_what, catalog, said, collapsed) => {
      const systemRef = catalog.systems[0].ref

      expect(draw({ catalog, expanded: { [systemRef]: said } }).systems[0].collapsed).toBe(
        collapsed,
      )
    })

    /** Typing a filter is asking to see what matched. A fold that survived it
     *  would answer a search with a shut box. */
    it('opens every System while a filter is showing matches', () => {
      const catalog = county(400)

      expect(draw({ catalog, filter: 'Channel 007' }).systems[0].collapsed).toBe(false)
      expect(
        draw({ catalog, filter: 'Channel 007', expanded: { 1: false } }).systems[0]
          .collapsed,
      ).toBe(false)
    })

    /** The header of a collapsed System still says how much of it is on, and
     *  its All on/off still acts on the whole System — that is the point of
     *  collapsing rather than hiding. */
    it('still counts and still acts while collapsed', () => {
      const [county400] = draw({ catalog: county(400) }).systems

      expect(county400).toMatchObject({ on: 400, total: 400, allOn: true })
      expect(county400.all).toEqual({ systemRef: 1, on: false })
    })

    /** ...and it builds none of them. Four hundred rows, each resolving an LED,
     *  is exactly the work folding a System is for — a panel that built them
     *  anyway would be a fold that only hid the cost. */
    it('draws no rows at all while collapsed', () => {
      expect(draw({ catalog: county(400) }).systems[0].rows).toEqual([])
    })
  })

  describe('activity on the row', () => {
    it('carries how busy a Talkgroup has been and when it last spoke', () => {
      const catalog = countyCatalog(2, 500_000)
      const [quieter, busiest] = draw({ catalog }).systems[0].rows

      expect(quieter).toMatchObject({ recentCalls: 1, lastCallAtMs: 500_000 - 120_000 })
      expect(busiest).toMatchObject({ recentCalls: 2, lastCallAtMs: 500_000 - 60_000 })
    })

    /** A Talkgroup quieter than the catalog's activity window carries nothing,
     *  which is not the same as having taken zero Calls in it — the row simply
     *  has no age to show. */
    it('says nothing about a Talkgroup the window did not hear', () => {
      const [quiet] = draw().systems[0].rows

      expect(quiet.recentCalls).toBe(0)
      expect(quiet.lastCallAtMs).toBeUndefined()
    })
  })

  /**
   * Whether anything drawn is measured from a clock — so a parked phone showing
   * a wall of switches and counts is not redrawing itself twice a minute for
   * nothing.
   */
  describe('whether the panel needs a clock', () => {
    it('needs none for a catalog with no ages and no Avoids', () => {
      expect(draw().ticking).toBe(false)
    })

    it('needs one for a last-heard age', () => {
      expect(draw({ catalog: county(3) }).ticking).toBe(true)
    })

    /** Sorted by activity the rows show *counts*, which nothing has to tick to
     *  keep true. */
    it('needs none for the same catalog sorted by activity', () => {
      expect(draw({ catalog: county(3), sort: 'active' }).ticking).toBe(false)
    })

    it.each([
      ['a timed Avoid counts down', 30 * 60_000, true],
      ['an indefinite one has nothing to wait for', 0, false],
    ])('%s', (_what, until, ticking) => {
      const avoided = { [talkgroupKey(100, 1)]: until }

      expect(draw({ selection: silenced(EVERYTHING, avoided), avoided }).ticking).toBe(
        ticking,
      )
    })

    /** A folded System draws no rows, so its ages are nobody's business — the
     *  clock follows what is on screen, not what the catalog holds. */
    it('needs none for ages inside a folded System', () => {
      expect(draw({ catalog: county(400) }).ticking).toBe(false)
      expect(draw({ catalog: county(400), expanded: { 1: true } }).ticking).toBe(true)
    })

    it('needs one for a pinned row’s age, wherever its System is', () => {
      expect(
        draw({ catalog: county(400), pinned: ['1:7'] }).ticking,
      ).toBe(true)
    })
  })
})

describe('what a row says about activity (#57)', () => {
  const MINUTE = 60_000

  it.each([
    ['a Talkgroup mid-transmission', 0, 'now'],
    ['one that stopped seconds ago', 59_000, 'now'],
    ['a clock running ahead of the server’s', -5_000, 'now'],
    ['one minute', MINUTE, '1m'],
    ['under the hour', 59 * MINUTE, '59m'],
    ['the hour', 60 * MINUTE, '1h'],
    ['most of the window', 23 * 60 * MINUTE, '23h'],
  ])('says %s is “%s” ago', (_what, ago, said) => {
    expect(lastHeard(1_000_000_000, 1_000_000_000 - ago)).toBe(said)
  })

  it.each([
    ['the window the server ships', 24 * 60 * MINUTE, '24h'],
    ['an hour', 60 * MINUTE, '1h'],
    ['under an hour', 30 * MINUTE, '30m'],
  ])('names %s as “%s” on the sort control', (_what, ms, said) => {
    expect(windowLabel(ms)).toBe(said)
  })
})

describe('Priority on a row (#58, spec US 27)', () => {
  it('is off for a Talkgroup nobody marked', () => {
    expect(rowsOf(draw()).every((row) => row.priority)).toBe(false)
  })

  it('marks the row the Listener named, and only that one', () => {
    const marked = rowsOf(draw({ priority: ['100:1'] })).filter((row) => row.priority)

    expect(marked.map((row) => row.key)).toEqual(['100:1'])
  })

  /** A **Pin** and a Priority look alike on the row and are different states in
   *  different slices — one arranges the panel, the other decides what plays
   *  next — so a row must be able to carry either without the other. */
  it('is independent of the Pin beside it', () => {
    const [row] = rowsOf(draw({ priority: ['100:1'], pinned: [] })).filter(
      (one) => one.key === '100:1',
    )

    expect(row).toMatchObject({ priority: true, pinned: false })
  })

  /** A Pinned row is the same Talkgroup lifted out of its section, so it has to
   *  read the same — a Priority visible in one copy and not the other would be
   *  the panel disagreeing with itself. */
  it('reads the same on a Pinned copy of the row', () => {
    const panel = draw({ priority: ['100:1'], pinned: ['100:1'] })

    expect(panel.pinned[0]).toMatchObject({ key: '100:1', priority: true })
  })
})
