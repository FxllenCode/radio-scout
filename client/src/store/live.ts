import { createSelector, createSlice, type PayloadAction } from '@reduxjs/toolkit'

import {
  controlsFor,
  feedPlays,
  feedStatus,
  type Controls,
  type FeedStatus,
} from '@/lib/feed'
import type { QuietSpan } from '@/lib/catchup'
import type { LiveStatus, Subscription } from '@/lib/liveFeed'
import {
  callsOf,
  enqueue,
  newest,
  priorityFrom,
  reorder,
  retain,
  takeNext,
  withdraw,
  type PriorityOf,
  type Queued,
  type QueuePolicy,
} from '@/lib/queue'
import {
  EVERYTHING,
  talkgroupKey,
  isSelected,
  isSystemHold,
  isTalkgroupHold,
  parseTalkgroupKey,
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
 *  [`queuePolicy`] decides what goes, and it is counted as missed. */
export const QUEUE_LIMIT = 100

/**
 * How the listening queue orders itself (#95, `@/lib/queue`), given what this
 * Listener has marked **Priority** (#58).
 *
 * One field on top of the limit, which is what #95's seam was for: the ordering
 * rule, the cap's rule and the re-order all read this one function, so nothing
 * below has to know what Priority *is*.
 */
const queuePolicy = (priorityOf: PriorityOf): QueuePolicy => ({
  limit: QUEUE_LIMIT,
  priorityOf,
})

/**
 * How long an **Avoid** can be taken back (#58, spec US 25).
 *
 * Long enough to notice a mis-tap and reach the snackbar, short enough that it
 * is gone before it becomes furniture. Held in the store rather than in the
 * component that draws it, so the deadline survives the Listener changing tabs
 * and the offer does not restart its window on the way back.
 */
export const AVOID_UNDO_MS = 8_000

/**
 * How long a **Selection** that arrived in a link can be taken back (#61, spec
 * US 30).
 *
 * Longer than [`AVOID_UNDO_MS`], because the two mistakes are noticed
 * differently. A mis-tapped Avoid announces itself as silence within seconds;
 * a link that replaced a Selection is noticed by *reading the panel* — which is
 * where a Listener opening one is headed, and takes longer than glancing at a
 * snackbar. What it costs to get wrong is larger too: an Avoid is one channel,
 * a Selection can be six picked out of four hundred.
 */
export const SELECTION_UNDO_MS = 15_000

/**
 * How many Calls the session log keeps (#58, spec US 28).
 *
 * Deep enough to answer "what was that ten minutes ago" on a busy county
 * system, bounded because a phone left on a dispatch channel all day would
 * otherwise hold every Call of the day in memory. Far deeper than
 * [`HISTORY_DEPTH`], which is a replay list and not a record.
 */
export const SESSION_LOG_LIMIT = 250

/** How many Call ids are remembered for de-duplication. Catch-up delivery is
 *  *at-least-once* (ADR-0004), so a Call can arrive twice; ids are compared as
 *  a set rather than a high-water mark because concurrent ingests can broadcast
 *  out of id order, and a watermark would drop the late one. */
const SEEN_LIMIT = 256

export interface LiveState {
  status: LiveStatus
  /** Calls waiting to play, **in the order they will play** (CONTEXT.md
   *  **Listening queue**) — arrival order until a Talkgroup has **Priority**,
   *  and the policy's order after (`@/lib/queue`, #95). Each carries the
   *  ordinal it arrived at, which is what lets a Priority change re-order a
   *  queue that is already deep (#58). */
  queue: Queued[]
  /** The next arrival ordinal. Monotonic and meaningless on its own — it exists
   *  only so two waiting Calls can be compared. A counter rather than a clock,
   *  so the reducer stays pure and replays identically. */
  arrivals: number
  /** Talkgroups this Listener marked **Priority** (#58, spec US 27), by
   *  [`talkgroupKey`].
   *
   *  Here rather than in the `panel` slice, whose rule is that nothing in it can
   *  change what plays — and this changes what plays *next*, which is the whole
   *  of what a queue is. Remembered per **Profile** all the same, beside the
   *  Selection and the Avoids. */
  priority: string[]
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
  /** Every **Avoid** in force, by [`talkgroupKey`] (spec US 14's timed
   *  30/60/120 min cycle). */
  avoided: Avoids
  /** Calls the listener will not hear: dropped by the server's `lagged` notice,
   *  by the queue cap, or given up by a jump to the newest (#58). The display
   *  admits them rather than hiding them.
   *
   *  A Call the Listener *dropped by hand* from the queue sheet is deliberately
   *  not one of them — `turnFeedOff`'s rule, one Call at a time: they read it
   *  and let it go. */
  missed: number
  /**
   * Everything heard this session, newest first (#58, spec US 28) — what RECENT
   * is a five-deep window onto.
   *
   * Each Call once, in the order it was *first* heard: a replay is the Listener
   * hearing it again rather than a new thing happening, and a log that moved it
   * would answer "what was that ten minutes ago" with a list that reorders
   * itself under the question. It also keeps `key={call.id}` unique, which is
   * #82's lesson one list along.
   *
   * Not persisted, unlike the **Profile**: *this session* is what it says.
   */
  sessionLog: Call[]
  /**
   * The **Avoid** that can still be taken back, and until when (#58, spec
   * US 25) — `null` once it has been used, dismissed, or replaced by a newer
   * one.
   *
   * It carries what the Avoid *displaced* rather than merely which Talkgroup
   * went quiet, because avoiding is two changes: the deadline, and the **Hold**
   * it releases when the two contradict. An undo that put back only the first
   * would silently cost the Listener their hold.
   *
   * What it cannot put back is the Calls the purge took — they are gone, and
   * the honest offer is "the channel is not muted" rather than "nothing
   * happened".
   */
  avoidUndo: AvoidUndo | null
  /** The **Selection** a link replaced, while it can still be put back
   *  (#61). `null` almost always: opening a Selection link is a rare thing
   *  to do, and undoing one rarer still. */
  selectionUndo: SelectionUndo | null
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
   * **Catch-up** is engaged (#59, spec US 23): the queue is draining faster
   * than real time — gaps skipped, rate raised — until the Listener is live.
   *
   * Here beside the queue rather than in `panel`, for [`LiveState.priority`]'s
   * reason: this changes what plays and how, which is the whole of what a queue
   * is. **Not persisted**, unlike the Selection and the Avoids: being behind is
   * a fact about right now, and a Listener who closed the app forty Calls behind
   * and opens it tomorrow is not behind any more — [`LiveState.sessionLog`]'s
   * rule, one field along.
   *
   * Nothing has to switch it off. `liveReducer` clears it whenever the queue is
   * empty, so "disengages automatically at live" is structural rather than a
   * line every reducer that shrinks the queue has to remember.
   */
  catchup: boolean
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

/** An **Avoid** that can still be taken back, and everything it displaced. */
export interface AvoidUndo {
  /** The Talkgroup that went quiet, by [`talkgroupKey`]. */
  key: string
  /** The deadline that was in force before, or absent if it was not avoided at
   *  all — the difference between undoing to "audible" and undoing to "still
   *  avoided, for another twenty minutes". */
  previous?: number
  /** The **Hold** that stood before, which avoiding may have released. */
  hold: Hold | null
  /** When the offer lapses. The moment rather than a countdown, so it survives
   *  the Listener changing tabs (`Avoids` own reasoning, one field along). */
  expiresAt: number
}

/** A **Selection** replaced by a link, and the one it replaced (#61). */
export interface SelectionUndo {
  /** What this browser was listening to before the link was opened. The whole
   *  matrix, because that is what a Selection *is* — restoring "the Talkgroups
   *  that were on" would lose what the Listener had decided about the ones they
   *  had not heard of yet (`lib/selection`, rule 2). */
  previous: Selection
  /** When the offer lapses — a moment, for [`AvoidUndo`]'s reason. */
  expiresAt: number
}

/** The state a listener who has never touched anything starts from. Exported
 *  so the store can hydrate the persisted selection into it (#12). */
export const initialLiveState: LiveState = {
  status: 'offline',
  queue: [],
  arrivals: 0,
  priority: [],
  current: null,
  history: [],
  playId: 0,
  seen: [],
  selection: EVERYTHING,
  hold: null,
  avoided: {},
  missed: 0,
  sessionLog: [],
  avoidUndo: null,
  selectionUndo: null,
  gap: false,
  // On. A Listener who has never touched the toggle gets audio playing, which
  // is what the app is for.
  feedOff: false,
  catchup: false,
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

/** What this Listener's marked Talkgroups mean as an ordering (#58) — built
 *  from the state rather than held beside it, so the set and the rule cannot
 *  come to disagree. */
function priorityIn(state: LiveState): PriorityOf {
  return priorityFrom(state.priority)
}

/**
 * Write a Call into the session log (#58, spec US 28).
 *
 * Once each and in first-heard order — see [`LiveState.sessionLog`]. Called
 * from the two places a Call becomes something the Listener saw happen:
 * [`play`], and the encrypted branch of `received`, which is the only record
 * that a channel was busy at all.
 */
function heard(state: LiveState, call: Call) {
  if (state.sessionLog.some((one) => one.id === call.id)) return
  state.sessionLog.unshift(call)
  if (state.sessionLog.length > SESSION_LOG_LIMIT) state.sessionLog.pop()
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
    heard(state, call)
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

/** What the scanner display is showing, and whether the transmission is over. */
export interface Displayed {
  /** The Call on the card, or `null` when the screen is showing a reason
   *  instead of a Call. */
  call: Call | null
  /** It has finished: the card is up, dimmed, and nothing is on the air. */
  ended: boolean
}

/**
 * The Call the screen is showing, which outlives the transmission (#56).
 *
 * The scanner display keeps the last Call up between transmissions, dimmed,
 * with its controls live — so *Hold* and *Avoid* mean the Call in front of the
 * Listener, not the one on the air. That distinction only exists because there
 * usually is no Call on the air: a chatty Talkgroup stopping is precisely the
 * moment somebody reaches for Avoid, and before this the reducer read
 * [`LiveState.current`], found `null`, and silently did nothing under a lit
 * button.
 *
 * **Written once, and read from three places that must not disagree** — the
 * reducers below through [`subjectOf`], the screen through [`selectDisplay`],
 * and [`controlsFor`], which gates those same controls on `onAir || hasRecent`.
 * Three spellings of one precedence is how a reachable button comes to act on a
 * Call the card is not showing.
 *
 * Two clauses carry it. `history[0]` is the whole of "the last Call", because
 * [`play`] files the Call it displaces at the head of that list. And
 * [`feedPlays`] is the gate: the two silences a Listener *chose* show them why
 * instead, and without it here the Call `turnFeedOff` had just filed would
 * still be reachable by *Avoid* with the feed shut.
 *
 * Written over the three fields rather than the slice so [`selectDisplay`] can
 * memoize on them — the [`matrixFrom`] precedent, and for the same reason: a
 * memoized selector over an Immer draft caches against a proxy that dies with
 * the reducer.
 */
function displayedIn(
  status: FeedStatus,
  current: Call | null,
  history: Call[],
): Displayed {
  const call = feedPlays(status) ? (current ?? history[0] ?? null) : null
  return { call, ended: call !== null && current === null }
}

/** [`displayedIn`] over a whole slice — what the reducers act on. */
function subjectOf(state: LiveState): Call | null {
  return displayedIn(statusOf(state), state.current, state.history).call
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

/**
 * Silence one named Talkgroup until `until`, and remember what that displaced
 * (#58).
 *
 * The whole of what avoiding *is*, written once, because two surfaces reach it:
 * the Live screen's control, which acts on the Call the display is showing
 * (#56), and the session log's quick action, which names a Talkgroup from a row
 * further down. A refusal's *shape* may differ between surfaces; the policy
 * underneath may not (#92's rule, one layer up).
 */
function applyAvoid(state: LiveState, key: string, until: number, at: number) {
  state.avoidUndo = {
    ...(key in state.avoided ? { previous: state.avoided[key] } : {}),
    key,
    hold: state.hold,
    expiresAt: at + AVOID_UNDO_MS,
  }
  state.avoided[key] = until
  // Holding a Talkgroup you've just muted is a contradiction; the avoid is the
  // newer intent. Compared as the whole key rather than as a bare Ref: a Ref is
  // unique only within one System (#47's `UnitScope` lesson), and #58 lets an
  // Avoid be placed on a Talkgroup that is not the one being displayed — so a
  // Ref-only test would release a hold on another System's channel of the same
  // number, which on screen looks like the hold simply vanishing.
  const held = state.hold
  if (held?.talkgroupRef != null && talkgroupKey(held.systemRef, held.talkgroupRef) === key) {
    state.hold = null
  }
  purge(state)
}

/**
 * Read the clock at the action creator rather than inside the reducer, so both
 * ways of placing an **Avoid** stay pure and replay identically — and so a test
 * can hand in the moment (`received`'s own convention).
 *
 * Written once because the two differ only in *which* Talkgroup they name, and
 * a second copy is how one of them comes to stamp a different clock.
 */
const stamped = <P,>(payload: P & { at?: number }) => ({
  payload: { at: Date.now(), ...payload },
})

/** Mark a Talkgroup **Priority**, or let it go, and re-order the queue in hand
 *  to match — written once, because two controls reach it: the panel row and
 *  the Live screen's control over the Call on the display. */
function markPriority(state: LiveState, key: string) {
  state.priority = state.priority.includes(key)
    ? state.priority.filter((one) => one !== key)
    : [...state.priority, key]
  state.queue = reorder(state.queue, priorityIn(state))
}

/** Narrow to one named Talkgroup, or let it go if that is the one already
 *  held — one control for both, so a list can offer *Hold* and *Release* from
 *  the same row. A hold naming a *different* Talkgroup moves rather than
 *  releases: from a list, naming a channel means "hold this one". */
function holdOn(state: LiveState, { systemRef, talkgroupRef }: TalkgroupKey) {
  if (state.hold?.systemRef === systemRef && state.hold.talkgroupRef === talkgroupRef) {
    state.hold = null
    return
  }
  state.hold = { systemRef, talkgroupRef }
  purge(state)
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
          delete state.avoided[talkgroupKey(key.systemRef, key.talkgroupRef)]
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
          if (parseTalkgroupKey(key).systemRef === systemRef) delete state.avoided[key]
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
     * is what drops bandwidth and battery to zero. Nothing takes over —
     * Radio-Scout does not notify (ADR-0014) — so switching the feed back on is
     * the only way back in.
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
          heard(state, call)
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
        state.arrivals += 1
        const { queue, dropped } = enqueue(
          state.queue,
          call,
          state.arrivals,
          queuePolicy(priorityIn(state)),
        )
        state.queue = queue
        state.missed += dropped.length
      },
    },

    /** The current Call finished, or the listener skipped it (spec US 12). */
    advance(state) {
      if (!state.current) return
      next(state)
    },

    /**
     * Play a Call again — the one playing, or one heard earlier this session
     * (spec US 13, and #58's session log).
     *
     * Looked up in [`LiveState.sessionLog`] rather than in the history, because
     * #58 gives a Listener a screen reaching 250 Calls back and RECENT reaches
     * five. Every Call the history holds is in the log — [`play`] writes it
     * there as it starts — so this is strictly the wider list, not a second
     * one. The queue behind it is untouched.
     */
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
          : state.sessionLog.find((call) => call.id === action.payload)
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

    /** Narrow to the System on the display, or let it go again — see
     *  [`displayedIn`] for why that is not always the System that is
     *  talking. */
    toggleHoldSystem(state) {
      if (isSystemHold(state.hold)) {
        state.hold = null
        return
      }
      const on = subjectOf(state)
      if (!on) return
      state.hold = { systemRef: on.systemRef, talkgroupRef: null }
      purge(state)
    },

    /** Narrow to the Talkgroup on the display, or let it go again.
     *
     *  Deliberately not [`holdOn`]: this is a lit toggle, so pressing it
     *  releases *whatever* Talkgroup hold stands, where naming a channel from a
     *  list means "hold this one" and only releases the one it named. Two
     *  controls, two meanings — the shared part is `purge`. */
    toggleHoldTalkgroup(state) {
      if (isTalkgroupHold(state.hold)) {
        state.hold = null
        return
      }
      const on = subjectOf(state)
      if (!on) return
      state.hold = {
        systemRef: on.systemRef,
        talkgroupRef: on.talkgroupRef,
      }
      purge(state)
    },

    /** Mute the Talkgroup on the display until `until` (0 = until released).
     *  The moment is computed by the caller so this stays a pure reducer.
     *
     *  This is the control [`displayedIn`] exists for: a Talkgroup that will not
     *  stop chattering is one a Listener silences a beat *after* it stops, not
     *  during. */
    avoid: {
      prepare: stamped<{ until: number }>,

      reducer(state, action: PayloadAction<{ until: number; at: number }>) {
        const call = subjectOf(state)
        if (!call) return
        applyAvoid(
          state,
          talkgroupKey(call.systemRef, call.talkgroupRef),
          action.payload.until,
          action.payload.at,
        )
      },
    },

    /** Silence a Talkgroup named from a list rather than from the display —
     *  the session log's quick action (#58, spec US 28). Same policy, same
     *  undo offer: [`applyAvoid`] is the only way either gets there. */
    avoidTalkgroup: {
      prepare: stamped<TalkgroupKey & { until: number }>,

      reducer(
        state,
        action: PayloadAction<TalkgroupKey & { until: number; at: number }>,
      ) {
        const { systemRef, talkgroupRef, until, at } = action.payload
        applyAvoid(state, talkgroupKey(systemRef, talkgroupRef), until, at)
      },
    },

    /**
     * Take back the last **Avoid** (#58, spec US 25).
     *
     * Both halves of what it displaced: the deadline that stood before — which
     * may itself be an Avoid, so this is not "make it audible" — and the
     * **Hold** avoiding released. What it cannot undo is the purge: those Calls
     * are gone, and the offer is honest about being "the channel is not muted"
     * rather than "nothing happened".
     *
     * The purge afterwards is not redundant. Restoring a Hold *narrows* the
     * matrix again, and the queue has been filling under the wider one since.
     */
    undoAvoid(state) {
      const undo = state.avoidUndo
      if (!undo) return

      if (undo.previous === undefined) {
        delete state.avoided[undo.key]
      } else {
        state.avoided[undo.key] = undo.previous
      }
      // Only put back a Hold that is *missing*. Within the grace window a
      // Listener can place a new one — from the controls, or from the session
      // log's own quick action — and an undo that overwrote it would revert a
      // later choice rather than the mis-tap it exists for.
      if (state.hold === null) state.hold = undo.hold
      state.avoidUndo = null
      purge(state)
    },

    /** The grace window ran out, or the Listener waved the offer away. The
     *  Avoid itself stands — this is only the offer going. */
    dismissAvoidUndo(state) {
      state.avoidUndo = null
    },

    /**
     * Listen to what a link says to listen to (#61, spec US 30).
     *
     * The **Hold** and the **Avoids** are deliberately left alone: a link
     * carries a Selection, and those are this Listener's own reading of the
     * moment — a twenty-minute Avoid placed a minute ago is not something
     * somebody else's link has an opinion about. Both are layers *over* the
     * Selection ([`matrixFrom`]), so they keep applying to whatever it becomes.
     */
    applyLinkedSelection(
      state,
      action: PayloadAction<{ selection: Selection; at: number }>,
    ) {
      const { selection, at } = action.payload
      state.selectionUndo = {
        previous: state.selection,
        expiresAt: at + SELECTION_UNDO_MS,
      }
      state.selection = selection
      purge(state)
    },

    /** Put back the **Selection** the link replaced (#61). The purge is not
     *  redundant, for [`undoAvoid`]'s reason: restoring may *narrow* the matrix
     *  again, and the queue has been filling under the linked one since. */
    undoSelectionLink(state) {
      const undo = state.selectionUndo
      if (!undo) return
      state.selection = undo.previous
      state.selectionUndo = null
      purge(state)
    },

    /** The grace window ran out, or the offer was waved away. The linked
     *  Selection stands — this is only the offer going. */
    dismissSelectionUndo(state) {
      state.selectionUndo = null
    },

    /** Let one Talkgroup back in, from the sheet listing what is silenced (#58,
     *  spec US 25) — where `clearAvoids` is the all-or-nothing instrument this
     *  exists beside. */
    clearAvoid(state, action: PayloadAction<string>) {
      delete state.avoided[action.payload]
    },

    /** Hold, or release, a Talkgroup named from a list (#58) — see
     *  [`holdOn`]. */
    toggleHoldOn(state, action: PayloadAction<TalkgroupKey>) {
      holdOn(state, action.payload)
    },

    /**
     * Mark a Talkgroup **Priority**, or let it go (#58, spec US 27).
     *
     * The queue in hand is re-ordered on the spot, which is the whole point:
     * a Listener reaches for Priority when they are already far behind, and a
     * promotion that only applied to Calls arriving *later* would do nothing
     * on screen at the one moment it was asked for (`lib/queue`'s [`reorder`]).
     */
    togglePriority(state, action: PayloadAction<string>) {
      markPriority(state, action.payload)
    },

    /** The same, for the Call the display is showing — the subject *Hold* and
     *  *Avoid* already act on (#56), so three lit controls cannot be about
     *  three different Calls. */
    togglePriorityShown(state) {
      const call = subjectOf(state)
      if (!call) return
      markPriority(state, talkgroupKey(call.systemRef, call.talkgroupRef))
    },

    /**
     * Play a waiting Call now (#58, spec US 24) — the queue sheet's *play*.
     *
     * By **id**, never by position: the sheet re-orders under the Listener's
     * thumb as Calls arrive, so an index would put a different Call on the air
     * than the one the finger went down on. A Call that has since played or
     * been purged is simply not there, and nothing happens.
     */
    playQueued(state, action: PayloadAction<number>) {
      const taken = withdraw(state.queue, action.payload)
      if (!taken.call) return
      state.queue = taken.queue
      play(state, taken.call)
    },

    /** Let a waiting Call go (#58, spec US 24). Not counted as missed — see
     *  [`LiveState.missed`]. */
    dropQueued(state, action: PayloadAction<number>) {
      const taken = withdraw(state.queue, action.payload)
      if (!taken.call) return
      state.queue = taken.queue
    },

    /**
     * Give up the backlog and play what was said most recently (#58, spec
     * US 24).
     *
     * The **newest arrival**, not the tail of the queue — under Priority the
     * tail is the lowest-ranked Call there is, and handing a Listener that
     * while calling it catching up would be the cap's old mistake in the other
     * direction.
     *
     * Everything given up is counted, per the ticket: "never silent". This is
     * the one Listener-initiated discard that is, because it is the one where
     * they did not look at what went.
     */
    jumpToNewest(state) {
      const latest = newest(state.queue)
      if (!latest) return
      state.missed += state.queue.length - 1
      state.queue = []
      play(state, latest)
    },

    /**
     * Drain the backlog (#59, spec US 23) — engaged from the queue sheet, which
     * is where a Listener looking at a number they cannot face will be.
     *
     * Nothing here disengages it: `liveReducer` does that the moment the queue
     * is empty, whichever reducer emptied it.
     */
    engageCatchup(state) {
      state.catchup = true
    },

    /** Stop draining and go back to real time. */
    stopCatchup(state) {
      state.catchup = false
    },

    /**
     * The **Quiet spans** of some queued Calls arrived (#59).
     *
     * Written **onto the Calls themselves** rather than kept in a table beside
     * them, because a Call read back from the Archive already carries its spans
     * — so this leaves one field, `call.quiet`, that every reader can trust
     * whichever way the Call arrived. Only a Call pushed over the live feed is
     * ever missing them, and that is because the frame goes out at ingest, ahead
     * of the scan (#46: nothing republishes a frame).
     *
     * **Every id asked about is written, not only the ones that answered.** A
     * Call with nothing to trim is absent from the server's reply, and left
     * undefined it would be asked about again on every pass — forever, for every
     * ordinary Call there is. An empty array is the answer "looked up, no gaps".
     */
    quietFound(
      state,
      action: PayloadAction<{ ids: number[]; spans: Record<string, QuietSpan[]> }>,
    ) {
      const { ids, spans } = action.payload
      const found = new Map(ids.map((id) => [id, spans[String(id)] ?? []]))
      for (const call of [...state.queue.map((entry) => entry.call), state.current]) {
        const quiet = call && found.get(call.id)
        if (call && quiet) call.quiet = quiet
      }
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
  avoidTalkgroup,
  chooseEverything,
  chooseSystem,
  chooseTalkgroups,
  clearAvoid,
  clearAvoids,
  connected,
  connecting,
  disconnected,
  dismissAvoidUndo,
  applyLinkedSelection,
  undoSelectionLink,
  dismissSelectionUndo,
  dropQueued,
  engageCatchup,
  expireAvoids,
  gapped,
  jumpToNewest,
  lagged,
  playQueued,
  quietFound,
  received,
  replay,
  stopCatchup,
  toggleHoldOn,
  toggleHoldSystem,
  toggleHoldTalkgroup,
  togglePriority,
  togglePriorityShown,
  turnFeedOff,
  turnFeedOn,
  undoAvoid,
} = liveSlice.actions

/**
 * The slice, plus the one rule no reducer owns: **Catch-up ends at live** (#59).
 *
 * Wrapped rather than repeated, because the queue can empty six ways — the last
 * Call coming off it, a jump to the newest, a drop, a purge after a Selection
 * change, a **Hold**, an **Avoid** — and a line in each is five chances to
 * forget one and a standing tax on every reducer added later. Here it is true by
 * construction, which is #92's argument one layer up.
 *
 * "At live" is an empty queue: nothing is waiting, so the Call now playing is
 * the newest there is, and playing *that* at one and a half times would be
 * hurrying through the present.
 */
export const liveReducer: typeof liveSlice.reducer = (state, action) => {
  const next = liveSlice.reducer(state, action)
  if (!next.catchup || next.queue.length > 0) return next
  return { ...next, catchup: false }
}

/** The slice of the store this module owns. */
interface WithLive {
  live: LiveState
}

export const selectLiveStatus = (state: WithLive): LiveStatus => state.live.status

export const selectLiveCall = (state: WithLive): Call | null => state.live.current

/** The `Q` count on the display. */
export const selectQueueDepth = (state: WithLive): number => state.live.queue.length

/**
 * The Calls waiting their turn, in the order they will play — so the queue
 * sheet (#58) reads top-down without knowing the ordering rule, and the
 * page-ahead can take the head.
 *
 * Memoized, because it unwraps the arrival stamps into a fresh array: an
 * unmemoized version would hand `selectUpcomingCall` a new array on every
 * `progressed`, which is several a second while a Call plays.
 */
export const selectQueue: (state: WithLive) => Call[] = createSelector(
  [(state: WithLive) => state.live.queue],
  callsOf,
)

/** Talkgroups this Listener marked **Priority** (#58, spec US 27) — what the
 *  panel rows read and what is persisted. */
export const selectPriority = (state: WithLive): string[] => state.live.priority

/** Does this Talkgroup outrank the routine traffic? */
export const selectIsPriority = (
  state: WithLive,
  systemRef: number,
  talkgroupRef: number,
): boolean => state.live.priority.includes(talkgroupKey(systemRef, talkgroupRef))

/** Everything heard this session, newest first (#58, spec US 28). */
export const selectSessionLog = (state: WithLive): Call[] => state.live.sessionLog

/** The **Avoid** that can still be taken back, or `null` (#58, spec US 25). */
export const selectAvoidUndo = (state: WithLive): AvoidUndo | null =>
  state.live.avoidUndo

/** The **Selection** a link replaced, while it can still be put back (#61). */
export const selectSelectionUndo = (state: WithLive): SelectionUndo | null =>
  state.live.selectionUndo

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

/** Is the queue draining faster than real time (#59, spec US 23)? */
export const selectIsCatchingUp = (state: WithLive): boolean => state.live.catchup


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

/**
 * What the scanner display is showing (#56) — the Call, and whether it is over.
 *
 * The same [`displayedIn`] the reducers resolve *Hold* and *Avoid* against, so
 * the card and the controls under it can never be about two different Calls.
 *
 * Memoized for [`selectLiveControls`]'s reason: the Live screen re-renders
 * several times a second while a Call plays, and a fresh object each time would
 * defeat every comparison downstream of it.
 */
export const selectDisplay: (state: WithLive) => Displayed = createSelector(
  [
    selectFeedStatus,
    (state: WithLive) => state.live.current,
    (state: WithLive) => state.live.history,
  ],
  displayedIn,
)

/** Is this Talkgroup silenced right now? *When* it lapses is on the deadline
 *  itself, which the Talkgroups panel reads off [`selectAvoids`] — a timed
 *  Avoid is coming back on its own, and the panel shows the difference. */
export const selectIsAvoided = (
  state: WithLive,
  systemRef: number,
  talkgroupRef: number,
): boolean => talkgroupKey(systemRef, talkgroupRef) in state.live.avoided

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
