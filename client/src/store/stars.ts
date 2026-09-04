/**
 * What this session has done to **Stars** (#66, spec US 37).
 *
 * # Why an override map rather than the answer itself
 *
 * A Star is the Instance's mark and the server holds it, so every Call already
 * carries the answer on the wire. What this slice adds is *instant*, and it has
 * to, because a Call lives in more than one place at once and only one of them
 * can be refetched:
 *
 * - in the archive's RTK Query cache (a search page, a Call by id, a radio's
 *   history), which a `Star` invalidation brings back up to date
 * - in `./live`'s history, queue and display, which arrived over a socket and
 *   which nothing republishes (#46)
 * - and in `./playback`'s page, which is a *snapshot* the Run walks
 *
 * Patching each of those would be three rules that can disagree about what a
 * Star is. One override, read by everything, is one rule — and it is also what
 * makes the tap feel instant on a Pi, where the refetch behind it is a round
 * trip the Listener should never be waiting on.
 *
 * The map only ever holds what somebody tapped, so it is bounded by taps and
 * cleared by a reload — which is safe precisely because it is an override of a
 * durable fact rather than the fact itself.
 */
import { createSlice, type PayloadAction } from '@reduxjs/toolkit'

export interface StarsState {
  /** What this session said about a Call, by id — absent meaning "nothing, ask
   *  the Call itself". */
  marks: Record<number, boolean>
}

export const initialStarsState: StarsState = { marks: {} }

const starsSlice = createSlice({
  name: 'stars',
  initialState: initialStarsState,
  reducers: {
    /** This session starred (or un-starred) a Call. */
    markStarred(state, action: PayloadAction<{ id: number; starred: boolean }>) {
      state.marks[action.payload.id] = action.payload.starred
    },

    /** Take an override back, because the server refused it.
     *
     *  Deliberately *forgetting* rather than writing the opposite: what a
     *  refused un-star should fall back to is `true`, and what a refused star
     *  should fall back to is whatever the page was fetched with — which is
     *  the same sentence, and only absence says it. */
    forgetStar(state, action: PayloadAction<number>) {
      delete state.marks[action.payload]
    },
  },
})

export const { forgetStar, markStarred } = starsSlice.actions

export const starsReducer = starsSlice.reducer

/** The slice of the store this module owns. */
interface WithStars {
  stars: StarsState
}

/** Is this Call starred — what this session said, else what the server did.
 *
 *  Takes the Call rather than an id, because the server's answer travels *on*
 *  it: a caller holding only an id could not tell "nobody starred it" from
 *  "this session has not touched it".
 */
export const selectStarred = (
  state: WithStars,
  call: { id: number; starred?: boolean },
): boolean => state.stars.marks[call.id] ?? call.starred ?? false
