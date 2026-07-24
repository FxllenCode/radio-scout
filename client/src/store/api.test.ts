import { configureStore } from '@reduxjs/toolkit'
import { setupListeners } from '@reduxjs/toolkit/query'
import { http, HttpResponse } from 'msw'
import { describe, expect, it } from 'vitest'

import { ORIGIN } from '@/test/handlers'
import { server } from '@/test/setup'

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
  it('getHealth queries /healthz as text and returns the body', async () => {
    const store = makeStore()
    const result = await store.dispatch(api.endpoints.getHealth.initiate())
    // The default MSW handler answers /healthz; onUnhandledRequest:'error'
    // means a wrong URL would have thrown instead.
    expect(result.data).toBe('ok')
  })

  it('surfaces the HTTP status as an error on a non-2xx response', async () => {
    server.use(
      http.get(`${ORIGIN}/healthz`, () => new HttpResponse('boom', { status: 500 })),
    )
    const store = makeStore()
    const result = await store.dispatch(api.endpoints.getHealth.initiate())

    expect(result.data).toBeUndefined()
    expect(result.isError).toBe(true)
    expect((result.error as { status?: number }).status).toBe(500)
  })

  it('surfaces a transport failure as a FETCH_ERROR', async () => {
    server.use(http.get(`${ORIGIN}/healthz`, () => HttpResponse.error()))
    const store = makeStore()
    const result = await store.dispatch(api.endpoints.getHealth.initiate())

    expect(result.isError).toBe(true)
    expect((result.error as { status?: string }).status).toBe('FETCH_ERROR')
  })
})
