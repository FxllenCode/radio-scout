import { act, screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { beforeEach, describe, expect, it, vi } from 'vitest'

import { feedOffKey } from '@/lib/persist'
import {
  avoid,
  chooseEverything,
  chooseTalkgroups,
  received,
  selectLiveMatrix,
  turnFeedOff,
  turnFeedOn,
} from '@/store/live'
import { makeStore } from '@/store/store'
import { enterLiveFeed, enterPlaybackMode } from '@/store/playback'
import { progressed } from '@/store/transport'
import { liveFeed } from '@/test/handlers'
import { server } from '@/test/setup'
import { fakePush } from '@/test/push'
import { createPush } from '@/lib/push'
import { renderApp } from '@/test/utils'
import type { Call } from '@/types'

/** What one test's socket did, recorded against that test. */
interface Recorded {
  subscriptions: Record<string, unknown>[]
  connections: number
  closes: number
}

let feed: Recorded = { subscriptions: [], connections: 0, closes: 0 }
/** What the server pushes as soon as a client connects. */
let greeting: unknown[] = []

beforeEach(() => {
  greeting = []
  // The socket the *previous* test opened is closed when its component
  // unmounts, but a frame already in flight can still land after this test has
  // begun — and a `sub` re-sent when a push subscription resolves (#16) makes
  // that a routine event rather than a rare one. Each test's handler therefore
  // closes over its own arrays rather than over the bindings above, so a late
  // frame is recorded against the test that caused it and nothing leaks
  // forward.
  const mine: Recorded = { subscriptions: [], connections: 0, closes: 0 }
  feed = mine
  server.use(
    liveFeed.addEventListener('connection', ({ client }) => {
      mine.connections += 1
      client.addEventListener('close', () => {
        mine.closes += 1
      })
      client.addEventListener('message', (event) => {
        mine.subscriptions.push(JSON.parse(String(event.data)))
      })
      // `greeting` is read live, not captured: a test sets it after this runs.
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
  await waitFor(() => expect(feed.subscriptions.length).toBeGreaterThan(0))
  return feed.subscriptions.at(-1)
}

/** A browser that remembers this listener switched the feed off (#80). */
function offStorage(): Storage {
  const map = new Map([[feedOffKey('default'), 'true']])
  return {
    get length() {
      return map.size
    },
    clear: () => map.clear(),
    getItem: (key) => map.get(key) ?? null,
    key: (index) => [...map.keys()][index] ?? null,
    removeItem: (key) => void map.delete(key),
    setItem: (key, value) => void map.set(key, value),
  }
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

  /** #91: the matrix is re-sent when the *listener* changes it, never because
   *  a Call is playing. Playback progress lands several times a second, and a
   *  matrix rebuilt per dispatch would put a `sub` frame on the wire behind
   *  every one of them — the bandwidth server-side filtering exists to save,
   *  spent on saying nothing changed. */
  it.each([
    ['the live feed', received(call), chooseEverything(false)],
    ['the archive', enterPlaybackMode(), enterLiveFeed()],
  ])(
    'does not re-subscribe while %s is playing',
    async (_source, start, changeTheMatrix) => {
      const store = makeStore()
      // Started before the socket exists, so the frame under test is the only
      // one this test can be counting.
      store.dispatch(start)
      renderApp('/', store)
      await lastSubscription()
      const sent = feed.subscriptions.length

      act(() => {
        for (const position of [0.5, 1, 1.5, 2]) {
          store.dispatch(progressed({ position, duration: 9 }))
        }
      })
      // A frame we *do* expect, so anything the progress put on the wire ahead
      // of it has landed by the time this one has: frames reach the recorder
      // asynchronously, and counting them without a marker to wait for would
      // pass by arriving early rather than by never arriving.
      act(() => void store.dispatch(changeTheMatrix))
      await waitFor(() =>
        expect(feed.subscriptions.length).toBeGreaterThan(sent),
      )

      expect(feed.subscriptions).toHaveLength(sent + 1)
    },
  )

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

  /** Spec US 14's auto-reactivate used to be a five-second sweep in *this*
   *  component. It belongs to the store now (#91) — a single timer at the
   *  deadline, provable without rendering anything — so what is left to check
   *  here is that the matrix it changes still reaches the server. */
  it('re-subscribes when an avoid lapses under it', async () => {
    // Real time still advances the fake clock, so `waitFor` below can wait on
    // a frame while `advanceTimersByTime` skips the half hour.
    vi.useFakeTimers({ shouldAdvanceTime: true })
    try {
      const store = makeStore()
      renderApp('/', store)
      await lastSubscription()

      act(() => {
        store.dispatch(received(call))
        store.dispatch(avoid({ until: Date.now() + 30 * 60_000 }))
      })

      // The Talkgroup comes off the matrix…
      await waitFor(() =>
        expect(feed.subscriptions.at(-1)).toMatchObject({
          sel: { '11': { '54241': false } },
        }),
      )
      const silenced = feed.subscriptions.length

      // …and goes back on it when its deadline passes, with the listener
      // touching nothing. Only the store's own clock ran.
      await act(async () => {
        vi.advanceTimersByTime(31 * 60_000)
      })

      await waitFor(() =>
        expect(feed.subscriptions.length).toBeGreaterThan(silenced),
      )
      expect(feed.subscriptions.at(-1)).toMatchObject({ all: true, sel: {} })
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
          feed.subscriptions.push(JSON.parse(String(event.data)))
        })
        if (sockets > 1) return
        client.send(JSON.stringify({ t: 'call', call }))
        setTimeout(() => client.close(), 0)
      }),
    )

    renderApp('/')

    await waitFor(() => expect(sockets).toBe(2), { timeout: 3_000 })
    await waitFor(() => expect(feed.subscriptions.at(-1)?.since).toBe(call.id))
  })

  it('holds one socket open across the whole app', async () => {
    const user = userEvent.setup()
    renderApp('/')
    await waitFor(() => expect(feed.connections).toBe(1))

    await user.click(screen.getByRole('link', { name: 'Search' }))
    await user.click(screen.getByRole('link', { name: 'Live' }))

    // Moving between tabs must not drop the feed — or the queue behind it.
    expect(feed.connections).toBe(1)
  })

  /**
   * Feed off is a **hard** off (#80, CONTEXT.md **Feed off**), and the socket is
   * where that becomes true. Playback mode narrows the subscription to nothing —
   * the connection stays up because the listener is still here. Off closes it,
   * so bandwidth and battery go to zero, and so Web Push takes over: the
   * server's "an open socket means someone is listening" rule then tells the
   * truth about a listener who stopped listening.
   */
  describe('feed off (#80)', () => {
    it('closes the socket and opens no other', async () => {
      const store = makeStore()
      renderApp('/', store)
      await waitFor(() => expect(feed.connections).toBe(1))

      act(() => {
        store.dispatch(turnFeedOff())
      })

      await waitFor(() => expect(feed.closes).toBe(1))
      // Nothing reconnects behind it — that is the difference between off and a
      // dropped connection, which `connectLiveFeed` retries by design.
      await new Promise((resolve) => setTimeout(resolve, 50))
      expect(feed.connections).toBe(1)
    })

    /**
     * Coming back **subscribes**, and subscribes from now.
     *
     * Both halves matter and the first is the one that bit: a handle sends
     * nothing until it is given a matrix, and `turnFeedOn` changes neither the
     * matrix nor the push token — so the effect that normally subscribes does not
     * re-run. Left to that, the listener got a connected socket with nothing
     * selected on the server: a green light and permanent silence.
     *
     * Counted **per connection** for exactly that reason. Asserting on the last
     * frame overall passes while the new socket says nothing at all, because the
     * frame from before the feed went off is still the most recent one.
     */
    it('reconnects and subscribes afresh, never backfilling the silence', async () => {
      greeting = [{ t: 'call', call }]
      const store = makeStore()
      renderApp('/', store)
      // A Call arrives first, so there *is* a cursor to have dropped.
      await waitFor(() => expect(feed.subscriptions.length).toBeGreaterThan(0))
      await screen.findByText('FD Dispatch')

      act(() => {
        store.dispatch(turnFeedOff())
      })
      await waitFor(() => expect(feed.closes).toBe(1))
      const beforeOn = feed.subscriptions.length

      act(() => {
        store.dispatch(turnFeedOn())
      })

      await waitFor(() => expect(feed.connections).toBe(2))
      await waitFor(() =>
        expect(feed.subscriptions.length).toBeGreaterThan(beforeOn),
      )
      // What the new socket was told: the listener's matrix, and no cursor.
      const afterOn = feed.subscriptions.slice(beforeOn)
      for (const frame of afterOn) {
        expect(frame).toMatchObject({ t: 'sub', all: true })
        expect(frame.since).toBeUndefined()
      }
    })

    /** A listener who left the feed off and reloaded gets silence, not a socket:
     *  the choice is honoured before anything connects, so their phone does not
     *  spend a round trip and a burst of audio learning what they already
     *  chose. */
    it('never opens a socket at all when the feed starts off', async () => {
      renderApp('/', makeStore({ storage: offStorage(), namespace: 'default' }))

      // Give it as long as a connection would have taken.
      await new Promise((resolve) => setTimeout(resolve, 100))
      expect(feed.connections).toBe(0)
      expect(await screen.findByText('FEED OFF')).toBeInTheDocument()
    })
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
