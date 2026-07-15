import { configureStore } from '@reduxjs/toolkit'
import { render, screen } from '@testing-library/react'
import { Provider } from 'react-redux'
import { afterEach, describe, expect, it, vi } from 'vitest'

import { api } from '@/store/api'

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
  afterEach(() => {
    vi.restoreAllMocks()
  })

  it('reports the server online when /healthz returns ok', async () => {
    vi.spyOn(globalThis, 'fetch').mockResolvedValue(
      new Response('ok', { status: 200 }),
    )
    renderWithStore()
    expect(await screen.findByText('online')).toBeInTheDocument()
  })

  it('reports the server unreachable when /healthz fails', async () => {
    vi.spyOn(globalThis, 'fetch').mockRejectedValue(new Error('network down'))
    renderWithStore()
    expect(await screen.findByText('unreachable')).toBeInTheDocument()
  })
})
