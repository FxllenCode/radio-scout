import { configureStore } from '@reduxjs/toolkit'
import { setupListeners } from '@reduxjs/toolkit/query'

import { api } from './api'
import { playbackReducer } from './playback'

/** The root store: the RTK Query API slice plus the client-only listening
 *  state. `playback` (archive queue + playback mode, #13) is here; selection,
 *  hold/avoid and the live listening queue join it in #11/#12. */
export function makeStore() {
  const store = configureStore({
    reducer: {
      [api.reducerPath]: api.reducer,
      playback: playbackReducer,
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
