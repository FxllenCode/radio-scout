import { describe, expect, it } from 'vitest'

import { searchCall, searchPage } from '@/test/handlers'
import type { Call } from '@/types'

import {
  advance,
  runView,
  sameSearch,
  type Run,
  type RunEvent,
  type RunSearch,
} from './run'

const call = searchCall
const page = (partial: Parameters<typeof searchPage>[0] = {}) =>
  searchPage({ results: [call(1), call(2), call(3)], limit: 50, ...partial })

const NEWEST: RunSearch = { sort: 'newest' }

/** Start a Run on `page`, then feed it the rest of the events in order. */
function walk(
  started: Parameters<typeof advance>[1],
  ...events: Parameters<typeof advance>[1][]
): Run | null {
  return events.reduce(
    (run, event) => advance(run, event),
    advance(null, started),
  )
}

/**
 * A Run's identity (#89).
 *
 * The thing this replaced was `JSON.stringify(pageQuery)`, whose own comment
 * admitted it was key-*order* sensitive and "fails silently" — so these are the
 * cases that would have broken it.
 */
describe('the search a Run belongs to', () => {
  it('is the same search however the filter object was built', () => {
    // #61 round-trips search state through the URL on every navigation, which
    // rebuilds the object and reorders its keys. Same search, either way round.
    expect(
      sameSearch(
        { sort: 'newest', system: 100, talkgroup: 54_241 },
        { talkgroup: 54_241, system: 100, sort: 'newest' },
      ),
    ).toBe(true)
  })

  /** A reset control clears a filter to `undefined` rather than deleting the
   *  key — which is what `updateFilters` already does today for every "Any …"
   *  option. An unset filter is no filter. */
  it('is the same search whether a cleared filter is absent or undefined', () => {
    expect(
      sameSearch({ sort: 'newest', system: undefined }, { sort: 'newest' }),
    ).toBe(true)
    expect(sameSearch({ sort: 'newest', tag: '' }, { sort: 'newest' })).toBe(true)
  })

  it.each([
    ['a filter added', { sort: 'newest' }, { sort: 'newest', system: 100 }],
    [
      'a filter changed',
      { sort: 'newest', system: 100 },
      { sort: 'newest', system: 200 },
    ],
    ['the ordering flipped', { sort: 'newest' }, { sort: 'oldest' }],
  ] as [string, RunSearch, RunSearch][])(
    'is a different search after %s',
    (_what, before, after) => {
      expect(sameSearch(before, after)).toBe(false)
    },
  )
})

describe('starting a Run', () => {
  it('plays the chosen Call and knows what follows it', () => {
    const run = walk({
      type: 'started',
      search: NEWEST,
      page: page(),
      index: 1,
    })

    const view = runView(run)
    expect(view.current).toEqual(call(2))
    expect(view.next).toEqual(call(3))
    expect(view.hasPrevious).toBe(true)
    expect(view.hasNext).toBe(true)
  })

  /** A selection the page cannot answer — a row index from a page that has
   *  since been replaced, or none at all. Changing nothing beats stopping
   *  whatever was already playing. */
  it.each([-1, 3, 99])('ignores a selection at index %i', (index) => {
    expect(advance(null, { type: 'started', search: NEWEST, page: page(), index }))
      .toBeNull()
  })

  /** Spec US 25: a Run walks the whole filtered set, so its position counts
   *  against the Archive rather than against the page it happens to hold. */
  it('counts its position against the whole filtered set', () => {
    const run = walk({
      type: 'started',
      search: NEWEST,
      page: page({ offset: 50, count: 421, hasMore: true }),
      index: 1,
    })

    expect(runView(run).position).toEqual({ index: 51, total: 421 })
  })
})

/** The transition table criterion 5 asks for: a Run started at a known Call, a
 *  sequence of gestures, and the Call that is playing afterwards. */
describe('walking a Run', () => {
  const started: RunEvent = {
    type: 'started',
    search: NEWEST,
    page: page(),
    index: 1,
  }

  it.each([
    { gestures: ['advanced'], playing: 3 },
    { gestures: ['back'], playing: 1 },
    { gestures: ['back', 'back'], playing: 1, why: 'holds at the first' },
    { gestures: ['advanced', 'back'], playing: 2 },
    { gestures: ['back', 'advanced', 'advanced'], playing: 3 },
  ] as { gestures: RunEvent['type'][]; playing: number; why?: string }[])(
    'after $gestures is playing call $playing',
    ({ gestures, playing }) => {
      const run = walk(started, ...gestures.map((type) => ({ type }) as RunEvent))

      expect(runView(run).current).toEqual(call(playing))
    },
  )

  it('ignores both gestures while nothing is playing', () => {
    expect(advance(null, { type: 'advanced' })).toBeNull()
    expect(advance(null, { type: 'back' })).toBeNull()
  })
})

/** The Run's third answer: which page of the Archive has to be on hand. One
 *  field for what used to be two separate mechanisms — #32's page-ahead and US
 *  25's roll-on — because they always wanted the same page. */
describe('the page that must be on hand', () => {
  /** A page with room either side of the page-ahead threshold. */
  const fifty = page({
    results: Array.from({ length: 50 }, (_, at) => call(at + 1)),
    count: 120,
    hasMore: true,
  })

  it('is the page after the Run’s own, once the lead runs short', () => {
    const run = walk({
      type: 'started',
      search: { sort: 'newest', system: 100 },
      page: fifty,
      index: 48,
    })

    expect(runView(run).wanted).toEqual({
      sort: 'newest',
      system: 100,
      limit: 50,
      offset: 50,
    })
  })

  it('is nothing while the lead is long enough to be worth nobody’s bandwidth', () => {
    const run = walk({
      type: 'started',
      search: NEWEST,
      page: fifty,
      index: 46,
    })

    expect(runView(run).wanted).toBeNull()
  })

  it('is nothing when there is no page beyond this one', () => {
    const run = walk({ type: 'started', search: NEWEST, page: page(), index: 0 })

    expect(runView(run).wanted).toBeNull()
  })

  it('is nothing at all when no Run is walking', () => {
    expect(runView(null).wanted).toBeNull()
  })
})

describe('running off the end of the loaded page', () => {
  it('waits for the next page rather than stopping', () => {
    const run = walk(
      {
        type: 'started',
        search: NEWEST,
        page: page({ count: 120, limit: 3, hasMore: true }),
        index: 2,
      },
      { type: 'advanced' },
    )

    const view = runView(run)
    // Nothing is playing during the gap, but the Run has not ended: it is
    // waiting for the page it named, which US 25's sequential playback needs.
    expect(view.current).toBeNull()
    expect(run).not.toBeNull()
    expect(view.wanted).toEqual({ sort: 'newest', limit: 3, offset: 3 })
  })

  it('ends when the Archive has nothing more to give', () => {
    const run = walk(
      { type: 'started', search: NEWEST, page: page(), index: 2 },
      { type: 'advanced' },
    )

    expect(run).toBeNull()
  })
})

describe('rolling onto the next page', () => {
  /** Three Calls, three more behind them. */
  const first = page({ count: 6, limit: 3, hasMore: true })
  const second = page({
    results: [call(4), call(5), call(6)],
    count: 6,
    limit: 3,
    offset: 3,
    hasMore: false,
  })
  const rolled: [RunEvent, RunEvent] = [
    { type: 'started', search: NEWEST, page: first, index: 2 },
    { type: 'advanced' },
  ]

  it('picks up at the top of the page it asked for, still counting against the Archive', () => {
    const run = walk(...rolled, { type: 'paged', window: { ...NEWEST, limit: 3, offset: 3 }, page: second })

    const view = runView(run)
    expect(view.current).toEqual(call(4))
    expect(view.position).toEqual({ index: 3, total: 6 })
    // The page it was waiting for arrived, so it wants nothing further.
    expect(view.wanted).toBeNull()
  })

  it('keeps walking straight through the boundary', () => {
    const run = walk(
      ...rolled,
      { type: 'paged', window: { ...NEWEST, limit: 3, offset: 3 }, page: second },
      { type: 'advanced' },
      { type: 'advanced' },
    )

    expect(runView(run).current).toEqual(call(6))
  })

  /** The page-ahead lands *before* the boundary is reached — that is the point
   *  of it. Taking it then would throw away the Call still playing. */
  it('ignores a page that arrives while it is still walking this one', () => {
    const run = walk(
      { type: 'started', search: NEWEST, page: first, index: 0 },
      { type: 'paged', window: { ...NEWEST, limit: 3, offset: 3 }, page: second },
    )

    expect(runView(run).current).toEqual(call(1))
  })

  /**
   * The page a Run is handed may belong to a *different search* — RTK Query
   * keeps its `data` across `skipToken`, so the page-ahead of the search a
   * Listener just left is still in hand when a Run on the new one starts. An
   * offset alone cannot tell the two apart: both searches have a page at 3.
   *
   * Getting this wrong is exactly what criterion 1 exists to prevent — the Run
   * would carry straight on playing Calls from the search that ended.
   */
  it('ignores a page of a search it is not walking, at the offset it wants', () => {
    const otherSearch = page({
      results: [call(9)],
      count: 6,
      limit: 3,
      offset: 3,
      hasMore: false,
    })
    const run = walk(...rolled, {
      type: 'paged',
      window: { sort: 'oldest', limit: 3, offset: 3 },
      page: otherSearch,
    })

    expect(runView(run).current).toBeNull()
    expect(runView(run).wanted).toEqual({ sort: 'newest', limit: 3, offset: 3 })
  })

  /** Two pages can be in flight at once across a filter change. A Run takes
   *  only the page it named. */
  it('ignores a page it never asked for', () => {
    const elsewhere = page({
      results: [call(9)],
      count: 6,
      limit: 3,
      offset: 300,
      hasMore: true,
    })
    const run = walk(...rolled, { type: 'paged', window: { ...NEWEST, limit: 3, offset: 300 }, page: elsewhere })

    expect(runView(run).current).toBeNull()
    // Still waiting for the one it did ask for.
    expect(runView(run).wanted).toEqual({ sort: 'newest', limit: 3, offset: 3 })
  })

  /** Every Call on the page is encrypted, so there is nothing on it to play
   *  (#42, spec US 9). Stopping there would strand a Run that has pages left. */
  it('rolls straight past a page with nothing playable on it', () => {
    const encryptedPage = page({
      results: [{ ...call(4), audioUrl: undefined, encrypted: true }],
      count: 9,
      limit: 3,
      offset: 3,
      hasMore: true,
    })
    const run = walk(...rolled, { type: 'paged', window: { ...NEWEST, limit: 3, offset: 3 }, page: encryptedPage })

    expect(runView(run).current).toBeNull()
    expect(runView(run).wanted).toEqual({ sort: 'newest', limit: 3, offset: 6 })
  })

  it('ends on an unplayable last page rather than waiting forever', () => {
    const encryptedLast = page({
      results: [{ ...call(4), audioUrl: undefined, encrypted: true }],
      count: 6,
      limit: 3,
      offset: 3,
      hasMore: false,
    })

    expect(walk(...rolled, { type: 'paged', window: { ...NEWEST, limit: 3, offset: 3 }, page: encryptedLast })).toBeNull()
  })
})

/** With the live feed still running, an archived Call *interrupts* it (spec US
 *  26): one Call, no set to walk, and the live feed's own listening queue
 *  untouched behind it. */
describe('interrupting the live feed', () => {
  const interrupted: RunEvent = {
    type: 'started',
    search: NEWEST,
    page: page({ count: 120, hasMore: true }),
    index: 1,
    interrupting: true,
  }

  it('walks one Call and nothing else', () => {
    const view = runView(walk(interrupted))

    expect(view.current).toEqual(call(2))
    expect(view.interrupting).toBe(true)
    expect(view.hasNext).toBe(false)
    expect(view.hasPrevious).toBe(false)
    expect(view.next).toBeNull()
  })

  /** However short the loaded page looks, there is no run to page through — so
   *  a page-ahead would be a request for nothing. */
  it('never asks for a page', () => {
    expect(runView(walk(interrupted)).wanted).toBeNull()
  })

  it('hands back to the live feed when it ends', () => {
    expect(walk(interrupted, { type: 'advanced' })).toBeNull()
  })

  /** Nothing precedes an interruption, so "previous" has nothing to mean — it
   *  must not silently double as "back to the live feed". */
  it('does nothing on a step back', () => {
    expect(walk(interrupted, { type: 'back' })).toEqual(walk(interrupted))
  })
})

/**
 * A search that changes ends the Run rather than silently walking the wrong
 * results (CONTEXT.md) — but it ends it *at the boundary*, not mid-Call. A
 * Listener who tweaks a dropdown while listening keeps hearing the Call they
 * are on and the rest of the page it belongs to; what stops is rolling onto a
 * page of a search they have not even seen the first page of.
 */
describe('the search changing under a Run', () => {
  const three = page({ count: 120, limit: 3, hasMore: true })
  const started: RunEvent = {
    type: 'started',
    search: NEWEST,
    page: three,
    index: 1,
  }
  const changed: RunEvent = {
    type: 'searched',
    search: { sort: 'newest', tag: 'Fire Dispatch' },
  }

  it('keeps playing what it holds, and stops asking for pages', () => {
    const run = walk(started, changed)

    expect(runView(run).current).toEqual(call(2))
    expect(runView(run).wanted).toBeNull()
  })

  /** The bug this replaces: the old roll-on read `hasMore` off the page now on
   *  screen, so a Run that exhausted after a filter change resumed at the top
   *  of the *new* search's page two — skipping its page one entirely. */
  it('ends at the boundary instead of jumping into the new search', () => {
    // The rest of the loaded page still plays; it is the page *after* it the
    // Run no longer has any business walking.
    const run = walk(started, changed, { type: 'advanced' })
    expect(runView(run).current).toEqual(call(3))

    expect(advance(run, { type: 'advanced' })).toBeNull()
  })

  it('ends outright when the change lands during a roll-on, with nothing playing', () => {
    const rolling = walk(
      { type: 'started', search: NEWEST, page: three, index: 2 },
      { type: 'advanced' },
    )

    expect(advance(rolling, changed)).toBeNull()
  })

  /** The identity is order-independent, so a filter cleared back to what the
   *  Run is walking re-arms it — the case a serialized key got wrong. */
  it('re-arms when the search comes back to the one it is walking', () => {
    const run = walk(started, changed, {
      type: 'searched',
      search: { tag: undefined, sort: 'newest' },
    })

    expect(runView(run).wanted).toEqual({ sort: 'newest', limit: 3, offset: 3 })
  })

  /** The Listener retyped the same search, or a rebuilt filter object arrived
   *  from the URL (#61). A Run mid roll-on must not be knocked off the page it
   *  is waiting for by news that nothing changed. */
  it('leaves a Run waiting at a boundary waiting, when nothing actually changed', () => {
    const rolling = walk(
      { type: 'started', search: NEWEST, page: three, index: 2 },
      { type: 'advanced' },
    )

    const same = advance(rolling, { type: 'searched', search: { sort: 'newest' } })

    expect(runView(same).wanted).toEqual({ sort: 'newest', limit: 3, offset: 3 })
    expect(
      advance(same, {
        type: 'paged',
        window: { ...NEWEST, limit: 3, offset: 3 },
        page: page({ results: [call(4)], offset: 3, limit: 3, count: 120, hasMore: true }),
      }),
    ).toMatchObject({ index: 0, rolling: false })
  })

  it('is nothing to a Run that is not there', () => {
    expect(advance(null, changed)).toBeNull()
  })
})

describe('standing a Run down', () => {
  it.each([
    ['while it is playing', [{ type: 'advanced' }]],
    ['while it is waiting for a page', [{ type: 'advanced' }, { type: 'advanced' }]],
  ] as [string, RunEvent[]][])('ends it %s', (_when, before) => {
    const run = walk(
      {
        type: 'started',
        search: NEWEST,
        page: page({ count: 120, limit: 3, hasMore: true }),
        index: 1,
      },
      ...before,
    )

    expect(advance(run, { type: 'stopped' })).toBeNull()
  })
})

/**
 * Criterion 3: **ordering is a parameter**. Walking search results newest-first
 * and playing forward in time from a row are the same Run configured
 * differently — which is what lets #63's DVR be a configuration rather than a
 * rewrite. Nothing here knows which way time runs; the ordering rides in the
 * search, so it rides into every page the Run goes on to ask for.
 */
describe('ordering', () => {
  const oldestFromHere: RunSearch = { sort: 'oldest', after: 1_700_000_000_000 }

  it('rolls onto the next page of the ordering it started in', () => {
    const run = walk({
      type: 'started',
      search: oldestFromHere,
      page: page({ count: 120, limit: 3, hasMore: true }),
      index: 2,
    })

    expect(runView(run).wanted).toEqual({
      sort: 'oldest',
      after: 1_700_000_000_000,
      limit: 3,
      offset: 3,
    })
  })

  it('walks oldest-first exactly as it walks newest-first', () => {
    const forward = walk(
      {
        type: 'started',
        search: oldestFromHere,
        page: page({ count: 3 }),
        index: 0,
      },
      { type: 'advanced' },
    )

    expect(runView(forward).current).toEqual(call(2))
    expect(runView(forward).position).toEqual({ index: 1, total: 3 })
  })

  it('is part of the identity, so flipping it disarms the Run', () => {
    const run = walk(
      {
        type: 'started',
        search: { sort: 'newest' },
        page: page({ count: 120, limit: 3, hasMore: true }),
        index: 1,
      },
      { type: 'searched', search: { sort: 'oldest' } },
    )

    expect(runView(run).wanted).toBeNull()
  })
})

/**
 * An encrypted Call is metadata and nothing else (#42, spec US 9) — the Archive
 * sends no `audioUrl` for one. Left in a Run, the audio element would get
 * `src={undefined}` and never fire `ended`, so the Run would stop dead on the
 * first one and look like a bug in the player.
 */
describe('Calls there is nothing to play', () => {
  const encrypted = (id: number): Call => ({
    ...call(id),
    audioUrl: undefined,
    encrypted: true,
  })
  const mixed = page({ results: [call(1), encrypted(2), call(3)] })

  it('walks straight past one, forwards', () => {
    const run = walk(
      { type: 'started', search: NEWEST, page: mixed, index: 0 },
      { type: 'advanced' },
    )

    expect(runView(run).current).toEqual(call(3))
  })

  it('walks straight past one, backwards', () => {
    const run = walk(
      { type: 'started', search: NEWEST, page: mixed, index: 2 },
      { type: 'back' },
    )

    expect(runView(run).current).toEqual(call(1))
  })

  /** Nothing offers this — an Archive row with no audio has no play button —
   *  but a Run that *began* on one would be stuck before it started. */
  it('refuses to start on one', () => {
    const run = walk({
      type: 'started',
      search: NEWEST,
      page: page({ results: [call(1), encrypted(2)] }),
      index: 1,
    })

    expect(run).toBeNull()
  })

  /** `total` stays the Archive's — every Call the search matched, audible or
   *  not — while the index counts the Calls the Run can actually walk. So on an
   *  Archive holding encrypted Calls the readout runs slightly behind the row
   *  numbers, which is what it has always done and is left alone here: #89 is a
   *  deepening, and the alternative is carrying each Call's true row through
   *  every page for a cosmetic count. */
  it('counts the position over the Calls it can walk', () => {
    const run = walk({
      type: 'started',
      search: NEWEST,
      page: page({ results: [call(1), encrypted(2), call(3)], count: 3 }),
      index: 2,
    })

    expect(runView(run).position).toEqual({ index: 1, total: 3 })
  })
})

/**
 * The fourth gesture in — and the Run's answer to it is *nothing*, which is
 * only expressible because the two offsets are named apart (criterion 4).
 *
 * The Listener browsing ahead with the paging buttons moves the window on
 * screen; the Run keeps walking its own. What this replaced conflated them, so
 * paging by hand had to stand the page-ahead down altogether — a run playing
 * page one lost its boundary warm the moment the Listener glanced at page two.
 */
describe('the Listener paging the screen by hand', () => {
  it('leaves the Run walking its own window', () => {
    const run = walk({
      type: 'started',
      search: NEWEST,
      page: page({
        results: Array.from({ length: 50 }, (_, at) => call(at + 1)),
        count: 120,
        hasMore: true,
      }),
      index: 48,
    })

    // The screen is now on offset 50, 100, wherever — the Run never hears about
    // it, and still names the page after its *own*.
    expect(runView(run).wanted).toEqual({
      sort: 'newest',
      limit: 50,
      offset: 50,
    })
  })
})
