import { act, fireEvent, screen, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { axe } from 'vitest-axe'

import { LONG_PRESS_MS } from '@/hooks/useLongPress'
import {
  advance,
  received,
  selectAvoidUndo,
  selectHold,
  selectIsAvoided,
  selectLiveCall,
  turnFeedOff,
} from '@/store/live'
import { makeStore } from '@/store/store'
import { renderApp } from '@/test/utils'
import type { Call } from '@/types'

function call(id: number, talkgroupRef = 1, over: Partial<Call> = {}): Call {
  return {
    id,
    systemRef: 100,
    systemLabel: 'Alpha',
    talkgroupRef,
    talkgroupLabel: `TG ${talkgroupRef}`,
    timestamp: Date.parse('2026-07-25T14:32:05'),
    audioUrl: `/api/call/${id}/audio`,
    ...over,
  }
}

/** Hear `calls` on the Live screen, then walk to the session log the way a
 *  Listener does — from the RECENT heading. */
async function heard(...calls: Call[]) {
  const store = makeStore()
  const view = renderApp('/', store)
  act(() => {
    for (const one of calls) store.dispatch(received(one, one.id))
    // Each Call finishing is what puts the next on the air, and what makes the
    // whole set something that was *heard* rather than something waiting.
    for (const _one of calls) store.dispatch(advance())
  })
  await userEvent.click(screen.getByRole('link', { name: 'Session log' }))
  return { ...view, store }
}

const rows = () =>
  within(screen.getByRole('list', { name: 'Session log' })).getAllByRole('listitem')

/** The tap/hold control of the row showing `label` — found by what a Listener
 *  reads rather than by position, since the list re-orders as Calls arrive. */
const rowFor = (label: string) =>
  within(rows().find((row) => within(row).queryByText(label)) as HTMLElement).getByRole(
    'button',
  )

/** Hold a row down long enough to open its actions. */
function hold(label: string) {
  const row = rowFor(label)
  fireEvent.pointerDown(row)
  act(() => void vi.advanceTimersByTime(LONG_PRESS_MS))
  fireEvent.pointerUp(row)
}

afterEach(() => vi.useRealTimers())

describe('the session log (#58, spec US 28)', () => {
  it('is reached from the Live screen and lists what was heard, newest first', async () => {
    await heard(call(1), call(2, 2), call(3, 3))

    expect(screen.getByRole('heading', { name: 'SESSION' })).toBeInTheDocument()
    expect(rows()).toHaveLength(3)
    expect(within(rows()[0]).getByText('TG 3')).toBeInTheDocument()
    expect(within(rows()[2]).getByText('TG 1')).toBeInTheDocument()
  })

  it('counts what it holds', async () => {
    await heard(call(1), call(2, 2))

    expect(screen.getByText('2 heard')).toBeInTheDocument()
  })

  /** rdio-scanner has nothing like this at all, so an empty one has to explain
   *  itself — and point at the archive, which holds everything from before. */
  it('says so, and where else to look, when nothing has been heard', async () => {
    const store = makeStore()
    renderApp('/session', store)

    expect(screen.getByText('Nothing heard yet')).toBeInTheDocument()
    expect(screen.getByRole('link', { name: 'search it' })).toHaveAttribute(
      'href',
      '/search',
    )
  })

  /**
   * The point of having it: RECENT reaches back five (spec US 13), and "what
   * was that ten minutes ago" is further back than that.
   */
  it('replays a Call RECENT could no longer reach', async () => {
    const many = Array.from({ length: 9 }, (_, at) => call(at + 1, at + 1))
    const { store } = await heard(...many)

    await userEvent.click(rowFor('TG 1'))

    expect(selectLiveCall(store.getState())?.id).toBe(1)
  })

  describe('with the feed off', () => {
    /** Replaying is audio, and a Listener who switched the feed off asked for
     *  none — the row says so rather than looking tappable and doing nothing
     *  (#88). */
    it('says its rows cannot replay, and does not', async () => {
      const { store } = await heard(call(1))
      act(() => void store.dispatch(turnFeedOff()))

      const row = rowFor('TG 1')
      expect(row).toHaveAttribute('aria-disabled', 'true')

      fireEvent.click(row)
      expect(selectLiveCall(store.getState())).toBeNull()
    })

    /**
     * ...but the row stays *pressable*, which is the whole reason it is
     * `aria-disabled` and not `disabled`: a disabled button fires no pointer
     * events, so gating it that way would take **Hold**, **Avoid** and
     * **Download** with it — none of them audio, and all of them what a
     * Listener browsing a silent feed came here for.
     */
    it('still opens the quick actions, which are not audio', async () => {
      vi.useFakeTimers({ shouldAdvanceTime: true })
      const { store } = await heard(call(1))
      act(() => void store.dispatch(turnFeedOff()))

      hold('TG 1')
      fireEvent.click(screen.getByRole('button', { name: 'Avoid this talkgroup' }))

      expect(selectIsAvoided(store.getState(), 100, 1)).toBe(true)
    })

    it('keeps the download reachable', async () => {
      vi.useFakeTimers({ shouldAdvanceTime: true })
      const { store } = await heard(call(1))
      act(() => void store.dispatch(turnFeedOff()))

      hold('TG 1')

      expect(screen.getByRole('link', { name: 'Download' })).toBeInTheDocument()
      // Replay is the one that goes, because it is the one that is audio.
      expect(screen.getByRole('button', { name: 'Replay' })).toBeDisabled()
    })
  })

  describe('the actions behind a long press', () => {
    it('opens a sheet naming the Call that was held', async () => {
      vi.useFakeTimers({ shouldAdvanceTime: true })
      await heard(call(1), call(2, 2))

      hold('TG 1')

      expect(screen.getByRole('dialog')).toHaveAccessibleName('TG 1')
    })

    it('holds that Call’s Talkgroup', async () => {
      vi.useFakeTimers({ shouldAdvanceTime: true })
      const { store } = await heard(call(1), call(2, 2))
      hold('TG 1')

      fireEvent.click(screen.getByRole('button', { name: 'Hold this talkgroup' }))

      expect(selectHold(store.getState())).toEqual({ systemRef: 100, talkgroupRef: 1 })
      expect(screen.queryByRole('dialog')).not.toBeInTheDocument()
    })

    /** One control for both directions, so a Listener who holds from here has
     *  a way to release from here. */
    it('offers to release a hold it already names', async () => {
      vi.useFakeTimers({ shouldAdvanceTime: true })
      const { store } = await heard(call(1))
      hold('TG 1')
      fireEvent.click(screen.getByRole('button', { name: 'Hold this talkgroup' }))

      hold('TG 1')
      fireEvent.click(screen.getByRole('button', { name: 'Release hold' }))

      expect(selectHold(store.getState())).toBeNull()
    })

    it('avoids that Call’s Talkgroup, undoably', async () => {
      vi.useFakeTimers({ shouldAdvanceTime: true })
      const { store } = await heard(call(1), call(2, 2))
      hold('TG 1')

      fireEvent.click(screen.getByRole('button', { name: 'Avoid this talkgroup' }))

      expect(selectIsAvoided(store.getState(), 100, 1)).toBe(true)
      expect(selectAvoidUndo(store.getState())?.key).toBe('100:1')
    })

    it('replays from the sheet as well as from the row', async () => {
      vi.useFakeTimers({ shouldAdvanceTime: true })
      const { store } = await heard(call(1), call(2, 2))
      hold('TG 1')

      fireEvent.click(screen.getByRole('button', { name: 'Replay' }))

      expect(selectLiveCall(store.getState())?.id).toBe(1)
    })

    it('downloads the audio, named by the server', async () => {
      vi.useFakeTimers({ shouldAdvanceTime: true })
      await heard(call(1))
      hold('TG 1')

      expect(screen.getByRole('link', { name: 'Download' })).toHaveAttribute(
        'href',
        '/api/call/1/download',
      )
    })

    /** The browser takes over from here, so the sheet has done its job — and a
     *  panel left standing over a page the Listener has finished with is one
     *  more tap they did not ask for. */
    it('gets out of the way once the download is taken', async () => {
      vi.useFakeTimers({ shouldAdvanceTime: true })
      await heard(call(1))
      hold('TG 1')

      fireEvent.click(screen.getByRole('link', { name: 'Download' }))

      expect(screen.queryByRole('dialog')).not.toBeInTheDocument()
    })

    it('closes without acting when it is dismissed', async () => {
      vi.useFakeTimers({ shouldAdvanceTime: true })
      const { store } = await heard(call(1))
      hold('TG 1')

      fireEvent.click(screen.getByRole('button', { name: 'Close' }))

      expect(screen.queryByRole('dialog')).not.toBeInTheDocument()
      expect(selectHold(store.getState())).toBeNull()
      expect(selectLiveCall(store.getState())).toBeNull()
    })

    /** An encrypted Call has no audio to offer (#42), so the row is there and
     *  the download is not — rather than a link to a 404. */
    it('offers no download for an encrypted Call', async () => {
      vi.useFakeTimers({ shouldAdvanceTime: true })
      const { audioUrl: _none, ...encrypted } = call(1)
      await heard({ ...encrypted, encrypted: true })

      hold('TG 1')

      expect(screen.queryByRole('link', { name: 'Download' })).not.toBeInTheDocument()
    })

    /** A press that opened the sheet must not also have replayed the Call on
     *  the way there. */
    it('does not replay the Call it was opened on', async () => {
      vi.useFakeTimers({ shouldAdvanceTime: true })
      const { store } = await heard(call(1), call(2, 2))
      expect(selectLiveCall(store.getState())).toBeNull()

      hold('TG 1')
      fireEvent.click(rowFor('TG 1'))

      expect(selectLiveCall(store.getState())).toBeNull()
    })

    /**
     * #58's fifth criterion. This list grows at the top as Calls are heard, so
     * a press that resolved "which row is this?" when its timer fired would
     * open an *Avoid* menu over whichever Call had slid into that place.
     */
    it('acts on the Call the finger went down on, however the list moved', async () => {
      vi.useFakeTimers({ shouldAdvanceTime: true })
      const { store } = await heard(call(1))
      const row = rowFor('TG 1')

      fireEvent.pointerDown(row)
      // Two more Calls arrive and are heard: every row moves down the list.
      act(() => {
        store.dispatch(received(call(2, 2), 2))
        store.dispatch(received(call(3, 3), 3))
      })
      act(() => void vi.advanceTimersByTime(LONG_PRESS_MS))

      expect(screen.getByRole('dialog')).toHaveAccessibleName('TG 1')
    })

    it('has no accessibility violations', async () => {
      vi.useFakeTimers({ shouldAdvanceTime: true })
      const { container } = await heard(call(1), call(2, 2))

      hold('TG 1')

      expect(await axe(container)).toHaveNoViolations()
    })
  })
})
