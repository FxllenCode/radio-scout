import { describe, expect, it } from 'vitest'

import { EXPORT_FORMATS, exportUrl, offerExport } from './export'
import type { Catalog } from '@/types'

const catalog = (over: Partial<Catalog['export']> = {}): Catalog => ({
  systems: [],
  activityWindowMs: 0,
  sharing: true,
  export: { enabled: true, maxCalls: 1000, ...over },
})

describe('exportUrl', () => {
  it('carries the search the listener is looking at', () => {
    const url = exportUrl({ talkgroup: 7, after: 1000, sort: 'newest' }, 'zip')

    expect(url).toBe('/api/calls/export?after=1000&format=zip&talkgroup=7')
  })

  /** The window is where the *screen* is, and an export is the whole of what
   *  matched — so neither offset nor limit may reach the URL, or a Listener
   *  would silently get page two of their own incident. */
  it('never carries the window the screen happens to be on', () => {
    const url = exportUrl({ talkgroup: 7, limit: 50, offset: 300 }, 'wav')

    expect(url).not.toContain('offset')
    expect(url).not.toContain('limit')
  })

  /** Ordering is the server's to override, so sending one would be sending
   *  something that cannot be honoured — and it would make two links to the
   *  same export look different. */
  it('never carries an ordering', () => {
    expect(exportUrl({ sort: 'oldest' }, 'zip')).toBe('/api/calls/export?format=zip')
  })

  it('carries a Selection, which is how the DVR exports its scope', () => {
    const url = exportUrl({ sel: '0_100.1', after: 5 }, 'wav')

    expect(url).toBe('/api/calls/export?after=5&format=wav&sel=0_100.1')
  })

  it('offers a zip and a stitched file, and nothing else', () => {
    expect(EXPORT_FORMATS.map((format) => format.id)).toEqual(['zip', 'wav'])
  })
})

describe('offerExport', () => {
  it('offers the control when the range fits', () => {
    expect(offerExport(catalog(), 412)).toEqual({ offered: true, count: 412 })
  })

  /** The whole reason the cap rides on the catalog: a Listener is told the
   *  number *before* they wait for a download that was never going to arrive
   *  (#64's rule — a control that is offered and then refused is a control that
   *  lies). */
  it('refuses a range over the cap, with both numbers', () => {
    const offer = offerExport(catalog({ maxCalls: 100 }), 4312)

    expect(offer?.offered).toBe(false)
    expect(offer?.why).toContain('4,312')
    expect(offer?.why).toContain('100')
  })

  it('refuses an empty range rather than offering an empty file', () => {
    expect(offerExport(catalog(), 0)).toEqual({
      offered: false,
      count: 0,
      why: 'Nothing here to export.',
    })
  })

  /** An Operator who turned exporting off turned the control off too, rather
   *  than leaving one that answers 404. `null` and `offered: false` are
   *  different answers: one draws nothing, the other draws a control that says
   *  why this range cannot go. */
  it('is not drawn at all where the instance does not export', () => {
    expect(offerExport(catalog({ enabled: false }), 12)).toBeNull()
  })

  /** A catalog that has not arrived is not an instance that refuses: the
   *  control waits rather than telling the Listener something untrue. */
  it('waits for a catalog rather than guessing', () => {
    expect(offerExport(undefined, 12)).toBeNull()
  })
})
