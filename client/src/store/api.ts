import { createApi, fetchBaseQuery } from '@reduxjs/toolkit/query/react'

import { searchParams } from '@/lib/archive'
import type { Catalog, FilterOptions, SearchPage, SearchQuery } from '@/types'

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

    /** Everything a listener can select from (#12, spec US 19). Unlike the
     *  filter options above this is the *configured* world, not the archived
     *  one — a Talkgroup whose Calls have aged out is still selectable. It is
     *  tagged `Call` because ingesting a Call for an unknown Talkgroup is what
     *  auto-populate (#8) grows the catalog by. */
    getCatalog: builder.query<Catalog, void>({
      query: () => ({ url: 'api/catalog' }),
      providesTags: ['Call'],
    }),
    // Live-feed hydration etc. are added by later tickets.
  }),
})

export const {
  useGetCatalogQuery,
  useGetFilterOptionsQuery,
  useGetHealthQuery,
  useSearchCallsQuery,
} = api
