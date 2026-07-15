import { configureStore } from '@reduxjs/toolkit'
import { setupListeners } from '@reduxjs/toolkit/query'
import { afterEach, describe, expect, it, vi } from 'vitest'

import { api } from './api'

function makeStore() {
  const store = configureStore({
    reducer: { [api.reducerPath]: api.reducer },
    middleware: (getDefaultMiddleware) =>
      getDefaultMiddleware().concat(api.middleware),
  })
  setupListeners(store.dispatch)
  return store
}

describe('api slice', () => {
  afterEach(() => {
    vi.restoreAllMocks()
  })

  it('getHealth queries /healthz as text and returns the body', async () => {
    const fetchSpy = vi
      .spyOn(globalThis, 'fetch')
      .mockResolvedValue(new Response('ok', { status: 200 }))

    const store = makeStore()
    const result = await store.dispatch(api.endpoints.getHealth.initiate())

    expect(result.data).toBe('ok')
    expect(fetchSpy).toHaveBeenCalledOnce()
    const request = fetchSpy.mock.calls[0][0] as Request
    expect(new URL(request.url).pathname).toBe('/healthz')
  })
})
