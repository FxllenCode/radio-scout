import { createSelector, createSlice, type PayloadAction } from '@reduxjs/toolkit'

import {
  advance,
  runView,
  type Run,
  type RunSearch,
  type RunWindow,
} from '@/lib/run'
import type { Call, SearchPage } from '@/types'

/** Live feed or archive — mutually exclusive, per CONTEXT.md. */
export type PlaybackMode = 'live' | 'playback'

export interface PlaybackState {
  mode: PlaybackMode
  /** The **Run** the Listener is walking, or `null` when they are walking none.
   *  Everything about what is playing, what follows and which page must be on
   *  hand lives in there ([`@/lib/run`]) — this slice only decides *when* the
   *  Run is told, and holds the answer. */
  run: Run | null
}

const initialState: PlaybackState = { mode: 'live', run: null }

/**
 * Playback mode, and the archive **Run** behind it (#13, spec US 25–26; #89).
 *
 * The two ways an archived Call reaches the speakers are one action, because
 * from the Listener's side they're one gesture — tapping a search result. What
 * differs is what the live feed is doing:
 *
 * - **Playback mode** (live feed off): the whole filtered result set becomes a
 *   Run and plays sequentially from the tapped Call, page after page.
 * - **Live feed on**: that single Call *interrupts*; when it finishes the live
 *   feed resumes exactly where it was, because its queue was never touched.
 *
 * That is the only decision this slice takes for itself. Every transition after
 * it belongs to the Run, which is a pure value and is tested as a table (#89) —
 * so this file has nothing left to get wrong, and a screen has nothing left to
 * remember.
 */
const playbackSlice = createSlice({
  name: 'playback',
  initialState,
  reducers: {
    /** Turn the live feed off and browse the archive. */
    enterPlaybackMode(state) {
      if (state.mode === 'playback') return
      state.mode = 'playback'
      state.run = null
    },

    /** Return to the live feed, dropping whatever the archive was playing. */
    enterLiveFeed(state) {
      if (state.mode === 'live') return
      state.mode = 'live'
      state.run = null
    },

    /** The Listener tapped the `index`-th row of a page of `search`. With the
     *  live feed still on, that Call interrupts it rather than starting a walk
     *  through the results — the one thing the mode decides. */
    startRun(
      state,
      action: PayloadAction<{
        search: RunSearch
        page: SearchPage
        index: number
      }>,
    ) {
      state.run = advance(state.run, {
        type: 'started',
        ...action.payload,
        interrupting: state.mode === 'live',
      })
    },

    /** The Call on the air ended, or the Listener skipped it. */
    next(state) {
      state.run = advance(state.run, { type: 'advanced' })
    },

    /** Step back one Call. */
    previous(state) {
      state.run = advance(state.run, { type: 'back' })
    },

    /** Stop playing from the archive. */
    stop(state) {
      state.run = advance(state.run, { type: 'stopped' })
    },

    /** A page of the archive is in hand, and the search it came from. The Run
     *  takes it only if it is the one it asked for — the right offset *of the
     *  right search* — so handing it a page it did not want costs nothing and
     *  the screen needs no condition of its own. */
    runPaged(
      state,
      action: PayloadAction<{ window: RunWindow; page: SearchPage }>,
    ) {
      state.run = advance(state.run, { type: 'paged', ...action.payload })
    },

    /** The Listener changed the search on screen. */
    searchChanged(state, action: PayloadAction<RunSearch>) {
      state.run = advance(state.run, {
        type: 'searched',
        search: action.payload,
      })
    },
  },
})

export const {
  enterLiveFeed,
  enterPlaybackMode,
  next,
  previous,
  runPaged,
  searchChanged,
  startRun,
  stop,
} = playbackSlice.actions

/** Every action this slice has, as a set rather than as eight names. The live
 *  slice mirrors two of them (#88) so a reducer that can see no other slice can
 *  still refuse a Call while the archive is playing; `store.test.ts` holds this
 *  to its known contents, so adding a ninth is a decision taken rather than a
 *  mirror silently going stale. */
export const playbackActions = playbackSlice.actions

export const playbackReducer = playbackSlice.reducer

/** The slice of the store this module owns. */
interface WithPlayback {
  playback: PlaybackState
}

export const selectPlaybackMode = (state: WithPlayback): PlaybackMode =>
  state.playback.mode

const selectRun = (state: WithPlayback): Run | null => state.playback.run

/** Everything the Run answers, computed once per change of the Run itself.
 *
 *  Memoized because it is one object rather than eight scalars: without this
 *  every reader would get a fresh identity on every action in the store and
 *  re-render for it — which is the same trap #91 found in the Talkgroups
 *  panel. A Run is replaced only when it actually changes, so this recomputes
 *  exactly then. */
export const selectRunView = createSelector([selectRun], runView)

export const selectCurrentCall = (state: WithPlayback): Call | null =>
  selectRunView(state).current

export const selectIsInterrupting = (state: WithPlayback): boolean =>
  selectRunView(state).interrupting

export const selectHasNext = (state: WithPlayback): boolean =>
  selectRunView(state).hasNext

export const selectHasPrevious = (state: WithPlayback): boolean =>
  selectRunView(state).hasPrevious

/** The Call queued behind the current one, which the player warms the HTTP
 *  cache with so it starts without a network round trip (#14). */
export const selectNextCall = (state: WithPlayback): Call | null =>
  selectRunView(state).next

/** Where playback sits in the whole filtered set — the "3 of 421" readout.
 *  `index` counts from the start of the archive, not of the loaded page. */
export const selectPlaybackPosition = (
  state: WithPlayback,
): { index: number; total: number } => selectRunView(state).position

/** Which page of the archive the Run needs on hand — for the boundary it is
 *  about to cross, or the one it is waiting at (#32, spec US 25). `null` when
 *  none is, which is what the screen skips the query on. */
export const selectWantedPage = (state: WithPlayback): RunWindow | null =>
  selectRunView(state).wanted

/** True while the Run has walked off the end of its page and is waiting for the
 *  next one. The screen hands over the page it holds on this, so a page already
 *  cached — the whole point of the page-ahead — is taken the moment it is
 *  wanted rather than only when a fetch happens to resolve. */
export const selectIsRolling = (state: WithPlayback): boolean =>
  selectRunView(state).rolling
