import { act, screen, waitFor, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { axe } from 'vitest-axe'
import { beforeEach, describe, expect, it } from 'vitest'

import { lagged, received, selectLiveMatrix, selectQueueDepth } from '@/store/live'
import { playResults } from '@/store/playback'
import { progressed, selectProgress } from '@/store/transport'
import { makeStore, type AppStore } from '@/store/store'
import { liveFeed } from '@/test/handlers'
import { server } from '@/test/setup'
import { renderApp } from '@/test/utils'
import type { Call } from '@/types'

/** Everything the client asked the server for, newest last. */
let subscriptions: Record<string, unknown>[] = []

beforeEach(() => {
  subscriptions = []
  server.use(
    liveFeed.addEventListener('connection', ({ client }) => {
      client.addEventListener('message', (event) => {
        subscriptions.push(JSON.parse(String(event.data)))
      })
    }),
  )
})

async function lastSubscription() {
  await waitFor(() => expect(subscriptions.length).toBeGreaterThan(0))
  return subscriptions.at(-1)
}

function call(overrides: Partial<Call> = {}): Call {
  return {
    id: 1,
    systemRef: 11,
    systemLabel: 'Fulton County',
    talkgroupRef: 54241,
    talkgroupLabel: 'FD Dispatch',
    talkgroupTag: 'Fire Dispatch',
    talkgroupGroup: 'Fire',
    frequency: 853_412_500,
    source: 1602381,
    timestamp: Date.parse('2026-07-25T14:32:05'),
    audioUrl: '/api/call/1/audio',
    ...overrides,
  }
}

/** Render the app on the Live screen with `calls` already arrived. */
function listening(...calls: Call[]): AppStore {
  const store = makeStore()
  renderApp('/', store)
  act(() => {
    for (const one of calls) store.dispatch(received({ call: one }))
  })
  return store
}

const display = () => screen.getByRole('region', { name: 'Scanner display' })

describe('LiveScreen', () => {
  describe('before anything has been heard', () => {
    it('says it is waiting rather than showing an empty display', () => {
      renderApp('/')

      expect(screen.getByText(/waiting for the first call/i)).toBeInTheDocument()
    })

    /** A listener staring at a silent scanner needs to know whether it's quiet
     *  or broken — so the link state is on the screen, not in a console. */
    it('shows the link state', async () => {
      renderApp('/')

      expect(await screen.findByText(/^connected$/i)).toBeInTheDocument()
    })

    it('offers nothing to hold, skip, or avoid', () => {
      renderApp('/')

      expect(screen.getByRole('button', { name: 'Hold system' })).toBeDisabled()
      expect(screen.getByRole('button', { name: 'Skip' })).toBeDisabled()
      expect(screen.getByRole('button', { name: 'Avoid' })).toBeDisabled()
      expect(screen.getByRole('button', { name: 'Replay' })).toBeDisabled()
    })
  })

  describe('the display (spec US 16-17)', () => {
    it('reads out everything about the Call being played', () => {
      listening(call())

      const readout = display()
      expect(within(readout).getByText('FD Dispatch')).toBeInTheDocument()
      expect(within(readout).getByText('Fulton County')).toBeInTheDocument()
      expect(within(readout).getByText(/Fire Dispatch/)).toBeInTheDocument()
      // Frequency in MHz, TGID and Unit ID as the recorder sent them.
      expect(screen.getByTestId('stat-frequency')).toHaveTextContent('853.412500')
      expect(screen.getByTestId('stat-talkgroup')).toHaveTextContent('54241')
      expect(screen.getByTestId('stat-unit')).toHaveTextContent('1602381')
    })

    it('falls back to Refs when the recorder sent no labels', () => {
      listening(
        call({
          systemLabel: undefined,
          talkgroupLabel: undefined,
          talkgroupTag: undefined,
          talkgroupGroup: undefined,
          source: undefined,
        }),
      )

      const readout = display()
      expect(within(readout).getByText('Talkgroup 54241')).toBeInTheDocument()
      expect(within(readout).getByText('System 11')).toBeInTheDocument()
      expect(screen.getByTestId('stat-unit')).toHaveTextContent('—')
    })

    it('counts what is waiting behind it', () => {
      const store = listening(call(), call({ id: 2 }), call({ id: 3 }))

      expect(selectQueueDepth(store.getState())).toBe(2)
      expect(screen.getByLabelText('Queued calls')).toHaveTextContent('2')
    })

    /** ADR-0004's `lagged`: rdio drops those Calls silently. Saying so is the
     *  point of the notice. */
    it('admits Calls a slow connection cost the listener', () => {
      const store = listening(call())

      act(() => {
        store.dispatch(lagged(3))
      })

      expect(screen.getByText(/3 missed/i)).toBeInTheDocument()
    })
  })

  describe('the controls (spec US 11-15)', () => {
    it('holds the System, narrowing what the server sends', async () => {
      const user = userEvent.setup()
      const store = listening(call())

      await user.click(screen.getByRole('button', { name: 'Hold system' }))

      expect(selectLiveMatrix(store.getState())).toEqual({
        all: false,
        sel: { '11': { '*': true } },
      })
      // The narrowing reaches the server, so the Calls stop arriving at all.
      expect(await lastSubscription()).toMatchObject({
        t: 'sub',
        all: false,
        sel: { '11': { '*': true } },
      })
      expect(screen.getByRole('button', { name: 'Hold system' })).toHaveAttribute(
        'aria-pressed',
        'true',
      )
    })

    it('holds the Talkgroup alone', async () => {
      const user = userEvent.setup()
      const store = listening(call())

      await user.click(screen.getByRole('button', { name: 'Hold talkgroup' }))

      expect(selectLiveMatrix(store.getState())).toEqual({
        all: false,
        sel: { '11': { '54241': true } },
      })
    })

    it('releases a hold on a second press', async () => {
      const user = userEvent.setup()
      const store = listening(call())

      await user.click(screen.getByRole('button', { name: 'Hold system' }))
      await user.click(screen.getByRole('button', { name: 'Hold system' }))

      expect(selectLiveMatrix(store.getState())).toEqual({ all: true, sel: {} })
    })

    it('skips to the next queued Call', async () => {
      const user = userEvent.setup()
      listening(call(), call({ id: 2, talkgroupLabel: 'PD Dispatch' }))

      await user.click(screen.getByRole('button', { name: 'Skip' }))

      expect(within(display()).getByText('PD Dispatch')).toBeInTheDocument()
    })

    it('avoids the Talkgroup and moves on', async () => {
      const user = userEvent.setup()
      const store = listening(
        call(),
        call({ id: 2, talkgroupRef: 99, talkgroupLabel: 'PD Dispatch' }),
      )

      await user.click(screen.getByRole('button', { name: 'Avoid' }))

      expect(selectLiveMatrix(store.getState())).toEqual({
        all: true,
        sel: { '11': { '54241': false } },
      })
      expect(within(display()).getByText('PD Dispatch')).toBeInTheDocument()
    })

    /** Spec US 14's timed mode: a chatty talkgroup goes away for a while and
     *  comes back on its own. */
    it.each([30, 60, 120])('avoids for %i minutes on request', async (minutes) => {
      const user = userEvent.setup()
      const store = listening(call())
      const before = Date.now()

      await user.click(
        screen.getByRole('button', { name: `Avoid for ${minutes} minutes` }),
      )

      const until = store.getState().live.avoided['11:54241']
      expect(until).toBeGreaterThanOrEqual(before + minutes * 60_000)
      expect(until).toBeLessThan(before + (minutes + 1) * 60_000)
    })

    /** An indefinite avoid has no deadline to lapse, so the display has to
     *  offer a way back — US 14's timed mode is the *optional* one. */
    it('lets the listener take an avoid back', async () => {
      const user = userEvent.setup()
      const store = listening(call())
      await user.click(screen.getByRole('button', { name: 'Avoid' }))

      await user.click(
        await screen.findByRole('button', { name: /Stop avoiding/ }),
      )

      expect(selectLiveMatrix(store.getState())).toEqual({ all: true, sel: {} })
      expect(
        screen.queryByRole('button', { name: /Stop avoiding/ }),
      ).not.toBeInTheDocument()
    })

    it('pauses without losing what is queued', async () => {
      const user = userEvent.setup()
      const store = listening(call(), call({ id: 2 }))

      await user.click(screen.getByRole('button', { name: 'Pause' }))

      expect(within(display()).getByText('FD Dispatch')).toBeInTheDocument()
      expect(selectQueueDepth(store.getState())).toBe(1)
      expect(screen.getByRole('button', { name: 'Resume' })).toBeInTheDocument()
    })
  })

  /** Spec US 26: an archived Call plays *over* the feed. The feed's own Call
   *  stays on the display — it is what comes back — but the waveform is not
   *  drawing that Call's playhead, because it isn't the one on the element. */
  it('stops drawing a playhead while an archived Call interrupts', async () => {
    const store = listening(call())
    act(() => {
      store.dispatch(
        playResults({ results: [{ ...call(), id: 900 }], index: 0 }),
      )
    })
    // Separately, because taking over the element resets the transport first.
    act(() => {
      store.dispatch(progressed({ position: 5, duration: 10 }))
    })

    expect(within(display()).getByText('FD Dispatch')).toBeInTheDocument()
    expect(selectProgress(store.getState())).toBeCloseTo(0.5)
    // Nothing on the live display is lit by the archive's progress.
    expect(
      within(display())
        .getByTestId('waveform')
        .querySelectorAll('[data-lit="true"]'),
    ).toHaveLength(0)
  })

  it('says so when the link is down', async () => {
    server.use(
      liveFeed.addEventListener('connection', ({ client }) => client.close()),
    )

    renderApp('/')

    expect(await screen.findByText(/no link to the server/i)).toBeInTheDocument()
  })

  /**
   * The LIVE FEED master toggle (#80) — rdio parity, and the one control the
   * app was missing. Pause is transport-only: the subscription keeps
   * streaming and the queue keeps filling behind it. This is the hard off.
   */
  describe('the live feed toggle (#80)', () => {
    const toggle = () => screen.getByRole('button', { name: 'Live feed' })

    it('reads as on, and says the feed is live', async () => {
      renderApp('/')

      expect(await screen.findByText(/^connected$/i)).toBeInTheDocument()
      expect(toggle()).toHaveAttribute('aria-pressed', 'true')
    })

    it('stops the Call and empties the queue when switched off', async () => {
      const user = userEvent.setup()
      const store = listening(call({ id: 1 }), call({ id: 2 }))
      expect(within(display()).getByText('FD Dispatch')).toBeInTheDocument()
      expect(selectQueueDepth(store.getState())).toBe(1)

      await user.click(toggle())

      expect(selectQueueDepth(store.getState())).toBe(0)
      expect(
        screen.queryByRole('region', { name: 'Scanner display' }),
      ).not.toBeInTheDocument()
      expect(toggle()).toHaveAttribute('aria-pressed', 'false')
    })

    /** A deliberate off is not a gap: the missed counter admits traffic the
     *  listener wanted and did not get, and silence they chose is not that. */
    it('does not count the silence as missed', async () => {
      const user = userEvent.setup()
      const store = listening(call())
      act(() => void store.dispatch(lagged(3)))

      await user.click(toggle())

      expect(screen.getByText(/3 missed/i)).toBeInTheDocument()
    })

    /** Its own state, distinct from both the green connected dot and NO LINK —
     *  a listener must be able to tell "I turned this off" from "the server went
     *  away", because only one of them is theirs to fix. */
    it('says FEED OFF, which is neither connected nor NO LINK', async () => {
      const user = userEvent.setup()
      renderApp('/')
      await screen.findByText(/^connected$/i)

      await user.click(toggle())

      expect(screen.getByText('FEED OFF')).toBeInTheDocument()
      expect(screen.queryByText(/^connected$/i)).not.toBeInTheDocument()
      expect(screen.queryByText('NO LINK')).not.toBeInTheDocument()
      expect(screen.getByText(/switched off/i)).toBeInTheDocument()
    })

    /** Nothing that acts on a Call makes sense with no feed and no Call — and a
     *  control that looks available but does nothing is worse than one that is
     *  plainly out of reach. */
    it('disables the feed-dependent controls while off', async () => {
      const user = userEvent.setup()
      listening(call())
      expect(screen.getByRole('button', { name: 'Skip' })).toBeEnabled()

      await user.click(toggle())

      // Every control that acts on a live Call — enumerated, because adding one
      // and forgetting to gate it is exactly the mistake this catches.
      for (const name of [
        'Hold system',
        'Hold talkgroup',
        'Skip',
        'Replay',
        'Pause',
        'Avoid',
        'Avoid for 30 minutes',
        'Avoid for 60 minutes',
        'Avoid for 120 minutes',
      ]) {
        expect(screen.getByRole('button', { name })).toBeDisabled()
      }
      // ...and the toggle itself stays reachable, or there is no way back.
      expect(toggle()).toBeEnabled()
    })

    /** The RECENT list replays a Call, so it is a feed-dependent control too —
     *  and the one most likely to be missed, because it is generated rather than
     *  written out. Switching off files the Call it cut off into history, so the
     *  list is *guaranteed* non-empty right afterwards: a row that looked
     *  clickable and did nothing would be the common case, not a corner. */
    it('puts the RECENT rows out of reach too, rather than letting them do nothing', async () => {
      const user = userEvent.setup()
      listening(call({ id: 1, talkgroupLabel: 'FD Dispatch' }))

      await user.click(toggle())

      const recent = screen.getByRole('list', { name: 'Recent calls' })
      const rows = within(recent).getAllByRole('button')
      expect(rows.length).toBeGreaterThan(0)
      for (const row of rows) expect(row).toBeDisabled()
    })

    it('comes back on and plays what arrives next', async () => {
      const user = userEvent.setup()
      const store = listening(call())
      await user.click(toggle())

      await user.click(toggle())

      expect(toggle()).toHaveAttribute('aria-pressed', 'true')
      act(() => void store.dispatch(received({ call: call({ id: 9 }) })))
      expect(within(display()).getByText('FD Dispatch')).toBeInTheDocument()
    })

    it('has no accessibility violations while off', async () => {
      const user = userEvent.setup()
      const { container } = renderApp('/')
      await user.click(toggle())

      expect(await axe(container)).toHaveNoViolations()
    })
  })

  describe('replay (spec US 13)', () => {
    /** US 13 asks for "current, previous, and back through the last five". The
     *  button covers the first two; the list below it reaches further back. */
    it('starts the Call that is playing over again', async () => {
      const user = userEvent.setup()
      const store = listening(call())
      const before = store.getState().live.playId

      await user.click(screen.getByRole('button', { name: 'Replay' }))

      expect(within(display()).getByText('FD Dispatch')).toBeInTheDocument()
      // Same Call, played again: only the play id says it restarted.
      expect(store.getState().live.playId).toBeGreaterThan(before)
    })

    it('brings the last Call back once the feed has fallen quiet', async () => {
      const user = userEvent.setup()
      listening(call())
      await user.click(screen.getByRole('button', { name: 'Skip' }))
      expect(screen.getByText(/waiting for the first call/i)).toBeInTheDocument()

      await user.click(screen.getByRole('button', { name: 'Replay' }))

      expect(within(display()).getByText('FD Dispatch')).toBeInTheDocument()
    })

    it('lists the recent Calls and replays a chosen one', async () => {
      const user = userEvent.setup()
      listening(
        call({ id: 1, talkgroupLabel: 'One' }),
        call({ id: 2, talkgroupLabel: 'Two' }),
        call({ id: 3, talkgroupLabel: 'Three' }),
      )
      await user.click(screen.getByRole('button', { name: 'Skip' }))
      await user.click(screen.getByRole('button', { name: 'Skip' }))

      const history = screen.getByRole('list', { name: 'Recent calls' })
      expect(within(history).getAllByRole('listitem')).toHaveLength(2)

      await user.click(within(history).getByRole('button', { name: /One/ }))

      expect(within(display()).getByText('One')).toBeInTheDocument()
    })

    /**
     * What the listener sees: replaying from RECENT put the Call back into
     * RECENT, so the list held it twice.
     *
     * The store tests pin the mechanism; this pins the symptom, because the list
     * is what anyone would actually notice — and `<li key={call.id}>` gave React
     * two children with the same key while it was wrong.
     */
    it('does not put a replayed Call back into RECENT', async () => {
      const user = userEvent.setup()
      listening(
        call({ id: 1, talkgroupLabel: 'One' }),
        call({ id: 2, talkgroupLabel: 'Two' }),
        call({ id: 3, talkgroupLabel: 'Three' }),
      )
      await user.click(screen.getByRole('button', { name: 'Skip' }))
      await user.click(screen.getByRole('button', { name: 'Skip' }))

      const history = screen.getByRole('list', { name: 'Recent calls' })
      await user.click(within(history).getByRole('button', { name: /One/ }))

      // One is playing, so it is not "recently played" any more. Matched on the
      // rows' accessible names, which is what a listener reads.
      expect(within(display()).getByText('One')).toBeInTheDocument()
      expect(within(history).queryAllByRole('button', { name: /One/ })).toEqual([])

      // ...and when it finishes, it comes back exactly once.
      await user.click(screen.getByRole('button', { name: 'Skip' }))
      expect(within(history).getAllByRole('button', { name: /One/ })).toHaveLength(1)
    })
  })

  it('has no accessibility violations with a Call on the display', async () => {
    const store = makeStore()
    const { container } = renderApp('/', store)
    act(() => {
      store.dispatch(received({ call: call() }))
    })

    expect(await axe(container)).toHaveNoViolations()
  })
})
