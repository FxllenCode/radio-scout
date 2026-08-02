/**
 * The live-feed socket (#11), speaking the protocol in
 * [ADR-0004](../../../docs/adr/0004-live-feed-raw-websocket.md).
 *
 * Deliberately framework-free: it knows about frames and reconnects, not about
 * Redux or React. What the listener has selected arrives through
 * [`subscribe`](#), and Calls leave through the handlers — so the store can own
 * hold/avoid/queue/history (all client state, per the ADR) and this module can
 * be tested against a real socket.
 *
 * The server pings on a heartbeat and reaps peers that stop answering. A browser
 * answers those pings for us and never surfaces them to JavaScript, so there is
 * no client-side watchdog here: a dead connection reaches us as a `close`, and
 * `close` is what we retry.
 */
import type { Call } from '@/types'

export type LiveStatus = 'offline' | 'connecting' | 'connected'

/** The subscription matrix as it goes on the wire (ADR-0004): `all` is the
 *  default, and `sel[system][talkgroup | "*"]` overrides it. */
export interface Subscription {
  all: boolean
  sel: Record<string, Record<string, boolean>>
}

export interface LiveFeedHandlers {
  onStatus(status: LiveStatus): void
  /** A Call to play. A **Backfill** Call (the server flags one `catchup`)
   *  arrives through here identically and deliberately unmarked: the listener
   *  wants to hear what they missed for the same reason they want to hear what
   *  just happened, and the store dedups by id either way. */
  onCall(call: Call): void
  onLagged(skipped: number): void
  /** The highest Call id seen so far, read at send time. It is sent **only**
   *  when re-subscribing after a drop, as the catch-up cursor — a cursor on an
   *  ordinary matrix change would ask the server to backfill traffic the
   *  listener just chose to stop hearing. */
  since(): number | undefined
  /** The token of the listener's push subscription (#16), read at send time
   *  because notifications can be turned on while the socket is already open.
   *  While the server holds this it sends no notifications to that device: a
   *  listener with the feed open is not a listener to wake. */
  pushToken?(): string | undefined
}

export interface LiveFeedOptions {
  /** First reconnect delay, doubling up to [`MAX_RETRY_MS`]. */
  retryMs?: number
}

export interface LiveFeedHandle {
  /** Tell the server what to send. Sent now if the socket is open, on connect
   *  otherwise, and again after every reconnect. */
  subscribe(subscription: Subscription): void
  /** Close for good — no further reconnects. */
  close(): void
}

const FIRST_RETRY_MS = 1_000
/** Backoff ceiling. A phone that wakes to a dead network shouldn't hammer it,
 *  but a listener shouldn't wait long once it's back either. */
const MAX_RETRY_MS = 30_000

/** `/api/live` on this origin, as a WebSocket URL. Same origin as everything
 *  else: in dev Vite proxies it, in production the binary serves it — and a
 *  page served over TLS must use `wss:`, or the browser blocks the socket. */
export function liveFeedUrl(
  at: { protocol: string; host: string } = location,
): string {
  const protocol = at.protocol === 'https:' ? 'wss:' : 'ws:'
  return `${protocol}//${at.host}/api/live`
}

/** Connect, and keep reconnecting until closed. */
export function connectLiveFeed(
  handlers: LiveFeedHandlers,
  { retryMs = FIRST_RETRY_MS }: LiveFeedOptions = {},
): LiveFeedHandle {
  let socket: WebSocket | undefined
  let subscription: Subscription | undefined
  let retry = retryMs
  let retryTimer: ReturnType<typeof setTimeout> | undefined
  let closed = false
  /** True once a connection has dropped: the next `sub` is a *re*-subscribe and
   *  carries the catch-up cursor. */
  let reconnecting = false

  function send({ catchUp = false } = {}) {
    if (!subscription || socket?.readyState !== WebSocket.OPEN) return
    const since = catchUp ? handlers.since() : undefined
    const push = handlers.pushToken?.()
    socket.send(JSON.stringify({ t: 'sub', ...subscription, since, push }))
  }

  function open() {
    handlers.onStatus('connecting')
    socket = new WebSocket(liveFeedUrl())

    socket.onopen = () => {
      retry = retryMs
      handlers.onStatus('connected')
      // Re-sent on every connection: the server holds the matrix per socket, so
      // a reconnect starts from nothing until we say otherwise — and a
      // reconnect asks for what it missed while it was away.
      send({ catchUp: reconnecting })
      reconnecting = false
    }

    socket.onmessage = (event) => receive(handlers, event.data)

    socket.onclose = () => {
      handlers.onStatus('offline')
      if (closed) return
      reconnecting = true
      retryTimer = setTimeout(open, retry)
      retry = Math.min(retry * 2, MAX_RETRY_MS)
    }

    // An error is always followed by a close, which is where the retry lives.
    socket.onerror = () => socket?.close()
  }

  open()

  return {
    subscribe(next) {
      subscription = next
      send()
    },
    close() {
      closed = true
      clearTimeout(retryTimer)
      socket?.close()
    },
  }
}

/** Route one server frame. Anything unparseable or unknown is ignored: the
 *  protocol is versioned and expected to grow, and a bad frame must never take
 *  the feed down. */
function receive(handlers: LiveFeedHandlers, data: unknown) {
  if (typeof data !== 'string') return

  let frame: { t?: string; call?: Call; skipped?: number }
  try {
    frame = JSON.parse(data)
  } catch {
    return
  }

  if (frame.t === 'call' && frame.call) {
    handlers.onCall(frame.call)
    return
  }
  if (frame.t === 'lagged' && typeof frame.skipped === 'number') {
    handlers.onLagged(frame.skipped)
  }
  // `hello` and `subscribed` need no action: the greeting's heartbeat cadence
  // is the server's business (the browser answers its pings), and the ack only
  // matters to a client that wants to serialize subscribe→ingest.
}
