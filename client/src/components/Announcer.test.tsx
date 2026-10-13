import { act, screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { axe } from 'vitest-axe'
import { describe, expect, it, vi } from 'vitest'

import { advance, received, replay } from '@/store/live'
import { makeStore } from '@/store/store'
import { renderApp } from '@/test/utils'
import type { Call } from '@/types'

function call(id: number, overrides: Partial<Call> = {}): Call {
  return {
    id,
    systemRef: 11,
    systemLabel: 'Fulton County',
    talkgroupRef: 54241,
    talkgroupLabel: 'FD Dispatch',
    audioUrl: `/api/call/${id}/audio`,
    ...overrides,
  }
}

const announcer = () => screen.getByTestId('announcer')

/**
 * Each Call, said out loud (#78) — the whole of the Live display, for a
 * Listener who cannot see it. In the shell, because the audio follows the
 * Listener between tabs (#56) and so must the readout.
 */
describe('the announcer (#78)', () => {
  it('is a polite live region that interrupts nothing', () => {
    renderApp('/')

    expect(announcer()).toHaveAttribute('aria-live', 'polite')
    expect(announcer()).toHaveAttribute('aria-atomic', 'true')
  })

  it('announces each Call as it starts, on whatever tab the Listener is on', async () => {
    const store = makeStore()
    renderApp('/talkgroups', store)

    act(() => void store.dispatch(received(call(1, { emergency: true }), 1)))
    await waitFor(() =>
      expect(announcer()).toHaveTextContent('Emergency. FD Dispatch, Fulton County'),
    )

    act(() => {
      store.dispatch(received(call(2, { talkgroupLabel: 'PD Dispatch' }), 2))
      store.dispatch(advance())
    })
    await waitFor(() => expect(announcer()).toHaveTextContent('PD Dispatch, Fulton County'))
  })

  /** The same words twice are still two Calls — a region whose text did not
   *  change is a region a screen reader does not read again. */
  it('announces a replay, though the words are the same', async () => {
    const store = makeStore()
    renderApp('/', store)
    act(() => void store.dispatch(received(call(1), 1)))
    await waitFor(() => expect(announcer()).toHaveTextContent('FD Dispatch'))
    const first = announcer().firstElementChild

    act(() => void store.dispatch(replay(1)))

    await waitFor(() => expect(announcer().firstElementChild).not.toBe(first))
    expect(announcer()).toHaveTextContent('FD Dispatch, Fulton County')
  })

  it('says when the feed is switched off and back on, but not where it started', async () => {
    const user = userEvent.setup()
    renderApp('/')
    await screen.findByText('connected')
    // The first connection is news: it is what tells a Listener who cannot
    // see the green lamp that the scanner is working.
    expect(announcer()).toHaveTextContent('Live feed connected.')

    await user.click(screen.getByRole('button', { name: 'Live feed' }))
    expect(announcer()).toHaveTextContent('Live feed switched off.')

    await user.click(screen.getByRole('button', { name: 'Live feed' }))
    await waitFor(() => expect(announcer()).toHaveTextContent('Live feed connected.'))
  })

  it('says nothing about the state the page opened in', () => {
    renderApp('/')

    expect(announcer()).toBeEmptyDOMElement()
  })

  it('passes axe on the shell that carries it', async () => {
    const { container } = renderApp('/')
    await screen.findByText('connected')

    expect(await axe(container)).toHaveNoViolations()
  })

  /** The page cannot make a sound until somebody taps it, and a Listener who
   *  cannot see the button has to be told — and not read the names of Calls
   *  that are not playing. */
  it('says a tap is needed, and names no Call the browser will not play', async () => {
    vi.spyOn(HTMLMediaElement.prototype, 'play').mockRejectedValue(
      new DOMException('needs a gesture', 'NotAllowedError'),
    )
    const store = makeStore()
    renderApp('/', store)
    act(() => void store.dispatch(received(call(1), 1)))

    await waitFor(() =>
      expect(announcer()).toHaveTextContent(
        'Tap to listen — your browser needs a tap before it will play sound.',
      ),
    )
    act(() => {
      store.dispatch(received(call(2, { talkgroupLabel: 'PD Dispatch' }), 2))
      store.dispatch(advance())
    })
    expect(announcer()).not.toHaveTextContent('PD Dispatch')
    vi.restoreAllMocks()
  })
})
