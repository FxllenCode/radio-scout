import { act, screen, waitFor, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { axe } from 'vitest-axe'
import { describe, expect, it } from 'vitest'

import { received, selectLiveCall, turnFeedOff } from '@/store/live'
import { enterPlaybackMode, selectPlaybackMode, startRun } from '@/store/playback'
import { selectIsPaused } from '@/store/transport'
import { makeStore, type AppStore } from '@/store/store'
import { ARCHIVE } from '@/test/handlers'
import { renderApp } from '@/test/utils'
import type { Call } from '@/types'

function call(overrides: Partial<Call> = {}): Call {
  return {
    id: 1,
    systemRef: 11,
    systemLabel: 'Fulton County',
    talkgroupRef: 54241,
    talkgroupLabel: 'FD Dispatch',
    audioUrl: '/api/call/1/audio',
    ...overrides,
  }
}

/** The app on a tab that is not Live, with `calls` already over the feed. */
function away(route = '/talkgroups', ...calls: Call[]): AppStore {
  const store = makeStore()
  renderApp(route, store)
  act(() => {
    for (const one of calls) store.dispatch(received(one, one.id))
  })
  return store
}

/** Named for the surface rather than for its content, because the Search
 *  screen's own bar is already the region called "Now playing" — and this one
 *  is beside it there. */
const strip = () => screen.getByRole('region', { name: 'Mini player' })
const noStrip = () => screen.queryByRole('region', { name: 'Mini player' })

/**
 * The mini-player strip (#56, spec US 54).
 *
 * The Live screen is a whole player; every other tab used to be silent about
 * what the app was doing, so a Listener on Search could not tell audio playing
 * from a feed switched off. This is that truth, docked above the tab bar.
 */
describe('the now-playing strip', () => {
  it('carries the live Call onto every other tab', () => {
    away('/talkgroups', call())

    expect(within(strip()).getByText('FD Dispatch')).toBeInTheDocument()
    expect(within(strip()).getByText('Live')).toBeInTheDocument()
  })

  /** The Live screen shows the same Call, larger, with the same controls —
   *  a strip there would spend the scarcest space on a phone repeating it. */
  it('stays off the Live screen, which is the full player', () => {
    away('/', call())

    expect(noStrip()).toBeNull()
  })

  it('is absent while the feed is simply quiet', () => {
    away('/search')

    expect(noStrip()).toBeNull()
  })

  describe('its controls', () => {
    it('pauses and resumes what is playing, wherever the Listener is', async () => {
      const user = userEvent.setup()
      const store = away('/settings', call())

      await user.click(within(strip()).getByRole('button', { name: 'Pause' }))
      expect(selectIsPaused(store.getState())).toBe(true)

      await user.click(within(strip()).getByRole('button', { name: 'Resume' }))
      expect(selectIsPaused(store.getState())).toBe(false)
    })

    it('skips to the next Call in the listening queue', async () => {
      const user = userEvent.setup()
      const store = away('/talkgroups', call({ id: 1 }), call({ id: 2 }))

      await user.click(within(strip()).getByRole('button', { name: 'Skip' }))

      expect(selectLiveCall(store.getState())?.id).toBe(2)
    })
  })

  /**
   * The half a player cannot show: **why it is quiet**. Both are silences the
   * Listener chose, and both are one tap from being undone — which is the whole
   * of what was missing from every tab but Live.
   */
  describe('when the feed is quiet by choice', () => {
    it('says the feed is off, and turns it back on', async () => {
      const user = userEvent.setup()
      const store = away('/search', call())
      act(() => void store.dispatch(turnFeedOff()))
      expect(within(strip()).getByText('FEED OFF')).toBeInTheDocument()

      await user.click(within(strip()).getByRole('button', { name: 'Turn on' }))

      expect(store.getState().live.feedOff).toBe(false)
      expect(noStrip()).toBeNull()
    })

    it('says the archive has the audio, and hands the feed back', async () => {
      const user = userEvent.setup()
      const store = away('/talkgroups')
      act(() => void store.dispatch(enterPlaybackMode()))
      expect(within(strip()).getByText('PLAYBACK')).toBeInTheDocument()

      await user.click(
        within(strip()).getByRole('button', { name: 'Back to live' }),
      )

      expect(selectPlaybackMode(store.getState())).toBe('live')
    })

    /** A Call the Listener can hear outranks any explanation of silence: in
     *  playback mode the source line *is* the explanation (spec US 25). */
    it('shows the archived Call rather than the reason, once one is playing', () => {
      const store = away('/talkgroups')
      act(() => {
        store.dispatch(enterPlaybackMode())
        store.dispatch(
          startRun({
            search: { sort: 'oldest' },
            page: {
              results: ARCHIVE,
              count: 421,
              limit: 50,
              offset: 0,
              hasMore: true,
            },
            index: 1,
          }),
        )
      })

      expect(within(strip()).getByText('Alpha Law')).toBeInTheDocument()
      expect(within(strip()).getByText('2 of 421')).toBeInTheDocument()
      expect(within(strip()).queryByText('PLAYBACK')).toBeNull()
    })
  })

  /**
   * The third source, driven for real (spec US 26).
   *
   * With the live feed **on**, tapping a search result *interrupts* it: the
   * listening queue is untouched and the feed resumes when the Call finishes.
   * The strip has to say so — "3 of 421" would claim a walk through results
   * that is not happening, and "Live" would claim the feed's own Call.
   *
   * Driven through the Search screen rather than by dispatching `startRun`,
   * because `interrupting` is decided by the *mode the Listener is in* when
   * they tap. A store-level test would set the flag it is meant to be checking.
   */
  it('says an archived Call is interrupting the live feed', async () => {
    const user = userEvent.setup()
    renderApp('/search')
    const list = await screen.findByRole('list', { name: 'Search results' })
    const rows = within(list).getAllByRole('listitem')

    await user.click(within(rows[0]).getByRole('button', { name: /^Play / }))

    await waitFor(() =>
      expect(
        within(strip()).getByText('Interrupting live feed'),
      ).toBeInTheDocument(),
    )
  })

  /**
   * …and the strip stays up on Search even though that screen draws its own bar
   * for the same Run. That bar is inline in a long result list and scrolls out
   * of view, which is the problem this ticket exists to fix; the Live screen is
   * exempt because its display *is* the screen and never scrolls away from what
   * it is about.
   */
  it('keeps saying what is playing on Search, whose own bar scrolls away', async () => {
    const user = userEvent.setup()
    renderApp('/search')
    const list = await screen.findByRole('list', { name: 'Search results' })
    const rows = within(list).getAllByRole('listitem')

    await user.click(within(rows[0]).getByRole('button', { name: /^Play / }))

    await waitFor(() => expect(strip()).toBeInTheDocument())
    expect(
      screen.getByRole('region', { name: 'Now playing' }),
    ).toBeInTheDocument()
  })

  it('has no accessibility violations', async () => {
    const store = makeStore()
    const { container } = renderApp('/talkgroups', store)
    act(() => void store.dispatch(received(call(), 1)))

    expect(await axe(container)).toHaveNoViolations()
  })
})
