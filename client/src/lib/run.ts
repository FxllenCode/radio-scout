/**
 * The **Run** — the ordered set of archived Calls a Listener is walking (#89).
 *
 * Pure: no React, no store, no network. Everything here is a value in and a
 * value out, which is what makes the transitions a table rather than a route
 * rendered and fed synthetic media events.
 */
import type { Call, SearchPage, SearchQuery } from '@/types'

import { searchParams } from './archive'

/** The search a Run belongs to: the filters and the ordering, never the window
 *  onto them. A Run's own window is its business ([`Run.from`]); the window on
 *  screen is the screen's. */
export type RunSearch = Omit<SearchQuery, 'limit' | 'offset'>

/**
 * Are these the same search — whatever built the objects?
 *
 * Structural, and deliberately not a serialized key: what this replaced was a
 * `JSON.stringify` whose own comment admitted it was key-*order* sensitive and
 * "failed silently", which #61 was about to break two ways at once (an explicit
 * reset control, and round-tripping search state through the URL).
 *
 * [`searchParams`] is reused rather than reimplemented because it already
 * decides both halves the same way the URL does: it sorts its keys, so key
 * order cannot matter, and it drops anything unset, so a filter *cleared* to
 * `undefined` is the same search as one that never carried the key.
 *
 * The **window is not part of the search**, so either side may carry one and it
 * is dropped — "is this the same search" has to have one answer whichever page
 * of it you happen to be holding, which is what lets a page *request*
 * ([`RunWindow`]) be compared against the search a Run is walking.
 */
export function sameSearch(a: SearchQuery, b: SearchQuery): boolean {
  return searchParams(withoutWindow(a)) === searchParams(withoutWindow(b))
}

const withoutWindow = ({ limit: _l, offset: _o, ...search }: SearchQuery) =>
  search

/**
 * A Run: which search, which Calls of it are loaded, and where in them the
 * Listener is.
 *
 * `null` — no Run — is a state of its own rather than a Run with nothing
 * playing, so "the Listener is not walking the Archive" is one value instead of
 * a combination of five fields that have to agree.
 */
export interface Run {
  /** The search this Run belongs to, compared with [`sameSearch`]. */
  search: RunSearch
  /** The Calls loaded, every one of them playable. */
  calls: Call[]
  /** Where `calls[0]` sits in the whole filtered set — **the Run's own
   *  window**, which is not the window on screen. The Listener may page the
   *  screen anywhere; this stays where the Run is. */
  from: number
  /** How many Calls a page of this Run holds, so the Run can name the page
   *  after its own rather than the page after the screen's. */
  limit: number
  /** Into `calls`; `-1` while nothing is playing. */
  index: number
  /** How many Calls the search matches in total, across every page. */
  total: number
  /** Whether the Archive has a page beyond this one. */
  hasMore: boolean
  /** Walked past the last loaded Call, waiting for the page it asked for. */
  rolling: boolean
  /** A single archived Call over a live feed that is still running (spec US
   *  26) — no set to walk and no page to roll onto. */
  interrupting: boolean
  /** The Run still belongs to the search on screen. A search change disarms it:
   *  it plays out what it holds and rolls onto nothing, rather than walking a
   *  page of results the Listener has not seen. */
  armed: boolean
}

/** A page of the Archive, named exactly as `GET /api/calls` takes one — so the
 *  Run's answer goes straight to the query with nothing translating it. */
export type RunWindow = RunSearch & { limit: number; offset: number }

/**
 * How few Calls may be left before the *next page* is worth fetching (#32).
 *
 * Two, so there is a Call still playing plus one behind it while the search and
 * the first audio file of the next page are on the wire — the boundary is the
 * one transition that costs both, and the only one a Listener notices. Fewer
 * would leave a two-second kerchunk as the whole lead; more would pull a page
 * (and an audio file) for every Listener who wanders off mid-page.
 */
const PAGE_AHEAD_WITHIN = 2

/** Everything a Listener's Run answers, in one value. */
export interface RunView {
  /** What is playing. */
  current: Call | null
  /** What follows it on the loaded page — #14's prefetch target. */
  next: Call | null
  /**
   * Which page of the Archive must be on hand, ready to hand back as
   * [`RunEvent`]`'paged'`. `null` when none is.
   *
   * One field for what used to be two mechanisms that always wanted the same
   * page: #32's page-ahead (fetch it *before* the boundary, so crossing costs
   * no round trip) and US 25's roll-on (the Run is at the boundary now and
   * cannot continue without it). The difference between them is only whether
   * anything is playing meanwhile.
   */
  wanted: RunWindow | null
  /** Where the Run sits in the whole filtered set: the "3 of 421" readout.
   *  `-1` while nothing is playing. */
  position: { index: number; total: number }
  hasNext: boolean
  hasPrevious: boolean
  /** True while this Run is a single Call over the live feed. */
  interrupting: boolean
  /** The Run is at a boundary with nothing playing, waiting on [`wanted`] to be
   *  handed to it. The difference between "fetch this before I need it" and
   *  "I need this now", which `wanted` alone does not say. */
  rolling: boolean
}

/** Only Calls there is something to play. An encrypted Call carries no
 *  `audioUrl`, so the audio element would get `src={undefined}` and never fire
 *  `ended` — the one thing that advances a Run. Established here, once, rather
 *  than checked again at every step. */
const playable = (calls: Call[]) => calls.filter((one) => one.audioUrl)

/**
 * Is there a page beyond this Run's own that it may go on to?
 *
 * Deliberately not "is the screen showing this search": the Run keeps its own
 * window ([`Run.from`]), so a Listener browsing ahead with the paging buttons
 * moves the screen and not the Run, and the roll-on that follows still lands on
 * the page the Run is actually walking towards. That separation is the whole
 * reason the two offsets are named apart.
 */
const wants = (run: Run) => run.armed && run.hasMore && !run.interrupting

/** The page after the Run's own — never the page after the screen's. */
const nextWindow = (run: Run): RunWindow => ({
  ...run.search,
  limit: run.limit,
  offset: run.from + run.limit,
})

/** The gestures a Run answers to. */
export type RunEvent =
  /** The Listener tapped the `index`-th row of a page of `search`. */
  | {
      type: 'started'
      search: RunSearch
      page: SearchPage
      index: number
      /** The live feed is still running, so this Call interrupts it. */
      interrupting?: boolean
    }
  /** The Call on the air ended, or the Listener skipped it. */
  | { type: 'advanced' }
  /** The Listener stepped back one Call. */
  | { type: 'back' }
  /** A page of the Archive is in hand, along with **the request it answers** —
   *  both, because an offset alone cannot say which search a page belongs to
   *  and every search has a page at every offset. */
  | { type: 'paged'; window: RunWindow; page: SearchPage }
  /** The Listener changed the search on screen. */
  | { type: 'searched'; search: RunSearch }
  /** The Listener stopped, or the Archive stopped being what plays at all —
   *  leaving playback mode, or handing the audio back to the live feed. */
  | { type: 'stopped' }

/**
 * The whole of what a Run does, as one transition.
 *
 * `null` in and `null` out are ordinary: a Run begins from nothing and ends to
 * nothing, so every caller has one shape to hold.
 */
export function advance(run: Run | null, event: RunEvent): Run | null {
  switch (event.type) {
    case 'started': {
      const { search, page, index, interrupting = false } = event
      // A stale or empty selection changes nothing — better than stopping
      // whatever is playing. One condition covers both it and a Call there is
      // nothing to play (#42, spec US 9): an index off either end of the page
      // reads `undefined`, and an encrypted Call carries no `audioUrl`. Nothing
      // offers the second, but a Run that *began* on one would be stuck before
      // it started.
      const chosen = page.results[index]
      if (!chosen?.audioUrl) return run
      // Interrupting is a single Call, never a run of them.
      const calls = interrupting ? [chosen] : playable(page.results)
      return {
        search,
        calls,
        from: interrupting ? 0 : page.offset,
        limit: page.limit,
        index: calls.indexOf(chosen),
        total: interrupting ? 1 : page.count,
        hasMore: interrupting ? false : page.hasMore,
        rolling: false,
        interrupting,
        armed: true,
      }
    }

    case 'advanced': {
      if (!run || run.index < 0) return run
      if (run.index + 1 < run.calls.length) {
        return { ...run, index: run.index + 1 }
      }
      // Past the last loaded Call. US 25 asks for sequential playback through
      // the *filtered results*, not through one page of them, so roll on —
      // where there is anywhere to roll on to. Where there is not, the Run is
      // over, which is one value rather than five fields left agreeing.
      if (!wants(run)) return null
      return { ...run, index: -1, rolling: true }
    }

    /** Nothing precedes the first loaded Call — a Run does not page backwards —
     *  so this holds there rather than stopping. */
    case 'back':
      return run && run.index > 0 ? { ...run, index: run.index - 1 } : run

    case 'paged': {
      // Only a Run waiting at a boundary takes a page, and only the page it
      // named. The page-ahead lands *before* the boundary — that is what it is
      // for — and taking it then would throw away the Call still playing.
      if (!run?.rolling) return run
      const { window, page } = event
      // Both halves of "the page it named", and the search is the half that is
      // easy to forget: a client caching by request keeps the answer to the
      // *previous* one in hand while the next is in flight, so the page-ahead
      // of a search the Listener has just left can arrive here — and every
      // search has a page at every offset, so an offset alone would take it and
      // carry on playing Calls from the search that ended. That is precisely
      // what a Run's identity exists to stop, so it is checked wherever Calls
      // enter a Run, not only where the search changes.
      const next = nextWindow(run)
      if (!sameSearch(run.search, window) || window.offset !== next.offset) {
        return run
      }

      const calls = playable(page.results)
      const rolled: Run = {
        ...run,
        calls,
        from: page.offset,
        limit: page.limit,
        index: calls.length > 0 ? 0 : -1,
        total: page.count,
        hasMore: page.hasMore,
        rolling: false,
      }
      // A page with nothing playable on it — every Call encrypted — is a page
      // to roll *past*, not a place to stop: the Archive may still have pages
      // behind it, and stopping would strand a Run that has somewhere to go.
      if (calls.length > 0) return rolled
      return wants(rolled) ? { ...rolled, rolling: true } : null
    }

    case 'searched': {
      if (!run) return null
      const armed = sameSearch(run.search, event.search)
      // Disarmed with nothing on the air — mid roll-on, or already stopped —
      // there is nothing left to play out, so the Run is simply over.
      if (!armed && run.index < 0) return null
      return { ...run, armed, rolling: run.rolling && armed }
    }

    case 'stopped':
      return null
  }
}

/** What the Run says, for a screen and for the controls. */
export function runView(run: Run | null): RunView {
  const current = run && run.index >= 0 ? run.calls[run.index] : null
  const hasNext =
    run !== null &&
    !run.interrupting &&
    run.index >= 0 &&
    run.index + 1 < run.calls.length

  // The lead has run short (page ahead), or has run out altogether and the Run
  // is waiting on the page to continue at all (roll on).
  const leadRunShort =
    run !== null &&
    wants(run) &&
    (run.rolling ||
      (run.index >= 0 && run.calls.length - 1 - run.index <= PAGE_AHEAD_WITHIN))

  return {
    current,
    next: run && hasNext ? run.calls[run.index + 1] : null,
    wanted: run && leadRunShort ? nextWindow(run) : null,
    position: {
      index: !run || run.index < 0 ? -1 : run.from + run.index,
      total: run?.total ?? 0,
    },
    hasNext,
    hasPrevious: run !== null && !run.interrupting && run.index > 0,
    interrupting: run?.interrupting ?? false,
    rolling: run?.rolling ?? false,
  }
}
