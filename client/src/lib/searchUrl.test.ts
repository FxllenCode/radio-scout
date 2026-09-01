import { describe, expect, it } from 'vitest'

import { sameSearch, type RunSearch } from './run'
import { DEFAULT_SEARCH_URL, readSearchUrl, writeSearchUrl, type SearchUrl } from './searchUrl'

const read = (query: string) => readSearchUrl(new URLSearchParams(query))

describe('reading a search off the URL', () => {
  it('is the default view when the URL says nothing', () => {
    expect(read('')).toEqual(DEFAULT_SEARCH_URL)
  })

  it('reads every filter the archive takes', () => {
    expect(
      read(
        'after=1000&before=2000&system=100&talkgroup=54241&group=Public' +
          '&tag=Fire&minDuration=3&unit=1200&mark=emergency&sort=oldest',
      ).search,
    ).toEqual({
      after: 1000,
      before: 2000,
      system: 100,
      talkgroup: 54241,
      group: 'Public',
      tag: 'Fire',
      minDuration: 3,
      unit: 1200,
      mark: 'emergency',
      sort: 'oldest',
    })
  })

  it('reads the page as the window on screen, not as part of the search', () => {
    const { search, offset } = read('offset=100&system=100')
    expect(offset).toBe(100)
    expect(search).not.toHaveProperty('offset')
  })

  it('reads a deep-linked Call beside the search it was found in', () => {
    const { call, search } = read('call=42&tag=Fire')
    expect(call).toBe(42)
    expect(search.tag).toBe('Fire')
  })
})

/**
 * A URL is typed by strangers and edited by hand, so everything read out of one
 * is checked the way `lib/persist` checks local storage — the difference being
 * that a bad *filter* would reach the wire as `after=NaN` and answer with
 * nothing at all, which reads exactly like an archive that is empty.
 */
describe('a URL that says something unusable', () => {
  it.each([
    ['a filter that is not a number', 'system=alpha', 'system'],
    ['an empty value', 'talkgroup=', 'talkgroup'],
    ['a number that is not finite', 'after=Infinity', 'after'],
    ['a mark this build has never heard of', 'mark=telepathy', 'mark'],
    ['a sort that is neither', 'sort=sideways', 'sort'],
  ])('drops %s', (_what, query) => {
    // Dropping every filter leaves the default view, which is what a URL saying
    // nothing usable ought to mean.
    expect(read(query)).toEqual(DEFAULT_SEARCH_URL)
  })

  it('keeps the filters beside the one it could not read', () => {
    expect(read('system=alpha&tag=Fire').search.tag).toBe('Fire')
  })

  it('reads a negative page as the first one', () => {
    expect(read('offset=-50').offset).toBe(0)
  })
})

describe('writing a search into the URL', () => {
  it('sorts its keys, so the same search is always the same URL', () => {
    expect(
      writeSearchUrl({ search: { tag: 'Fire', system: 100, sort: 'oldest' }, offset: 0 }),
    ).toBe('sort=oldest&system=100&tag=Fire')
  })

  it('says nothing for the default view, so /search carries no query', () => {
    expect(writeSearchUrl(DEFAULT_SEARCH_URL)).toBe('')
  })

  it('drops a filter that was cleared', () => {
    expect(
      writeSearchUrl({ search: { tag: 'Fire', group: undefined }, offset: 0 }),
    ).toBe('tag=Fire')
  })

  it('carries the page and the deep-linked Call', () => {
    expect(writeSearchUrl({ search: {}, offset: 50, call: 42 })).toBe(
      'call=42&offset=50',
    )
  })
})

/**
 * The round trip is what makes the URL the *state* rather than a copy of it: a
 * search rebuilt from a link has to be the search that was linked, and — since
 * a **Run**'s identity is its search compared structurally (#89) — has to be
 * the *same Run*, or reloading a page would silently end the walk it restored.
 */
describe('the round trip', () => {
  const CASES: SearchUrl[] = [
    DEFAULT_SEARCH_URL,
    { search: { sort: 'oldest' }, offset: 0 },
    {
      search: { after: 1_756_732_000_000, before: 1_756_736_000_000, sort: 'newest' },
      offset: 0,
    },
    { search: { system: 100, talkgroup: 54241, sort: 'newest' }, offset: 150 },
    { search: { group: 'Law & Order', tag: 'Fire Dispatch', sort: 'newest' }, offset: 0 },
    {
      search: { minDuration: 15, unit: 1200, mark: 'tone', sort: 'oldest' },
      offset: 50,
      call: 42,
    },
  ]

  /** Every case above spells its `sort`, because a read supplies the default —
   *  which is the point: `/search`, `?sort=newest` and a filter object that
   *  never named one are one view, one URL and one **Run**. */
  it('reads the default ordering back onto a search that named none', () => {
    expect(read(writeSearchUrl({ search: { tag: 'Fire' }, offset: 0 })).search).toEqual({
      tag: 'Fire',
      sort: 'newest',
    })
  })

  it.each(CASES)('survives being written and read back: %j', (state) => {
    expect(readSearchUrl(new URLSearchParams(writeSearchUrl(state)))).toEqual(state)
  })

  it.each(CASES)('comes back as the same Run: %j', (state) => {
    const back = readSearchUrl(new URLSearchParams(writeSearchUrl(state))).search
    expect(sameSearch(state.search as RunSearch, back)).toBe(true)
  })
})
