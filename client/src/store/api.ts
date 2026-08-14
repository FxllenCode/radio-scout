import { createApi, fetchBaseQuery } from '@reduxjs/toolkit/query/react'

import { loginFailure, statusOf } from '@/lib/adminError'
import { searchParams } from '@/lib/archive'
import type {
  AdminApiKey,
  AdminAssignment,
  AdminLabel,
  AdminSession,
  AdminSystem,
  AdminTalkgroup,
  AdminTalkgroupQuery,
  AdminUnit,
  AdminUnitQuery,
  Catalog,
  CuratedPage,
  DocumentReport,
  FilterOptions,
  IssuedApiKey,
  Listing,
  LogPage,
  LogQuery,
  MemberDelta,
  MemberRef,
  MergeReport,
  RangeDelta,
  RangeReport,
  SearchPage,
  SearchQuery,
  Span,
  UnitHistory,
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
    /** **The CSRF token, attached once.**
     *
     *  Every state-changing admin request must echo its session's token (#19),
     *  and the curation surface (#49) is two dozen of them. Reading it from the
     *  session already in the cache means a mutation added later is protected
     *  by construction — the client-side counterpart to the server mounting its
     *  guard as a prefix layer rather than as a check each handler remembers.
     *
     *  Only on mutations: a safe method needs no token, and it is same-origin
     *  either way. */
    prepareHeaders: (headers, { type, getState }) => {
      if (type !== 'mutation') return headers
      const session = api.endpoints.getAdminSession.select()(getState() as never)
      if (session.data) headers.set('x-csrf-token', session.data.csrf_token)
      return headers
    },
  }),
  tagTypes: [
    'Call',
    'AdminSession',
    'Log',
    'System',
    'Talkgroup',
    'Group',
    'Tag',
    'Unit',
    'ApiKey',
  ],
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
     *  auto-populate (#8) grows the catalog by — and `Talkgroup` because
     *  curation (#49) is the other way it changes. */
    getCatalog: builder.query<Catalog, void>({
      query: () => ({ url: 'api/catalog' }),
      providesTags: ['Call', 'Talkgroup'],
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
    adminLogout: builder.mutation<void, void>({
      query: () => ({ url: 'api/admin/logout', method: 'POST' }),
      invalidatesTags: ['AdminSession', 'Log'],
    }),

    /** One radio's history (#47, spec US 44) — where it talks and since when.
     *
     *  Tagged `Call` like the searches beside it: ingesting a Call is what
     *  moves every number in it, and a unit CSV — or #49's roster — is what
     *  names it. */
    getUnitHistory: builder.query<UnitHistory, { systemRef: number; ref: number }>({
      query: ({ systemRef, ref }) => ({ url: `api/unit/${systemRef}/${ref}` }),
      providesTags: ['Call', 'Unit'],
    }),

    /** The operator log (#30). Newest first, filtered and paged server-side —
     *  a month of logging is far more than a page. */
    getLogs: builder.query<LogPage, LogQuery>({
      query: (filters) => ({ url: `api/admin/logs?${searchParams(filters)}` }),
      providesTags: ['Log'],
    }),

    // -- Curation (#49, spec US 45–46) ------------------------------------
    //
    // One row, one request — where rdio-scanner has a single `PUT` of the whole
    // configuration document in which a row's *absence* means deletion. The
    // tags are what make an edit reach the other screens showing the same fact:
    // renaming a Group moves every Talkgroup row carrying its name, and
    // blacklisting a channel moves the System row whose list holds the Ref.

    getSystems: builder.query<Listing<AdminSystem>, void>({
      query: () => ({ url: 'api/admin/systems' }),
      providesTags: ['System'],
    }),
    createSystem: builder.mutation<AdminSystem, Partial<AdminSystem>>({
      query: (body) => ({ url: 'api/admin/systems', method: 'POST', body }),
      invalidatesTags: ['System', 'Talkgroup', 'Call'],
    }),
    updateSystem: builder.mutation<AdminSystem, { id: number; patch: Partial<AdminSystem> }>({
      query: ({ id, patch }) => ({
        url: `api/admin/systems/${id}`,
        method: 'PATCH',
        body: patch,
      }),
      invalidatesTags: ['System', 'Talkgroup', 'Call'],
    }),
    deleteSystem: builder.mutation<void, { id: number; force?: boolean }>({
      query: ({ id, force }) => ({
        url: `api/admin/systems/${id}${force ? '?force=true' : ''}`,
        method: 'DELETE',
      }),
      invalidatesTags: ['System', 'Talkgroup', 'Unit', 'Call'],
    }),

    getAdminTalkgroups: builder.query<CuratedPage<AdminTalkgroup>, AdminTalkgroupQuery>({
      query: (filters) => ({ url: `api/admin/talkgroups?${searchParams(filters)}` }),
      providesTags: ['Talkgroup'],
    }),
    createTalkgroup: builder.mutation<AdminTalkgroup, Partial<AdminTalkgroup>>({
      query: (body) => ({ url: 'api/admin/talkgroups', method: 'POST', body }),
      invalidatesTags: ['Talkgroup', 'System', 'Group', 'Tag', 'Call'],
    }),
    updateTalkgroup: builder.mutation<
      AdminTalkgroup,
      { id: number; patch: Partial<AdminTalkgroup> }
    >({
      query: ({ id, patch }) => ({
        url: `api/admin/talkgroups/${id}`,
        method: 'PATCH',
        body: patch,
      }),
      invalidatesTags: ['Talkgroup', 'System', 'Group', 'Tag', 'Call'],
    }),
    deleteTalkgroup: builder.mutation<void, { id: number; force?: boolean }>({
      query: ({ id, force }) => ({
        url: `api/admin/talkgroups/${id}${force ? '?force=true' : ''}`,
        method: 'DELETE',
      }),
      invalidatesTags: ['Talkgroup', 'System', 'Group', 'Tag', 'Call'],
    }),
    /** Spec US 46 — one action over every selected row, in one transaction. */
    assignTalkgroups: builder.mutation<{ changed: number }, AdminAssignment>({
      query: (body) => ({ url: 'api/admin/talkgroups/assign', method: 'POST', body }),
      invalidatesTags: ['Talkgroup', 'Group', 'Tag', 'Call'],
    }),

    getGroups: builder.query<Listing<AdminLabel>, void>({
      query: () => ({ url: 'api/admin/groups' }),
      providesTags: ['Group'],
    }),
    createGroup: builder.mutation<AdminLabel, string>({
      query: (name) => ({ url: 'api/admin/groups', method: 'POST', body: { name } }),
      invalidatesTags: ['Group', 'Talkgroup', 'Call'],
    }),
    updateGroup: builder.mutation<AdminLabel, { id: number; name: string }>({
      query: ({ id, name }) => ({
        url: `api/admin/groups/${id}`,
        method: 'PATCH',
        body: { name },
      }),
      invalidatesTags: ['Group', 'Talkgroup', 'Call'],
    }),
    deleteGroup: builder.mutation<void, number>({
      query: (id) => ({ url: `api/admin/groups/${id}`, method: 'DELETE' }),
      invalidatesTags: ['Group', 'Talkgroup', 'Call'],
    }),

    getTags: builder.query<Listing<AdminLabel>, void>({
      query: () => ({ url: 'api/admin/tags' }),
      providesTags: ['Tag'],
    }),
    createTag: builder.mutation<AdminLabel, string>({
      query: (name) => ({ url: 'api/admin/tags', method: 'POST', body: { name } }),
      invalidatesTags: ['Tag', 'Talkgroup', 'Call'],
    }),
    updateTag: builder.mutation<AdminLabel, { id: number; name: string }>({
      query: ({ id, name }) => ({
        url: `api/admin/tags/${id}`,
        method: 'PATCH',
        body: { name },
      }),
      invalidatesTags: ['Tag', 'Talkgroup', 'Call'],
    }),
    deleteTag: builder.mutation<void, number>({
      query: (id) => ({ url: `api/admin/tags/${id}`, method: 'DELETE' }),
      invalidatesTags: ['Tag', 'Talkgroup', 'Call'],
    }),

    getAdminUnits: builder.query<CuratedPage<AdminUnit>, AdminUnitQuery>({
      query: (filters) => ({ url: `api/admin/units?${searchParams(filters)}` }),
      providesTags: ['Unit'],
    }),
    createUnit: builder.mutation<AdminUnit, Partial<AdminUnit>>({
      query: (body) => ({ url: 'api/admin/units', method: 'POST', body }),
      invalidatesTags: ['Unit', 'Call'],
    }),
    updateUnit: builder.mutation<AdminUnit, { id: number; patch: Partial<AdminUnit> }>({
      query: ({ id, patch }) => ({
        url: `api/admin/units/${id}`,
        method: 'PATCH',
        body: patch,
      }),
      invalidatesTags: ['Unit', 'Call'],
    }),
    deleteUnit: builder.mutation<void, number>({
      query: (id) => ({ url: `api/admin/units/${id}`, method: 'DELETE' }),
      invalidatesTags: ['Unit', 'Call'],
    }),

    // -- Merge curation (#50, spec US 17) ---------------------------------
    //
    // The member Refs and Ranges #49 deliberately left off both listings,
    // because carrying them would cost a query per row on a page of five
    // hundred. Here there is exactly one row, so they are a read of their own.

    getMembers: builder.query<Listing<MemberRef>, number>({
      query: (id) => ({ url: `api/admin/talkgroups/${id}/members` }),
      providesTags: ['Talkgroup'],
    }),

    /** **The preview**: the real transaction, rolled back (#18's dry run).
     *
     *  A mutation rather than a query because it is a `POST` with a body — and
     *  deliberately **invalidating nothing**, which is the whole reason it is a
     *  separate endpoint from the fold below. A preview that invalidated
     *  `Talkgroup` would refetch every listing to show a page that has not
     *  changed, and would do it on every click of a button whose entire promise
     *  is that nothing happened. */
    previewFold: builder.mutation<MergeReport, { id: number; delta: MemberDelta }>({
      query: ({ id, delta }) => ({
        url: `api/admin/talkgroups/${id}/members?dryRun`,
        method: 'POST',
        body: delta,
      }),
    }),

    /** ...and the same request for real. A fold re-points archived Calls, so
     *  `Call` goes with the configuration tags: every search page and the
     *  catalog behind the panel are now answering with a channel that changed. */
    foldMembers: builder.mutation<MergeReport, { id: number; delta: MemberDelta }>({
      query: ({ id, delta }) => ({
        url: `api/admin/talkgroups/${id}/members`,
        method: 'POST',
        body: delta,
      }),
      invalidatesTags: ['Talkgroup', 'System', 'Group', 'Tag', 'Call'],
    }),

    getRanges: builder.query<Listing<Span>, number>({
      query: (id) => ({ url: `api/admin/units/${id}/ranges` }),
      providesTags: ['Unit'],
    }),
    /** Add and remove spans, wholly or not at all — an overlap refuses the
     *  edit rather than applying the half of it that fits. */
    setRanges: builder.mutation<RangeReport, { id: number; delta: RangeDelta }>({
      query: ({ id, delta }) => ({
        url: `api/admin/units/${id}/ranges`,
        method: 'POST',
        body: delta,
      }),
      invalidatesTags: ['Unit', 'Call'],
    }),

    // -- The configuration document (#51, spec US 47) ---------------------

    /** **The preview**: the real transaction, rolled back. Invalidates nothing,
     *  for the reason the fold preview gives — a request whose whole promise is
     *  that nothing happened must not make every listing refetch. */
    previewConfig: builder.mutation<DocumentReport, unknown>({
      query: (document) => ({
        url: 'api/admin/config/import?dryRun',
        method: 'POST',
        body: document,
      }),
    }),
    /** ...and the same document for real. Everything is invalidated because
     *  everything may have moved: this is the one write that touches every
     *  entity at once. */
    importConfig: builder.mutation<DocumentReport, unknown>({
      query: (document) => ({
        url: 'api/admin/config/import',
        method: 'POST',
        body: document,
      }),
      invalidatesTags: ['System', 'Talkgroup', 'Group', 'Tag', 'Unit', 'ApiKey', 'Call'],
    }),

    getApiKeys: builder.query<Listing<AdminApiKey>, void>({
      query: () => ({ url: 'api/admin/api-keys' }),
      providesTags: ['ApiKey'],
    }),
    /** Issue one. The response is **the only sight of the secret there will
     *  ever be** — it is stored hashed, so nothing can show it again. */
    createApiKey: builder.mutation<
      IssuedApiKey,
      { label?: string | null; systemRef?: number | null }
    >({
      query: (body) => ({ url: 'api/admin/api-keys', method: 'POST', body }),
      invalidatesTags: ['ApiKey'],
    }),
    updateApiKey: builder.mutation<AdminApiKey, { id: number; patch: Partial<AdminApiKey> }>({
      query: ({ id, patch }) => ({
        url: `api/admin/api-keys/${id}`,
        method: 'PATCH',
        body: patch,
      }),
      invalidatesTags: ['ApiKey'],
    }),
    deleteApiKey: builder.mutation<void, number>({
      query: (id) => ({ url: `api/admin/api-keys/${id}`, method: 'DELETE' }),
      invalidatesTags: ['ApiKey'],
    }),
    // Live-feed hydration etc. are added by later tickets.
  }),
})

export const {
  useAdminLoginMutation,
  useAdminLogoutMutation,
  useAssignTalkgroupsMutation,
  useCreateApiKeyMutation,
  useCreateGroupMutation,
  useCreateSystemMutation,
  useCreateTagMutation,
  useCreateTalkgroupMutation,
  useCreateUnitMutation,
  useDeleteApiKeyMutation,
  useDeleteGroupMutation,
  useDeleteSystemMutation,
  useDeleteTagMutation,
  useDeleteTalkgroupMutation,
  useDeleteUnitMutation,
  useFoldMembersMutation,
  useGetAdminSessionQuery,
  useGetAdminTalkgroupsQuery,
  useGetAdminUnitsQuery,
  useGetApiKeysQuery,
  useGetCatalogQuery,
  useGetFilterOptionsQuery,
  useGetGroupsQuery,
  useGetHealthQuery,
  useGetLogsQuery,
  useGetMembersQuery,
  useGetRangesQuery,
  useGetSystemsQuery,
  useGetTagsQuery,
  useGetUnitHistoryQuery,
  useImportConfigMutation,
  usePreviewConfigMutation,
  usePreviewFoldMutation,
  useSearchCallsQuery,
  useSetRangesMutation,
  useUpdateApiKeyMutation,
  useUpdateGroupMutation,
  useUpdateSystemMutation,
  useUpdateTagMutation,
  useUpdateTalkgroupMutation,
  useUpdateUnitMutation,
} = api
