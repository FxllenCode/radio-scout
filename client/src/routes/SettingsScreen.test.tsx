import { screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { http, HttpResponse } from 'msw'
import { describe, expect, it } from 'vitest'

import { fakePush } from '@/test/push'
import { ORIGIN } from '@/test/handlers'
import { server } from '@/test/setup'
import { renderWithProviders } from '@/test/utils'

import { createPush, type Push } from '@/lib/push'

import { SettingsScreen } from './SettingsScreen'

const renderScreen = (push?: Push) =>
  renderWithProviders(<SettingsScreen />, { push })

/** The notifications switch, once the handle has settled. */
const toggle = () => screen.findByRole('switch', { name: /notifications/i })

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

describe('the notifications switch (#16)', () => {
  it('is offered once the server and the browser can both do it', async () => {
    renderScreen(createPush({ environment: fakePush() }))

    expect(await toggle()).toHaveAttribute('aria-checked', 'false')
  })

  // The permission prompt is spent once per origin, so it is spent on a tap —
  // never on a screen simply being opened (ADR-0005).
  it('asks the browser only when the listener taps it', async () => {
    const environment = fakePush()
    renderScreen(createPush({ environment }))

    const control = await toggle()
    expect(environment.asked).toBe(0)

    await userEvent.click(control)

    await waitFor(() => expect(control).toHaveAttribute('aria-checked', 'true'))
    expect(environment.asked).toBe(1)
  })

  it('turns them off again', async () => {
    const environment = fakePush({ permission: 'granted', subscribed: true })
    renderScreen(createPush({ environment }))

    const control = await toggle()
    await waitFor(() => expect(control).toHaveAttribute('aria-checked', 'true'))
    await userEvent.click(control)

    await waitFor(() => expect(control).toHaveAttribute('aria-checked', 'false'))
    expect(environment.unsubscribed).toBe(true)
  })

  it('says a refused permission can only be undone in the browser', async () => {
    renderScreen(createPush({ environment: fakePush({ permission: 'denied' }) }))

    expect(await screen.findByText('blocked')).toBeInTheDocument()
    expect(screen.queryByRole('switch')).not.toBeInTheDocument()
  })

  // The iOS gate, and the one message that is actually actionable there.
  it('sends an uninstalled iPhone to the Home Screen first', async () => {
    Object.defineProperty(navigator, 'standalone', {
      value: false,
      configurable: true,
    })
    renderScreen(createPush({ environment: fakePush({ unsupported: true }) }))

    expect(
      await screen.findByText(/add to home screen/i),
    ).toBeInTheDocument()
  })

  it('says so when the server has no identity to sign with', async () => {
    server.use(
      http.get(
        `${ORIGIN}/api/push/key`,
        () => new HttpResponse(null, { status: 404 }),
      ),
    )
    renderScreen(createPush({ environment: fakePush() }))

    expect(await screen.findByText('unavailable')).toBeInTheDocument()
  })
})
