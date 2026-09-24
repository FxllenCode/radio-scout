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
 *
 * ## Design notes (moved verbatim from CLAUDE.md, #110)
 *
 * **The DVR is a Run whose position is a time, and its "playlist" is not a media format (#63, spec US 39–40).** `routes/DvrScreen.tsx` over `lib/dvr.ts`: a scope, a range, an anchor, and the two searches those imply. No product has one — rdio-scanner's time travel is the Previous button, one page of results at a time. Six things follow.
 *
 * **It is a **Run**, so the serving criterion is met by there being no serve path.** CONTEXT.md already says walking search results, playing forward from a row and a DVR are one concept configured differently, so this screen starts an ordinary oldest-first Run and the page-ahead, the roll-on, the prefetch and what-plays-next are `lib/run`'s. The "playlist with discontinuity markers" is therefore the ordered page of Calls the archive already answers with, each carrying the URL of an object already being served: no stitching, no concatenation, no transcoding, because nothing new is served at all. A real HLS playlist was the other reading and is wrong — a WAV is not a valid HLS segment and Trunk Recorder uploads WAV by default, and ADR-0005's revised ladder already puts MMS out of reach because anything making it viable makes the **Station stream** (#74) strictly better. [ADR-0005's #63 amendment](docs/adr/0005-client-audio-media-session-background.md) is the argument; what a Listener hears as *gapless* is #14's prefetch plus #59's Quiet-span trim taking out the recorder's own dead air.
 *
 * **A scrub re-anchors the Run, where #62's identical gesture deliberately does not.** The Search ribbon moves the window of *results* and leaves playback alone (#89's separation); a DVR's position **is** an instant, so `at` is the search's own lower bound and scrubbing mints a new Run there. That is also why `DensityRibbon` now takes a **resolved bucket** rather than an offset and an ordering: two screens place the marker from two different facts — `bucketOfOffset` for a page, `bucketOfInstant` for a playhead — and the component keeps knowing nothing about what a position means.
 *
 * **The scope is always a Selection, including when it is one channel.** `channelScope` mints a one-entry matrix rather than reaching for `?talkgroup=`, so the DVR has **one** scoping rule. It matters because the two differ: `sel=` answers `Selection::reaches_channels`'s *question* — the live feed's own, which sees **Patch**es — where `?talkgroup=` compares the Call's canonical channel and stops. It answers it with a **second implementation**, since a predicate over one Call cannot be a `WHERE` clause; `tests/archive.rs::the_sql_filter_answers_what_the_selection_itself_would` is what holds the two together, enumerating the matrix *exhaustively* (two Talkgroups and the wildcard, each absent/on/off, times the global default — 54 scanners) rather than proving four arms with four hand-picked strings. This is #62's bucket arithmetic one module along, and the reason is the same: either implementation alone answers confidently and wrongly. A DVR of a channel that was patched that night would otherwise go silent exactly when the county was busiest, and a DVR of "one channel" and a DVR of "a Selection holding one channel" would answer differently.
 *
 * **The link spelling is one format in two languages, and neither side's own round trip can prove it.** `client/src/lib/selectionEncoding.json` is a table both `selectionUrl.ts` and `selection.rs` are held to — #62's `assert_activity_buckets` reasoning, without a runtime bridge to share. A drift there is not a crash: the server answers a DVR with a perfectly ordinary page of somebody else's channels. Building it found one real divergence — the client normalised a System's Ref through `Number()` and left a Talkgroup's raw, so a hand-typed `05` addressed a key nothing would ever match — fixed rather than pinned.
 *
 * **The speed and trim are Catch-up's levers and deliberately not Catch-up.** CONTEXT.md reserves that word for draining the *listening queue*, which "ends when the queue is empty"; a DVR ends at the end of a range. So there are two flags — `live.catchup` and `playback.hurrying` — and one question, `selectIsHurrying`, asked of the **transport**, because the transport is already the module that knows which source owns the element. One flag would mean a Listener catching up finding a DVR at 1.5×, and a DVR left hurrying speeding up the live feed it hands back to. It is cleared by a **reducer wrapper** when the Run ends rather than in six reducers, which is #59's own trick one slice over: a rate left set would follow the Listener to a Search result they then hear fast with no control on screen to undo it.
 *
 * And **seeking inside a Call is an intent, not a position.** `transport.seek` carries a second *and a nonce*, because asking for the same second twice is two requests; the player applies it keyed on the nonce alone, or turning the speed up mid-Call would drag the Listener back to wherever they last scrubbed. `sourceChanged` clears it, or a seek would land on the next Call — which in a DVR is a different minute of a different hour. The element then range-requests what it lacks, which `src/serve.rs` has answered since #10. The lock screen's `seekto` stays **unbound**: `lib/mediaSession.ts`'s recorded reason — a Call is seconds long — survives a DVR, because a DVR still plays one Call at a time. **The iOS gate re-opens**, as it does for every change to media-source behaviour: research §14 gains **Step 10** (a scrub across Calls changes `src`, which Step 9 does not cover).
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
