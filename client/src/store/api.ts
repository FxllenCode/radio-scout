import { createApi, fetchBaseQuery } from '@reduxjs/toolkit/query/react'

/** The single RTK Query API slice. Everything is same-origin: in dev the Vite
 *  proxy forwards to the Rust backend, and in production the SPA is served by
 *  the binary itself, so relative URLs Just Work. */
export const api = createApi({
  reducerPath: 'api',
  // `fetchFn` calls the current global `fetch` at request time rather than
  // capturing it at creation — resilient to polyfills and cleanly mockable.
  baseQuery: fetchBaseQuery({
    baseUrl: '/',
    fetchFn: (...args) => fetch(...args),
  }),
  tagTypes: ['Call'],
  endpoints: (builder) => ({
    /** Server liveness — proves the one-origin wiring end to end. */
    getHealth: builder.query<string, void>({
      query: () => ({ url: 'healthz', responseHandler: 'text' }),
    }),
    // Archive search, live-feed hydration, etc. are added by later tickets.
  }),
})

export const { useGetHealthQuery } = api
