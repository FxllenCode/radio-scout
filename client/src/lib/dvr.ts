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
import { numberIn } from './searchUrl'
import { rangeOf } from './dateRange'
import type { ActivityQuery, SearchQuery } from '@/types'
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
 * Taking this DVR away as a file (#65, spec US 33).
 *
 * The **whole range**, not from the anchor — for [`dvrActivity`]'s reason one
 * control along: the range is what is on the timeline and what the Listener
 * chose, where the anchor is only where they happen to be listening at this
 * second. An export that started at the playhead would silently shrink every
 * time somebody scrubbed forward before downloading.
 */
export function dvrExport(view: DvrView): SearchQuery {
  return {
    sel: encodeSelection(view.scope),
    after: view.from,
    before: view.to,
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

/**
 * The DVR after a change of scope or range.
 *
 * Two rules, both of which the screen would otherwise have to remember:
 *
 * **It re-anchors**, because the old anchor described a stretch of time that
 * may not be in the new range at all.
 *
 * **It refuses a range with no width**, keeping the one it has. A
 * `datetime-local` emits every intermediate value as it is retyped — `0002`,
 * `0020`, `0202`, `2027` — so a range momentarily running backwards is the
 * ordinary case rather than a mistake, and [`readDvrUrl`] would answer one by
 * throwing *both* bounds away and falling back to its default. The Listener
 * would watch their whole range vanish and both boxes be rewritten under the
 * cursor, which is exactly the failure `DateField` exists to prevent one layer
 * down.
 */
export function rescoped(view: DvrView, patch: Partial<DvrView>): DvrView {
  const next = { ...view, ...patch }
  if (next.to <= next.from) return view
  return { ...next, at: next.from }
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


const clamp = (value: number, low: number, high: number) =>
  Math.min(Math.max(value, low), high)

/**
 * A channel as the scope picker's option value, and back.
 *
 * Written once because a `<select>` carries strings: three call sites building
 * `` `${systemRef}:${talkgroupRef}` `` by hand and one parsing it with `split`
 * is four chances for the spelling to disagree with itself, and the failure
 * would be a picker that silently selects nothing.
 */
export function channelOption(channel: TalkgroupKey): string {
  return `${channel.systemRef}:${channel.talkgroupRef}`
}

/** The channel an option value names, or `undefined` for the ones that name
 *  none — the empty value, the shared-selection placeholder, and anything a
 *  hand-edited DOM might produce. */
export function readChannelOption(value: string): TalkgroupKey | undefined {
  const [systemRef, talkgroupRef] = value.split(':')
  if (!isRef(systemRef) || !isRef(talkgroupRef)) return undefined
  return { systemRef: Number(systemRef), talkgroupRef: Number(talkgroupRef) }
}

const isRef = (value: string | undefined) => value !== undefined && /^\d+$/.test(value)
