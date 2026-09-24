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
 *
 * ## Design notes (moved verbatim from CLAUDE.md, #110)
 *
 * **The URL is the search state, and a link is a value both ends read the same way (#61, spec US 30–31).** `lib/searchUrl.ts` is the round trip between a `RunSearch` + a window and a query string; `lib/selectionUrl.ts` is the same for a **Selection**; `lib/dateRange.ts` is the four presets; `lib/share.ts` is how either leaves the app. rdio-scanner addresses nothing but its `?id=` Profile, so a search there is unbookmarkable, unshareable, and gone the moment you look at something else. Six things follow.
 *
 * **The URL is the state, not a copy kept in step with one.** `SearchScreen` holds no filter state at all — it reads `useSearchParams` and writes it — so a reload, the back button, a tab switch and a link somebody sent are one mechanism rather than four features that each have to remember the others. The consequence that pays for it: the Run is told the search changed from **where the change is observed**, in one effect, rather than at every control that could cause one — which is the only way the back button gets the same treatment as a dropdown (#92's argument one layer up).
 *
 * **A search rebuilt from a link has to be the *same **Run***, and that is what `readSearchUrl` normalizes for.** Identity is the search compared structurally (#89), so a `sort` the URL happened to omit would be a different Run from the one that wrote it — and reloading a page would silently end the walk it had just restored. So the default ordering is spelled once: a URL naming no `sort` reads back as `newest`, and a search that *is* `newest` writes none. `/search`, `?sort=newest` and a filter object that never named one are one view, one URL and one Run. The window rides *beside* the search rather than inside it, for the reason #89 named the two offsets apart.
 *
 * **A URL is typed by strangers, so everything read out of one is checked** — `lib/persist`'s rule, reaching one layer further out. The failure it prevents is specific: `Number('alpha')` is `NaN`, which reaches the wire as `after=NaN` and answers with nothing at all, which reads exactly like an archive that is empty. A *search* drops the filter it could not read and keeps the rest, because a bad filter costs that filter; a *Selection* link is refused **whole**, because it is one statement made by somebody else and half of their scanner is not a thing they meant to send.
 *
 * **A preset resolves to instants, and the matrix is what a Selection link carries.** `when=today` would mean something different tomorrow, so what reaches the URL is `after=`/`before=` in milliseconds and a link is a *view* — which is what "any view is a bookmark" asks for. The calendar presets are built from local date parts rather than by subtracting 86,400,000 ms, because a day is not always that long, and `before` is exclusive-by-one-millisecond so today and yesterday share no instant. On the other side, a Selection travels as the whole ADR-0004 matrix rather than as a list of what is on: auto-populate (#8) mints Talkgroups the recipient's catalog has and the sender's does not, and only the matrix answers *what to do about a Talkgroup nobody has decided on* the same way at both ends (`lib/selection`'s rule 2, reaching one more place).
 *
 * **Opening a Selection link carries the way back, and a Call link does not need one.** A `?sel=` replaces something the Listener built by hand without their having touched it, which is the **Avoid** undo's case exactly (#58) — same shell, same deadline-as-a-moment, `SELECTION_UNDO_MS` longer than an Avoid's because the mistake is noticed by *reading the panel* rather than by hearing silence. The **Hold** and the **Avoids** are deliberately untouched by a link — they are this Listener's reading of the moment, and they are layers *over* the Selection, so they keep applying to whatever it becomes.
 *
 * **Both links are *consumed*: acted on, then taken out of the address bar, replacing rather than pushing.** What stays in the URL is what describes the screen — the filters — and neither of these does: one is folded into state the browser goes on remembering, the other is how the screen was opened. The failure that proves it is the Call's, and `/code-review` found it: the shell remembers where the Search tab was (including its query), so a `?call=` left in meant the Call was re-played, interrupting whatever was on, **every time the Listener came back to the tab** — a guard ref made it once per *mount*, which is not once per Call. Replacing rather than pushing matters for the same reason: pushing leaves the link one Back away from firing again. A Call that is **gone** is deliberately *not* consumed, because nothing was acted on and the sentence saying so has to survive the next render.
 *
 * **A dropdown earns a history entry; a keystroke does not.** `updateFilters` names the control being typed into, so the first keystroke in a field pushes — Back returns to the search *before* the typing — and the rest replace. Without it, typing a four-digit unit Ref left four entries and Back walked it digit by digit, which is the acceptance criterion ("back/forward work") failing on the one control a Listener types into most. The same review pass found the Run being told the search had changed on a *page turn*, because `filters` is rebuilt whenever any of the URL moves: the effect compares with `sameSearch` — the predicate the Run itself uses — rather than by reference.
 *
 * **One hook mints every link** (`hooks/useShareLink`), because three controls on two screens each owe the same three things: reach the platform, say what happened, and take the saying back down. That last one had already been forgotten once — the Talkgroups copy carried the notice and not the timer, so "Link copied." sat under its panel bar for the rest of the session. #92's rule, one layer up: a surface's shape may differ, the policy underneath may not.
 *
 * And **playing forward from a row is a Run configured differently, not a second kind of playback** — CONTEXT.md says so, and it is one lazy query plus the `startRun` that already exists, anchored `{sort: 'oldest', after: call.timestamp}`. The list on screen is untouched, which is the separation #89 named `Run.from` and `windowOffset` apart for. Two smaller things the build had to answer: the date inputs became **controlled**, so they had to keep the *text* and re-derive only when the bound shown is not the one the text already means (`2026-07-25` is a valid instant — at UTC midnight — so a half-typed date that parses would otherwise be rewritten under the cursor mid-word); and the Search **tab** remembers where it was, in the shell rather than in the tab bar, because a bar linking at a bare `/search` throws away eight taps of filters every time a Listener looks at Talkgroups.
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

  // A flag, and written only when it is on (#66). `starred: false` would be a
  // structurally different search from one that never mentioned it, and
  // therefore a different **Run** — so the "off" state has exactly one
  // spelling, which is absence.
  if (isTruthy(params.get('starred'))) search.starred = true

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

/** The spellings a yes/no parameter is read by (#66).
 *
 *  The *accepted* set is the server's (`Params::flag` in `src/query.rs`),
 *  because a URL is typed by hand as often as it is generated. What it does
 *  with the rest is deliberately **not** the server's: `Params::flag` refuses
 *  an unreadable value with a named 400, and this reads everything else — a
 *  missing parameter included — as *off*. That is `readSearchUrl`'s own rule
 *  one filter along ("dropping one filter costs the Listener that filter;
 *  refusing the whole URL would cost them the link"), and it is safe here in a
 *  way it would not be for a numeric bound: `starred=maybe` asks for the whole
 *  archive, where `after=NaN` would ask for none of it. */
function isTruthy(value: string | null): boolean {
  return value !== null && ['true', '1', 'yes'].includes(value.toLowerCase())
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
