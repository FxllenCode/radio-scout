import { createSlice, type PayloadAction } from '@reduxjs/toolkit'

import type { LiveStatus, Subscription } from '@/lib/liveFeed'
import {
  EVERYTHING,
  isSelected,
  restrictToSystem,
  restrictToTalkgroup,
  setEverything,
  setSystem,
  setTalkgroups,
  type Selection,
  type TalkgroupKey,
} from '@/lib/selection'
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
  /** What the listener has chosen to hear (CONTEXT.md **Selection**, #12).
   *  Held here rather than in a slice of its own because it is the base every
   *  arriving Call is judged against, alongside the hold and the avoids. */
  selection: Selection
  hold: Hold | null
  /** `systemRef:talkgroupRef` → the moment the avoid lapses, `0` for never
   *  (spec US 14's timed 30/60/120 min cycle). */
  avoided: Record<string, number>
  /** Calls the listener will not hear: dropped by the server's `lagged` notice
   *  or by the queue cap. The display admits them rather than hiding them. */
  missed: number
  /** The listener has switched the live feed **off** (CONTEXT.md **Feed off**,
   *  #80) — a hard off, not a pause.
   *
   *  Deliberately not a fourth `status`: `status` is what the *socket* is doing
   *  and `offline` already means the network went, which is the thing CONTEXT.md
   *  says not to confuse this with. Held here so it persists beside the
   *  Selection, and so nothing arriving can override a choice. */
  feedOff: boolean
}

/** The state a listener who has never touched anything starts from. Exported
 *  so the store can hydrate the persisted selection into it (#12). */
export const initialLiveState: LiveState = {
  status: 'offline',
  queue: [],
  current: null,
  history: [],
  playId: 0,
  seen: [],
  selection: EVERYTHING,
  hold: null,
  avoided: {},
  missed: 0,
  // On. A Listener who has never touched the toggle gets audio playing, which
  // is what the app is for.
  feedOff: false,
}

const avoidKey = (systemRef: number, talkgroupRef: number) =>
  `${systemRef}:${talkgroupRef}`

/** The `systemRef:talkgroupRef` key read back as the pair it encodes. */
function parseAvoidKey(key: string): TalkgroupKey {
  const [systemRef, talkgroupRef] = key.split(':')
  return { systemRef: Number(systemRef), talkgroupRef: Number(talkgroupRef) }
}

/**
 * Does the listener still want this Call?
 *
 * Judged against the very matrix the server was sent, so the client can't
 * disagree with the server about what "selected" means. That matters in the
 * window before a new matrix lands, for purging Calls already waiting, and for
 * **patched** Calls (spec US 18): the server delivers a Call that reaches any
 * selected Talkgroup in its patch, and the client must not then throw it away.
 *
 * The matrix is passed in rather than rebuilt per Call — purging walks a queue
 * of up to [`QUEUE_LIMIT`] on a Pi-class phone.
 */
function wants(matrix: Subscription, call: Call): boolean {
  return [call.talkgroupRef, ...(call.patches ?? [])].some((talkgroupRef) =>
    isSelected(matrix, call.systemRef, talkgroupRef),
  )
}

/** [`wants`] against this state's current matrix — the single-Call case. */
function wanted(state: LiveState, call: Call): boolean {
  return wants(matrixOf(state), call)
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

/** Drop whatever the listener no longer wants — after a selection change, a
 *  hold, or an avoid. */
function purge(state: LiveState) {
  const matrix = matrixOf(state)
  state.queue = state.queue.filter((call) => wants(matrix, call))
  if (state.current && !wants(matrix, state.current)) next(state)
}

/**
 * The subscription matrix this state asks the server for (ADR-0004).
 *
 * Three layers, each an exception to the one under it: the **Selection** is the
 * base (#12); a **Hold** narrows it to one System or Talkgroup (spec US 11);
 * and each **Avoid** is a Talkgroup silenced on top (US 14). The server resolves
 * the most specific entry first, so the layers survive as a single flat matrix.
 */
function matrixOf(state: LiveState): Subscription {
  const { hold, selection } = state
  const held =
    hold === null
      ? selection
      : hold.talkgroupRef === null
        ? restrictToSystem(selection, hold.systemRef)
        : restrictToTalkgroup(hold.systemRef, hold.talkgroupRef)

  return silenced(held, state)
}

/** `base` with every avoided Talkgroup turned off on top of it. */
function silenced(base: Selection, state: LiveState): Selection {
  return setTalkgroups(base, Object.keys(state.avoided).map(parseAvoidKey), false)
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
  initialState: initialLiveState,
  reducers: {
    /** Turn Talkgroups on or off — one row in the panel, or every Talkgroup
     *  behind a Group/Tag category chip (spec US 19–20). Selecting one lifts
     *  any avoid on it: the panel would otherwise show it on while the avoid
     *  kept it silent. */
    chooseTalkgroups(
      state,
      action: PayloadAction<{ keys: TalkgroupKey[]; on: boolean }>,
    ) {
      const { keys, on } = action.payload
      state.selection = setTalkgroups(state.selection, keys, on)
      if (on) {
        for (const key of keys) {
          delete state.avoided[avoidKey(key.systemRef, key.talkgroupRef)]
        }
      }
      purge(state)
    },

    /** A System's all-on / all-off (spec US 21). "All on" means all on, so it
     *  lifts that System's avoids too. */
    chooseSystem(state, action: PayloadAction<{ systemRef: number; on: boolean }>) {
      const { systemRef, on } = action.payload
      state.selection = setSystem(state.selection, systemRef, on)
      if (on) {
        for (const key of Object.keys(state.avoided)) {
          if (parseAvoidKey(key).systemRef === systemRef) delete state.avoided[key]
        }
      }
      purge(state)
    },

    /** The global all-on / all-off (spec US 21), avoids with it. */
    chooseEverything(state, action: PayloadAction<boolean>) {
      state.selection = setEverything(action.payload)
      if (action.payload) state.avoided = {}
      purge(state)
    },

    connecting(state) {
      state.status = 'connecting'
    },

    connected(state) {
      state.status = 'connected'
    },

    disconnected(state) {
      state.status = 'offline'
    },

    /**
     * Switch the live feed off (#80) — the master off rdio's LIVE FEED button
     * has and Pause is not.
     *
     * A **hard** off, per CONTEXT.md: the Call playing stops, the queue clears,
     * and the cursor goes so that coming back starts from now rather than
     * replaying the silence. Closing the socket is `LiveFeedLink`'s half — that
     * is what drops bandwidth and battery to zero, and what hands the listener
     * back to Web Push, whose "an open socket means someone is listening" rule
     * now tells the truth.
     *
     * `missed` is untouched on purpose. It admits traffic the listener *wanted*
     * and did not get; silence they chose is not a gap they missed.
     */
    turnFeedOff(state) {
      state.feedOff = true
      state.queue = []
      // `play(…, null)` rather than clearing `current`, so the Call being cut
      // off is filed under history: switched off is not the same as never heard,
      // and it should be there to replay once the feed is back.
      play(state, null)
      state.since = undefined
    },

    /** Back on. The socket reconnects and subscribes fresh — with no cursor,
     *  there is nothing to backfill. The Selection, the hold and the avoids are
     *  all as the listener left them. */
    turnFeedOn(state) {
      state.feedOff = false
    },

    /** A Call arrived over the feed: play it if the feed is quiet, else queue
     *  it. `catchup` Calls (ADR-0004 reconnect backfill) arrive the same way —
     *  a listener coming back wants to hear what they missed. */
    received(state, action: PayloadAction<{ call: Call; catchup?: boolean }>) {
      const { call } = action.payload
      // Off means off (#80). A Call still in flight when the socket closed, or
      // one the server sent before it noticed, is not played and not counted:
      // the listener asked for silence, and this is what that costs.
      if (state.feedOff) return
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
      // Nothing plays while the feed is off (#80). The Replay control is
      // disabled there, but replay is reachable from the RECENT list and from
      // the lock screen's previous button (`previousCall`), and a Call started
      // from either would be audio playing under a FEED OFF header with the
      // socket shut.
      if (state.feedOff) return
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
      // Same promise `turnFeedOff` keeps (#80): `missed` admits traffic the
      // listener *wanted* and did not get. A notice buffered on the socket, or
      // in flight while it closes, arrives after the switch — and charging them
      // for silence they chose would make the counter a lie.
      if (state.feedOff) return
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
  chooseEverything,
  chooseSystem,
  chooseTalkgroups,
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
  turnFeedOff,
  turnFeedOn,
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

/** Whether the listener has switched the live feed off (#80). Distinct from
 *  `status === 'offline'`, which is the network's answer rather than theirs. */
export const selectIsFeedOff = (state: WithLive): boolean => state.live.feedOff

export const selectIsAvoided = (
  state: WithLive,
  systemRef: number,
  talkgroupRef: number,
): boolean => selectAvoidUntil(state, systemRef, talkgroupRef) !== undefined

/** When a Talkgroup's avoid lapses — `0` for "until the listener says
 *  otherwise", `undefined` for one that isn't avoided. The Talkgroups panel
 *  shows the difference: a timed avoid is coming back on its own (#12). */
export const selectAvoidUntil = (
  state: WithLive,
  systemRef: number,
  talkgroupRef: number,
): number | undefined => state.live.avoided[avoidKey(systemRef, talkgroupRef)]

/** What the listener has chosen to hear, before a hold or an avoid narrows it
 *  (#12) — what the Talkgroups panel draws and what is persisted. */
export const selectSelection = (state: WithLive): Selection => state.live.selection

/**
 * The selection as the Talkgroups panel draws it: what the listener will
 * actually hear, so an avoided Talkgroup reads off there and the panel's counts
 * agree with its rows.
 *
 * A **hold** is deliberately not folded in. It is a temporary narrowing the
 * Live screen owns (spec US 11) — showing it here would make the panel claim
 * the listener had deselected every other System.
 */
export const selectAudibleSelection = (state: WithLive): Selection =>
  silenced(state.live.selection, state.live)

/** The subscription matrix this state asks the server for (ADR-0004) —
 *  selection, hold and avoids flattened into one, per [`matrixOf`]. */
export const selectLiveMatrix = (state: WithLive): Subscription =>
  matrixOf(state.live)
