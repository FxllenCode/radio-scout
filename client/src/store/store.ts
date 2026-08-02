import { configureStore } from '@reduxjs/toolkit'
import { setupListeners } from '@reduxjs/toolkit/query'

import {
  loadAvoids,
  loadFeedOff,
  loadHold,
  loadSelection,
  namespaceOf,
  saveAvoids,
  saveFeedOff,
  saveHold,
  saveSelection,
} from '@/lib/persist'

import { api } from './api'
import { createAvoidClock } from './avoids'
import { expireAvoids, initialLiveState, liveReducer, type LiveState } from './live'
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
  // Read against the clock, so an **Avoid** whose hour ran out while the tab
  // was closed is simply not in force on the way back in (#91).
  const rememberedAvoids = storage && loadAvoids(storage, namespace, Date.now())
  const rememberedHold = storage && loadHold(storage, namespace)
  const hydrated = {
    ...(remembered ? { selection: remembered } : {}),
    ...(rememberedFeedOff === undefined ? {} : { feedOff: rememberedFeedOff }),
    ...(rememberedAvoids ? { avoided: rememberedAvoids } : {}),
    ...(rememberedHold === undefined ? {} : { hold: rememberedHold }),
  }
  const store = configureStore({
    reducer: {
      [api.reducerPath]: api.reducer,
      live: liveReducer,
      playback: playbackReducer,
      transport: transportReducer,
    },
    middleware: (getDefaultMiddleware) =>
      // Prepended, as RTK requires of a listener middleware, so an effect sees
      // the action before anything downstream can stop it.
      getDefaultMiddleware()
        .prepend(createAvoidClock().middleware)
        .concat(api.middleware),
    // Spread unconditionally: with nothing remembered `hydrated` is empty and
    // this is `initialLiveState`, which is what no preloaded state would have
    // given anyway.
    preloadedState: { live: { ...initialLiveState, ...hydrated } },
  })
  // Enables refetchOnFocus / refetchOnReconnect behavior.
  setupListeners(store.dispatch)

  if (storage) {
    /** Write `read`'s value out whenever it changes, and never otherwise — a
     *  Call arrives every few seconds, and persisting on each one would cost a
     *  phone its battery for nothing. */
    const remember = <T,>(read: (live: LiveState) => T, save: (value: T) => void) => {
      let last = read(store.getState().live)
      store.subscribe(() => {
        const next = read(store.getState().live)
        if (next === last) return
        last = next
        save(next)
      })
    }

    // What a **Profile** is, per CONTEXT.md: its Selection, its Avoid list and
    // its Hold state — plus the feed-off switch #80 added.
    remember((live) => live.selection, (it) => saveSelection(storage, namespace, it))
    remember((live) => live.avoided, (it) => saveAvoids(storage, namespace, it))
    remember((live) => live.hold, (it) => saveHold(storage, namespace, it))
    remember((live) => live.feedOff, (it) => saveFeedOff(storage, namespace, it))
  }

  // A store hydrated holding an Avoid has had no action to wake its clock with
  // (`./avoids`) — preloaded state arrives without one. This is that action:
  // what had already lapsed was dropped on the way in, so it changes nothing,
  // and its only job is to get the next deadline scheduled. Dispatched
  // unconditionally rather than behind "is there a deadline to schedule", which
  // is the clock's own rule and belongs to the clock.
  store.dispatch(expireAvoids(Date.now()))

  return store
}

/** The app's store. Tests build their own with [`makeStore`] so RTK Query's
 *  cache and the playback queue never leak between them. */
export const store = makeStore()

export type AppStore = ReturnType<typeof makeStore>
export type RootState = ReturnType<AppStore['getState']>
export type AppDispatch = AppStore['dispatch']
