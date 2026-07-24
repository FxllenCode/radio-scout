import { configureStore } from '@reduxjs/toolkit'
import { render, screen } from '@testing-library/react'
import { http, HttpResponse } from 'msw'
import { Provider } from 'react-redux'
import { describe, expect, it } from 'vitest'

import { api } from '@/store/api'
import { ORIGIN } from '@/test/handlers'
import { server } from '@/test/setup'

import { SettingsScreen } from './SettingsScreen'

function renderWithStore() {
  const store = configureStore({
    reducer: { [api.reducerPath]: api.reducer },
    middleware: (getDefaultMiddleware) =>
      getDefaultMiddleware().concat(api.middleware),
  })
  return render(
    <Provider store={store}>
      <SettingsScreen />
    </Provider>,
  )
}

describe('SettingsScreen', () => {
  it('reports the server online when /healthz returns ok', async () => {
    renderWithStore() // default handler answers "ok"
    expect(await screen.findByText('online')).toBeInTheDocument()
  })

  it('reports the server unreachable when /healthz fails', async () => {
    server.use(http.get(`${ORIGIN}/healthz`, () => HttpResponse.error()))
    renderWithStore()
    expect(await screen.findByText('unreachable')).toBeInTheDocument()
  })

  it('reports unknown before the health check resolves', () => {
    renderWithStore()
    // Synchronous first paint, before the query settles.
    expect(screen.getByText('checking…')).toBeInTheDocument()
  })
})
