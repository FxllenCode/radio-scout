import { createSlice, type PayloadAction } from '@reduxjs/toolkit'

import type { LiveStatus, Subscription } from '@/lib/liveFeed'
import type { Call } from '@/types'

import { enterPlaybackMode } from './playback'

/** What the listener has narrowed the feed to: a whole System, or one
 *  Talkgroup within it (CONTEXT.md **Hold**). */
export interface Hold {
  systemRef: number
  /** `null` holds the whole System. */
  talkgroupRef: number | null
}

/** A hold on a whole System, versus one on a single Talkgroup. Named once so
 *  the reducers and the display can't drift apart on what `talkgroupRef: null`
 *  means. */
export const isSystemHold = (hold: Hold | null): boolean =>
  hold?.talkgroupRef === null

export const isTalkgroupHold = (hold: Hold | null): boolean =>
  hold?.talkgroupRef != null

/** How many played Calls stay replayable (spec US 13: "back through the last
 *  five"). */
export const HISTORY_DEPTH = 5

/** Ceiling on the listening queue. A phone that fell far behind must not grow
 *  an unbounded queue of stale traffic on a Pi-class device; past this the
 *  oldest waiting Calls are dropped and counted as missed. */
export const QUEUE_LIMIT = 100

/** How many Call ids are remembered for de-duplication. Catch-up delivery is
 *  *at-least-once* (ADR-0004), so a Call can arrive twice; ids are compared as
 *  a set rather than a high-water mark because concurrent ingests can broadcast
 *  out of id order, and a watermark would drop the late one. */
const SEEN_LIMIT = 256

/** The Talkgroup key the server reads as "every Talkgroup in this System". */
const WILDCARD = '*'

export interface LiveState {
  status: LiveStatus
  /** Calls waiting to play, oldest first (CONTEXT.md **Listening queue**). */
  queue: Call[]
  /** What the feed is playing now. */
  current: Call | null
  /** Recently played, newest first — what Replay walks. */
  history: Call[]
  /** Bumped whenever playback (re)starts, so replaying the Call already loaded
   *  still restarts the element, whose `src` never changed. */
  playId: number
  /** Highest Call id seen: the `since` cursor for reconnect catch-up. */
  since?: number
  /** Recently seen Call ids, oldest first — what makes at-least-once delivery
   *  idempotent for the listener. */
  seen: number[]
  hold: Hold | null
  /** `systemRef:talkgroupRef` → the moment the avoid lapses, `0` for never
   *  (spec US 14's timed 30/60/120 min cycle). */
  avoided: Record<string, number>
  /** Calls the listener will not hear: dropped by the server's `lagged` notice
   *  or by the queue cap. The display admits them rather than hiding them. */
  missed: number
}

const initialState: LiveState = {
  status: 'offline',
  queue: [],
  current: null,
  history: [],
  playId: 0,
  seen: [],
  hold: null,
  avoided: {},
  missed: 0,
}

const avoidKey = (systemRef: number, talkgroupRef: number) =>
  `${systemRef}:${talkgroupRef}`

/** Does the listener still want this Call, given the hold and the avoid list?
 *  The server filters too — this is what keeps the queue honest in the window
 *  before a new matrix lands, and what purges Calls already waiting. */
function wanted(state: LiveState, call: Call): boolean {
  if (state.avoided[avoidKey(call.systemRef, call.talkgroupRef)] !== undefined) {
    return false
  }
  const { hold } = state
  if (!hold) return true
  if (hold.systemRef !== call.systemRef) return false
  return hold.talkgroupRef === null || hold.talkgroupRef === call.talkgroupRef
}

/** Start `call`, filing whatever was playing under history. */
function play(state: LiveState, call: Call | null) {
  if (state.current) {
    state.history.unshift(state.current)
    state.history = state.history.slice(0, HISTORY_DEPTH)
  }
  state.current = call
  state.playId += 1
}

/** Take the next Call off the queue, or fall quiet. */
function next(state: LiveState) {
  play(state, state.queue.shift() ?? null)
}

/** Drop whatever the listener no longer wants — after a hold or an avoid. */
function purge(state: LiveState) {
  state.queue = state.queue.filter((call) => wanted(state, call))
  if (state.current && !wanted(state, state.current)) next(state)
}

/**
 * The live feed as the listener experiences it (#11, spec US 9–17).
 *
 * Everything here is *client* state, per ADR-0004: the server holds only a
 * subscription matrix per connection, and hold, avoid, skip, replay, history
 * and the queue live here. What the server does hold is derived from this slice
 * ([`selectLiveMatrix`]) and re-sent whenever it changes, so narrowing the feed
 * stops Calls reaching the device at all rather than filtering them on arrival —
 * bandwidth and battery, which is the point of server-side filtering.
 */
const liveSlice = createSlice({
  name: 'live',
  initialState,
  reducers: {
    connecting(state) {
      state.status = 'connecting'
    },

    connected(state) {
      state.status = 'connected'
    },

    disconnected(state) {
      state.status = 'offline'
    },

    /** A Call arrived over the feed: play it if the feed is quiet, else queue
     *  it. `catchup` Calls (ADR-0004 reconnect backfill) arrive the same way —
     *  a listener coming back wants to hear what they missed. */
    received(state, action: PayloadAction<{ call: Call; catchup?: boolean }>) {
      const { call } = action.payload
      // Catch-up is at-least-once (ADR-0004): a Call ingested in the window
      // between connect and the backfill query arrives twice, and hearing it
      // twice is the listener's problem to be spared.
      if (state.seen.includes(call.id)) return
      state.seen.push(call.id)
      if (state.seen.length > SEEN_LIMIT) state.seen.shift()

      // The cursor counts every Call the server sent, even one filtered out
      // here, or a reconnect would ask for it again.
      state.since = Math.max(state.since ?? 0, call.id)

      if (!wanted(state, call)) return
      if (!state.current) {
        play(state, call)
        return
      }
      state.queue.push(call)
      if (state.queue.length > QUEUE_LIMIT) {
        // The stalest go first, and are admitted rather than vanishing.
        state.missed += state.queue.length - QUEUE_LIMIT
        state.queue = state.queue.slice(-QUEUE_LIMIT)
      }
    },

    /** The current Call finished, or the listener skipped it (spec US 12). */
    advance(state) {
      if (!state.current) return
      next(state)
    },

    /** Play a Call again — the one playing, or one from the history (spec
     *  US 13). The queue behind it is untouched. */
    replay(state, action: PayloadAction<number>) {
      const again =
        state.current?.id === action.payload
          ? state.current
          : state.history.find((call) => call.id === action.payload)
      if (!again) return

      // Reaching back past the Call that was playing files *it* under history
      // too, so the listener can get back to what they interrupted.
      if (state.current && state.current.id !== again.id) {
        play(state, again)
        return
      }
      state.current = again
      state.playId += 1
    },

    /** Narrow to the System that's talking, or let it go again. */
    toggleHoldSystem(state) {
      if (isSystemHold(state.hold)) {
        state.hold = null
        return
      }
      if (!state.current) return
      state.hold = { systemRef: state.current.systemRef, talkgroupRef: null }
      purge(state)
    },

    /** Narrow to the Talkgroup that's talking, or let it go again. */
    toggleHoldTalkgroup(state) {
      if (isTalkgroupHold(state.hold)) {
        state.hold = null
        return
      }
      if (!state.current) return
      state.hold = {
        systemRef: state.current.systemRef,
        talkgroupRef: state.current.talkgroupRef,
      }
      purge(state)
    },

    /** Mute the Talkgroup that's talking until `until` (0 = until released).
     *  The moment is computed by the caller so this stays a pure reducer. */
    avoid(state, action: PayloadAction<{ until: number }>) {
      const call = state.current
      if (!call) return

      state.avoided[avoidKey(call.systemRef, call.talkgroupRef)] =
        action.payload.until
      // Holding a Talkgroup you've just muted is a contradiction; the avoid is
      // the newer intent.
      if (state.hold?.talkgroupRef === call.talkgroupRef) state.hold = null
      purge(state)
    },

    /** Let a Talkgroup back in before its time is up — an indefinite avoid has
     *  no other way back (spec US 14's timed mode is the *optional* one). */
    clearAvoids(state) {
      state.avoided = {}
    },

    /** Let every avoid whose time has come lapse (spec US 14's auto-reactivate).
     *  `now` is passed in rather than read, so the reducer stays pure. */
    expireAvoids(state, action: PayloadAction<number>) {
      for (const [key, until] of Object.entries(state.avoided)) {
        if (until !== 0 && until <= action.payload) delete state.avoided[key]
      }
    },

    /** The server told us a slow connection cost us Calls (ADR-0004 `lagged`). */
    lagged(state, action: PayloadAction<number>) {
      state.missed += action.payload
    },
  },

  extraReducers: (builder) => {
    // The live feed and playback mode are mutually exclusive (CONTEXT.md): going
    // to the archive silences the feed here *and*, via the matrix, stops the
    // server sending it. The cursor survives so coming back doesn't refetch the
    // world.
    builder.addCase(enterPlaybackMode, (state) => {
      state.queue = []
      state.current = null
      state.history = []
      state.playId += 1
    })
  },
})

export const {
  advance,
  avoid,
  clearAvoids,
  connected,
  connecting,
  disconnected,
  expireAvoids,
  lagged,
  received,
  replay,
  toggleHoldSystem,
  toggleHoldTalkgroup,
} = liveSlice.actions

export const liveReducer = liveSlice.reducer

/** The slice of the store this module owns. */
interface WithLive {
  live: LiveState
}

export const selectLiveStatus = (state: WithLive): LiveStatus => state.live.status

export const selectLiveCall = (state: WithLive): Call | null => state.live.current

/** The `Q` count on the display. */
export const selectQueueDepth = (state: WithLive): number => state.live.queue.length

/** The Calls waiting their turn, oldest first. */
export const selectQueue = (state: WithLive): Call[] => state.live.queue

export const selectHistory = (state: WithLive): Call[] => state.live.history

export const selectPlayId = (state: WithLive): number => state.live.playId

export const selectHold = (state: WithLive): Hold | null => state.live.hold

/** How many Talkgroups are muted right now (spec US 14). */
export const selectAvoidedCount = (state: WithLive): number =>
  Object.keys(state.live.avoided).length

export const selectSince = (state: WithLive): number | undefined => state.live.since

export const selectMissed = (state: WithLive): number => state.live.missed

export const selectIsAvoided = (
  state: WithLive,
  systemRef: number,
  talkgroupRef: number,
): boolean => state.live.avoided[avoidKey(systemRef, talkgroupRef)] !== undefined

/**
 * The subscription matrix this state asks the server for (ADR-0004).
 *
 * A listener starts on everything; a hold narrows to one System (`"*"`, since
 * the client can only name the Talkgroups it has heard) or one Talkgroup; and
 * each avoid is an explicit exception layered on top — which is why the server
 * resolves the most specific entry first.
 */
export const selectLiveMatrix = (state: WithLive): Subscription => {
  const { hold, avoided } = state.live
  const sel: Record<string, Record<string, boolean>> = {}

  if (hold) {
    sel[hold.systemRef] = {
      [hold.talkgroupRef ?? WILDCARD]: true,
    }
  }
  for (const key of Object.keys(avoided)) {
    const [systemRef, talkgroupRef] = key.split(':')
    sel[systemRef] = { ...sel[systemRef], [talkgroupRef]: false }
  }
  return { all: hold === null, sel }
}
