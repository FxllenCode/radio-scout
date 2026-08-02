import { act, screen } from '@testing-library/react'
import { describe, expect, it } from 'vitest'

import { offerInstall } from '@/test/install'
import { deploy, installServiceWorker } from '@/test/serviceWorker'
import { renderApp } from '@/test/utils'

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
