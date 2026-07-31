import { http, HttpResponse, ws } from 'msw'

import type { Call, Catalog, FilterOptions, LogEvent, LogPage } from '@/types'

/** Same-origin base our relative RTK Query calls resolve to under jsdom (the
 *  Request shim in setup.ts rewrites `/foo` → `http://localhost/foo`). Handlers
 *  are absolute so `msw/node` matches them without browser `location`. */
export const ORIGIN = 'http://localhost'

/** A small archive the search handlers page over — newest first, matching what
 *  the real `GET /api/calls` returns. */
export const ARCHIVE: Call[] = [
  {
    id: 3,
    systemRef: 200,
    systemLabel: 'Beta',
    talkgroupRef: 1,
    talkgroupLabel: 'Beta Dispatch',
    talkgroupTag: 'Fire',
    talkgroupGroup: 'Public',
    timestamp: Date.parse('2026-07-25T14:32:05'),
    durationMs: 8250,
    audioUrl: '/api/call/3/audio',
  },
  {
    id: 2,
    systemRef: 100,
    systemLabel: 'Alpha',
    talkgroupRef: 2,
    talkgroupLabel: 'Alpha Law',
    talkgroupTag: 'Law',
    talkgroupGroup: 'Emergency',
    timestamp: Date.parse('2026-07-25T14:30:00'),
    durationMs: 94_200,
    emergency: true,
    audioUrl: '/api/call/2/audio',
  },
  {
    id: 1,
    systemRef: 100,
    systemLabel: 'Alpha',
    talkgroupRef: 1,
    talkgroupLabel: 'Alpha Fire',
    talkgroupTag: 'Fire',
    talkgroupGroup: 'Emergency',
    timestamp: Date.parse('2026-07-25T14:00:00'),
    audioUrl: '/api/call/1/audio',
  },
]

export const FILTER_OPTIONS: FilterOptions = {
  systems: [
    { ref: 100, label: 'Alpha' },
    { ref: 200, label: 'Beta' },
  ],
  talkgroups: [
    { systemRef: 100, ref: 1, label: 'Alpha Fire', tag: 'Fire' },
    { systemRef: 100, ref: 2, label: 'Alpha Law', tag: 'Law' },
    { systemRef: 200, ref: 1, label: 'Beta Dispatch', tag: 'Fire' },
  ],
  groups: ['Emergency', 'Public'],
  tags: ['Fire', 'Law'],
  dateStartMs: Date.parse('2026-07-25T14:00:00'),
  dateStopMs: Date.parse('2026-07-25T14:32:05'),
}

/** What `GET /api/catalog` serves (#12): the same two Systems the archive
 *  above has Calls for, plus a Talkgroup that has none — the catalog is the
 *  configured world, not the archived one. */
export const CATALOG: Catalog = {
  systems: [
    {
      ref: 100,
      label: 'Alpha',
      talkgroups: [
        { ref: 1, label: 'Alpha Fire', tag: 'Fire', groups: ['Emergency'], led: 'red' },
        { ref: 2, label: 'Alpha Law', tag: 'Law', groups: ['Emergency', 'Public'] },
        { ref: 3, label: 'Alpha Quiet', tag: 'Ops', groups: ['Public'] },
      ],
    },
    {
      ref: 200,
      label: 'Beta',
      talkgroups: [
        { ref: 1, label: 'Beta Dispatch', tag: 'Fire', groups: ['Public'] },
      ],
    },
  ],
}

/** One page of an archive, honoring `limit`/`offset` so pagination is real.
 *
 *  Defaults to `ARCHIVE`; a test about crossing page *boundaries* (#32) passes
 *  its own longer archive rather than restating the slicing. */
export function archivePage(url: URL, rows: Call[] = ARCHIVE) {
  const limit = Number(url.searchParams.get('limit') ?? 100)
  const offset = Number(url.searchParams.get('offset') ?? 0)
  const results = rows.slice(offset, offset + limit)
  return {
    results,
    count: rows.length,
    limit,
    offset,
    hasMore: offset + results.length < rows.length,
  }
}

/** What the operator log surface serves (#30), newest first — one event of
 *  each level, with the structured fields and the correlation ref that make an
 *  event more than a sentence. */
export const LOG_EVENTS: LogEvent[] = [
  {
    id: 3,
    atMs: Date.parse('2026-07-25T14:32:05'),
    level: 'WARN',
    target: 'radio_scout::ingest',
    message: 'ingest rejected',
    fields: { reason: 'duplicate' },
    requestId: '0123456789abcdef',
  },
  {
    id: 2,
    atMs: Date.parse('2026-07-25T14:30:00'),
    level: 'ERROR',
    target: 'radio_scout::http_log',
    message: 'request failed',
    fields: { stage: 'store-call' },
    requestId: 'fedcba9876543210',
  },
  {
    id: 1,
    atMs: Date.parse('2026-07-25T14:00:00'),
    level: 'INFO',
    target: 'radio_scout::ingest',
    message: 'call stored',
  },
]

/** The levels a `level=` floor admits, mirroring `src/logview.rs`. */
const LEVELS_AT_OR_ABOVE: Record<string, string[]> = {
  error: ['ERROR'],
  warn: ['ERROR', 'WARN'],
  info: ['ERROR', 'WARN', 'INFO'],
}

/** One page of `LOG_EVENTS`, honoring `level`/`after`/`before`/`limit`/`offset`
 *  so a filter test asserts against a server that actually filters. */
export function logPage(url: URL): LogPage {
  const level = url.searchParams.get('level')
  const after = url.searchParams.get('after')
  const before = url.searchParams.get('before')
  const limit = Number(url.searchParams.get('limit') ?? 100)
  const offset = Number(url.searchParams.get('offset') ?? 0)

  const matched = LOG_EVENTS.filter(
    (event) =>
      (!level || (LEVELS_AT_OR_ABOVE[level] ?? []).includes(event.level)) &&
      (!after || event.atMs >= Number(after)) &&
      (!before || event.atMs < Number(before)),
  )
  const results = matched.slice(offset, offset + limit)
  return {
    results,
    count: matched.length,
    limit,
    offset,
    hasMore: offset + results.length < matched.length,
  }
}

/** The server's VAPID public key in tests — RFC 8291's application-server key,
 *  the same constant the Rust harness uses, so the two halves of #16 are
 *  described by one value. */
export const VAPID_PUBLIC_KEY =
  'BP4z9KsN6nGRTbVYI_c7VJSPQTBtkgcy27mlmlMoZIIgDll6e3vCYLocInmYWAmS6TlzAC8wEqKK6PBru3jl7A8'

/** The live-feed socket, same origin as the API (ADR-0004). Tests that care
 *  what the feed sends override this link through `server.use(...)`. */
export const liveFeed = ws.link(`${ORIGIN.replace('http', 'ws')}/api/live`)

/** Shared MSW request handlers (ADR-0010: mock at the network boundary, never
 *  `fetch`/module mocks). Per-test overrides go through `server.use(...)`. */
export const handlers = [
  // A quiet feed: connects, greets, and pushes nothing. Every screen mounts the
  // shell, so every test opens this socket.
  liveFeed.addEventListener('connection', ({ client }) =>
    client.send(JSON.stringify({ t: 'hello', protocol: 1, heartbeatMs: 30_000 })),
  ),
  http.get(`${ORIGIN}/healthz`, () => new HttpResponse('ok', { status: 200 })),
  http.get(`${ORIGIN}/api/calls`, ({ request }) =>
    HttpResponse.json(archivePage(new URL(request.url))),
  ),
  http.get(`${ORIGIN}/api/calls/filters`, () =>
    HttpResponse.json(FILTER_OPTIONS),
  ),
  http.get(`${ORIGIN}/api/catalog`, () => HttpResponse.json(CATALOG)),
  // The admin surface (#19) as an unauthenticated browser sees it: no session,
  // so the Logs view (#30) asks for a password. Tests that sign in override
  // these through `server.use(...)`.
  http.get(
    `${ORIGIN}/api/admin/session`,
    () => new HttpResponse('admin session required\n', { status: 401 }),
  ),
  http.get(
    `${ORIGIN}/api/admin/logs`,
    () => new HttpResponse('admin session required\n', { status: 401 }),
  ),
  // Web Push (#16). A server that has an identity, takes subscriptions, and
  // forgets them on request — the shape every screen mounts against.
  http.get(`${ORIGIN}/api/push/key`, () =>
    HttpResponse.json({ key: VAPID_PUBLIC_KEY }),
  ),
  http.post(`${ORIGIN}/api/push/subscribe`, () =>
    HttpResponse.json({ token: 'a-subscription-token' }),
  ),
  http.post(
    `${ORIGIN}/api/push/unsubscribe`,
    () => new HttpResponse(null, { status: 204 }),
  ),
  /** A Call's audio. Nothing in jsdom decodes it — it is here because the
   *  player prefetches the next Call's audio (#14), and an unhandled request
   *  is a test failure. */
  http.get(
    `${ORIGIN}/api/call/:id/audio`,
    () =>
      new HttpResponse('audio-bytes', {
        headers: { 'content-type': 'audio/mpeg' },
      }),
  ),
]
