import { act, screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { describe, expect, it } from 'vitest'

import { offerInstall } from '@/test/install'
import { deploy, installServiceWorker } from '@/test/serviceWorker'
import { renderApp, routerProbe } from '@/test/utils'

describe('App', () => {
  it('renders the Live screen and primary nav at the root route', async () => {
    renderApp('/')

    expect(
      screen.getByRole('navigation', { name: 'Primary' }),
    ).toBeInTheDocument()
    expect(screen.getByRole('heading', { name: 'LIVE' })).toBeInTheDocument()
    expect(
      await screen.findByText(/waiting for the first call/i),
    ).toBeInTheDocument()
  })

  it('offers to install from the shell, so every screen can be asked from', () => {
    renderApp('/search')

    act(() => void offerInstall())

    expect(screen.getByText(/install radio-scout/i)).toBeInTheDocument()
  })

  // One docked slot, and a waiting version is the rarer, more actionable of the
  // two — a listener who reloads gets asked to install again anyway.
  it('lets a waiting version take the banner slot from the install offer', async () => {
    const container = installServiceWorker()
    renderApp('/')
    act(() => void offerInstall())
    expect(screen.getByText(/install radio-scout/i)).toBeInTheDocument()

    await deploy(container)

    expect(screen.getByText(/new version is ready/i)).toBeInTheDocument()
    expect(screen.queryByText(/install radio-scout/i)).not.toBeInTheDocument()
  })
})

/**
 * A tab switch is not a search being abandoned (#61, spec US 30).
 *
 * The URL is the search state, so leaving `/search` and coming back through a
 * plain `/search` link would silently discard filters the Listener spent eight
 * taps setting. The tab remembers where the Listener was.
 */
describe('the Search tab (#61)', () => {
  it('comes back to the search that was left', async () => {
    const user = userEvent.setup()
    renderApp('/search?tag=Fire&sort=oldest')
    await screen.findByRole('search', { name: 'Archive filters' })

    await user.click(screen.getByRole('link', { name: 'Live' }))
    expect(await screen.findByRole('heading', { name: 'LIVE' })).toBeInTheDocument()
    await user.click(screen.getByRole('link', { name: 'Search' }))

    // Exactly where the Listener was, character for character — not a
    // canonicalised version of it. Coming back somewhere subtly different from
    // where you left is worse than not coming back at all.
    await waitFor(() =>
      expect(routerProbe.location).toBe('/search?tag=Fire&sort=oldest'),
    )
    expect(screen.getByLabelText('Tag')).toHaveValue('Fire')
  })

  it('is the bare search until one has been made', async () => {
    const user = userEvent.setup()
    renderApp('/')

    await user.click(screen.getByRole('link', { name: 'Search' }))

    await waitFor(() => expect(routerProbe.location).toBe('/search'))
  })
})
