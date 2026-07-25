import { createApi, fetchBaseQuery } from '@reduxjs/toolkit/query/react'

import { searchParams } from '@/lib/archive'
import type { FilterOptions, SearchPage, SearchQuery } from '@/types'

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
    /** Archive search (#13, spec US 24). One page arrives fully denormalized,
     *  so nothing here needs a follow-up fetch per Call. */
    searchCalls: builder.query<SearchPage, SearchQuery>({
      query: (search) => ({ url: `api/calls?${searchParams(search)}` }),
      providesTags: ['Call'],
    }),

    /** The cascading filter options for the filters already chosen — only
     *  values that have Calls behind them (#13). */
    getFilterOptions: builder.query<FilterOptions, SearchQuery>({
      query: (search) => ({ url: `api/calls/filters?${searchParams(search)}` }),
      providesTags: ['Call'],
    }),
    // Live-feed hydration etc. are added by later tickets.
  }),
})

export const {
  useGetFilterOptionsQuery,
  useGetHealthQuery,
  useSearchCallsQuery,
} = api
