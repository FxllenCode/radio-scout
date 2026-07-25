import { configureStore } from '@reduxjs/toolkit'
import { setupListeners } from '@reduxjs/toolkit/query'

import { api } from './api'
import { liveReducer } from './live'
import { playbackReducer } from './playback'
import { transportReducer } from './transport'

/** The root store: the RTK Query API slice plus the client-only listening state
 *  ADR-0004 keeps off the server — `live` (queue, hold, avoid, history, #11),
 *  `playback` (archive results + playback mode, #13), and `transport` (what the
 *  one shared `<audio>` element is doing, #11/#14). The Talkgroup selection
 *  joins them in #12. */
export function makeStore() {
  const store = configureStore({
    reducer: {
      [api.reducerPath]: api.reducer,
      live: liveReducer,
      playback: playbackReducer,
      transport: transportReducer,
    },
    middleware: (getDefaultMiddleware) =>
      getDefaultMiddleware().concat(api.middleware),
  })
  // Enables refetchOnFocus / refetchOnReconnect behavior.
  setupListeners(store.dispatch)
  return store
}

/** The app's store. Tests build their own with [`makeStore`] so RTK Query's
 *  cache and the playback queue never leak between them. */
export const store = makeStore()

export type AppStore = ReturnType<typeof makeStore>
export type RootState = ReturnType<AppStore['getState']>
export type AppDispatch = AppStore['dispatch']
