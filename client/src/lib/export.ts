/**
 * Taking a range of the archive with you (#65, spec US 33).
 *
 * Pure: filters and a catalog in, a URL and an offer out. The screens own
 * *when* an export is offered; this owns what it means — which is what keeps
 * the Search screen's control and the DVR's the same control rather than two
 * that agree today.
 *
 * # Why the offer is a value
 *
 * The cap rides on `GET /api/catalog` for the reason `sharing` does: a control
 * that is offered and then refused is a control that lies (#64). Here that goes
 * one further, because the refusal has a *number* in it — a Listener has to be
 * told "that range holds 4,312 calls" before they wait for a download that was
 * never going to arrive, not after.
 */
import type { Catalog, SearchQuery } from '@/types'

import { searchParams } from './archive'

/** Which of the two things an export is. */
export type ExportFormat = 'zip' | 'wav'

/** The two, as the dialog offers them. One list, so the Search screen and the
 *  DVR cannot come to disagree about what an export can be. */
export const EXPORT_FORMATS: { id: ExportFormat; label: string; detail: string }[] = [
  {
    id: 'zip',
    label: 'Zip of calls',
    detail: 'Every call as its own file, plus a manifest listing them.',
  },
  {
    id: 'wav',
    label: 'One stitched file',
    detail: 'All of it end to end, oldest first — playable anywhere.',
  },
]

/** Where an export of `search` downloads from.
 *
 *  **The window never rides along.** `offset`/`limit` say where the *screen*
 *  is, and an export is the whole of what matched — the same separation a
 *  **Run** keeps (`lib/run`), for the same reason. Nor does `sort`: the server
 *  overrides it (an incident has one useful order), so sending one would make
 *  two links to the same export look like different exports. */
export function exportUrl(search: SearchQuery, format: ExportFormat): string {
  const { sort: _sort, limit: _limit, offset: _offset, ...filters } = search
  return `/api/calls/export?${searchParams({ ...filters, format } as SearchQuery)}`
}

/** Whether this range can go, and what to say when it cannot. */
export interface ExportOffer {
  offered: boolean
  /** How many Calls the current search matched. */
  count: number
  /** Why not, when it is not offered. */
  why?: string
}

/**
 * What this instance will let a Listener take away, given what they are looking
 * at — or **`null`, meaning there is no control here at all**.
 *
 * The two are genuinely different answers and the caller must not conflate
 * them: `null` is an instance that does not export (or a catalog that has not
 * arrived yet), where an offer that is merely `offered: false` is a control
 * that *is* drawn and says why this particular range cannot go. Returning
 * `null` rather than a third field is what stops the screen guarding the same
 * question twice — the shape #50 reaches for: make it unexpressible rather than
 * check it in two places.
 */
export function offerExport(catalog: Catalog | undefined, count: number): ExportOffer | null {
  // A catalog that has not arrived is not an instance that refuses; either way
  // there is nothing to draw yet.
  if (!catalog?.export.enabled) return null
  if (count === 0) return { offered: false, count, why: 'Nothing here to export.' }
  if (count > catalog.export.maxCalls) {
    return {
      offered: false,
      count,
      why: `That range holds ${count.toLocaleString()} calls; at most ${catalog.export.maxCalls.toLocaleString()} can be exported at once. Narrow the range.`,
    }
  }
  return { offered: true, count }
}
