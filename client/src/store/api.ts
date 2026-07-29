import { createApi, fetchBaseQuery } from '@reduxjs/toolkit/query/react'

import { loginFailure, statusOf } from '@/lib/adminError'
import { searchParams } from '@/lib/archive'
import type {
  AdminSession,
  Catalog,
  FilterOptions,
  LogPage,
  LogQuery,
  SearchPage,
  SearchQuery,
} from '@/types'

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
  tagTypes: ['Call', 'AdminSession', 'Log'],
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
    /** Whether this browser holds a live admin session (#19), and the CSRF
     *  token bound to it. A page that has been reloaded still has the httpOnly
     *  cookie but not the token, so it asks — which is also how the Logs view
     *  knows whether to show a password field or the log. */
    getAdminSession: builder.query<AdminSession, void>({
      query: () => ({ url: 'api/admin/session' }),
      providesTags: ['AdminSession'],
    }),

    /** Open one. The session rides an httpOnly cookie the browser stores
     *  itself; nothing about it is reachable from script (ADR-0008). */
    adminLogin: builder.mutation<AdminSession, string>({
      query: (password) => ({
        url: 'api/admin/login',
        method: 'POST',
        body: { password },
      }),
      // The refusal is turned into words here, where the `Retry-After` header
      // is still reachable — `fetchBaseQuery`'s error carries only the status,
      // and a lockout an operator cannot tell from a typo is #19's whole point.
      transformErrorResponse: (error, meta) =>
        loginFailure(statusOf(error), meta?.response?.headers.get('retry-after')),
      invalidatesTags: ['AdminSession', 'Log'],
    }),

    /** ...and close it, server-side — ADR-0008 chose session state over a JWT
     *  precisely so revocation is real, and clearing the cookie alone would
     *  leave the session live. */
    adminLogout: builder.mutation<void, string>({
      query: (csrfToken) => ({
        url: 'api/admin/logout',
        method: 'POST',
        headers: { 'x-csrf-token': csrfToken },
      }),
      invalidatesTags: ['AdminSession', 'Log'],
    }),

    /** The operator log (#30). Newest first, filtered and paged server-side —
     *  a month of logging is far more than a page. */
    getLogs: builder.query<LogPage, LogQuery>({
      query: (filters) => ({ url: `api/admin/logs?${searchParams(filters)}` }),
      providesTags: ['Log'],
    }),
    // Live-feed hydration etc. are added by later tickets.
  }),
})

export const {
  useAdminLoginMutation,
  useAdminLogoutMutation,
  useGetAdminSessionQuery,
  useGetCatalogQuery,
  useGetFilterOptionsQuery,
  useGetHealthQuery,
  useGetLogsQuery,
  useSearchCallsQuery,
} = api
