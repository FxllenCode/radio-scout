/**
 * The Search screen's state, as a URL (#61, spec US 30).
 *
 * Pure: a `URLSearchParams` in and a value out, or the reverse. The screen owns
 * *when* the URL changes; this owns what it means, which is what makes
 * "bookmark this view" and "back goes where I came from" one mechanism rather
 * than two features.
 *
 * # Why this is a module and not four `searchParams.get` calls
 *
 * Because the URL is the **state**, not a copy of it — so what comes back out
 * has to be *identical* to what went in, in the one sense the app compares
 * searches: [`sameSearch`]. A **Run** ends when the search it belongs to
 * changes (#89), so a filter object rebuilt from a link that differed from the
 * one it was built from — a `sort` the URL omitted, an `offset` that crept into
 * the search — would end the Run it had just restored, and look for all the
 * world like playback stopping on its own.
 *
 * Two rules follow, and they are the whole of the design:
 *
 * - **The window is not part of the search.** `offset` is where the *screen*
 *   is, which the Run deliberately does not share ([`Run.from`]). It rides in
 *   the URL beside the search rather than inside it.
 * - **The default is spelled once.** A URL that omits `sort` reads back as
 *   `newest`, and a search that *is* `newest` writes no `sort` — so `/search`
 *   with no query and `/search?sort=newest` are one view with one URL, and
 *   neither is a different Run from the other.
 */
import type { Mark, SearchQuery } from '@/types'
import { MARKS } from '@/types'

import { searchParams } from './archive'
import type { RunSearch } from './run'

/** Everything the Search screen's URL carries. */
export interface SearchUrl {
  /** The filters and the ordering — a **Run**'s identity, exactly. */
  search: RunSearch
  /** Which page of results is on screen, in rows from the start. */
  offset: number
  /** A Call linked to directly (spec US 30), which the screen plays on arrival.
   *  Beside the search rather than in it: the Call is fetched by id and is not
   *  required to be one the filters would have found. */
  call?: number
}

/** What `/search` with no query means: everything, newest first, page one. */
export const DEFAULT_SEARCH_URL: SearchUrl = { search: { sort: 'newest' }, offset: 0 }

/** The ordering a URL that names none is taken to mean. */
const DEFAULT_SORT: SearchQuery['sort'] = 'newest'

/** The numeric filters, all read the same way — a URL is typed by strangers and
 *  edited by hand, and `Number('alpha')` is `NaN`, which reaches the wire as
 *  `after=NaN` and answers with nothing at all. That reads exactly like an
 *  archive that is empty, which is why every one of these is checked
 *  (`lib/persist`'s rule, one layer out). */
const NUMERIC = [
  'after',
  'before',
  'system',
  'talkgroup',
  'minDuration',
  'unit',
] as const

/** The free-text filters, whose only unusable value is a blank one. */
const TEXT = ['group', 'tag'] as const

/** The state a `URLSearchParams` describes, with anything unreadable left out
 *  rather than passed on. Dropping one filter costs the Listener that filter;
 *  refusing the whole URL would cost them the link. */
export function readSearchUrl(params: URLSearchParams): SearchUrl {
  const search: RunSearch = {}

  for (const key of NUMERIC) {
    const value = numberIn(params, key)
    if (value !== undefined) search[key] = value
  }
  for (const key of TEXT) {
    const value = params.get(key)
    if (value) search[key] = value
  }

  const mark = params.get('mark')
  if (isMark(mark)) search.mark = mark

  const sort = params.get('sort')
  search.sort = sort === 'oldest' || sort === 'newest' ? sort : DEFAULT_SORT

  const call = numberIn(params, 'call')
  const offset = numberIn(params, 'offset')

  return {
    search,
    // A page before the first one is the first one — the alternative is a
    // request the archive answers with nothing, on a URL somebody trimmed.
    offset: offset === undefined ? 0 : Math.max(0, Math.trunc(offset)),
    ...(call === undefined ? {} : { call }),
  }
}

/** Everything that reaches the query string: a `SearchQuery`'s own keys plus
 *  the linked Call, which is not one of them. Its own type rather than a cast,
 *  so [`searchParams`] is not handed a lie about what it is serializing. */
interface SearchLinkParams extends SearchQuery {
  call?: number
}

/** This state as a query string — sorted and empties dropped, because
 *  [`searchParams`] is what decides both and a Run's identity is what it
 *  answers. */
export function writeSearchUrl({ search, offset, call }: SearchUrl): string {
  const params: SearchLinkParams = {
    ...search,
    // Written only when it is not what a bare URL already means, so the default
    // view's URL is the bare one.
    sort: search.sort === DEFAULT_SORT ? undefined : search.sort,
    ...(offset > 0 ? { offset } : {}),
    ...(call === undefined ? {} : { call }),
  }
  return searchParams(params)
}

/**
 * A parameter read as a finite number, or `undefined` for absent, blank, and
 * everything that is not one.
 *
 * Exported because the **DVR**'s URL reads its bounds the same way (#63), and
 * the reason it is a function at all is a trap worth having in one place:
 * `Number('alpha')` is `NaN`, which reaches the wire as `after=NaN` and answers
 * with nothing, which reads exactly like an archive that is empty.
 */
export function numberIn(params: URLSearchParams, key: string): number | undefined {
  const raw = params.get(key)
  if (raw === null || raw.trim() === '') return undefined
  const value = Number(raw)
  return Number.isFinite(value) ? value : undefined
}

const isMark = (value: string | null): value is Mark =>
  MARKS.includes(value as Mark)
