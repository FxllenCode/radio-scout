import { screen } from '@testing-library/react'
import { http, HttpResponse } from 'msw'
import { describe, expect, it } from 'vitest'

import { ORIGIN } from '@/test/handlers'
import { server } from '@/test/setup'
import { renderWithProviders } from '@/test/utils'

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
