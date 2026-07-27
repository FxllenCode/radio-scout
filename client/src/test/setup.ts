import '@testing-library/jest-dom'
import { afterAll, afterEach, beforeAll, expect } from 'vitest'
import { setupServer } from 'msw/node'
import * as axeMatchers from 'vitest-axe/matchers'

import { handlers } from './handlers'

// In jsdom the global `Request`/`fetch` are Node's (undici), which reject
// relative URLs — but a browser resolves them against the document origin, and
// our same-origin calls (RTK Query's baseUrl '/', the audio prefetch) are
// relative. Resolve relative request URLs against a test origin so those calls
// parse like they do in a real browser.
const absolute = (input: RequestInfo | URL) =>
  typeof input === 'string' && input.startsWith('/')
    ? `http://localhost${input}`
    : input

const BaseRequest = globalThis.Request
class RelativeAwareRequest extends BaseRequest {
  constructor(input: RequestInfo | URL, init?: RequestInit) {
    super(absolute(input), init)
  }
}
globalThis.Request = RelativeAwareRequest as typeof Request

// jsdom has no media stack at all: `play`/`pause`/`load` raise "not
// implemented" and return nothing, so a player that drives its element
// imperatively can't run — let alone be observed. These give them the shape a
// browser has (a promise from `play`, the matching events) without pretending
// to decode anything. Writable + configurable so a test can spy on them, or
// make `play` reject the way a browser blocking autoplay does.
for (const [name, value] of Object.entries({
  play(this: HTMLMediaElement) {
    this.dispatchEvent(new Event('play'))
    return Promise.resolve()
  },
  pause(this: HTMLMediaElement) {
    this.dispatchEvent(new Event('pause'))
  },
  load() {},
})) {
  Object.defineProperty(HTMLMediaElement.prototype, name, {
    value,
    writable: true,
    configurable: true,
  })
}

// a11y assertions (`toHaveNoViolations`).
expect.extend(axeMatchers)

// MSW: intercept at the network boundary. Unhandled requests are an error so a
// test that hits an unexpected endpoint fails loudly instead of silently.
export const server = setupServer(...handlers)
beforeAll(() => {
  server.listen({ onUnhandledRequest: 'error' })
  // MSW's interceptor replaced `fetch` just now, and it parses the URL before
  // handing it on — so a *bare* relative fetch (one that doesn't build a
  // `Request` first) has to be resolved outside it, not just in the subclass
  // above. This rewrites the URL and nothing else: MSW still owns every
  // response, so the network boundary stays where ADR-0010 puts it.
  const intercepted = globalThis.fetch
  globalThis.fetch = ((input: RequestInfo | URL, init?: RequestInit) =>
    intercepted(absolute(input), init)) as typeof fetch
})
afterEach(() => server.resetHandlers())

// The Selection is persisted per browser (spec US 22, `lib/persist.ts`), and
// jsdom hands every test in a file the *same* storage — so a test that changes
// what the listener hears silently changes what the next test's store hydrates,
// and a Call on a Talkgroup an earlier test switched off simply never arrives.
//
// It bit only on Linux, which is the reason it is worth a comment: Node 22 ships
// an experimental `localStorage` of its own that shadows jsdom's unless
// `--localstorage-file` is given, so on a Mac the persistence quietly no-ops and
// the leak is invisible. Tests that care about storage pass their own
// (`test/storage.ts`); this is for the ones that never think about it.
afterEach(() => {
  try {
    globalThis.localStorage?.clear()
  } catch {
    // A context genuinely without one. Nothing was persisted either.
  }
})
afterAll(() => server.close())
