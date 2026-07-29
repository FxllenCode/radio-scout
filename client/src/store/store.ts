import { configureStore } from '@reduxjs/toolkit'
import { setupListeners } from '@reduxjs/toolkit/query'

import {
  loadFeedOff,
  loadSelection,
  namespaceOf,
  saveFeedOff,
  saveSelection,
} from '@/lib/persist'

import { api } from './api'
import { initialLiveState, liveReducer } from './live'
import { playbackReducer } from './playback'
import { transportReducer } from './transport'

/** How a store is told where to remember the selection. Tests pass their own;
 *  the app takes the browser's local storage and the `?id=` in its URL.
 *
 *  `storage` is optional in the type as well as the call because a browser can
 *  genuinely be without one — site data blocked, or a sandboxed context — and
 *  a scanner that runs unremembered is better than one that won't start. */
export interface StoreOptions {
  storage?: Storage
  namespace?: string
}

/** The root store: the RTK Query API slice plus the client-only listening state
 *  ADR-0004 keeps off the server — `live` (the **Selection**, queue, hold,
 *  avoid, history; #11/#12), `playback` (archive results + playback mode, #13),
 *  and `transport` (what the one shared `<audio>` element is doing, #11/#14).
 *
 *  The selection is hydrated from local storage on the way in and written back
 *  whenever it changes (spec US 22) — and only then, so a Call arriving every
 *  few seconds costs nothing. */
export function makeStore(options: StoreOptions = {}) {
  // `'storage' in options`, not a destructuring default: saying `storage:
  // undefined` out loud means "this browser has none", and a default would
  // silently hand back `globalThis.localStorage` instead — the opposite of what
  // was asked for, on the one path whose whole point is not touching it.
  const storage = 'storage' in options ? options.storage : globalThis.localStorage
  const namespace = options.namespace ?? namespaceOf()
  const remembered = storage && loadSelection(storage, namespace)
  const rememberedFeedOff = storage && loadFeedOff(storage, namespace)
  const hydrated = {
    ...(remembered ? { selection: remembered } : {}),
    ...(rememberedFeedOff === undefined ? {} : { feedOff: rememberedFeedOff }),
  }
  const store = configureStore({
    reducer: {
      [api.reducerPath]: api.reducer,
      live: liveReducer,
      playback: playbackReducer,
      transport: transportReducer,
    },
    middleware: (getDefaultMiddleware) =>
      getDefaultMiddleware().concat(api.middleware),
    // Spread unconditionally: with nothing remembered `hydrated` is empty and
    // this is `initialLiveState`, which is what no preloaded state would have
    // given anyway.
    preloadedState: { live: { ...initialLiveState, ...hydrated } },
  })
  // Enables refetchOnFocus / refetchOnReconnect behavior.
  setupListeners(store.dispatch)

  if (storage) {
    let last = store.getState().live.selection
    let lastFeedOff = store.getState().live.feedOff
    store.subscribe(() => {
      const { selection, feedOff } = store.getState().live
      if (selection !== last) {
        last = selection
        saveSelection(storage, namespace, selection)
      }
      // Written on the switch, not on every Call — same reason as above, and a
      // listener flips this once in a session at most.
      if (feedOff !== lastFeedOff) {
        lastFeedOff = feedOff
        saveFeedOff(storage, namespace, feedOff)
      }
    })
  }
  return store
}

/** The app's store. Tests build their own with [`makeStore`] so RTK Query's
 *  cache and the playback queue never leak between them. */
export const store = makeStore()

export type AppStore = ReturnType<typeof makeStore>
export type RootState = ReturnType<AppStore['getState']>
export type AppDispatch = AppStore['dispatch']
