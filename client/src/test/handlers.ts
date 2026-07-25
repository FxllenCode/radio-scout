import { http, HttpResponse, ws } from 'msw'

import type { Call, FilterOptions } from '@/types'

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

/** One page of `ARCHIVE`, honoring `limit`/`offset` so pagination is real. */
export function archivePage(url: URL) {
  const limit = Number(url.searchParams.get('limit') ?? 100)
  const offset = Number(url.searchParams.get('offset') ?? 0)
  const results = ARCHIVE.slice(offset, offset + limit)
  return {
    results,
    count: ARCHIVE.length,
    limit,
    offset,
    hasMore: offset + results.length < ARCHIVE.length,
  }
}

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
