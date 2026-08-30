import { act, screen, waitFor, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { axe } from 'vitest-axe'
import { beforeEach, describe, expect, it } from 'vitest'

import {
  advance,
  gapped,
  lagged,
  received,
  selectHold,
  selectIsAvoided,
  selectLiveCall,
  selectLiveMatrix,
  selectPriority,
  selectQueueDepth,
} from '@/store/live'
import {
  enterPlaybackMode,
  selectPlaybackMode,
  startRun,
} from '@/store/playback'
import { progressed, selectProgress } from '@/store/transport'
import { makeStore, type AppStore } from '@/store/store'
import { liveFeed, searchPage } from '@/test/handlers'
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
    unitRef: 1602381,
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
    for (const one of calls) store.dispatch(received(one, one.id))
  })
  return store
}

const display = () => screen.getByRole('region', { name: 'Scanner display' })

/**
 * The accessible name of every control on the screen the listener can actually
 * press — the derived enablement (#88) read back off the rendered page.
 *
 * Scoped to `main` so the tab bar and the docked banner, which belong to the
 * shell and not to the feed, stay out of it. The RECENT rows are in here too:
 * they replay a Call, so they are feed-dependent controls like any other, and
 * they are the ones most easily forgotten because they are generated.
 */
const reachable = () =>
  within(screen.getByRole('main'))
    .getAllByRole('button')
    .filter((button) => !button.hasAttribute('disabled'))
    .map((button) => button.getAttribute('aria-label') ?? button.textContent?.trim())

/** The feed's master switch is never out of reach, or there is no way back. */
const ALWAYS_REACHABLE = ['Live feed']

const toggle = () => screen.getByRole('button', { name: 'Live feed' })

describe('LiveScreen', () => {
  describe('before anything has been heard', () => {
    /** Two different silences, and the screen distinguishes them (#88): a
     *  connection being *made* is not one that has gone, and it is the state
     *  every visit opens in. Saying "no link" there would make the words
     *  worthless the rest of the time. */
    it('says it is connecting, then that it is waiting', async () => {
      renderApp('/')

      expect(screen.getByText(/connecting to the server/i)).toBeInTheDocument()
      expect(
        await screen.findByText(/waiting for the first call/i),
      ).toBeInTheDocument()
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
      // Frequency in MHz, TGID and the radio as the recorder sent them.
      expect(screen.getByTestId('stat-frequency')).toHaveTextContent('853.412500')
      expect(screen.getByTestId('stat-talkgroup')).toHaveTextContent('54241')
      expect(screen.getByTestId('stat-unit')).toHaveTextContent('1602381')
    })

    /** The radio that keyed, named where anybody has named it and tappable
     *  through to its history either way (#47, spec US 42/44). rdio-scanner
     *  shows the bare number and nothing behind it. */
    it('names the radio that keyed, and links to its history', () => {
      listening(call({ unitRef: 1602381, unitLabel: 'MEDIC 7' }))

      const unit = within(display()).getByRole('link', {
        name: 'History for unit MEDIC 7',
      })
      expect(unit).toHaveTextContent('MEDIC 7')
      expect(unit).toHaveAttribute('href', '/unit/11/1602381')
    })

    /** "Who was that ten minutes ago" answered from the list it happened in —
     *  the recent rows carry the same link. */
    it('links the radio on every recent row too', async () => {
      const user = userEvent.setup()
      listening(call({ id: 1, unitRef: 1602381, unitLabel: 'MEDIC 7' }))
      await user.click(screen.getByRole('button', { name: 'Skip' }))

      const recent = screen.getByRole('list', { name: 'Recent calls' })
      expect(
        within(recent).getByRole('link', { name: 'History for unit MEDIC 7' }),
      ).toHaveAttribute('href', '/unit/11/1602381')
    })

    it('falls back to Refs when the recorder sent no labels', () => {
      listening(
        call({
          systemLabel: undefined,
          talkgroupLabel: undefined,
          talkgroupTag: undefined,
          talkgroupGroup: undefined,
          unitRef: undefined,
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
      expect(screen.getByLabelText(/Queued calls/)).toHaveTextContent('2')
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

    /** ADR-0004's `gap`: a **Backfill** could not reach back far enough, so the
     *  listener's history has a hole nobody can count. Saying "some" is what
     *  stops a silent truncation reading as having missed nothing. */
    it('admits a Backfill that could not reach back far enough', () => {
      const store = listening(call())

      act(() => {
        store.dispatch(gapped())
      })

      expect(screen.getByText(/some missed/i)).toBeInTheDocument()
    })

    /** Both at once: a number that is known to be an undercount. */
    it('marks a counted gap as a floor when there is one it could not count', () => {
      const store = listening(call())

      act(() => {
        store.dispatch(lagged(3))
        store.dispatch(gapped())
      })

      expect(screen.getByText(/3\+ missed/i)).toBeInTheDocument()
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

    /**
     * An indefinite avoid has no deadline to lapse, so the display has to offer
     * a way back — US 14's timed mode is the *optional* one.
     *
     * Since #58 that way is the Avoid sheet rather than a bare "clear them
     * all": the control names how many are in force and opens the list, and
     * clearing the lot is one of the things inside it.
     */
    it('lets the listener take an avoid back', async () => {
      const user = userEvent.setup()
      const store = listening(call())
      await user.click(screen.getByRole('button', { name: 'Avoid' }))

      await user.click(await screen.findByRole('button', { name: /Avoiding 1/ }))
      await user.click(await screen.findByRole('button', { name: 'Clear all' }))

      expect(selectLiveMatrix(store.getState())).toEqual({ all: true, sel: {} })
      expect(
        screen.queryByRole('button', { name: /Avoiding/ }),
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
        startRun({
          search: {},
          page: searchPage({ results: [{ ...call(), id: 900 }] }),
          index: 0,
        }),
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

    /**
     * Nothing that acts on a Call makes sense with no feed and no Call — and a
     * control that looks available but does nothing is worse than one plainly
     * out of reach.
     *
     * Asserted as "everything except the two that must stay reachable" rather
     * than as a list of the nine that must not (#88). A hand-written list is
     * silent about the control somebody adds next, which is the mistake this
     * exists to catch; the complement covers it by construction.
     */
    it('puts every control except the way back out of reach while off', async () => {
      const user = userEvent.setup()
      listening(call())
      expect(screen.getByRole('button', { name: 'Skip' })).toBeEnabled()

      await user.click(toggle())

      expect(reachable()).toEqual(ALWAYS_REACHABLE)
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
      act(() => void store.dispatch(received(call({ id: 9 }), 9)))
      expect(within(display()).getByText('FD Dispatch')).toBeInTheDocument()
    })

    it('has no accessibility violations while off', async () => {
      const user = userEvent.setup()
      const { container } = renderApp('/')
      await user.click(toggle())

      expect(await axe(container)).toHaveNoViolations()
    })
  })

  /**
   * The second reason the feed can be quiet (#88). Playback mode and the live
   * feed are mutually exclusive (CONTEXT.md), so the screen has to say which of
   * the two silences this is — a green light over a feed that is deliberately
   * not delivering is the same lie FEED OFF was invented to stop telling.
   *
   * The banner, the mini-player and the one-tap way back belong to #56; this is
   * the header and the controls telling the truth about the derived cause.
   */
  describe('playback mode', () => {
    const playArchive = () => {
      const store = listening(call())
      act(() => void store.dispatch(enterPlaybackMode()))
      return store
    }

    it('says the archive has the audio, rather than showing a live feed', async () => {
      const store = listening(call())
      await screen.findByText(/^connected$/i)

      act(() => void store.dispatch(enterPlaybackMode()))

      expect(screen.getByText('PLAYBACK')).toBeInTheDocument()
      expect(screen.queryByText(/^connected$/i)).not.toBeInTheDocument()
      expect(screen.getByText(/playing from the archive/i)).toBeInTheDocument()
    })

    /** The socket stays open with an empty subscription — this is not FEED OFF,
     *  and offering the way back to a feed that was never switched off would be
     *  a different lie. */
    it('leaves the feed switched on, because it was never switched off', () => {
      playArchive()

      expect(toggle()).toHaveAttribute('aria-pressed', 'true')
      expect(screen.queryByText('FEED OFF')).not.toBeInTheDocument()
    })

    /**
     * A **Hold** outlives the Call it was placed on, and entering playback mode
     * does not lift it — so this is the one control that would still be
     * reachable without the gate, and the only version of this test that bites.
     * Everything else is disabled anyway once `enterPlaybackMode` clears the
     * Call and the RECENT list.
     */
    it('puts the live controls out of reach, as an off feed does', async () => {
      const user = userEvent.setup()
      const store = listening(call())
      await user.click(screen.getByRole('button', { name: 'Hold system' }))
      expect(screen.getByRole('button', { name: 'Hold system' })).toBeEnabled()

      act(() => void store.dispatch(enterPlaybackMode()))

      // …except the way back, which #56 adds beside the master switch: with
      // every other control dead, a screen offering no way out of the state it
      // is describing is the gap this ticket closes.
      expect(reachable()).toEqual([...ALWAYS_REACHABLE, 'Back to live'])
    })

    /**
     * **One tap back** (#56). The way out is its own control rather than the
     * LIVE FEED switch: that switch reads *on* here and is right to — the feed
     * was never switched off, playback borrowed the audio — so overloading it
     * would trade one lie for another. #88 pinned that reading; this closes the
     * gap it left.
     */
    it('offers one tap back to the live feed', async () => {
      const user = userEvent.setup()
      const store = playArchive()

      await user.click(screen.getByRole('button', { name: 'Back to live' }))

      expect(selectPlaybackMode(store.getState())).toBe('live')
      expect(await screen.findByText(/^connected$/i)).toBeInTheDocument()
    })

    /** It belongs to playback mode alone — a feed the Listener switched off has
     *  its own way back, and one that is merely live has nothing to return
     *  from. */
    it('offers it nowhere else', async () => {
      const user = userEvent.setup()
      listening(call())
      expect(
        screen.queryByRole('button', { name: 'Back to live' }),
      ).toBeNull()

      await user.click(toggle())

      expect(
        screen.queryByRole('button', { name: 'Back to live' }),
      ).toBeNull()
    })
  })

  /**
   * **The card outlives the transmission** (#56, spec US 54).
   *
   * A scanner's readout does not go blank the instant a Talkgroup unkeys, and
   * neither should this one: what was just said is what a Listener is still
   * reading, and — the reason this ticket exists — *the moment they reach for
   * Avoid*. Before this the display fell back to "waiting for the first call",
   * which was a lie about an archive with Calls in it and left Hold and Avoid
   * pointing at nothing.
   */
  describe('the last Call, after it has ended (#56)', () => {
    /** Skip with an empty queue: nothing is on the air, and the feed is fine. */
    const fallenQuiet = async (user: ReturnType<typeof userEvent.setup>) => {
      const store = listening(call())
      await user.click(screen.getByRole('button', { name: 'Skip' }))
      return store
    }

    it('stays on the display rather than blanking the screen', async () => {
      const user = userEvent.setup()
      await fallenQuiet(user)

      expect(within(display()).getByText('FD Dispatch')).toBeInTheDocument()
      expect(screen.queryByText(/waiting for the first call/i)).toBeNull()
    })

    /** Dimmed, and *said* — a card that looks identical to a Call in progress
     *  would be the same lie in the other direction. The word is what a screen
     *  reader gets, since the dimming is not something it can see. */
    it('says the transmission has ended', async () => {
      const user = userEvent.setup()
      await fallenQuiet(user)

      expect(within(display()).getByText(/^ended$/i)).toBeInTheDocument()
    })

    it('says no such thing while a Call is actually playing', () => {
      listening(call())

      expect(within(display()).queryByText(/^ended$/i)).toBeNull()
    })

    /** The whole point: a chatty Talkgroup stops keying, and *that* is when
     *  somebody reaches for the mute. */
    it('lets Avoid act on it, which is when a Listener reaches for Avoid', async () => {
      const user = userEvent.setup()
      const store = await fallenQuiet(user)

      await user.click(screen.getByRole('button', { name: 'Avoid' }))

      expect(selectIsAvoided(store.getState(), 11, 54241)).toBe(true)
    })

    it('lets Hold act on it too', async () => {
      const user = userEvent.setup()
      const store = await fallenQuiet(user)

      await user.click(screen.getByRole('button', { name: 'Hold talkgroup' }))

      expect(selectHold(store.getState())).toEqual({
        systemRef: 11,
        talkgroupRef: 54241,
      })
    })

    /** Nothing is on the air, so the two controls that act on *audio* stay out
     *  of reach — a card that re-enabled everything would be a different lie. */
    it('leaves Skip and Pause out of reach, because there is no audio', async () => {
      const user = userEvent.setup()
      await fallenQuiet(user)

      expect(screen.getByRole('button', { name: 'Skip' })).toBeDisabled()
      expect(screen.getByRole('button', { name: 'Pause' })).toBeDisabled()
    })

    /** The two silences the Listener chose keep #88's own words: there is
     *  nothing to act on there, so a card with every control dead would say
     *  less than the sentence that tells them how to get the feed back. */
    it('gives way to the reason when the feed is one the Listener switched off', async () => {
      const user = userEvent.setup()
      listening(call())

      await user.click(toggle())

      expect(
        screen.queryByRole('region', { name: 'Scanner display' }),
      ).toBeNull()
      expect(screen.getByText(/switched off/i)).toBeInTheDocument()
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
      // Still on the card, dimmed and marked over (#56) — which is what makes
      // Replay's target the thing the Listener is looking at.
      expect(within(display()).getByText(/^ended$/i)).toBeInTheDocument()

      await user.click(screen.getByRole('button', { name: 'Replay' }))

      expect(within(display()).getByText('FD Dispatch')).toBeInTheDocument()
      expect(within(display()).queryByText(/^ended$/i)).toBeNull()
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
      store.dispatch(received(call(), 1))
    })

    expect(await axe(container)).toHaveNoViolations()
  })
})

describe('LiveScreen — what the recorder knew (#42)', () => {
  it('badges an emergency on the scanner display', async () => {
    // The emergency bit is the one thing on a Call a listener must not have to
    // go looking for (#42, and what #53 pushes on).
    listening(call({ emergency: true }))

    expect(within(display()).getByTitle('Emergency')).toBeInTheDocument()
  })

  it('leaves the display unadorned for an ordinary call', () => {
    listening(call())

    expect(within(display()).queryByTitle('Emergency')).toBeNull()
    expect(within(display()).queryByTitle('Encrypted')).toBeNull()
  })

  /**
   * An encrypted Call is activity, not audio (#42, spec US 9) — it never plays.
   * Since #56 it is still *shown*: it goes straight to the head of RECENT, and
   * the card shows the last Call there is, dimmed and marked over. That is the
   * honest reading — the channel was busy and this Listener heard nothing — and
   * it is the only thing that keeps the card and the controls in step, because
   * Hold and Avoid resolve the same Call whether or not it is drawn. An
   * encrypted Talkgroup that will not stop keying is a prime candidate for
   * Avoid, and before this there was no way to reach one from the Live screen
   * at all.
   */
  it('shows encrypted activity without ever playing it', () => {
    const { audioUrl: _none, ...metadataOnly } = call({ id: 7 })
    const store = listening({ ...metadataOnly, encrypted: true })

    // Nothing is playing: an encrypted Call has no audio, and the transport
    // would otherwise sit on it forever with no `src` to end.
    expect(selectLiveCall(store.getState())).toBeNull()
    expect(within(display()).getByText(/^ended$/i)).toBeInTheDocument()
    expect(within(display()).getByTitle('Encrypted')).toBeInTheDocument()
    const recent = screen.getByRole('list', { name: 'Recent calls' })
    expect(within(recent).getByTitle('Encrypted')).toBeInTheDocument()
  })

  /** …and it can be silenced from the card it is drawn on. */
  it('lets an encrypted Talkgroup be avoided from the display', async () => {
    const user = userEvent.setup()
    const { audioUrl: _none, ...metadataOnly } = call({ id: 7 })
    const store = listening({ ...metadataOnly, encrypted: true })

    await user.click(screen.getByRole('button', { name: 'Avoid' }))

    expect(selectIsAvoided(store.getState(), 11, 54241)).toBe(true)
  })

  /**
   * **Patch provenance, on the display and in RECENT** (#56, spec US 54).
   *
   * The criterion names the display first, and this is the surface where it
   * matters most: a Listener hearing traffic on a channel they did not select
   * is owed the reason, and a **Patch** is the reason (CONTEXT.md — the server
   * delivers a Call that reaches any selected member). rdio-scanner routes
   * patched traffic correctly and then tells a Listener nothing at all.
   */
  it('badges a patched call on the display and in RECENT alike', async () => {
    const user = userEvent.setup()
    listening(call({ patches: [54242, 54255] }))

    expect(
      within(display()).getByTitle('Patched to 54242, 54255'),
    ).toBeInTheDocument()

    // …and it survives the Call ending, because the badge belongs to the Call
    // rather than to it being on the air.
    await user.click(screen.getByRole('button', { name: 'Skip' }))
    const recent = screen.getByRole('list', { name: 'Recent calls' })
    expect(
      within(recent).getByTitle('Patched to 54242, 54255'),
    ).toBeInTheDocument()
    expect(
      within(display()).getByTitle('Patched to 54242, 54255'),
    ).toBeInTheDocument()
  })

  it('leaves an unpatched call unadorned', () => {
    listening(call())

    expect(within(display()).queryByTitle(/^Patched/)).toBeNull()
  })

  it('badges an encrypted call in RECENT while another one plays', () => {
    const { audioUrl: _none, ...metadataOnly } = call({ id: 7 })
    listening(call({ id: 1 }), { ...metadataOnly, encrypted: true })

    expect(within(display()).getByText('FD Dispatch')).toBeInTheDocument()
    const recent = screen.getByRole('list', { name: 'Recent calls' })
    expect(within(recent).getByTitle('Encrypted')).toBeInTheDocument()
  })
})

/**
 * **Priority** from the Live screen (#58, spec US 27).
 *
 * The Talkgroups panel is the other place it is set, and this is the one that
 * matters in the moment: a Listener works out that dispatch should outrank
 * tactical chatter *while hearing it*, and the panel is two taps and a
 * four-hundred-row list away.
 */
describe('Priority on the Call being shown (#58)', () => {
  const mark = () => screen.getByRole('button', { name: /priority/i })

  it('marks the Talkgroup on the display', async () => {
    const user = userEvent.setup()
    const store = listening(call())

    await user.click(mark())

    expect(selectPriority(store.getState())).toEqual(['11:54241'])
    expect(mark()).toHaveAttribute('aria-pressed', 'true')
  })

  it('lets it go again', async () => {
    const user = userEvent.setup()
    const store = listening(call())

    await user.click(mark())
    await user.click(mark())

    expect(selectPriority(store.getState())).toEqual([])
  })

  /** #56's rule: the display outlives the transmission, so the controls under
   *  it go on meaning something after a chatty Talkgroup stops keying — which
   *  is exactly when a Listener reaches for one. */
  it('acts on the Call the display kept up after it ended', async () => {
    const user = userEvent.setup()
    const store = listening(call())
    await user.click(screen.getByRole('button', { name: 'Skip' }))

    await user.click(mark())

    expect(selectPriority(store.getState())).toEqual(['11:54241'])
  })

  it('is out of reach with nothing on the display', () => {
    renderApp('/')

    expect(mark()).toBeDisabled()
  })

  /** It reads as pressed for the Talkgroup pressing it would act on, so the
   *  state on screen and the state the button changes are the same one. */
  it('reads off the Call now showing, not the one before it', async () => {
    const user = userEvent.setup()
    const store = listening(call())
    await user.click(mark())

    act(() => {
      store.dispatch(received(call({ id: 2, talkgroupRef: 999 }), 2))
      store.dispatch(advance())
    })

    expect(mark()).toHaveAttribute('aria-pressed', 'false')
  })
})

describe('the way to the session log (#58, spec US 28)', () => {
  /** RECENT reaches back five; the log reaches back to when the app opened. The
   *  way there is from the list it extends. */
  it('is offered from the RECENT heading once something has been heard', async () => {
    const user = userEvent.setup()
    listening(call())

    await user.click(screen.getByRole('button', { name: 'Skip' }))
    await user.click(screen.getByRole('link', { name: 'Session log' }))

    expect(screen.getByRole('heading', { name: 'SESSION' })).toBeInTheDocument()
  })
})
