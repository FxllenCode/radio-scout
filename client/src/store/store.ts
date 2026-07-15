import { configureStore } from '@reduxjs/toolkit'
import { setupListeners } from '@reduxjs/toolkit/query'

import { api } from './api'

/** The root store. Client-only listening state (selection, queue, playback,
 *  hold/avoid) will be added as RTK slices in #11/#12; for now the store hosts
 *  the RTK Query API slice. */
export const store = configureStore({
  reducer: {
    [api.reducerPath]: api.reducer,
  },
  middleware: (getDefaultMiddleware) =>
    getDefaultMiddleware().concat(api.middleware),
})

// Enables refetchOnFocus / refetchOnReconnect behavior.
setupListeners(store.dispatch)

export type RootState = ReturnType<typeof store.getState>
export type AppDispatch = typeof store.dispatch
