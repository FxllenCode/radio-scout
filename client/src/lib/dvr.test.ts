import { describe, expect, it } from 'vitest'

import { EVERYTHING, type Selection } from './selection'
import { encodeSelection } from './selectionUrl'
import {
  channelOption,
  channelScope,
  dvrActivity,
  dvrExport,
  dvrLink,
  dvrSearch,
  playheadMs,
  readChannelOption,
  readDvrUrl,
  rescoped,
  soleChannel,
  writeDvrUrl,
  type DvrView,
} from './dvr'

/** A moment with a whole hour in front of it, so the default range is easy to
 *  read in an expectation. */
const NOW = Date.UTC(2026, 6, 25, 12, 0, 0)
const HOUR = 3_600_000

const read = (query: string, fallback: Selection = EVERYTHING, now = NOW) =>
  readDvrUrl(new URLSearchParams(query), fallback, now)

describe('the DVR a URL describes', () => {
  it('falls back to the scanner the Listener arrived with', () => {
    const mine: Selection = { all: false, sel: { 100: { 5: true } } }
    expect(read('', mine).scope).toEqual(mine)
  })

  it('prefers the scope the link names, so a link means one thing at both ends', () => {
    const sent: Selection = { all: false, sel: { 200: { '*': true } } }
    expect(read(`sel=${encodeSelection(sent)}`, EVERYTHING).scope).toEqual(sent)
  })

  it('takes the last hour when the link names no range', () => {
    const view = read('')
    expect(view.from).toBe(NOW - HOUR)
    expect(view.to).toBe(NOW)
  })

  it('anchors at the start of the range unless the link says otherwise', () => {
    expect(read('').at).toBe(NOW - HOUR)
    expect(read(`after=1000&before=9000&at=4000`).at).toBe(4000)
  })

  it.each([
    ['before the range', 'after=1000&before=9000&at=0', 1000],
    ['past the range', 'after=1000&before=9000&at=99999', 9000],
  ])('clamps an anchor %s into it', (_why, query, expected) => {
    expect(read(query).at).toBe(expected)
  })

  it.each([
    ['a range that runs backwards', 'after=9000&before=1000'],
    ['a range of no width at all', 'after=1000&before=1000'],
    ['a bound that is not a number', 'after=alpha&before=9000'],
    ['a scope that is not a Selection', 'sel=nope&after=1000&before=9000'],
  ])('falls back rather than answering with nothing: %s', (_why, query) => {
    const view = read(query)
    expect(view.to).toBeGreaterThan(view.from)
  })

  it('refuses a link whose scope it cannot read, rather than half of it', () => {
    // `lib/selectionUrl`'s rule, one layer out: a scope is one statement, and
    // half a stranger's scanner is not a thing they meant to send.
    const mine: Selection = { all: false, sel: { 100: { 5: true } } }
    expect(read('sel=0_100.nope', mine).scope).toEqual(mine)
  })
})

describe('a DVR as a URL', () => {
  const view: DvrView = {
    scope: { all: false, sel: { 100: { 5: true } } },
    from: 1000,
    to: 9000,
    at: 1000,
  }

  it('round-trips', () => {
    expect(read(writeDvrUrl(view))).toEqual(view)
  })

  it('spells no anchor when the anchor is where the range begins', () => {
    expect(writeDvrUrl(view)).not.toContain('at=')
    expect(writeDvrUrl({ ...view, at: 4000 })).toContain('at=4000')
  })
})

describe('what a DVR asks the archive for', () => {
  const view: DvrView = {
    scope: { all: false, sel: { 100: { 5: true } } },
    from: 1000,
    to: 9000,
    at: 4000,
  }

  it('walks forwards from the anchor, never from the start of the range', () => {
    // A DVR is a **Run** configured differently (CONTEXT.md), and scrubbing is
    // re-anchoring it — so the anchor is the search's own lower bound.
    expect(dvrSearch(view)).toEqual({
      sel: '0_100.5',
      after: 4000,
      before: 9000,
      sort: 'oldest',
    })
  })

  it('draws the whole range whatever the anchor, or the timeline would shrink under the thumb', () => {
    expect(dvrActivity(view)).toMatchObject({ after: 1000, before: 9000, sel: '0_100.5' })
  })

  /** An export is the range the Listener chose, not where they happen to be
   *  listening — or it would silently shrink every time somebody scrubbed
   *  forward before downloading (#65, spec US 33). */
  it('exports the whole range, not from the playhead', () => {
    expect(dvrExport(view)).toEqual({ sel: '0_100.5', after: 1000, before: 9000 })
  })
})

describe('one channel as a scope', () => {
  it('is a Selection with one entry, so the DVR has one scoping rule', () => {
    expect(channelScope(100, 5)).toEqual({ all: false, sel: { 100: { 5: true } } })
    expect(soleChannel(channelScope(100, 5))).toEqual({ systemRef: 100, talkgroupRef: 5 })
  })

  it.each<[string, Selection]>([
    ['everything', EVERYTHING],
    ['nothing', { all: false, sel: {} }],
    ['a whole System', { all: false, sel: { 100: { '*': true } } }],
    ['two channels', { all: false, sel: { 100: { 5: true, 6: true } } }],
    ['two Systems', { all: false, sel: { 100: { 5: true }, 200: { 6: true } } }],
    ['a channel switched off', { all: true, sel: { 100: { 5: false } } }],
  ])('is not what %s is', (_what, selection) => {
    expect(soleChannel(selection)).toBeUndefined()
  })
})

describe('where the playhead is on the timeline', () => {
  const view: DvrView = { scope: EVERYTHING, from: 1000, to: 9000, at: 4000 }

  it('is the playing Call plus how far into it the element has got', () => {
    expect(playheadMs(view, { timestamp: 5000 }, 1.5)).toBe(6500)
  })

  it('is the anchor while nothing is playing, so the marker never leaves the range', () => {
    expect(playheadMs(view, null, 0)).toBe(4000)
    expect(playheadMs(view, {}, 3)).toBe(4000)
  })
})

describe('opening a search in the DVR', () => {
  it('carries the channel the search was pinned to, as the Selection it is', () => {
    expect(dvrLink({ system: 100, talkgroup: 5, sort: 'newest' })).toBe(
      new URLSearchParams({ sel: '0_100.5' }).toString(),
    )
  })

  it('carries a Selection the search already had', () => {
    expect(dvrLink({ sel: '1_100.-5' })).toBe(
      new URLSearchParams({ sel: '1_100.-5' }).toString(),
    )
  })

  it.each([
    ['a System with no channel', { system: 100 }],
    ['a channel with no System — a Ref means nothing on its own', { talkgroup: 5 }],
    ['nothing at all', {}],
  ])('names no scope for %s, so the DVR opens on the Listener own scanner', (_why, search) => {
    expect(dvrLink(search)).not.toContain('sel=')
  })

  it('carries a range the search named', () => {
    const params = new URLSearchParams(dvrLink({ after: 1000, before: 9000 }))
    expect(params.get('after')).toBe('1000')
    expect(params.get('before')).toBe('9000')
  })

  it.each([
    ['only one end', { after: 1000 }],
    ['a range that runs backwards', { after: 9000, before: 1000 }],
  ])('names no range for %s, so the DVR picks its own', (_why, search) => {
    expect(dvrLink(search)).not.toContain('after=')
  })

  it('never carries the filters a DVR cannot honour', () => {
    // A DVR's scope is a Selection and nothing else — a tag, a mark or a
    // duration filter has no spelling here, and carrying one silently would
    // make a link that says less than the screen it came from.
    expect(dvrLink({ tag: 'Fire', mark: 'emergency', minDuration: 3 })).toBe('')
  })
})

describe('changing the scope or the range', () => {
  const view: DvrView = {
    scope: { all: false, sel: { 100: { 5: true } } },
    from: 1000,
    to: 9000,
    at: 4000,
  }

  it('re-anchors at the start of the new range', () => {
    // The old anchor described a stretch of time that may no longer be in it.
    expect(rescoped(view, { from: 2000 })).toMatchObject({ from: 2000, at: 2000 })
  })

  it.each([
    ['a lower bound dragged past the upper', { from: 99_999 }],
    ['an upper bound dragged below the lower', { to: 0 }],
    ['both ends on the same instant', { from: 5000, to: 5000 }],
  ])('keeps the range it has rather than taking one with no width: %s', (_why, patch) => {
    // A `datetime-local` emits every intermediate year as it is retyped
    // (`0002`, `0020`, `0202`, `2027`), so a range momentarily running
    // backwards is the ordinary case and not a mistake. Taking it would throw
    // the Listener's whole range away and rewrite both boxes under the cursor.
    expect(rescoped(view, patch)).toEqual(view)
  })

  it('takes a range that is merely different', () => {
    expect(rescoped(view, { from: 2000, to: 3000 })).toEqual({
      ...view,
      from: 2000,
      to: 3000,
      at: 2000,
    })
  })
})

describe('a channel as a picker option', () => {
  it('round-trips, so the spelling lives in one place', () => {
    expect(readChannelOption(channelOption({ systemRef: 100, talkgroupRef: 5 }))).toEqual({
      systemRef: 100,
      talkgroupRef: 5,
    })
  })

  it.each(['', 'shared', '100', 'alpha:5', '100:alpha'])(
    'reads %j as no channel at all',
    (value) => {
      expect(readChannelOption(value)).toBeUndefined()
    },
  )
})
