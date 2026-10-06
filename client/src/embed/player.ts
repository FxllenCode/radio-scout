/**
 * What an **Embed** page plays, decided purely (#75).
 *
 * The page another site frames (`src/embed/mount.ts` is the adapter, and
 * `src/embed.rs` on the server says why it is shaped this way) is a list of a
 * Selection's recent Calls, a **Listen live** button, and one `<audio>`
 * element. Everything it decides — what is shown, what is playing, what waits,
 * what a reader's press does, what a dropped socket or a deleted embed does —
 * is this function from an event to the next state, so every rule is a value a
 * test states and the DOM, the socket and the media element are adapters
 * around it. `lib/run.ts`'s `advance` is the same idea for the app.
 *
 * Deliberately smaller than the app's `live`/`transport` slices: no hold, no
 * avoid, no history, no Priority. A reader of a fire department's homepage
 * presses one button.
 */
import type { LiveStatus } from '@/lib/liveFeed'
import type { Selection } from '@/lib/selection'
import type { Call } from '@/types'

/** What `GET /api/embed?t=…` answers. */
export interface EmbedFeed {
  name: string
  selection: Selection
  /** Newest first, open listening only. */
  calls: Call[]
}

export interface PlayerState {
  /** `loading` until the first feed answers; `unavailable` once the embed is
   *  known to be gone; `unreachable` while a first load keeps failing; `ready`
   *  from the first successful load on, whatever happens to the network after
   *  — a page that has something to show keeps showing it. */
  phase: 'loading' | 'ready' | 'unavailable' | 'unreachable'
  name?: string
  selection?: Selection
  /** Newest first, at most [`MAX_SHOWN`], one entry per Call. */
  calls: Call[]
  /** The reader pressed Listen live and has not pressed Stop. */
  listening: boolean
  /** The live socket, while listening; `offline` otherwise. */
  live: LiveStatus
  /** The Call on the one `<audio>` element. */
  playing?: number
  /** Calls that arrived live while another played, oldest first. */
  queue: number[]
  /** The Call that last would not play — an Instance gone away, or audio
   *  pruned since it was listed — until anything is tried again. */
  failed?: number
}

export type PlayerEvent =
  | { type: 'loaded'; feed: EmbedFeed }
  | { type: 'gone' }
  | { type: 'unreachable' }
  | { type: 'listen' }
  | { type: 'stop' }
  | { type: 'status'; status: LiveStatus }
  | { type: 'arrived'; call: Call }
  /** The reader pressed a row. */
  | { type: 'play'; id: number }
  /** The element finished. */
  | { type: 'ended' }
  /** The element could not play what it was given. Moves on as an end does,
   *  and is *said*, so a tap on a dead Instance is not a button that does
   *  nothing. */
  | { type: 'failed' }

/** How many Calls the page lists. The feed sends twenty; this is how far the
 *  list may grow while a reader listens before the oldest fall off the end. */
export const MAX_SHOWN = 50

/** How many Calls may wait behind the one playing. A busy county outruns the
 *  speaker, and a queue that only grows would be playing the afternoon at
 *  night — so past this the oldest waiting are passed over, the way the Station
 *  stream stays within two minutes of live (#74). */
export const MAX_QUEUED = 10

export const INITIAL: PlayerState = {
  phase: 'loading',
  calls: [],
  listening: false,
  live: 'offline',
  queue: [],
}

/** The Call on the element, if any. */
export function nowPlaying(state: PlayerState): Call | undefined {
  return state.calls.find((it) => it.id === state.playing)
}

/** The next state. Returns `state` itself when an event changes nothing, so an
 *  adapter can skip the work. */
export function step(state: PlayerState, event: PlayerEvent): PlayerState {
  switch (event.type) {
    case 'loaded':
      return {
        ...state,
        failed: undefined,
        phase: 'ready',
        name: event.feed.name,
        selection: event.feed.selection,
        calls: merged(state.calls, event.feed.calls),
      }
    case 'gone':
      return { ...INITIAL, phase: 'unavailable' }
    case 'unreachable':
      // A page that has something to show keeps showing it (`phase`).
      return state.phase === 'ready' ? state : { ...state, phase: 'unreachable' }
    case 'listen':
      return { ...state, listening: true, live: 'connecting', failed: undefined }
    case 'stop':
      return {
        ...state,
        listening: false,
        live: 'offline',
        playing: undefined,
        queue: [],
      }
    case 'status':
      return state.listening && state.live !== event.status
        ? { ...state, live: event.status }
        : state
    case 'arrived':
      return arrived(state, event.call)
    case 'play': {
      // The row playing is a toggle: pressing it moves on, as its end would.
      if (event.id === state.playing) return next(state)
      const pressed = state.calls.find((it) => it.id === event.id)
      if (pressed?.audioUrl === undefined) return state
      return { ...state, playing: pressed.id, failed: undefined }
    }
    case 'ended':
      return next(state)
    case 'failed':
      return { ...next(state), failed: state.playing }
  }
}

/** Whatever waits, if the reader is listening — else nothing. */
function next(state: PlayerState): PlayerState {
  const [following, ...rest] = state.listening ? state.queue : []
  return { ...state, playing: following, queue: rest }
}

function arrived(state: PlayerState, call: Call): PlayerState {
  // The socket is only open while listening; this is the late frame from one
  // the reader already closed.
  if (!state.listening) return state
  // At-least-once delivery (ADR-0004): a Backfill can hand back a Call this page
  // already has.
  if (state.calls.some((it) => it.id === call.id)) return state

  const calls = merged(state.calls, [call])
  // An Encrypted Call is listed — the activity is the fact — and never played.
  if (call.audioUrl === undefined) return { ...state, calls }
  if (state.playing === undefined) {
    return { ...state, calls, playing: call.id, failed: undefined }
  }
  return { ...state, calls, queue: [...state.queue, call.id].slice(-MAX_QUEUED) }
}

/** One list, newest first, each Call once — the order a reader expects of
 *  "recent calls", whether a Call came from a load or off the socket. */
function merged(shown: Call[], incoming: Call[]): Call[] {
  const byId = new Map<number, Call>()
  for (const call of [...shown, ...incoming]) byId.set(call.id, call)
  return [...byId.values()]
    .sort((a, b) => (b.timestamp ?? 0) - (a.timestamp ?? 0) || b.id - a.id)
    .slice(0, MAX_SHOWN)
}
