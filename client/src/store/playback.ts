import { createSlice, type PayloadAction } from '@reduxjs/toolkit'

import type { Call } from '@/types'

/** Live feed or archive — mutually exclusive, per CONTEXT.md. */
export type PlaybackMode = 'live' | 'playback'

export interface PlaybackState {
  mode: PlaybackMode
  /** The archive Calls loaded for sequential playback, in the order the search
   *  returned them. Deliberately *not* the **listening queue** (CONTEXT.md),
   *  which belongs to the live feed and this slice never touches. While
   *  interrupting the live feed this holds exactly one Call. */
  results: Call[]
  /** Index into `results` of the Call playing now; `-1` when nothing is. */
  index: number
  /** Where `results` starts within the whole filtered set, so the readout can
   *  count against the archive rather than the page. */
  offset: number
  /** How many Calls the filters match in total, across every page. */
  total: number
  /** True while an archived Call is playing *over* the live feed (US 26). The
   *  live feed's own listening queue is untouched and resumes when this
   *  clears — this slice never owns live state. */
  interrupting: boolean
  /** Set when playback walked past the last loaded Call. The screen's cue to
   *  load the next page and keep going, which is what makes playback mode
   *  sequential across the *whole* result set and not just one page (US 25). */
  exhausted: boolean
}

const initialState: PlaybackState = {
  mode: 'live',
  results: [],
  index: -1,
  offset: 0,
  total: 0,
  interrupting: false,
  exhausted: false,
}

/** Back to "nothing from the archive is playing". */
function idle(state: PlaybackState) {
  state.results = []
  state.index = -1
  state.offset = 0
  state.total = 0
  state.interrupting = false
  state.exhausted = false
}

/**
 * Playback mode and the archive results behind it (#13, spec US 25–26).
 *
 * The two ways an archived Call reaches the speakers are one action, because
 * from the listener's side they're one gesture — tapping a search result. What
 * differs is what the live feed is doing:
 *
 * - **Playback mode** (live feed off): the whole filtered result set becomes the
 *   playback order and runs sequentially from the tapped Call, page after page.
 * - **Live feed on**: that single Call *interrupts*; when it finishes the live
 *   feed resumes exactly where it was, because its queue was never touched.
 */
const playbackSlice = createSlice({
  name: 'playback',
  initialState,
  reducers: {
    /** Turn the live feed off and browse the archive. */
    enterPlaybackMode(state) {
      if (state.mode === 'playback') return
      state.mode = 'playback'
      idle(state)
    },

    /** Return to the live feed, dropping whatever the archive was playing. */
    enterLiveFeed(state) {
      if (state.mode === 'live') return
      state.mode = 'live'
      idle(state)
    },

    /** Play the `index`-th Call of a page of search results. `offset`/`total`
     *  place that page within the whole filtered set. */
    playResults(
      state,
      action: PayloadAction<{
        results: Call[]
        index: number
        offset?: number
        total?: number
      }>,
    ) {
      const { results, index, offset = 0, total } = action.payload
      const chosen = results[index]
      // A stale or empty selection changes nothing — better than stopping
      // whatever is currently playing.
      if (index < 0 || !chosen) return
      // ...and neither does choosing a Call there is nothing to play (#42,
      // spec US 9). Nothing offers this — an encrypted archive row has no play
      // button — but a run that *began* on one would be stuck before it started.
      if (!chosen.audioUrl) return

      // Everything a run walks must be playable, established once here rather
      // than checked again in `next` and `previous`.
      //
      // An encrypted Call carries no `audioUrl`, so the audio element would get
      // `src={undefined}` — and an element with no source never fires `ended`,
      // which is the only thing that advances a run. Left in, the first
      // encrypted result would stop playback mode dead and look like a bug in
      // the player. The counts stay the server's, so the readout still says how
      // far through the *archive* the listener is, not how far through the
      // subset that happens to be audible.
      const playable = results.filter((one) => one.audioUrl)

      state.exhausted = false
      if (state.mode === 'playback') {
        state.results = playable
        state.index = playable.indexOf(chosen)
        state.offset = offset
        state.total = total ?? results.length
        state.interrupting = false
      } else {
        // Interrupting the live feed is a single Call, never a run of them.
        state.results = [chosen]
        state.index = 0
        state.offset = 0
        state.total = 1
        state.interrupting = true
      }
    },

    /** The current Call finished or was skipped: advance, or hand back to the
     *  live feed if this was an interruption. */
    next(state) {
      if (state.interrupting) {
        idle(state)
        return
      }
      if (state.index < 0) return
      if (state.index + 1 < state.results.length) {
        state.index += 1
        return
      }
      // Past the last loaded Call: stop, and let the screen decide whether
      // another page follows.
      state.index = -1
      state.exhausted = true
    },

    /** Step back one Call, holding at the first. Nothing precedes an
     *  interruption, so there it does nothing at all. */
    previous(state) {
      if (state.interrupting || state.index <= 0) return
      state.index -= 1
    },

    /** Stop playing, keeping the loaded result set so the listener can resume. */
    stop(state) {
      if (state.interrupting) {
        idle(state)
        return
      }
      state.index = -1
      state.exhausted = false
    },
  },
})

export const {
  enterLiveFeed,
  enterPlaybackMode,
  next,
  playResults,
  previous,
  stop,
} = playbackSlice.actions

export const playbackReducer = playbackSlice.reducer

/** The slice of the store this module owns. */
interface WithPlayback {
  playback: PlaybackState
}

export const selectPlaybackMode = (state: WithPlayback): PlaybackMode =>
  state.playback.mode

export const selectCurrentCall = (state: WithPlayback): Call | null =>
  state.playback.results[state.playback.index] ?? null

export const selectIsInterrupting = (state: WithPlayback): boolean =>
  state.playback.interrupting

export const selectIsExhausted = (state: WithPlayback): boolean =>
  state.playback.exhausted

/** Whether a Call follows the current one *within the loaded page* — an
 *  interruption has none, because "next" there means back to the live feed. */
export const selectHasNext = (state: WithPlayback): boolean =>
  !state.playback.interrupting &&
  state.playback.index >= 0 &&
  state.playback.index + 1 < state.playback.results.length

export const selectHasPrevious = (state: WithPlayback): boolean =>
  !state.playback.interrupting && state.playback.index > 0

/** The Call queued behind the current one, which the player warms the HTTP
 *  cache with so it starts without a network round trip (#14). */
export const selectNextCall = (state: WithPlayback): Call | null =>
  selectHasNext(state) ? state.playback.results[state.playback.index + 1] : null

/** How many Calls still follow the current one on the loaded page — the lead
 *  #14's prefetch has left to work with before it runs out of page. */
export const selectRemainingInPage = (state: WithPlayback): number =>
  state.playback.index < 0
    ? 0
    : state.playback.results.length - 1 - state.playback.index

/**
 * How few Calls may be left before the *next page* is worth fetching (#32).
 *
 * Two, so there is a Call still playing plus one behind it while the search and
 * the first audio file of the next page are on the wire — the boundary is the
 * one transition that costs both, and the only one a listener notices. Fewer
 * would leave a two-second kerchunk as the whole lead; more would pull a page
 * (and an audio file) for every listener who wanders off mid-page.
 */
export const PAGE_AHEAD_WITHIN = 2

/**
 * Whether playback is close enough to the end of the loaded page that the next
 * one should already be on its way (#32).
 *
 * False for an **interruption**, which is a single Call over the live feed with
 * no run to page through, and false in live-feed mode generally: there is no
 * result set there, so there is no boundary to cross. Whether a next page
 * actually exists is the screen's to know — this only says the lead has run
 * short.
 */
export const selectIsNearingPageEnd = (state: WithPlayback): boolean =>
  state.playback.mode === 'playback' &&
  !state.playback.interrupting &&
  state.playback.index >= 0 &&
  selectRemainingInPage(state) <= PAGE_AHEAD_WITHIN

/** Where playback sits in the whole filtered set — the "3 of 421" readout.
 *  `index` counts from the start of the archive, not of the loaded page. */
export const selectPlaybackPosition = (
  state: WithPlayback,
): { index: number; total: number } => ({
  index:
    state.playback.index < 0 ? -1 : state.playback.offset + state.playback.index,
  total: state.playback.total,
})
