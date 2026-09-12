import { createApi, fetchBaseQuery } from '@reduxjs/toolkit/query/react'

import { withGrant } from '@/lib/access'
import { loginFailure, statusOf } from '@/lib/adminError'
import { searchParams } from '@/lib/archive'

import { grantHeld, selectGrant } from './access'
import { forgetStar, markStarred } from './stars'
import type { QuietSpan } from '@/lib/catchup'
import type {
  ActivityQuery,
  AdminAccessCode,
  AdminApiKey,
  AdminAssignment,
  AdminDownstream,
  AdminEvent,
  AdminEventDetail,
  AdminLabel,
  AdminSession,
  AdminShareLink,
  AdminSystem,
  AdminTalkgroup,
  AdminTalkgroupQuery,
  AdminToneProfile,
  AdminUnit,
  AdminUnitQuery,
  AdminWebhook,
  Call,
  Catalog,
  CuratedPage,
  DocumentReport,
  EventShare,
  FrozenEvent,
  FilterOptions,
  IssuedAccessCode,
  IssuedApiKey,
  ListenerQuery,
  Listing,
  LogPage,
  LogQuery,
  MemberDelta,
  MemberRef,
  MergeReport,
  NewAccessCode,
  NewDownstream,
  NewEvent,
  NewToneProfile,
  NewWebhook,
  RangeDelta,
  RangeReport,
  SearchPage,
  SearchQuery,
  Series,
  ShareLink,
  Span,
  UnitHistory,
  Unlocked,
} from '@/types'

/** The single RTK Query API slice. Everything is same-origin: in dev the Vite
 *  proxy forwards to the Rust backend, and in production the SPA is served by
 *  the binary itself, so relative URLs Just Work. */
/** The plain same-origin fetch, before the **grant** is put on it. */
const sameOrigin = fetchBaseQuery({
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
})

/**
 * **The grant, attached once** (#68, spec US 52).
 *
 * `prepareHeaders` cannot do this: a grant rides the **query string**, which is
 * the one part of a URL an `<audio src>`, a `WebSocket` and a `fetch` can all
 * carry and the one part `http_log` never writes down (ADR-0008). So the base
 * query is wrapped rather than configured, and the rewrite happens for every
 * endpoint there is — which is the same property the CSRF token gets from
 * `prepareHeaders` and for the same reason: an endpoint added later inherits it
 * instead of remembering it.
 *
 * A request with no grant is byte-for-byte the request this app sent before
 * this feature existed, which is what keeps "an instance with no codes behaves
 * exactly as today" true on this side of the wire too.
 */
const baseQuery: typeof sameOrigin = (args, apiArgs, extra) => {
  const grant = selectGrant(apiArgs.getState() as Parameters<typeof selectGrant>[0])
  if (!grant) return sameOrigin(args, apiArgs, extra)
  const granted =
    typeof args === 'string'
      ? withGrant(args, grant)
      : { ...args, url: withGrant(args.url, grant) }
  return sameOrigin(granted, apiArgs, extra)
}

export const api = createApi({
  reducerPath: 'api',
  baseQuery,
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
    'Downstream',
    'Webhook',
    'ToneProfile',
    'ShareLink',
    'Star',
    'Event',
    'AccessCode',
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
      providesTags: ['Call', 'Star'],
    }),

    /**
     * One Call, by id — what a deep link resolves through (#61, spec US 30).
     *
     * Deliberately not a search: a link names a Call, and requiring it to also
     * match the filters on screen would make "open this Call" fail for the
     * commonest reason there is — the recipient was looking at something else.
     * The server flattens `CallDetail` over a search row, so what comes back is
     * a superset of a [`Call`] and needs no separate shape here.
     */
    getCall: builder.query<Call, number>({
      query: (id) => ({ url: `api/call/${id}` }),
      providesTags: ['Call', 'Star'],
    }),

    /** The cascading filter options for the filters already chosen — only
     *  values that have Calls behind them (#13). */
    getFilterOptions: builder.query<FilterOptions, SearchQuery>({
      query: (search) => ({ url: `api/calls/filters?${searchParams(search)}` }),
      providesTags: ['Call'],
    }),

    /**
     * How busy the Archive was under a search (#62, spec US 34–35).
     *
     * The same filters the results came from, so the ribbon above them is a
     * picture of *those* results — and deliberately **without** `limit` and
     * `offset`, which is what keeps turning a page from refetching it. One
     * cache entry per search, however far into it the Listener has walked.
     */
    getActivity: builder.query<Series, ActivityQuery>({
      query: (activity) => ({ url: `api/calls/activity?${searchParams(activity)}` }),
      providesTags: ['Call'],
    }),

    /**
     * Where a window of queued Calls is quiet (#59, spec US 23).
     *
     * **The only way a Call in the listening queue can get its spans.** It was
     * pushed over the live feed at ingest, before anything looked at its audio,
     * and nothing republishes a frame (#46) — so a Call read back from the
     * Archive carries them on itself and one delivered live never does.
     *
     * Asked only while **Catch-up** is engaged, and only about the head of the
     * queue: a Listener who is not behind never sends this request. Untagged,
     * because a Call's gaps are a fact about audio that is already written and
     * cannot change under a `Call` invalidation — where re-fetching on every
     * arriving Call is exactly the cost this feature must not have.
     */
    getQuietSpans: builder.query<Record<string, QuietSpan[]>, number[]>({
      query: (ids) => ({ url: `api/calls/quiet?ids=${ids.join(',')}` }),
    }),

    /**
     * Star a Call, or take the Star off again (#66, spec US 37).
     *
     * **One endpoint for both verbs**, because the optimistic rule is one rule:
     * say so immediately, and take it back only if the server refuses. Two
     * endpoints would be two copies of that, and the second is the one that
     * gets it wrong.
     *
     * The optimistic mark is what makes the tap instant, and it is not an
     * optimisation — a Call lives in the archive cache, in `live`'s history and
     * in `playback`'s page at once, and only the first of those can be
     * refetched (`store/stars`). The invalidation behind it is `Star` alone
     * rather than `Call`, so un-starring inside a `?starred=1` search still
     * takes the row away without also re-fetching the catalog, the density
     * ribbon and the filter options on every tap.
     */
    setStar: builder.mutation<{ starred: boolean }, { id: number; starred: boolean }>({
      query: ({ id, starred }) => ({
        url: `api/call/${id}/star`,
        method: starred ? 'POST' : 'DELETE',
      }),
      invalidatesTags: ['Star'],
      async onQueryStarted({ id, starred }, { dispatch, queryFulfilled }) {
        dispatch(markStarred({ id, starred }))
        try {
          await queryFulfilled
        } catch {
          dispatch(forgetStar(id))
        }
      },
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
      providesTags: ['Call', 'Unit', 'Star'],
    }),

    /** The operator log (#30). Newest first, filtered and paged server-side —
     *  a month of logging is far more than a page. */
    getLogs: builder.query<LogPage, LogQuery>({
      query: (filters) => ({ url: `api/admin/logs?${searchParams(filters)}` }),
      providesTags: ['Log'],
    }),

    /**
     * Peak Listeners over time (#62, spec US 41).
     *
     * Behind the admin session, unlike every other read here: the Archive is
     * open because listening is open, and how many people take that up is the
     * Operator's own business. Untagged — nothing a browser does invalidates
     * it, and it is a chart of the past.
     */
    getListenerHistory: builder.query<Series, ListenerQuery>({
      query: (range) => ({ url: `api/admin/listeners?${searchParams(range)}` }),
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
      invalidatesTags: [
        'System',
        'Talkgroup',
        'Group',
        'Tag',
        'Unit',
        'ApiKey',
        'Downstream',
        'Call',
      ],
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

    /** **Access codes** (#68, spec US 52) — the Listener-facing credentials that
     *  open a restricted channel. The listing carries neither secret: the code
     *  is Argon2id at rest and cannot be read back at all, and the grant is
     *  shown exactly once, by [`createAccessCode`]. */
    getAccessCodes: builder.query<Listing<AdminAccessCode>, void>({
      query: () => ({ url: 'api/admin/codes' }),
      providesTags: ['AccessCode'],
    }),
    createAccessCode: builder.mutation<IssuedAccessCode, NewAccessCode>({
      query: (body) => ({ url: 'api/admin/codes', method: 'POST', body }),
      invalidatesTags: ['AccessCode'],
    }),
    /** Edit one. A `code` in the patch **rotates it**, and the grant every
     *  browser is holding goes with it — which is why the answer to a rotation
     *  is worth showing, and why one without a `code` is not. */
    updateAccessCode: builder.mutation<
      AdminAccessCode,
      { id: number; patch: Partial<NewAccessCode> }
    >({
      query: ({ id, patch }) => ({
        url: `api/admin/codes/${id}`,
        method: 'PATCH',
        body: patch,
      }),
      invalidatesTags: ['AccessCode'],
    }),
    deleteAccessCode: builder.mutation<void, number>({
      query: (id) => ({ url: `api/admin/codes/${id}`, method: 'DELETE' }),
      invalidatesTags: ['AccessCode'],
    }),

    /**
     * Prove knowledge of an **Access code** and be handed its grant (#68).
     *
     * The code goes up in a **body**, never a query string: it is the secret an
     * Operator chose and the one thing here that must not end up in a URL a
     * browser remembers. What comes back rides in local storage from then on,
     * and is 128 minted bits rather than a word thirty people were told.
     *
     * **The grant is held here, and the refetch is dispatched by hand** — not
     * through `invalidatesTags`, which is the one place the order matters.
     * Unlocking changes *what every read answers with* (the catalog's locked
     * rows, the search page, the filter options), so everything has to come
     * back; but `invalidatesTags` fires on the fulfilled action, before any
     * caller could have stored what came back, so the refetch would go out
     * **without the grant** and answer with exactly the channels the Listener
     * just unlocked being absent. Storing it first and invalidating second is
     * the whole fix, and doing both here is what keeps a second caller from
     * having to know.
     */
    unlock: builder.mutation<Unlocked, string>({
      query: (code) => ({ url: 'api/unlock', method: 'POST', body: { code } }),
      async onQueryStarted(_code, { dispatch, queryFulfilled }) {
        // Caught, because a refused unlock is an ordinary outcome — a mistyped
        // code, an expired one — and `queryFulfilled` rejects on every one of
        // them. The sheet is what tells the Listener; there is nothing to do
        // here but not crash the tab.
        const unlocked = await queryFulfilled.catch(() => undefined)
        if (!unlocked) return
        dispatch(grantHeld(unlocked.data.grant))
        dispatch(api.util.invalidateTags(['Call', 'Talkgroup', 'AccessCode']))
      },
    }),

    /** **Downstream** peers (#52), with their health beside them — queue depth,
     *  last success, consecutive failures. The listing is the operator-facing
     *  status surface until #70 exists. */
    getDownstreams: builder.query<Listing<AdminDownstream>, void>({
      query: () => ({ url: 'api/admin/downstreams' }),
      providesTags: ['Downstream'],
    }),
    createDownstream: builder.mutation<AdminDownstream, NewDownstream>({
      query: (body) => ({ url: 'api/admin/downstreams', method: 'POST', body }),
      invalidatesTags: ['Downstream'],
    }),
    /** An edit that omits `apiKey` leaves the stored credential alone — the
     *  screen can never show it again, so re-scoping a peer must not require
     *  re-typing it. */
    updateDownstream: builder.mutation<
      AdminDownstream,
      { id: number; patch: Partial<NewDownstream> }
    >({
      query: ({ id, patch }) => ({
        url: `api/admin/downstreams/${id}`,
        method: 'PATCH',
        body: patch,
      }),
      invalidatesTags: ['Downstream'],
    }),
    deleteDownstream: builder.mutation<void, number>({
      query: (id) => ({ url: `api/admin/downstreams/${id}`, method: 'DELETE' }),
      invalidatesTags: ['Downstream'],
    }),

    /** **Webhooks** (#54), with their health beside them, on the Downstream
     *  listing's terms. The URL never comes back — see [`AdminWebhook`]. */
    getWebhooks: builder.query<Listing<AdminWebhook>, void>({
      query: () => ({ url: 'api/admin/webhooks' }),
      providesTags: ['Webhook'],
    }),
    createWebhook: builder.mutation<AdminWebhook, NewWebhook>({
      query: (body) => ({ url: 'api/admin/webhooks', method: 'POST', body }),
      invalidatesTags: ['Webhook'],
    }),
    /** An edit that omits `url` leaves the stored credential alone — the screen
     *  can never show it again, so re-scoping must not require re-pasting it. */
    updateWebhook: builder.mutation<
      AdminWebhook,
      { id: number; patch: Partial<NewWebhook> }
    >({
      query: ({ id, patch }) => ({
        url: `api/admin/webhooks/${id}`,
        method: 'PATCH',
        body: patch,
      }),
      invalidatesTags: ['Webhook'],
    }),
    deleteWebhook: builder.mutation<void, number>({
      query: (id) => ({ url: `api/admin/webhooks/${id}`, method: 'DELETE' }),
      invalidatesTags: ['Webhook'],
    }),

    /**
     * Mint (or re-mint) a Call's expiring public link (#64, spec US 32).
     *
     * A **mutation** rather than a query, because it writes: there is one live
     * link per Call, so asking for one either issues a token or pushes an
     * existing window out. Unauthenticated on purpose — a Listener holds no
     * credential, and the abuse bound is that one row rather than a gate.
     *
     * It invalidates the Operator's listing, which is the screen that would
     * otherwise be stale about what this Instance is currently sharing.
     */
    shareCall: builder.mutation<ShareLink, number>({
      query: (id) => ({ url: `api/call/${id}/share`, method: 'POST' }),
      invalidatesTags: ['ShareLink'],
    }),

    /** Every link a Listener has minted, for the one thing an Operator does
     *  about one. No create and no edit — minting is the Listener's, and a link
     *  that has run out is re-shared rather than extended. */
    getShareLinks: builder.query<CuratedPage<AdminShareLink>, { limit: number; offset: number }>({
      query: (window) => ({
        url: `api/admin/shares?limit=${window.limit}&offset=${window.offset}`,
      }),
      providesTags: ['ShareLink'],
    }),
    deleteShareLink: builder.mutation<void, number>({
      query: (id) => ({ url: `api/admin/shares/${id}`, method: 'DELETE' }),
      invalidatesTags: ['ShareLink'],
    }),

    /**
     * **Events** (#67, spec US 38) — incidents frozen against **Retention**.
     *
     * Every one of these is admin-gated, unlike `shareCall` next door, and the
     * difference is what the write *costs*: a share link is bounded by the Calls
     * table, and freezing copies audio that no policy here can ever reclaim.
     *
     * One tag for the lot. An Operator works on one incident at a time, and a
     * per-id tag would buy a cache key that a create could not invalidate.
     */
    getEvents: builder.query<CuratedPage<AdminEvent>, { limit: number; offset: number }>({
      query: (window) => ({
        url: `api/admin/events?limit=${window.limit}&offset=${window.offset}`,
      }),
      providesTags: ['Event'],
    }),
    getEvent: builder.query<AdminEventDetail, number>({
      query: (id) => ({ url: `api/admin/events/${id}` }),
      providesTags: ['Event'],
    }),
    createEvent: builder.mutation<FrozenEvent, NewEvent>({
      query: (body) => ({ url: 'api/admin/events', method: 'POST', body }),
      invalidatesTags: ['Event'],
    }),
    updateEvent: builder.mutation<
      AdminEvent,
      { id: number; patch: Partial<Pick<NewEvent, 'name' | 'notes'>> & { shared?: boolean } }
    >({
      query: ({ id, patch }) => ({
        url: `api/admin/events/${id}`,
        method: 'PATCH',
        body: patch,
      }),
      invalidatesTags: ['Event'],
    }),
    deleteEvent: builder.mutation<void, number>({
      query: (id) => ({ url: `api/admin/events/${id}`, method: 'DELETE' }),
      invalidatesTags: ['Event'],
    }),
    /** Freeze Calls into an Event. Answers with the **whole Event again**, report
     *  and all — the same document a create answers with, so "Calls were added"
     *  has one shape on the wire rather than two. */
    addEventCalls: builder.mutation<FrozenEvent, { id: number; callIds: number[] }>({
      query: ({ id, callIds }) => ({
        url: `api/admin/events/${id}/calls`,
        method: 'POST',
        body: { callIds },
      }),
      invalidatesTags: ['Event'],
    }),
    removeEventCall: builder.mutation<void, { id: number; member: number }>({
      query: ({ id, member }) => ({
        url: `api/admin/events/${id}/calls/${member}`,
        method: 'DELETE',
      }),
      invalidatesTags: ['Event'],
    }),
    /** The link, fetched **only when an Operator asks to copy it** — which is
     *  what keeps a credential out of every listing that renders Events. */
    getEventShare: builder.query<EventShare, number>({
      query: (id) => ({ url: `api/admin/events/${id}/share` }),
      providesTags: ['Event'],
    }),

    /** **Tone profiles** (#55, spec US 20) — a sub-resource of the Talkgroup
     *  they are paged on, because a profile that named no channel would be
     *  looked for in every Call this Instance takes.
     *
     *  One tag for the lot rather than one per channel: an Operator edits one
     *  channel's profiles at a time, and a per-id tag would buy nothing but a
     *  cache key nobody could invalidate correctly from a delete, whose route
     *  does not name the Talkgroup. */
    getToneProfiles: builder.query<Listing<AdminToneProfile>, number>({
      query: (talkgroupId) => ({
        url: `api/admin/talkgroups/${talkgroupId}/tones`,
      }),
      providesTags: ['ToneProfile'],
    }),
    createToneProfile: builder.mutation<
      AdminToneProfile,
      { talkgroupId: number; body: NewToneProfile }
    >({
      query: ({ talkgroupId, body }) => ({
        url: `api/admin/talkgroups/${talkgroupId}/tones`,
        method: 'POST',
        body,
      }),
      invalidatesTags: ['ToneProfile'],
    }),
    updateToneProfile: builder.mutation<
      AdminToneProfile,
      { id: number; patch: Partial<NewToneProfile> }
    >({
      query: ({ id, patch }) => ({
        url: `api/admin/tones/${id}`,
        method: 'PATCH',
        body: patch,
      }),
      invalidatesTags: ['ToneProfile'],
    }),
    deleteToneProfile: builder.mutation<void, number>({
      query: (id) => ({ url: `api/admin/tones/${id}`, method: 'DELETE' }),
      invalidatesTags: ['ToneProfile'],
    }),
    // Live-feed hydration etc. are added by later tickets.
  }),
})

export const {
  useAdminLoginMutation,
  useAdminLogoutMutation,
  useAddEventCallsMutation,
  useAssignTalkgroupsMutation,
  useCreateAccessCodeMutation,
  useCreateApiKeyMutation,
  useCreateDownstreamMutation,
  useCreateEventMutation,
  useCreateToneProfileMutation,
  useCreateWebhookMutation,
  useCreateGroupMutation,
  useCreateSystemMutation,
  useCreateTagMutation,
  useCreateTalkgroupMutation,
  useCreateUnitMutation,
  useDeleteAccessCodeMutation,
  useDeleteApiKeyMutation,
  useDeleteDownstreamMutation,
  useDeleteEventMutation,
  useDeleteToneProfileMutation,
  useDeleteWebhookMutation,
  useDeleteGroupMutation,
  useDeleteShareLinkMutation,
  useDeleteSystemMutation,
  useDeleteTagMutation,
  useDeleteTalkgroupMutation,
  useDeleteUnitMutation,
  useFoldMembersMutation,
  useGetActivityQuery,
  useGetAdminSessionQuery,
  useGetAdminTalkgroupsQuery,
  useGetAdminUnitsQuery,
  useGetAccessCodesQuery,
  useGetApiKeysQuery,
  useGetCallQuery,
  useGetCatalogQuery,
  useGetListenerHistoryQuery,
  useGetQuietSpansQuery,
  useSetStarMutation,
  useGetDownstreamsQuery,
  useGetEventQuery,
  useGetEventsQuery,
  useGetToneProfilesQuery,
  useGetWebhooksQuery,
  useGetFilterOptionsQuery,
  useGetGroupsQuery,
  useGetHealthQuery,
  useGetLogsQuery,
  useGetMembersQuery,
  useGetRangesQuery,
  useGetShareLinksQuery,
  useLazyGetEventShareQuery,
  useRemoveEventCallMutation,
  useGetSystemsQuery,
  useGetTagsQuery,
  useGetUnitHistoryQuery,
  useImportConfigMutation,
  usePreviewConfigMutation,
  usePreviewFoldMutation,
  useLazySearchCallsQuery,
  useSearchCallsQuery,
  useShareCallMutation,
  useSetRangesMutation,
  useUnlockMutation,
  useUpdateAccessCodeMutation,
  useUpdateApiKeyMutation,
  useUpdateDownstreamMutation,
  useUpdateEventMutation,
  useUpdateToneProfileMutation,
  useUpdateWebhookMutation,
  useUpdateGroupMutation,
  useUpdateSystemMutation,
  useUpdateTagMutation,
  useUpdateTalkgroupMutation,
  useUpdateUnitMutation,
} = api
