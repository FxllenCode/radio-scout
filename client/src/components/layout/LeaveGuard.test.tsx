import { act, screen } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { describe, expect, it } from 'vitest'

import { enterPlaybackMode, startRun } from '@/store/playback'
import { makeStore } from '@/store/store'
import { ARCHIVE, searchPage } from '@/test/handlers'
import { deploy, installServiceWorker } from '@/test/serviceWorker'
import { renderApp } from '@/test/utils'

/** What a browser does as the tab closes: asks every listener whether to
 *  hold it, and holds it if any of them said so. */
function closeTheTab(): boolean {
  const event = new Event('beforeunload', { cancelable: true })
  window.dispatchEvent(event)
  return event.defaultPrevented
}

/**
 * The leave-guard (#78) — rdio's "Live Feed is ON, do you really want to
 * leave?", and the lost affordance it names. A scanner left running is a
 * scanner somebody means to keep listening to, and an accidental Cmd-W on a
 * quiet channel would otherwise end the night's listening without a word.
 */
describe('the leave-guard (#78)', () => {
  it('holds the tab while the live feed is on', async () => {
    renderApp('/')
    await screen.findByText('connected')

    expect(closeTheTab()).toBe(true)
  })

  it('lets the tab go once the Listener has switched the feed off', async () => {
    const user = userEvent.setup()
    renderApp('/')
    await user.click(screen.getByRole('button', { name: 'Live feed' }))

    expect(closeTheTab()).toBe(false)
  })

  it('lets it go while browsing the archive with nothing playing', () => {
    const store = makeStore()
    renderApp('/search', store)
    act(() => void store.dispatch(enterPlaybackMode()))

    expect(closeTheTab()).toBe(false)
  })

  /** The archive is listening too — rdio guards only its feed. */
  it('holds the tab while the archive is playing', () => {
    const store = makeStore()
    renderApp('/search', store)
    act(() => {
      store.dispatch(enterPlaybackMode())
      store.dispatch(
        startRun({ search: {}, page: searchPage({ results: ARCHIVE }), index: 0 }),
      )
    })

    expect(closeTheTab()).toBe(true)
  })

  /** The app's own reload to take an update is not the Listener leaving —
   *  asking "do you really want to leave?" there would be the app arguing
   *  with the button that was just pressed. */
  it('stands aside for the reload that takes an update', async () => {
    const user = userEvent.setup()
    const container = installServiceWorker()
    renderApp('/')
    await screen.findByText('connected')
    await deploy(container)

    await user.click(screen.getByRole('button', { name: 'Reload' }))

    expect(closeTheTab()).toBe(false)
  })
})
