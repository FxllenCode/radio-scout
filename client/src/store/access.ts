/**
 * The **grant** this browser is holding (#68, spec US 52).
 *
 * One field, and it is a slice rather than a module variable for one reason: it
 * has to be readable **synchronously by the base query** — every request carries
 * it, and a hook cannot be called from there — *and* it has to be something the
 * UI re-renders on, because unlocking must bring the locked rows back without a
 * reload. A slice is both.
 *
 * It is persisted (`lib/access`), unlike `./stars` and like a **Profile**: a
 * Listener who unlocked a channel this morning should not be asked again this
 * afternoon. That is the ticket's "remembers the code per browser" — and what is
 * remembered is the **grant**, not the code, so the thing sitting in local
 * storage is 128 minted bits rather than a word thirty people were told.
 */
import { createSlice, type PayloadAction } from '@reduxjs/toolkit'

import type { Stale } from '@/lib/access'

export interface AccessState {
  /** What every request carries, or nothing at all — which is where every
   *  Listener starts and where, on an Instance that gates nothing, they stay. */
  grant?: string
  /** Why the grant was let go of, when it was let go of because the server said
   *  it had stopped working. Held so the Listener can be **told once**: every
   *  read has already degraded to open listening, so nothing is broken, and
   *  silently going back to fewer channels is the one outcome nobody would
   *  understand. */
  stale?: Stale
}

export const initialAccessState: AccessState = {}

const accessSlice = createSlice({
  name: 'access',
  initialState: initialAccessState,
  reducers: {
    /** An unlock succeeded: hold this from now on.
     *
     *  Clears `stale` in the same step, because a Listener who has just unlocked
     *  is not owed a notice about the grant they replaced. */
    grantHeld(state, action: PayloadAction<string>) {
      state.grant = action.payload
      state.stale = undefined
    },

    /** The catalog says the grant is not a live code.
     *
     *  **Dropped rather than kept and ignored**, so nothing sends a credential
     *  that does not work — and the reason is kept, which is the whole of what
     *  the Listener is told. Guarded on holding one, so the notice cannot be
     *  raised twice by two catalog fetches answering the same thing. */
    grantExpired(state, action: PayloadAction<Stale>) {
      if (!state.grant) return
      state.grant = undefined
      state.stale = action.payload
    },

    /** The Listener locked up again, or read the notice.
     *
     *  One reducer for both, because they are one thing: stop holding this, and
     *  stop saying anything about it. */
    grantReleased(state) {
      state.grant = undefined
      state.stale = undefined
    },

    /** The notice has been read; the grant (if any) is untouched. */
    staleNoticeDismissed(state) {
      state.stale = undefined
    },
  },
})

export const { grantExpired, grantHeld, grantReleased, staleNoticeDismissed } =
  accessSlice.actions

export const accessReducer = accessSlice.reducer

/** What this browser is holding — read by the base query on **every** request.
 *
 *  Optional-chained on purpose: the base query reads this out of whatever store
 *  it finds itself in, and a store assembled without this slice — which is what
 *  a focused test of another slice builds — must mean *no grant* rather than a
 *  crash on the first request it makes. */
export const selectGrant = (state: {
  access?: AccessState
}): string | undefined => state.access?.grant

/** Why it let go of the last one, if it is owed a sentence about that. */
export const selectStaleGrant = (state: {
  access?: AccessState
}): Stale | undefined => state.access?.stale
