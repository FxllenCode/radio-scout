import { createSlice, type PayloadAction } from '@reduxjs/toolkit'

import type { Call } from '@/types'

import type { Subscription } from '@/lib/liveFeed'

import {
  advance,
  replay,
  selectHistory,
  selectLiveCall,
  selectLiveMatrix,
  selectPlayId,
  selectQueue,
  type LiveState,
} from './live'
import {
  next,
  previous,
  selectCurrentCall,
  selectNextCall,
  selectPlaybackMode,
  type PlaybackState,
} from './playback'
import type { AppDispatch } from './store'

/**
 * The transport: what the one `<audio>` element is doing, and who it belongs to.
 *
 * Two sources feed that element — the live feed (#11) and the archive (#13) —
 * and ADR-0005 gives us exactly one of it. So "is it paused" and "how far in"
 * are asked once, here, rather than once per source where the two could
 * disagree; and the controls (in-app and lock-screen alike) dispatch *intents*
 * that this module routes to whichever source currently owns the audio.
 */
export interface TransportState {
  /** Held rather than stopped: the Call keeps its place, the element is paused
   *  (spec US 15). */
  paused: boolean
  /** Seconds into the current Call, as the element last reported. */
  position: number
  /** The current Call's length in seconds, `0` until the element knows it. */
  duration: number
}

const initialState: TransportState = {
  paused: false,
  position: 0,
  duration: 0,
}

const transportSlice = createSlice({
  name: 'transport',
  initialState,
  reducers: {
    pause(state) {
      state.paused = true
    },

    resume(state) {
      state.paused = false
    },

    /** The element was handed a different Call — by either source, or by a
     *  replay of the one already loaded. A pause that outlived the Call it
     *  applied to would leave the store claiming "paused" over audible audio,
     *  and last Call's position would draw over this one's waveform. */
    sourceChanged(state) {
      state.paused = false
      state.position = 0
      state.duration = 0
    },

    progressed(
      state,
      action: PayloadAction<{ position: number; duration: number }>,
    ) {
      state.position = action.payload.position
      state.duration = action.payload.duration
    },
  },
})

export const { pause, progressed, resume, sourceChanged } = transportSlice.actions

export const transportReducer = transportSlice.reducer

/** Everything the transport reads across the store. */
interface TransportRoot {
  transport: TransportState
  live: LiveState
  playback: PlaybackState
}

/**
 * The Call on the element right now.
 *
 * An archived Call wins: it is either interrupting the live feed (spec US 26)
 * or playing with the feed off (US 25). Either way it is what the listener
 * hears, and the feed's own Call waits underneath.
 */
export const selectNowPlaying = (state: TransportRoot): Call | null =>
  selectCurrentCall(state) ?? selectLiveCall(state)

/** Is the live feed the source on the element? (The archive isn't playing.) */
export const selectIsLiveSource = (state: TransportRoot): boolean =>
  selectCurrentCall(state) === null

/**
 * The Call the player should warm the HTTP cache with (#14): whatever plays
 * next *from the source that owns the audio* — the next archive result, or the
 * head of the listening queue.
 */
export const selectUpcomingCall = (state: TransportRoot): Call | null =>
  selectIsLiveSource(state) ? (selectQueue(state)[0] ?? null) : selectNextCall(state)

/** Bumped whenever playback (re)starts — including a replay of the Call already
 *  loaded, which the element would otherwise ignore. */
export const selectSourceId = (state: TransportRoot): number =>
  selectPlayId(state)

export const selectIsPaused = (state: TransportRoot): boolean =>
  state.transport.paused

/** How far through the current Call, in `[0, 1]` — `0` until the element
 *  reports a duration to measure against. */
export const selectProgress = (state: TransportRoot): number => {
  const { position, duration } = state.transport
  if (!Number.isFinite(duration) || duration <= 0) return 0
  return Math.min(1, position / duration)
}

/**
 * What the client asks the server to send it (ADR-0004).
 *
 * Playback mode turns the live feed off (CONTEXT.md: they are mutually
 * exclusive), and "off" should mean the server stops sending — not merely that
 * the client stops playing. Otherwise a listener browsing the archive keeps
 * paying for a feed they can't hear.
 */
export const selectSubscription = (state: TransportRoot): Subscription =>
  selectPlaybackMode(state) === 'playback'
    ? { all: false, sel: {} }
    : selectLiveMatrix(state)

/** Skip forward: the next archive result, or the next Call in the queue. */
export const nextCall =
  () => (dispatch: AppDispatch, getState: () => TransportRoot) => {
    dispatch(selectIsLiveSource(getState()) ? advance() : next())
  }

/**
 * Step back. The archive walks its results; the live feed has no "before now",
 * so its previous is a **replay** of the last Call played (spec US 13) — which
 * is also what a lock-screen back button should do on a scanner.
 */
export const previousCall =
  () => (dispatch: AppDispatch, getState: () => TransportRoot) => {
    const state = getState()
    if (!selectIsLiveSource(state)) {
      dispatch(previous())
      return
    }
    const last = selectHistory(state)[0]
    if (last) dispatch(replay(last.id))
  }

/** One button for both meanings, since the element has only one state. */
export const togglePause =
  () => (dispatch: AppDispatch, getState: () => TransportRoot) => {
    dispatch(selectIsPaused(getState()) ? resume() : pause())
  }
