import { http, HttpResponse } from 'msw'

/** Same-origin base our relative RTK Query calls resolve to under jsdom (the
 *  Request shim in setup.ts rewrites `/foo` → `http://localhost/foo`). Handlers
 *  are absolute so `msw/node` matches them without browser `location`. */
export const ORIGIN = 'http://localhost'

/** Shared MSW request handlers (ADR-0010: mock at the network boundary, never
 *  `fetch`/module mocks). Per-test overrides go through `server.use(...)`. */
export const handlers = [
  http.get(`${ORIGIN}/healthz`, () => new HttpResponse('ok', { status: 200 })),
]
