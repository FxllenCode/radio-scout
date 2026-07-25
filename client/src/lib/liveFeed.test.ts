import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

import { liveFeed } from '@/test/handlers'
import { server } from '@/test/setup'
import type { Call } from '@/types'

import {
  connectLiveFeed,
  liveFeedUrl,
  type LiveFeedHandle,
  type LiveStatus,
} from './liveFeed'

/** Everything the client sent, and a hook for what the server says back. */
let sent: string[] = []
let connections = 0
let onConnect: (client: {
  send(data: string | ArrayBuffer): void
  close(): void
}) => void

beforeEach(() => {
  sent = []
  connections = 0
  onConnect = (client) => client.send(JSON.stringify({ t: 'hello', protocol: 1 }))
  server.use(
    liveFeed.addEventListener('connection', ({ client }) => {
      connections += 1
      client.addEventListener('message', (event) => {
        sent.push(String(event.data))
      })
      onConnect(client)
    }),
  )
})

let handle: LiveFeedHandle | undefined
afterEach(() => handle?.close())

function call(id: number): Call {
  return {
    id,
    systemRef: 11,
    talkgroupRef: 100,
    audioUrl: `/api/call/${id}/audio`,
  }
}

interface Recorder {
  statuses: LiveStatus[]
  calls: { call: Call; catchup: boolean }[]
  lagged: number[]
  /** The cursor the client reads when it re-subscribes after a drop. */
  since: number | undefined
}

/** Connect with recording handlers. */
function connect(options?: { retryMs?: number }): Recorder {
  const recorder: Recorder = {
    statuses: [],
    calls: [],
    lagged: [],
    since: undefined,
  }
  handle = connectLiveFeed(
    {
      onStatus: (status) => recorder.statuses.push(status),
      onCall: (received, catchup) => recorder.calls.push({ call: received, catchup }),
      onLagged: (skipped) => recorder.lagged.push(skipped),
      since: () => recorder.since,
    },
    options,
  )
  return recorder
}

const lastSub = () => JSON.parse(sent.at(-1) ?? '{}')

describe('live feed client', () => {
  /** Same origin as the API: Vite proxies it in dev, the binary serves it in
   *  production (ADR-0007), so the socket only ever swaps the scheme. */
  it('talks to /api/live on this origin', () => {
    expect(liveFeedUrl()).toBe(`ws://${location.host}/api/live`)
  })

  /** A page served over TLS must open `wss:` — a browser blocks a plain socket
   *  from a secure page outright. */
  it('follows the page onto TLS', () => {
    expect(liveFeedUrl({ protocol: 'https:', host: 'scanner.example.com' })).toBe(
      'wss://scanner.example.com/api/live',
    )
  })

  it('opens the socket and subscribes with what it was given', async () => {
    const recorder = connect()

    handle!.subscribe({ all: true, sel: {} })

    await vi.waitFor(() => expect(recorder.statuses).toContain('connected'))
    await vi.waitFor(() => expect(sent).toHaveLength(1))
    expect(lastSub()).toEqual({ t: 'sub', all: true, sel: {} })
  })

  /** A matrix set before the socket is open must not be lost — the listener's
   *  selection is known long before the connection is. */
  it('sends a subscription made before the socket opened', async () => {
    const recorder = connect()
    handle!.subscribe({ all: false, sel: { '11': { '*': true } } })

    await vi.waitFor(() => expect(sent).toHaveLength(1))
    expect(lastSub().sel).toEqual({ '11': { '*': true } })
    expect(recorder.statuses[0]).toBe('connecting')
  })

  it('re-sends the matrix whenever the listener changes it', async () => {
    connect()
    handle!.subscribe({ all: true, sel: {} })
    await vi.waitFor(() => expect(sent).toHaveLength(1))

    handle!.subscribe({ all: false, sel: { '11': { '100': true } } })

    await vi.waitFor(() => expect(sent).toHaveLength(2))
    expect(lastSub().sel).toEqual({ '11': { '100': true } })
  })

  /** ADR-0004's cursor is for *reconnects*. Sending it when the listener holds
   *  or avoids would ask the server to backfill the very traffic they just
   *  chose to stop hearing. */
  it('does not ask for a backfill when the listener changes their mind', async () => {
    const recorder = connect()
    recorder.since = 42

    handle!.subscribe({ all: true, sel: {} })
    await vi.waitFor(() => expect(sent).toHaveLength(1))
    handle!.subscribe({ all: false, sel: { '11': { '*': true } } })

    await vi.waitFor(() => expect(sent).toHaveLength(2))
    expect(sent.map((frame) => JSON.parse(frame).since)).toEqual([
      undefined,
      undefined,
    ])
  })

  it('hands over live Calls, and flags the backfilled ones', async () => {
    onConnect = (client) => {
      client.send(JSON.stringify({ t: 'call', call: call(1) }))
      client.send(JSON.stringify({ t: 'call', call: call(2), catchup: true }))
    }
    const recorder = connect()

    await vi.waitFor(() => expect(recorder.calls).toHaveLength(2))
    expect(recorder.calls[0]).toEqual({ call: call(1), catchup: false })
    expect(recorder.calls[1]).toEqual({ call: call(2), catchup: true })
  })

  it('reports how many Calls a lagging connection cost', async () => {
    onConnect = (client) => client.send(JSON.stringify({ t: 'lagged', skipped: 7 }))
    const recorder = connect()

    await vi.waitFor(() => expect(recorder.lagged).toEqual([7]))
  })

  /** The server may grow frames we don't know yet (ADR-0004 announces a
   *  protocol version for exactly that), and a bad frame must never take the
   *  feed down. */
  it('ignores frames it cannot use', async () => {
    onConnect = (client) => {
      // Binary frames are not ours: audio never rides this socket (ADR-0002).
      client.send(new Uint8Array([1, 2, 3]).buffer)
      client.send('not json at all')
      client.send(JSON.stringify({ t: 'something-new' }))
      client.send(JSON.stringify({ t: 'call' })) // no call in it
      client.send(JSON.stringify({ t: 'call', call: call(1) }))
    }
    const recorder = connect()

    await vi.waitFor(() => expect(recorder.calls).toHaveLength(1))
    expect(recorder.calls[0].call).toEqual(call(1))
  })

  describe('when the connection drops', () => {
    it('reconnects and asks for what it missed', async () => {
      onConnect = (client) => setTimeout(() => client.close(), 0)
      const recorder = connect({ retryMs: 1 })
      handle!.subscribe({ all: true, sel: {} })
      recorder.since = 9 // Calls arrived before the drop

      await vi.waitFor(() => expect(connections).toBeGreaterThan(1))
      expect(recorder.statuses).toContain('offline')
      await vi.waitFor(() => expect(sent.length).toBeGreaterThan(1))
      // The selection survives the drop, and the cursor — read now, not when
      // the listener last touched anything — asks for the gap (ADR-0004).
      expect(lastSub()).toEqual({ t: 'sub', all: true, sel: {}, since: 9 })
    })

    it('stops trying once the feed is closed for good', async () => {
      onConnect = (client) => setTimeout(() => client.close(), 0)
      connect({ retryMs: 1 })

      await vi.waitFor(() => expect(connections).toBeGreaterThan(0))
      handle!.close()
      const settled = connections

      await new Promise((resolve) => setTimeout(resolve, 20))
      expect(connections).toBe(settled)
    })
  })
})
