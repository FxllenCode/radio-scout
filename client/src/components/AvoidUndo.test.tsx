import { act, screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { afterEach, describe, expect, it, vi } from 'vitest'

import {
  AVOID_UNDO_MS,
  received,
  selectAvoidUndo,
  selectHold,
  selectIsAvoided,
  toggleHoldOn,
} from '@/store/live'
import { makeStore } from '@/store/store'
import { renderApp } from '@/test/utils'
import type { Call } from '@/types'

const CALL: Call = {
  id: 1,
  systemRef: 100,
  systemLabel: 'Alpha',
  talkgroupRef: 1,
  talkgroupLabel: 'Alpha Fire',
  timestamp: Date.parse('2026-07-25T14:32:05'),
  audioUrl: '/api/call/1/audio',
}

function listening(route = '/') {
  const store = makeStore()
  const view = renderApp(route, store)
  act(() => void store.dispatch(received(CALL, 1)))
  return { ...view, store }
}

const avoidButton = () => screen.getByRole('button', { name: 'Avoid' })
const undoButton = () => screen.findByRole('button', { name: 'Undo' })

afterEach(() => vi.useRealTimers())

describe('the Avoid undo (#58, spec US 25)', () => {
  it('is absent until something is avoided', () => {
    listening()

    expect(screen.queryByRole('button', { name: 'Undo' })).not.toBeInTheDocument()
  })

  /**
   * Avoid is the one control whose effect is *silence*, so a mis-tap looks
   * exactly like a channel that went quiet. Everything else on the screen
   * announces itself; this is the announcement, and it carries the way back.
   */
  it('names what was silenced, and offers the way back', async () => {
    const user = userEvent.setup()
    listening()

    await user.click(avoidButton())

    const bar = screen.getByRole('region', { name: 'Avoid undo' })
    await waitFor(() => expect(bar).toHaveTextContent('Alpha Fire'))
    expect(await undoButton()).toBeInTheDocument()
  })

  it('lets the Talkgroup back in and takes the offer down', async () => {
    const user = userEvent.setup()
    const { store } = listening()
    await user.click(avoidButton())

    await user.click(await undoButton())

    expect(selectIsAvoided(store.getState(), 100, 1)).toBe(false)
    expect(screen.queryByRole('button', { name: 'Undo' })).not.toBeInTheDocument()
  })

  /** Avoiding a Talkgroup you are holding releases the hold, so an undo that
   *  put back only the silence would be a half-undo that quietly cost the
   *  Listener their hold. */
  it('puts back the Hold the Avoid released', async () => {
    const user = userEvent.setup()
    const { store } = listening()
    act(() => {
      store.dispatch(toggleHoldOn({ systemRef: 100, talkgroupRef: 1 }))
    })
    await user.click(avoidButton())
    expect(selectHold(store.getState())).toBeNull()

    await user.click(await undoButton())

    expect(selectHold(store.getState())).toEqual({ systemRef: 100, talkgroupRef: 1 })
  })

  /** The grace window: the offer goes on its own, and the Avoid stands. */
  it('lapses after the grace window, leaving the Avoid in force', async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true })
    const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime })
    const { store } = listening()
    await user.click(avoidButton())
    expect(await undoButton()).toBeInTheDocument()

    act(() => void vi.advanceTimersByTime(AVOID_UNDO_MS))

    expect(screen.queryByRole('button', { name: 'Undo' })).not.toBeInTheDocument()
    expect(selectIsAvoided(store.getState(), 100, 1)).toBe(true)
  })

  it('is still there a moment before the window is up', async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true })
    const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime })
    listening()
    await user.click(avoidButton())

    act(() => void vi.advanceTimersByTime(AVOID_UNDO_MS - 100))

    expect(await undoButton()).toBeInTheDocument()
  })

  /**
   * The reason it lives in the shell rather than on the Live screen: a Listener
   * who avoids a channel and then goes to look at Talkgroups has spent none of
   * the window, and a bar drawn by the screen they left would be gone.
   */
  it('follows the Listener off the Live screen', async () => {
    const user = userEvent.setup()
    listening()
    await user.click(avoidButton())

    await user.click(screen.getByRole('link', { name: 'Talkgroups' }))

    expect(await undoButton()).toBeInTheDocument()
  })

  /** The deadline is a moment in the store, not a countdown the component
   *  owns — so a remount waits out what is *left* rather than starting again
   *  and giving a Listener an offer that never expires. */
  it('does not restart its window when the Listener comes back', async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true })
    const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime })
    const { store } = listening()
    await user.click(avoidButton())
    const offered = selectAvoidUndo(store.getState())?.expiresAt

    await user.click(screen.getByRole('link', { name: 'Talkgroups' }))
    await user.click(screen.getByRole('link', { name: 'Live' }))

    expect(selectAvoidUndo(store.getState())?.expiresAt).toBe(offered)
  })
})
