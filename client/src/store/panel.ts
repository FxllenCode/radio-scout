/**
 * How the Talkgroups panel is *arranged* (#57, spec US 29) — what is pinned,
 * which Systems are folded away, and what order the rows are in.
 *
 * A slice of its own rather than more fields on `./live`, and the line between
 * them is what the state can change: nothing here changes what plays. A **Pin**
 * is "a panel-ordering affordance only" (CONTEXT.md) and so are the other two,
 * where every field in `live` is either the **Selection** the server is sent or
 * something arriving over it. So a bug in here can misdraw the panel and can
 * never silence a Talkgroup.
 *
 * It is remembered per **Profile** all the same (`lib/persist`), because a
 * Listener who pinned their six daily channels out of four hundred has done
 * real work, and a Listener who opened the county System did too.
 */
import { createSlice, type PayloadAction } from '@reduxjs/toolkit'

import type { PanelMemory, PanelSort } from '@/lib/panel'

/** Exactly what [`panelOf`] arranges the panel by, and exactly what is
 *  remembered of it — one shape, so the slice, the persistence layer and the
 *  derivation cannot disagree about what a panel's arrangement *is*. */
export type PanelState = PanelMemory

export const initialPanelState: PanelState = { sort: 'name', pinned: [], expanded: {} }

const panelSlice = createSlice({
  name: 'panel',
  initialState: initialPanelState,
  reducers: {
    /** Hold this Talkgroup at the top of the panel, or let it go (spec US 29). */
    togglePin(state, action: PayloadAction<string>) {
      const key = action.payload
      state.pinned = state.pinned.includes(key)
        ? state.pinned.filter((pinned) => pinned !== key)
        : [...state.pinned, key]
    },

    /** Open or fold away a System's section. The value is what the Listener
     *  asked for, not a toggle: the state it is toggling *from* is the size
     *  rule, which this slice cannot see and should not learn. */
    showSystem(state, action: PayloadAction<{ systemRef: number; shown: boolean }>) {
      state.expanded[action.payload.systemRef] = action.payload.shown
    },

    sortPanel(state, action: PayloadAction<PanelSort>) {
      state.sort = action.payload
    },
  },
})

export const { showSystem, sortPanel, togglePin } = panelSlice.actions

export const panelReducer = panelSlice.reducer

/** The slice of the store this module owns. */
interface WithPanel {
  panel: PanelState
}

export const selectPinned = (state: WithPanel): string[] => state.panel.pinned

export const selectExpandedSystems = (state: WithPanel): Record<number, boolean> =>
  state.panel.expanded

export const selectPanelSort = (state: WithPanel): PanelSort => state.panel.sort
