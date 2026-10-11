import { act, screen, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { http, HttpResponse } from 'msw'
import { axe } from 'vitest-axe'
import { afterEach, beforeEach, describe, expect, it } from 'vitest'

import { SKIP_WAITING } from '@/lib/serviceWorker'
import { instanceIs, ORIGIN } from '@/test/handlers'
import { beIosSafari, offerInstall, type FakeInstallPromptEvent } from '@/test/install'
import { deploy, installServiceWorker } from '@/test/serviceWorker'
import { server } from '@/test/setup'
import { renderApp, renderWithProviders } from '@/test/utils'

import { SettingsScreen } from './SettingsScreen'

const renderScreen = () => renderWithProviders(<SettingsScreen />)

describe('SettingsScreen', () => {
  it('reports the server online when /healthz returns ok', async () => {
    renderScreen() // default handler answers "ok"
    expect(await screen.findByText('online')).toBeInTheDocument()
  })

  it('reports the server unreachable when /healthz fails', async () => {
    server.use(http.get(`${ORIGIN}/healthz`, () => HttpResponse.error()))
    renderScreen()
    expect(await screen.findByText('unreachable')).toBeInTheDocument()
  })

  it('reports unknown before the health check resolves', () => {
    renderScreen()
    // Synchronous first paint, before the query settles.
    expect(screen.getByText('checking…')).toBeInTheDocument()
  })

  // The way in to the operator log (#30). It is a link rather than a section
  // here because the log is admin-only, and asking for a password is that
  // screen's job, not this one's.
  it('offers the operator log', () => {
    renderScreen()

    expect(screen.getByRole('link', { name: /logs/i })).toHaveAttribute(
      'href',
      '/settings/logs',
    )
  })
})

/** What the Operator offers their Listeners (#77, spec US 57–58). */
describe('SettingsScreen — what the Operator offers', () => {
  it('offers the Reference page and the help link when there are some', async () => {
    server.use(instanceIs({ reference: true, helpUrl: 'mailto:ops@example.com' }))
    renderScreen()

    expect(await screen.findByRole('link', { name: /Reference/ })).toHaveAttribute(
      'href',
      '/reference',
    )
    const help = screen.getByRole('link', { name: /Help/ })
    expect(help).toHaveAttribute('href', 'mailto:ops@example.com')
    expect(help).toHaveTextContent('write to the operator')
  })

  it('names a help page as one', async () => {
    server.use(instanceIs({ helpUrl: 'https://example.com/help' }))
    renderScreen()

    expect(await screen.findByRole('link', { name: /Help/ })).toHaveTextContent(
      'from the operator',
    )
  })

  /** An entry that leads nowhere is a control that lies. */
  it('offers neither when the Operator has given neither', async () => {
    renderScreen()
    await screen.findByText('online')

    expect(screen.queryByRole('link', { name: /Reference/ })).not.toBeInTheDocument()
    expect(screen.queryByRole('link', { name: /Help/ })).not.toBeInTheDocument()
  })
})

/**
 * The install funnel, reopened (#78, from #60). The banner asks once and takes
 * "not now" for an answer; Settings is where a Listener who changes their mind
 * goes looking.
 */
describe('Settings → install', () => {
  beforeEach(() => globalThis.localStorage?.clear())

  it('offers the install a dismissed banner gave up', async () => {
    const user = userEvent.setup()
    renderApp('/settings')
    let event!: FakeInstallPromptEvent
    act(() => void (event = offerInstall()))
    await user.click(screen.getByRole('button', { name: 'Dismiss install prompt' }))
    expect(screen.queryByText(/install radio-scout/i)).not.toBeInTheDocument()

    await user.click(screen.getByRole('button', { name: 'Install app' }))

    expect(event.prompted).toBe(1)
  })

  it('gives the Share-sheet steps on iOS, where there is no prompt to fire', () => {
    beIosSafari()
    renderApp('/settings')

    const entry = screen.getByRole('listitem', { name: 'Install' })
    expect(entry).toHaveTextContent(/share/i)
    expect(entry).toHaveTextContent(/add to home screen/i)
  })

  /** A control that would be refused is not offered — here, an install this
   *  browser cannot do, or one already done. */
  it('offers nothing where there is nothing to install', () => {
    renderApp('/settings')

    expect(screen.queryByRole('listitem', { name: 'Install' })).not.toBeInTheDocument()
  })
})

/**
 * A waiting version, out of the way (#78). The banner can be waved off — it
 * applies on the next cold start anyway — and the reload stays to hand here.
 */
describe('Settings → update', () => {
  it('keeps the reload a dismissed update banner offered', async () => {
    const user = userEvent.setup()
    const container = installServiceWorker()
    renderApp('/settings')
    const waiting = await deploy(container)

    await user.click(screen.getByRole('button', { name: 'Dismiss update notice' }))
    // The banner is gone; the row below says the same and stays.
    expect(screen.queryByRole('button', { name: 'Dismiss update notice' })).not.toBeInTheDocument()

    const entry = screen.getByRole('listitem', { name: 'Update' })
    await user.click(within(entry).getByRole('button', { name: 'Reload to update' }))

    expect(waiting.posted).toEqual([SKIP_WAITING])
  })

  it('says nothing about updates while there is none', () => {
    renderApp('/settings')

    expect(screen.queryByRole('listitem', { name: 'Update' })).not.toBeInTheDocument()
  })
})

/** Offline is not unreachable (#78): one is this device, the other the server. */
describe('Settings → server', () => {
  afterEach(() => {
    delete (navigator as { onLine?: boolean }).onLine
  })

  it('says this device is offline rather than that the server is unreachable', async () => {
    server.use(http.get(`${ORIGIN}/healthz`, () => HttpResponse.error()))
    Object.defineProperty(navigator, 'onLine', { configurable: true, get: () => false })
    renderApp('/settings')

    expect(await screen.findByText('this device is offline')).toBeInTheDocument()
    expect(screen.queryByText('unreachable')).not.toBeInTheDocument()
  })
})

describe('Settings, with everything it can offer drawn (#78)', () => {
  it('passes axe with the update and install rows up', async () => {
    const container = installServiceWorker()
    beIosSafari()
    const view = renderApp('/settings')
    await deploy(container)
    await screen.findByText('online')

    expect(screen.getByRole('listitem', { name: 'Update' })).toBeInTheDocument()
    expect(screen.getByRole('listitem', { name: 'Install' })).toBeInTheDocument()
    expect(await axe(view.container)).toHaveNoViolations()
  })
})
