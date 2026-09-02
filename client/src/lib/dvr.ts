/**
 * The **DVR** (#63, spec US 39): one talkgroup or a **Selection**, a range of
 * time, and where in it the Listener is.
 *
 * Pure — a `URLSearchParams` in and a value out, or the reverse, plus the two
 * searches that value implies. The screen owns *when* any of it changes; this
 * owns what it means.
 *
 * # A DVR is a Run configured differently, and the difference is the anchor
 *
 * CONTEXT.md says so outright, so there is no second kind of playback here:
 * [`dvrSearch`] is an ordinary `RunSearch`, oldest-first, and everything about
 * walking it — the page-ahead, the roll-on, what plays next — is `lib/run`'s
 * already.
 *
 * What a DVR adds is that **its position is a time**. The Search screen's
 * ribbon scrubs by moving a window of *results* (#62), because that is what a
 * page of search results has; a DVR scrubs by moving where the Run *starts*,
 * because "rewind the county to 2am" names an instant and not a row number.
 * So the anchor is the search's own lower bound and a scrub mints a new Run,
 * where the same gesture on the Search screen deliberately leaves the Run
 * alone.
 *
 * # One scoping rule
 *
 * The scope is always a Selection, including when the Listener picked a single
 * channel — [`channelScope`] mints a one-entry matrix rather than reaching for
 * `?talkgroup=`. The server filters it with [`Selection::reaches_channels`]'s
 * own question (`src/archive.rs`), which sees **Patch**es where the archive's
 * `talkgroup` filter compares the canonical channel and stops. Two rules would
 * mean a DVR of one channel and a DVR of a Selection holding only that channel
 * answering differently, which is exactly the sort of difference nobody finds
 * until a patch is running.
 */
import type { Selection, TalkgroupKey } from './selection'
import { WILDCARD } from './selection'
import { decodeSelection, encodeSelection } from './selectionUrl'
import { rangeOf } from './dateRange'
import type { ActivityQuery } from '@/types'
import type { RunSearch } from './run'

/** How many bars a DVR timeline asks for — `RIBBON_BUCKETS`' reasoning, and
 *  deliberately its own constant: this one spans a range the Listener chose
 *  rather than whatever the archive happens to hold. */
export const TIMELINE_BUCKETS = 120

/** The range a DVR opens on when a link names none. The narrowest preset, for
 *  `PRESETS`' reason: "what was that just now" is the question asked most. */
const DEFAULT_RANGE = 'last-hour' as const

/** Everything a DVR's URL carries. */
export interface DvrView {
  /** Which channels this DVR reaches. Always a matrix, never a channel — see
   *  the module note. */
  scope: Selection
  /** The range, in unix milliseconds. `to` is inclusive, as `SearchQuery`
   *  reads a `before`. */
  from: number
  to: number
  /** Where the Run starts: the instant the Listener last scrubbed to, clamped
   *  into the range. Not the playhead — that moves several times a second and
   *  belongs to the element, not the address bar. */
  at: number
}

/**
 * The DVR a URL describes, with anything unreadable replaced rather than
 * passed on — `readSearchUrl`'s rule, and for its reason: a URL is typed by
 * strangers, and `Number('alpha')` reaches the wire as `after=NaN`, which
 * answers with nothing at all and reads exactly like a quiet night.
 *
 * `fallback` is the scanner the Listener arrived with. It is a parameter and
 * not a live read on purpose: a DVR of "my scanner" that re-scoped itself
 * every time a Talkgroup was toggled in the panel would end the **Run** it was
 * walking (#89) for a gesture on another screen.
 */
export function readDvrUrl(
  params: URLSearchParams,
  fallback: Selection,
  now: number,
): DvrView {
  const encoded = params.get('sel')
  // All or nothing, like the reader below it: a scope is one statement, and
  // half a stranger's scanner is not what they sent.
  const scope = (encoded ? decodeSelection(encoded) : undefined) ?? fallback

  const from = numberIn(params, 'after')
  const to = numberIn(params, 'before')
  // A range has to have width, or the DVR has nothing to walk and the timeline
  // nothing to draw. Both bounds or neither: half a range is not one.
  const range =
    from !== undefined && to !== undefined && to > from
      ? { after: from, before: to }
      : rangeOf(DEFAULT_RANGE, now)

  const at = numberIn(params, 'at')
  return {
    scope,
    from: range.after,
    to: range.before,
    at: clamp(at ?? range.after, range.after, range.before),
  }
}

/** This DVR as a query string — sorted and empties dropped, like every other
 *  link the app mints. */
export function writeDvrUrl(view: DvrView): string {
  const params = new URLSearchParams({
    sel: encodeSelection(view.scope),
    after: String(view.from),
    before: String(view.to),
  })
  // Spelled only when it is not what a bare range already means, so the URL of
  // a DVR nobody has scrubbed is the plain one — `writeSearchUrl`'s rule about
  // the default ordering, for its reason.
  if (view.at !== view.from) params.set('at', String(view.at))
  params.sort()
  return params.toString()
}

/**
 * The **Run** this DVR walks: forwards from the anchor to the end of the range.
 *
 * `after` is the *anchor*, never the range's start — re-anchoring is what
 * scrubbing does, and a Run whose search did not move would go on playing from
 * where it was.
 */
export function dvrSearch(view: DvrView): RunSearch {
  return {
    sel: encodeSelection(view.scope),
    after: view.at,
    before: view.to,
    sort: 'oldest',
  }
}

/**
 * The timeline over it: the **whole** range, whatever the anchor.
 *
 * Keyed on the range rather than the anchor so a scrub costs no aggregate — and
 * so the picture does not shrink under the thumb as the Listener drags along
 * it, which is what a chart of "from here to the end" would do.
 */
export function dvrActivity(view: DvrView): ActivityQuery {
  return {
    sel: encodeSelection(view.scope),
    after: view.from,
    before: view.to,
    buckets: TIMELINE_BUCKETS,
  }
}

/**
 * A search on screen, as a DVR link (#63) — the gesture "rewind *this*".
 *
 * Deliberately a **query string** rather than a [`DvrView`], because the two
 * things it may not know are exactly the two [`readDvrUrl`] has defaults for:
 * a scope naming no channel opens on the Listener's own scanner, and a search
 * with no dates opens on the DVR's own range. Returning a view would have
 * forced this to invent both.
 *
 * Only the scope and the range travel. A DVR's scope *is* a Selection, so a
 * tag, a mark or a duration filter has no spelling here — and carrying one
 * silently would produce a link that says less than the screen it came from
 * while looking as though it said the same.
 */
export function dvrLink(search: RunSearch): string {
  const params = new URLSearchParams()

  // A Ref means something only inside a System, so half a channel is not one.
  const scope =
    search.sel ??
    (search.system !== undefined && search.talkgroup !== undefined
      ? encodeSelection(channelScope(search.system, search.talkgroup))
      : undefined)
  if (scope !== undefined) params.set('sel', scope)

  const { after, before } = search
  if (after !== undefined && before !== undefined && before > after) {
    params.set('after', String(after))
    params.set('before', String(before))
  }

  params.sort()
  return params.toString()
}

/** One channel, as the Selection it is. */
export function channelScope(
  systemRef: number,
  talkgroupRef: number,
): Selection {
  return { all: false, sel: { [systemRef]: { [talkgroupRef]: true } } }
}

/**
 * The one channel a scope names, if it names exactly one — which is what the
 * channel picker shows its own value from.
 *
 * Derived rather than remembered beside the scope, so the picker cannot come to
 * disagree with what is actually being played.
 */
export function soleChannel(scope: Selection): TalkgroupKey | undefined {
  if (scope.all) return undefined
  const systems = Object.entries(scope.sel)
  if (systems.length !== 1) return undefined
  const [systemRef, talkgroups] = systems[0]
  const entries = Object.entries(talkgroups)
  if (entries.length !== 1) return undefined
  const [talkgroupRef, on] = entries[0]
  if (!on || talkgroupRef === WILDCARD) return undefined
  return { systemRef: Number(systemRef), talkgroupRef: Number(talkgroupRef) }
}

/**
 * Where the playhead is in wall-clock time: the playing Call's own instant plus
 * how far into it the element has got.
 *
 * The anchor while nothing is playing — and while a Call whose instant nobody
 * recorded is, because a marker placed from an unknown would wander off the
 * timeline rather than say it did not know.
 */
export function playheadMs(
  view: DvrView,
  playing: { timestamp?: number } | null,
  positionSeconds: number,
): number {
  if (playing?.timestamp === undefined) return view.at
  return playing.timestamp + positionSeconds * 1000
}

/** A parameter read as a finite number, or `undefined` for absent, blank, and
 *  everything that is not one. */
function numberIn(params: URLSearchParams, key: string): number | undefined {
  const raw = params.get(key)
  if (raw === null || raw.trim() === '') return undefined
  const value = Number(raw)
  return Number.isFinite(value) ? value : undefined
}

const clamp = (value: number, low: number, high: number) =>
  Math.min(Math.max(value, low), high)
