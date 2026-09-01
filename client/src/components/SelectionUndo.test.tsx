import { act, screen } from '@testing-library/react'
import { afterEach, describe, expect, it, vi } from 'vitest'

import { EVERYTHING } from '@/lib/selection'
import { SELECTION_UNDO_MS, selectSelection } from '@/store/live'
import { makeStore } from '@/store/store'
import { renderApp } from '@/test/utils'

afterEach(() => vi.useRealTimers())

/** The offer's window is the store's, and its deadline is a *moment* — so the
 *  bar is a timer holding no policy, and a remount waits out what is left
 *  rather than starting again (`AvoidUndo`'s reasoning, #58). */
describe('the linked-Selection undo (#61, spec US 30)', () => {
  it('is absent until a link has replaced something', () => {
    renderApp('/talkgroups')

    expect(screen.queryByRole('region', { name: 'Selection undo' })).toBeNull()
  })

  it('stands until its window is up, and leaves the linked Selection when it goes', async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true })
    const store = makeStore()
    renderApp('/talkgroups?sel=0_100.2', store)
    expect(
      await screen.findByRole('region', { name: 'Selection undo' }),
    ).toBeInTheDocument()

    act(() => void vi.advanceTimersByTime(SELECTION_UNDO_MS - 100))
    expect(screen.getByRole('region', { name: 'Selection undo' })).toBeInTheDocument()

    act(() => void vi.advanceTimersByTime(100))

    expect(screen.queryByRole('region', { name: 'Selection undo' })).toBeNull()
    expect(selectSelection(store.getState())).not.toEqual(EVERYTHING)
  })
})
