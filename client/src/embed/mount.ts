/**
 * The **Embed** page, wired (#75): a feed to load, a socket to open when the
 * reader asks, one `<audio>` element, and [`step`] deciding everything.
 *
 * This is the adapter around `player.ts`. It owns three things the pure half
 * cannot, and each follows the state rather than being told separately:
 *
 * - **The feed** — loaded once, again when the reader presses Listen (so the
 *   list is current from that moment), and retried with backoff while a first
 *   load keeps failing. A page that has loaded keeps what it has whatever the
 *   network does afterwards; a `404` is a deleted embed, said in a sentence.
 * - **The socket** — the app's own `lib/liveFeed`, open exactly while the
 *   state says `listening`, subscribed to the embed's Selection, and carrying
 *   **no grant**: the frame's address is public, so the page is a Listener
 *   holding nothing (`src/embed.rs`). Its reconnect and its **Backfill** cursor
 *   are the app's, so a dropped Instance comes back with what it missed. It
 *   follows the Selection the *latest* read answered — the read Listen makes
 *   lands after the socket opens, and an embed re-scoped since the page loaded
 *   must not go on playing what it used to.
 * - **The element** — one, as in the app (ADR-0005), with its source following
 *   `playing`. A `play()` the browser refuses puts the page back to its button,
 *   because the reader's next press is the gesture it wants; an interrupted one
 *   is ignored; one that fails to load is moved past *and said*, which is what
 *   a tap on an Instance that has gone away looks like.
 *
 * **What it does not do: notice a deletion while a reader listens.** The feed
 * is read on load and on Listen, so a deleted embed is "no longer available"
 * to every reader who arrives or presses Listen after it — and a reader already
 * listening keeps the open live feed until they stop or reload. That is open
 * listening they could reach without the embed, so it leaks nothing; a poll per
 * listener to cut them off sooner would be the cost this page exists to avoid.
 */
import { MAX_RETRY_MS, connectLiveFeed, type LiveFeedHandle } from '@/lib/liveFeed'
import { encodeSelection } from '@/lib/selectionUrl'

import { INITIAL, nowPlaying, step, type PlayerEvent, type PlayerState } from './player'
import { createView } from './view'

export interface MountOptions {
  /** The embed's token — the frame's own `?t=`. */
  token: string | null
  /** The first wait before retrying a feed that would not load, doubling to
   *  [`MAX_RETRY_MS`]. */
  retryMs?: number
  /** The live socket's own first reconnect wait — `lib/liveFeed`'s, which
   *  defaults to a second. */
  liveRetryMs?: number
}

export interface Mounted {
  destroy(): void
}

/** A homepage left open on a dead Instance should not hammer it, so the feed
 *  backs off to the live socket's own ceiling. */
const FIRST_RETRY_MS = 2_000

export function mountEmbed(
  root: HTMLElement,
  { token, retryMs = FIRST_RETRY_MS, liveRetryMs }: MountOptions,
): Mounted {
  let state: PlayerState = INITIAL
  let socket: LiveFeedHandle | undefined
  /** What the socket was last told to send, in its canonical spelling — so a
   *  re-scoped embed, read again while a reader listens, re-subscribes rather
   *  than playing the old one, and a feed that says the same thing again (in
   *  whatever order the server's map came out in) does not. */
  let subscribed: string | undefined
  /** The highest **emission** heard — the Backfill cursor a reconnect sends. */
  let cursor: number | undefined
  let retry = retryMs
  let retryTimer: ReturnType<typeof setTimeout> | undefined
  let destroyed = false

  const audio = document.createElement('audio')
  audio.preload = 'none'
  audio.addEventListener('ended', () => dispatch({ type: 'ended' }))
  audio.addEventListener('error', () => dispatch({ type: 'failed' }))

  const view = createView(root, audio, {
    onListen() {
      dispatch({ type: 'listen' })
      void load()
    },
    onStop: () => dispatch({ type: 'stop' }),
    onPlay: (id) => dispatch({ type: 'play', id }),
  })

  /** Events raised *while* one is being handled — the socket reports
   *  `connecting` from inside `connectLiveFeed`, before it has returned the
   *  handle — wait their turn, so following the state never re-enters itself. */
  const pending: PlayerEvent[] = []
  let handling = false

  function dispatch(event: PlayerEvent) {
    if (destroyed) return
    pending.push(event)
    if (handling) return
    handling = true
    try {
      for (let next = pending.shift(); next; next = pending.shift()) handle(next)
    } finally {
      handling = false
    }
  }

  function handle(event: PlayerEvent) {
    const before = state
    state = step(state, event)
    if (state === before) return
    followSocket()
    if (state.playing !== before.playing) followElement()
    view.render(state)
  }

  function followSocket() {
    if (state.listening && !socket && state.selection) {
      socket = connectLiveFeed({
        onStatus: (status) => dispatch({ type: 'status', status }),
        onCall: (call, seq) => {
          cursor = Math.max(cursor ?? seq, seq)
          dispatch({ type: 'arrived', call })
        },
        // A page a reader is listening to has no use for either: it is not an
        // archive, and what it skipped is still on the scanner it links to.
        onLagged: () => {},
        onGap: () => {},
        since: () => cursor,
      }, { retryMs: liveRetryMs })
    } else if (!state.listening && socket) {
      socket.close()
      socket = undefined
      subscribed = undefined
    }
    const selection = state.selection
    if (socket && selection && encodeSelection(selection) !== subscribed) {
      subscribed = encodeSelection(selection)
      socket.subscribe({ all: selection.all, sel: selection.sel })
    }
  }

  function followElement() {
    const playing = nowPlaying(state)
    if (playing?.audioUrl === undefined) {
      audio.pause()
      return
    }
    audio.src = playing.audioUrl
    audio.play().catch((error: unknown) => {
      if (error instanceof DOMException && error.name === 'NotAllowedError') {
        dispatch({ type: 'stop' })
      }
    })
  }

  async function load() {
    if (token === null) {
      dispatch({ type: 'gone' })
      return
    }
    try {
      const response = await fetch(`/api/embed?${new URLSearchParams({ t: token })}`, {
        cache: 'no-store',
      })
      if (response.status === 404) {
        dispatch({ type: 'gone' })
        return
      }
      if (!response.ok) throw new Error(`feed answered ${response.status}`)
      dispatch({ type: 'loaded', feed: await response.json() })
      retry = retryMs
    } catch {
      dispatch({ type: 'unreachable' })
      if (state.phase === 'unreachable' && !destroyed) {
        retryTimer = setTimeout(() => void load(), retry)
        retry = Math.min(retry * 2, MAX_RETRY_MS)
      }
    }
  }

  view.render(state)
  void load()

  return {
    destroy() {
      destroyed = true
      clearTimeout(retryTimer)
      socket?.close()
      audio.pause()
      root.replaceChildren()
    },
  }
}
