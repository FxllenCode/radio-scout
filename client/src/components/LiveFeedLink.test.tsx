import { act, screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { beforeEach, describe, expect, it, vi } from 'vitest'

import {
  avoid,
  chooseTalkgroups,
  received,
  selectLiveMatrix,
} from '@/store/live'
import { makeStore } from '@/store/store'
import { enterPlaybackMode } from '@/store/playback'
import { liveFeed } from '@/test/handlers'
import { server } from '@/test/setup'
import { fakePush } from '@/test/push'
import { createPush } from '@/lib/push'
import { renderApp } from '@/test/utils'
import type { Call } from '@/types'

let subscriptions: Record<string, unknown>[] = []
let connections = 0
/** What the server pushes as soon as a client connects. */
let greeting: unknown[] = []

beforeEach(() => {
  subscriptions = []
  connections = 0
  greeting = []
  server.use(
    liveFeed.addEventListener('connection', ({ client }) => {
      connections += 1
      client.addEventListener('message', (event) => {
        subscriptions.push(JSON.parse(String(event.data)))
      })
      for (const frame of greeting) client.send(JSON.stringify(frame))
    }),
  )
})

const call: Call = {
  id: 42,
  systemRef: 11,
  systemLabel: 'Fulton County',
  talkgroupRef: 54241,
  talkgroupLabel: 'FD Dispatch',
  audioUrl: '/api/call/42/audio',
}

async function lastSubscription() {
  await waitFor(() => expect(subscriptions.length).toBeGreaterThan(0))
  return subscriptions.at(-1)
}

describe('the live-feed link and Web Push (#16)', () => {
  /** A listener with notifications on, holding the server's token. */
  const subscribed = () => createPush({ environment: fakePush({ permission: 'granted', subscribed: true }) })

  // While this socket is open the listener is demonstrably listening, so the
  // server holds its notifications; presenting the subscription id is how it
  // knows which listener that is.
  it('tells the server which push subscription is listening', async () => {
    renderApp('/', undefined, subscribed())

    await waitFor(async () =>
      expect(await lastSubscription()).toMatchObject({
        push: 'a-subscription-token',
      }),
    )
  })

  it('sends no subscription id when notifications are off', async () => {
    renderApp('/', undefined, createPush({ environment: fakePush() }))

    expect(await lastSubscription()).toEqual({ t: 'sub', all: true, sel: {} })
  })

  // Notifications are about *watched* Talkgroups, so a Selection the listener
  // changes has to reach the server's copy — otherwise a phone in a pocket
  // keeps being woken by a Talkgroup its owner turned off an hour ago.
  it('re-registers the Selection when the listener changes it', async () => {
    const push = subscribed()
    const store = makeStore()
    const synced = vi.spyOn(push, 'sync')
    renderApp('/', store, push)
    await lastSubscription()

    act(() => {
      store.dispatch(
        chooseTalkgroups({
          keys: [{ systemRef: 11, talkgroupRef: 54241 }],
          on: false,
        }),
      )
    })

    await waitFor(() => expect(synced).toHaveBeenCalled())
    expect(synced.mock.lastCall?.[0]).toMatchObject({
      sel: { 11: { 54241: false } },
    })
  })
})

describe('the live-feed link', () => {
  it('asks for everything until the listener says otherwise', async () => {
    renderApp('/')

    expect(await lastSubscription()).toEqual({ t: 'sub', all: true, sel: {} })
  })

  /** The whole point of the feed: a Call pushed by the server plays, with no
   *  request from the client (spec US 9). */
  it('plays a Call the server pushes', async () => {
    greeting = [{ t: 'call', call }]

    renderApp('/')

    expect(await screen.findByText('FD Dispatch')).toBeInTheDocument()
    await waitFor(() =>
      expect(screen.getByTestId('call-player')).toHaveAttribute(
        'src',
        '/api/call/42/audio',
      ),
    )
  })

  /** ADR-0004 catch-up: what arrived while the listener was away is queued, not
   *  dropped — rdio loses it. */
  it('takes the backfill a reconnect brings with it', async () => {
    greeting = [
      { t: 'call', call, catchup: true },
      { t: 'call', call: { ...call, id: 43, talkgroupLabel: 'PD Dispatch' } },
    ]

    renderApp('/')

    expect(await screen.findByText('FD Dispatch')).toBeInTheDocument()
    expect(screen.getByLabelText('Queued calls')).toHaveTextContent('1')
  })

  /** ADR-0004's `lagged` notice: rdio drops those Calls without a word. */
  it('passes on how many Calls a slow connection cost', async () => {
    greeting = [{ t: 'call', call }, { t: 'lagged', skipped: 4 }]

    renderApp('/')

    expect(await screen.findByText(/4 missed/i)).toBeInTheDocument()
  })

  /** Spec US 14: a timed avoid has to come back on its own, so something has to
   *  notice its moment passed. */
  it('lets a timed avoid lapse without the listener touching anything', async () => {
    vi.useFakeTimers()
    try {
      const store = makeStore()
      renderApp('/', store)
      store.dispatch(received({ call }))
      store.dispatch(avoid({ until: Date.now() + 30 * 60_000 }))
      expect(selectLiveMatrix(store.getState()).sel).toEqual({
        '11': { '54241': false },
      })

      await act(async () => {
        vi.advanceTimersByTime(31 * 60_000)
      })

      expect(selectLiveMatrix(store.getState())).toEqual({ all: true, sel: {} })
    } finally {
      vi.useRealTimers()
    }
  })

  /** ADR-0004: on reconnect the client hands back the last Call id it saw, and
   *  the server backfills the gap. rdio just loses those Calls. */
  it('asks for what it missed when the socket comes back', async () => {
    let sockets = 0
    server.use(
      liveFeed.addEventListener('connection', ({ client }) => {
        sockets += 1
        client.addEventListener('message', (event) => {
          subscriptions.push(JSON.parse(String(event.data)))
        })
        if (sockets > 1) return
        client.send(JSON.stringify({ t: 'call', call }))
        setTimeout(() => client.close(), 0)
      }),
    )

    renderApp('/')

    await waitFor(() => expect(sockets).toBe(2), { timeout: 3_000 })
    await waitFor(() => expect(subscriptions.at(-1)?.since).toBe(call.id))
  })

  it('holds one socket open across the whole app', async () => {
    const user = userEvent.setup()
    renderApp('/')
    await waitFor(() => expect(connections).toBe(1))

    await user.click(screen.getByRole('link', { name: 'Search' }))
    await user.click(screen.getByRole('link', { name: 'Live' }))

    // Moving between tabs must not drop the feed — or the queue behind it.
    expect(connections).toBe(1)
  })

  /** CONTEXT.md: the live feed and playback mode are mutually exclusive, and
   *  "off" should mean the server stops sending. */
  it('stops asking for Calls in playback mode', async () => {
    const store = makeStore()
    renderApp('/', store)
    await lastSubscription()

    store.dispatch(enterPlaybackMode())

    await waitFor(async () =>
      expect(await lastSubscription()).toEqual({ t: 'sub', all: false, sel: {} }),
    )
  })
})
