import { act, screen, waitFor, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { axe } from 'vitest-axe'

import {
  avoidTalkgroup,
  received,
  selectAvoidedCount,
  selectIsAvoided,
} from '@/store/live'
import { makeStore } from '@/store/store'
import { renderApp } from '@/test/utils'
import type { Call } from '@/types'

/** A Call on a Talkgroup the catalog fixture *does* name, so the sheet's
 *  ordinary path — a row under its real label — is what is asserted. */
const CALL: Call = {
  id: 1,
  systemRef: 100,
  systemLabel: 'Alpha',
  talkgroupRef: 1,
  talkgroupLabel: 'Alpha Fire',
  timestamp: Date.parse('2026-07-25T14:32:05'),
  audioUrl: '/api/call/1/audio',
}

const NOW = Date.parse('2026-07-25T14:32:05')

function listening() {
  const store = makeStore()
  const view = renderApp('/', store)
  act(() => void store.dispatch(received(CALL, 1)))
  return { ...view, store }
}

const openSheet = async (user: ReturnType<typeof userEvent.setup>) =>
  user.click(await screen.findByRole('button', { name: /Avoiding \d/ }))

const rows = () =>
  within(screen.getByRole('list', { name: 'Avoided talkgroups' })).getAllByRole(
    'listitem',
  )

afterEach(() => vi.useRealTimers())

describe('the Avoid sheet (#58, spec US 25)', () => {
  /**
   * The control used to be "clear every Avoid you have", so a Listener who
   * mis-tapped one had to surrender a deliberate two-hour Avoid to fix it — and
   * had no way at all to see what they were holding.
   */
  it('opens from the Avoiding count and names each silenced Talkgroup', async () => {
    const user = userEvent.setup()
    const { store } = listening()
    act(() => {
      store.dispatch(avoidTalkgroup({ systemRef: 100, talkgroupRef: 1, until: 0 }))
      store.dispatch(avoidTalkgroup({ systemRef: 100, talkgroupRef: 2, until: 0 }))
    })

    await openSheet(user)

    await waitFor(() => expect(rows()).toHaveLength(2))
    expect(within(rows()[0]).getByText('Alpha Fire')).toBeInTheDocument()
    expect(within(rows()[1]).getByText('Alpha Law')).toBeInTheDocument()
  })

  it('lets one back in and leaves the rest silenced', async () => {
    const user = userEvent.setup()
    const { store } = listening()
    act(() => {
      store.dispatch(avoidTalkgroup({ systemRef: 100, talkgroupRef: 1, until: 0 }))
      store.dispatch(avoidTalkgroup({ systemRef: 100, talkgroupRef: 2, until: 0 }))
    })
    await openSheet(user)

    await user.click(
      await screen.findByRole('button', { name: 'Stop avoiding Alpha Fire' }),
    )

    expect(selectIsAvoided(store.getState(), 100, 1)).toBe(false)
    expect(selectIsAvoided(store.getState(), 100, 2)).toBe(true)
  })

  it('still offers to clear the lot, and closes when it does', async () => {
    const user = userEvent.setup()
    const { store } = listening()
    act(() => {
      store.dispatch(avoidTalkgroup({ systemRef: 100, talkgroupRef: 1, until: 0 }))
    })
    await openSheet(user)

    await user.click(screen.getByRole('button', { name: 'Clear all' }))

    expect(selectAvoidedCount(store.getState())).toBe(0)
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument()
  })

  /** A timed Avoid is coming back on its own and an indefinite one is not —
   *  which is the whole difference a Listener is looking at this list to see. */
  it('tells a timed Avoid from one that stands until it is cleared', async () => {
    vi.useFakeTimers({ now: NOW, shouldAdvanceTime: true })
    const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime })
    const { store } = listening()
    act(() => {
      store.dispatch(avoidTalkgroup({ systemRef: 100, talkgroupRef: 1, until: 0 }))
      store.dispatch(
        avoidTalkgroup({
          systemRef: 100,
          talkgroupRef: 2,
          until: NOW + 30 * 60_000,
        }),
      )
    })

    await openSheet(user)

    await waitFor(() => expect(rows()).toHaveLength(2))
    expect(within(rows()[0]).getByText('until cleared')).toBeInTheDocument()
    expect(within(rows()[1]).getByText('30 min left')).toBeInTheDocument()
  })

  /** An Avoid outlives the Talkgroup it was placed on. A row that vanished
   *  would leave a channel silenced with no way to lift it, since the panel
   *  draws the catalog and could not show it either. */
  it('lists an Avoid the catalog cannot name, under its Refs', async () => {
    const user = userEvent.setup()
    const { store } = listening()
    act(() => {
      store.dispatch(avoidTalkgroup({ systemRef: 99, talkgroupRef: 42, until: 0 }))
    })

    await openSheet(user)

    expect(within(rows()[0]).getByText('Talkgroup 42')).toBeInTheDocument()
    expect(within(rows()[0]).getByText('System 99 · 42')).toBeInTheDocument()
    expect(
      screen.getByRole('button', { name: 'Stop avoiding Talkgroup 42' }),
    ).toBeInTheDocument()
  })

  /** Emptying the list from inside it leaves the sheet up and saying so. */
  it('says nothing is avoided once the last one goes', async () => {
    const user = userEvent.setup()
    const { store } = listening()
    act(() => {
      store.dispatch(avoidTalkgroup({ systemRef: 100, talkgroupRef: 1, until: 0 }))
    })
    await openSheet(user)

    await user.click(
      await screen.findByRole('button', { name: 'Stop avoiding Alpha Fire' }),
    )

    expect(screen.getByText('Nothing is avoided.')).toBeInTheDocument()
  })

  it('has no accessibility violations', async () => {
    const user = userEvent.setup()
    const { container, store } = listening()
    act(() => {
      store.dispatch(avoidTalkgroup({ systemRef: 100, talkgroupRef: 1, until: 0 }))
    })

    await openSheet(user)
    await waitFor(() => expect(rows()).toHaveLength(1))

    expect(await axe(container)).toHaveNoViolations()
  })
})
