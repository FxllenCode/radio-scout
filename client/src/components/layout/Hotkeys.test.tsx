import { act, screen } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { describe, expect, it } from 'vitest'

import {
  received,
  selectHold,
  selectIsAvoided,
  selectLiveCall,
  selectPlayId,
} from '@/store/live'
import { enterPlaybackMode, startRun } from '@/store/playback'
import { selectIsPaused } from '@/store/transport'
import { makeStore, type AppStore } from '@/store/store'
import { searchPage } from '@/test/handlers'
import { renderApp } from '@/test/utils'
import type { Call } from '@/types'

function call(id: number, talkgroupRef = 54241): Call {
  return {
    id,
    systemRef: 11,
    systemLabel: 'Fulton County',
    talkgroupRef,
    talkgroupLabel: `TG ${talkgroupRef}`,
    audioUrl: `/api/call/${id}/audio`,
  }
}

/** The app on `route`, with `calls` already over the feed. */
function listeningOn(route: string, ...calls: Call[]): AppStore {
  const store = makeStore()
  renderApp(route, store)
  act(() => {
    for (const one of calls) store.dispatch(received(one, one.id))
  })
  return store
}

/**
 * Hotkeys (#78, spec US 55) — on every tab, because the audio follows the
 * Listener between tabs and a key that only worked on Live would do nothing
 * half the time.
 */
describe('hotkeys (#78)', () => {
  it('pauses and resumes from another tab', async () => {
    const user = userEvent.setup()
    const store = listeningOn('/talkgroups', call(1))

    await user.keyboard(' ')
    expect(selectIsPaused(store.getState())).toBe(true)
    await user.keyboard('k')
    expect(selectIsPaused(store.getState())).toBe(false)
  })

  it('skips to the next Call', async () => {
    const user = userEvent.setup()
    const store = listeningOn('/search', call(1), call(2, 54242))

    await user.keyboard('n')

    expect(selectLiveCall(store.getState())?.id).toBe(2)
  })

  it('replays the Call on the air', async () => {
    const user = userEvent.setup()
    const store = listeningOn('/', call(1))
    const before = selectPlayId(store.getState())

    await user.keyboard('r')

    expect(selectPlayId(store.getState())).toBe(before + 1)
    expect(selectLiveCall(store.getState())?.id).toBe(1)
  })

  it('avoids the Talkgroup on the display', async () => {
    const user = userEvent.setup()
    const store = listeningOn('/', call(1))

    await user.keyboard('a')

    expect(selectIsAvoided(store.getState(), 11, 54241)).toBe(true)
  })

  it('holds the Talkgroup, and with Shift the System', async () => {
    const user = userEvent.setup()
    const store = listeningOn('/', call(1))

    await user.keyboard('h')
    expect(selectHold(store.getState())).toEqual({ systemRef: 11, talkgroupRef: 54241 })

    await user.keyboard('h')
    await user.keyboard('{Shift>}H{/Shift}')
    expect(selectHold(store.getState())).toEqual({ systemRef: 11, talkgroupRef: null })
  })

  /** A key reaches exactly what a button does: in **Playback mode** the live
   *  controls are dead on screen, and so are their keys. */
  it('does nothing a dead control would not', async () => {
    const user = userEvent.setup()
    const store = listeningOn('/', call(1))
    act(() => void store.dispatch(enterPlaybackMode()))

    await user.keyboard('a')
    await user.keyboard('h')

    expect(selectIsAvoided(store.getState(), 11, 54241)).toBe(false)
    expect(selectHold(store.getState())).toBeNull()
  })

  it('leaves keys alone while the Listener is typing', async () => {
    const user = userEvent.setup()
    const store = listeningOn('/talkgroups', call(1))

    await user.type(await screen.findByRole('searchbox', { name: 'Filter talkgroups' }), 'an')

    expect(selectIsAvoided(store.getState(), 11, 54241)).toBe(false)
    expect(selectLiveCall(store.getState())?.id).toBe(1)
  })

  /** The keys are written on the controls, so a screen reader can say them
   *  and a pointer can hover for them. */
  it('names its keys on the Live controls', () => {
    listeningOn('/', call(1))

    expect(screen.getByRole('button', { name: 'Skip' })).toHaveAttribute(
      'aria-keyshortcuts',
      'N ArrowRight',
    )
    expect(screen.getByRole('button', { name: 'Hold system' })).toHaveAttribute(
      'aria-keyshortcuts',
      'Shift+H',
    )
  })

  /** A key with nothing to act on is not taken from the browser — Space still
   *  scrolls a page with nothing playing, as it always did. */
  it('leaves Space to the page when nothing is playing', () => {
    renderApp('/search')
    const space = new KeyboardEvent('keydown', { key: ' ', cancelable: true, bubbles: true })

    act(() => void window.dispatchEvent(space))

    expect(space.defaultPrevented).toBe(false)
  })

  it('takes Space from the page while something is playing', () => {
    listeningOn('/search', call(1))
    const space = new KeyboardEvent('keydown', { key: ' ', cancelable: true, bubbles: true })

    act(() => void window.dispatchEvent(space))

    expect(space.defaultPrevented).toBe(true)
  })

  /** Replay's own rule (spec US 13): with nothing on the air, the last Call
   *  comes back. */
  it('brings the last Call back when nothing is on the air', async () => {
    const user = userEvent.setup()
    const store = listeningOn('/', call(1))
    await user.keyboard('n')
    expect(selectLiveCall(store.getState())).toBeNull()

    await user.keyboard('r')

    expect(selectLiveCall(store.getState())?.id).toBe(1)
  })

  /** In the archive the live controls are dead, and their keys with them —
   *  but Pause and Skip are the archive's too, as the mini-player's are. */
  it('pauses an archive Run, and replays nothing of the feed', async () => {
    const user = userEvent.setup()
    const store = listeningOn('/search', call(1))
    act(() => {
      store.dispatch(enterPlaybackMode())
      store.dispatch(startRun({ search: {}, page: searchPage(), index: 0 }))
    })
    const before = selectPlayId(store.getState())

    await user.keyboard('r')
    await user.keyboard('{Shift>}H{/Shift}')
    expect(selectPlayId(store.getState())).toBe(before)
    expect(selectHold(store.getState())).toBeNull()

    await user.keyboard(' ')
    expect(selectIsPaused(store.getState())).toBe(true)
  })
})
