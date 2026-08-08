import { createSelector, createSlice, type PayloadAction } from '@reduxjs/toolkit'

import {
  controlsFor,
  feedPlays,
  feedStatus,
  type Controls,
  type FeedStatus,
} from '@/lib/feed'
import type { LiveStatus, Subscription } from '@/lib/liveFeed'
import { enqueue, retain, takeNext, type QueuePolicy } from '@/lib/queue'
import {
  EVERYTHING,
  avoidKey,
  isSelected,
  isSystemHold,
  isTalkgroupHold,
  parseAvoidKey,
  silenced,
  restrictToSystem,
  restrictToTalkgroup,
  setEverything,
  setSystem,
  setTalkgroups,
  type Avoids,
  type Hold,
  type Selection,
  type TalkgroupKey,
} from '@/lib/selection'
import type { Call } from '@/types'

import { enterLiveFeed, enterPlaybackMode } from './playback'

/** How many played Calls stay replayable (spec US 13: "back through the last
 *  five"). */
export const HISTORY_DEPTH = 5

/** Ceiling on the listening queue. A phone that fell far behind must not grow
 *  an unbounded queue of stale traffic on a Pi-class device; past this
 *  [`QUEUE_POLICY`] decides what goes, and it is counted as missed. */
export const QUEUE_LIMIT = 100

/**
 * How the listening queue orders itself (#95, `@/lib/queue`).
 *
 * **FIFO** — no `priorityOf` — because nothing yet gives a Listener a way to
 * mark a Talkgroup **Priority**; #58 is where that arrives, and it arrives
 * *here*, as one field, rather than as an edit to the three reducers below and
 * the cap rule. The cap already honours a Priority the day one exists.
 */
const QUEUE_POLICY: QueuePolicy = { limit: QUEUE_LIMIT }

/** How many Call ids are remembered for de-duplication. Catch-up delivery is
 *  *at-least-once* (ADR-0004), so a Call can arrive twice; ids are compared as
 *  a set rather than a high-water mark because concurrent ingests can broadcast
 *  out of id order, and a watermark would drop the late one. */
const SEEN_LIMIT = 256

export interface LiveState {
  status: LiveStatus
  /** Calls waiting to play, **in the order they will play** (CONTEXT.md
   *  **Listening queue**) — arrival order until a Talkgroup has **Priority**,
   *  and the policy's order after (`@/lib/queue`, #95). */
  queue: Call[]
  /** What the feed is playing now. */
  current: Call | null
  /** Recently played, newest first — what Replay walks. */
  history: Call[]
  /** Bumped whenever playback (re)starts, so replaying the Call already loaded
   *  still restarts the element, whose `src` never changed. */
  playId: number
  /** Highest **emission** the server has sent: the `since` cursor for a
   *  **Backfill** (#94). Not the highest Call id — a Call a **Delay** held back
   *  is stored early and goes out late, so its id says nothing about what this
   *  listener has already heard. */
  since?: number
  /** Recently seen Call ids, oldest first — what makes at-least-once delivery
   *  idempotent for the listener. */
  seen: number[]
  /** What the listener has chosen to hear (CONTEXT.md **Selection**, #12).
   *  Held here rather than in a slice of its own because it is the base every
   *  arriving Call is judged against, alongside the hold and the avoids. */
  selection: Selection
  hold: Hold | null
  /** Every **Avoid** in force, by [`avoidKey`] (spec US 14's timed
   *  30/60/120 min cycle). */
  avoided: Avoids
  /** Calls the listener will not hear: dropped by the server's `lagged` notice
   *  or by the queue cap. The display admits them rather than hiding them. */
  missed: number
  /** A **Backfill** could not reach back as far as we asked, so there is a hole
   *  in this listener's history that only archive search can fill (ADR-0004).
   *
   *  A flag rather than a count, because the server cannot say how many it could
   *  not carry without counting the whole archive — and "some" is what the
   *  listener needs to know either way. It does not clear: a gap in what someone
   *  heard does not heal. */
  gap: boolean
  /** The listener has switched the live feed **off** (CONTEXT.md **Feed off**,
   *  #80) — a hard off, not a pause.
   *
   *  Deliberately not a fourth `status`: `status` is what the *socket* is doing
   *  and `offline` already means the network went, which is the thing CONTEXT.md
   *  says not to confuse this with. Held here so it persists beside the
   *  Selection, and so nothing arriving can override a choice. */
  feedOff: boolean
  /**
   * The listener is playing the Archive instead (CONTEXT.md **Playback mode**).
   *
   * Not a mirror of the `playback` slice so much as this slice remembering
   * *why* it went quiet: it already emptied itself when playback mode began and
   * then forgot the reason immediately, which is how a Call landing a moment
   * later started playing over the archive (#88). Set by the two actions that
   * are the only way to change modes, so the two cannot disagree — and a
   * reducer, which can see no other slice, can still refuse.
   *
   * Not persisted, unlike [`feedOff`]: a reload starts on the live feed.
   */
  inPlayback: boolean
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
  gap: false,
  // On. A Listener who has never touched the toggle gets audio playing, which
  // is what the app is for.
  feedOff: false,
  inPlayback: false,
}

/**
 * Why the live feed is or is not delivering, from this slice alone (#88).
 *
 * The whole of the answer is here — the listener's switch, the mode they are
 * in, and the socket's condition — so the reducers below and the display read
 * one derived value rather than each assembling their own from the parts.
 */
const statusOf = (state: LiveState): FeedStatus =>
  feedStatus({ off: state.feedOff, playback: state.inPlayback, link: state.status })

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

/**
 * Start `call`, filing whatever was playing under history.
 *
 * **The Call playing is never also in the history** (#82). Replay reaches back
 * into that list to choose what to play next, so without this the Call it picked
 * stayed there while it played — visible in RECENT as something "recently
 * played" that is in fact playing now — and was filed a *second* time when it
 * finished, leaving the list holding it twice and `<li key={call.id}>` handing
 * React two children with the same key.
 *
 * Enforced here rather than in `replay` because it is an invariant of the pair
 * of fields, not a quirk of one caller: `next`, `purge` and the lock screen all
 * arrive through this function, and any of them could reach for a Call the list
 * is holding.
 */
function play(state: LiveState, call: Call | null) {
  if (state.current) {
    state.history.unshift(state.current)
  }
  if (call) {
    state.history = state.history.filter((one) => one.id !== call.id)
  }
  state.history = state.history.slice(0, HISTORY_DEPTH)
  state.current = call
  state.playId += 1
}

/** Take the next Call off the queue, or fall quiet. Which Call that is belongs
 *  to the policy (`@/lib/queue`), not to this reducer. */
function next(state: LiveState) {
  const taken = takeNext(state.queue)
  state.queue = taken.queue
  play(state, taken.next)
}

/**
 * Let every **Avoid** whose deadline has passed lapse (spec US 14's
 * auto-reactivate).
 *
 * The one place a deadline is compared to a clock, and the clock is always
 * handed in: a reducer that read `Date.now()` would decide differently on every
 * replay of the same actions. `0` is "until the listener says otherwise" and is
 * never a deadline.
 *
 * Immer only marks the map changed if something is actually deleted, which is
 * what keeps [`selectLiveMatrix`] memoized across the Calls that ask.
 */
function expire(state: LiveState, now: number) {
  for (const [key, until] of Object.entries(state.avoided)) {
    if (until !== 0 && until <= now) delete state.avoided[key]
  }
}

/** Drop whatever the listener no longer wants — after a selection change, a
 *  hold, or an avoid. */
function purge(state: LiveState) {
  const matrix = matrixOf(state)
  state.queue = retain(state.queue, (call) => wants(matrix, call))
  if (state.current && !wants(matrix, state.current)) next(state)
}

/**
 * The subscription matrix this state asks the server for (ADR-0004).
 *
 * Three layers, each an exception to the one under it: the **Selection** is the
 * base (#12); a **Hold** narrows it to one System or Talkgroup (spec US 11);
 * and each **Avoid** is a Talkgroup silenced on top (US 14). The server resolves
 * the most specific entry first, so the layers survive as a single flat matrix.
 *
 * Written over the three fields rather than over the state so the selector
 * below can memoize on them (#91). A reducer still has [`matrixOf`], because a
 * memoized selector over an Immer draft would be cached against a proxy that
 * stops being valid the moment the reducer returns.
 */
function matrixFrom(
  selection: Selection,
  hold: Hold | null,
  avoided: Avoids,
): Subscription {
  const held =
    hold === null
      ? selection
      : hold.talkgroupRef === null
        ? restrictToSystem(selection, hold.systemRef)
        : restrictToTalkgroup(hold.systemRef, hold.talkgroupRef)

  return silenced(held, avoided)
}

/** [`matrixFrom`] over a whole slice — what the reducers judge a Call against. */
function matrixOf(state: LiveState): Subscription {
  return matrixFrom(state.selection, state.hold, state.avoided)
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

    /**
     * A Call arrived over the feed: play it if the feed is quiet, else queue
     * it. A **Backfill** Call (ADR-0004) arrives the same way — a listener
     * coming back wants to hear what they missed.
     *
     * `at` is the moment it arrived, supplied by the action creator so the
     * reducer stays pure, and it is what the Avoids in force are judged
     * against (#91). The store keeps its own clock for lapsing them
     * (`./avoids`), but a browser throttles a backgrounded tab's timers — so a
     * Call that arrives after an Avoid's deadline must be heard on the
     * strength of the deadline itself, not on the clock having woken to notice
     * it.
     */
    received: {
      prepare: (call: Call, seq: number, at: number = Date.now()) => ({
        payload: { call, seq, at },
      }),

      reducer(state, action: PayloadAction<{ call: Call; seq: number; at: number }>) {
        const { call, seq, at } = action.payload
        // Asked before the Call is judged, so it is judged against the Avoids
        // that are actually in force — and so the matrix the socket re-sends
        // is the one that lets the Talkgroup through again.
        expire(state, at)

        // A silence the listener asked for is not interrupted (#88). A Call
        // still in flight when the socket closed, or one the server sent before
        // it saw the new matrix, is not played and not counted: they asked for
        // silence, and this is what that costs.
        if (!feedPlays(statusOf(state))) return
        // Catch-up is at-least-once (ADR-0004): a Call ingested in the window
        // between connect and the backfill query arrives twice, and hearing it
        // twice is the listener's problem to be spared.
        if (state.seen.includes(call.id)) return
        state.seen.push(call.id)
        if (state.seen.length > SEEN_LIMIT) state.seen.shift()

        // The cursor counts every Call the server sent, even one filtered out
        // here, or a reconnect would ask for it again. The **emission**, not the
        // Call's id (#94): the two are different orderings of the same Calls,
        // and a cursor over ids would step past a Call that was held back.
        state.since = Math.max(state.since ?? 0, seq)

        if (!wanted(state, call)) return

        // An encrypted Call is activity, not audio (#42, spec US 9): it goes
        // straight into RECENT so the listener sees the channel is busy, and it
        // never becomes `current` and never joins the queue.
        //
        // This is not tidiness. There is nothing to play — the server sends no
        // `audioUrl` for one — so making it `current` would leave the audio
        // element with no source, and an element with no source never fires
        // `ended`. The feed would stop on it silently and forever, with
        // everything queued behind it frozen.
        if (call.encrypted) {
          state.history.unshift(call)
          state.history = state.history.slice(0, HISTORY_DEPTH)
          return
        }

        if (!state.current) {
          play(state, call)
          return
        }
        // Where it lands, and what the cap gives up to fit it, are the policy's
        // (#95) — lowest **Priority** first and stalest within it, so a full
        // queue can no longer discard the one Talkgroup the Listener said
        // mattered. Whatever went is counted rather than vanishing.
        const { queue, dropped } = enqueue(state.queue, call, QUEUE_POLICY)
        state.queue = queue
        state.missed += dropped.length
      },
    },

    /** The current Call finished, or the listener skipped it (spec US 12). */
    advance(state) {
      if (!state.current) return
      next(state)
    },

    /** Play a Call again — the one playing, or one from the history (spec
     *  US 13). The queue behind it is untouched. */
    replay(state, action: PayloadAction<number>) {
      // Nothing plays while the listener has the feed off or is playing the
      // archive (#80, #88). The Replay control is disabled there, but replay is
      // reachable from the RECENT list and from the lock screen's previous
      // button (`previousCall`), and a Call started from either would be audio
      // playing under a FEED OFF header with the socket shut.
      if (!feedPlays(statusOf(state))) return
      const again =
        state.current?.id === action.payload
          ? state.current
          : state.history.find((call) => call.id === action.payload)
      if (!again) return

      // Every case goes through `play` — including replaying the Call already
      // playing, where it files that Call and then filters it straight back out,
      // leaving history untouched and bumping the `playId` the element needs to
      // start over (`src` has not moved).
      //
      // Assigning `state.current` here instead is what let a Call be both
      // playing and listed in RECENT (#82), and it did it twice over: once by
      // leaving the replayed Call in the list, and once on the path where the
      // feed was already quiet, which had no Call to file and so skipped the
      // bookkeeping altogether.
      play(state, again)
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
     *  `now` is passed in rather than read, so the reducer stays pure. Dispatched
     *  by the store's own clock (`./avoids`) at each deadline. */
    expireAvoids(state, action: PayloadAction<number>) {
      expire(state, action.payload)
    },

    /** The server's **Backfill** left a hole: it could not reach back as far as
     *  we asked, and only archive search can fill the rest (ADR-0004 `gap`).
     *
     *  Unconditional, unlike `lagged` below. A gap is a fact about what this
     *  listener has already missed, not about traffic arriving now — so a
     *  listener who turned the feed off between asking and being answered has
     *  the same hole in their history either way. */
    gapped(state) {
      state.gap = true
    },

    /** The server told us a slow connection cost us Calls (ADR-0004 `lagged`). */
    lagged(state, action: PayloadAction<number>) {
      // Same promise `turnFeedOff` keeps (#80): `missed` admits traffic the
      // listener *wanted* and did not get. A notice buffered on the socket, or
      // in flight while it closes, arrives after the switch — and charging them
      // for silence they chose would make the counter a lie.
      if (!feedPlays(statusOf(state))) return
      state.missed += action.payload
    },
  },

  extraReducers: (builder) => {
    // The live feed and playback mode are mutually exclusive (CONTEXT.md): going
    // to the archive silences the feed here *and*, via the matrix, stops the
    // server sending it. The cursor survives so coming back doesn't refetch the
    // world.
    //
    // Recording the mode is what makes that exclusion *hold* rather than happen
    // once (#88): the subscription goes empty but the socket stays open, so a
    // Call in flight lands after this sweep and would find nothing playing.
    builder.addCase(enterPlaybackMode, (state) => {
      state.inPlayback = true
      state.queue = []
      state.current = null
      state.history = []
      state.playId += 1
    })

    // ...and released, or the feed would never come back.
    builder.addCase(enterLiveFeed, (state) => {
      state.inPlayback = false
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
  gapped,
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

/** The Calls waiting their turn, in the order they will play — so a queue sheet
 *  (#58) reads top-down without knowing the ordering rule. */
export const selectQueue = (state: WithLive): Call[] => state.live.queue

export const selectHistory = (state: WithLive): Call[] => state.live.history

export const selectPlayId = (state: WithLive): number => state.live.playId

export const selectHold = (state: WithLive): Hold | null => state.live.hold

/** Every **Avoid** in force and when each lapses — what the Talkgroups panel
 *  badges a row with, and the input both matrices below are silenced by. */
export const selectAvoids = (state: WithLive): Avoids => state.live.avoided

/** How many Talkgroups are muted right now (spec US 14). */
export const selectAvoidedCount = (state: WithLive): number =>
  Object.keys(state.live.avoided).length

export const selectSince = (state: WithLive): number | undefined => state.live.since

export const selectMissed = (state: WithLive): number => state.live.missed

/** Does this listener's history have a hole a **Backfill** could not fill? */
export const selectHasGap = (state: WithLive): boolean => state.live.gap

/**
 * Why the live feed is or is not delivering (#88) — the one answer the banner,
 * the LED, the controls and the reducers' guards all read.
 *
 * Distinct from [`selectLiveStatus`], which is what the *socket* is doing and
 * knows nothing of the listener's choices.
 */
export const selectFeedStatus = (state: WithLive): FeedStatus =>
  statusOf(state.live)

/**
 * Which Live-screen controls the listener can reach, given the feed's standing
 * and what there is to act on (#88).
 *
 * Memoized, so the record keeps its identity between renders: the Live screen
 * re-renders several times a second while a Call plays (the waveform's
 * progress), and a fresh object each time would defeat every downstream
 * comparison.
 */
export const selectLiveControls: (state: WithLive) => Controls = createSelector(
  [
    selectFeedStatus,
    (state: WithLive) => state.live.current,
    (state: WithLive) => state.live.hold,
    (state: WithLive) => state.live.history,
  ],
  (status, current, hold, history) =>
    controlsFor(status, {
      onAir: current !== null,
      systemHold: isSystemHold(hold),
      talkgroupHold: isTalkgroupHold(hold),
      hasRecent: history.length > 0,
    }),
)

/** Is this Talkgroup silenced right now? *When* it lapses is on the deadline
 *  itself, which the Talkgroups panel reads off [`selectAvoids`] — a timed
 *  Avoid is coming back on its own, and the panel shows the difference. */
export const selectIsAvoided = (
  state: WithLive,
  systemRef: number,
  talkgroupRef: number,
): boolean => avoidKey(systemRef, talkgroupRef) in state.live.avoided

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
 *
 * Memoized, and that is the point rather than a nicety (#91). It allocates a
 * fresh matrix on every call, so an unmemoized version missed every reference
 * comparison downstream — and the Live screen dispatches playback progress
 * several times a second, which is how 400 panel rows came to redraw several
 * times a second *because* audio was playing.
 */
export const selectAudibleSelection: (state: WithLive) => Selection =
  createSelector([selectSelection, selectAvoids], silenced)

/**
 * The subscription matrix this state asks the server for (ADR-0004) —
 * selection, hold and avoids flattened into one, per [`matrixFrom`].
 *
 * Memoized for the same reason, and with a caller who had already discovered
 * it: `LiveFeedLink` compared this by serializing it to a string, because
 * comparing it by reference could only ever say "changed".
 */
export const selectLiveMatrix: (state: WithLive) => Subscription = createSelector(
  [selectSelection, selectHold, selectAvoids],
  matrixFrom,
)
