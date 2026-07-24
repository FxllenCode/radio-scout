import '@testing-library/jest-dom'
import { afterAll, afterEach, beforeAll, expect } from 'vitest'
import { setupServer } from 'msw/node'
import * as axeMatchers from 'vitest-axe/matchers'

import { handlers } from './handlers'

// In jsdom the global `Request`/`fetch` are Node's (undici), which reject
// relative URLs — but a browser resolves them against the document origin, and
// our same-origin RTK Query calls (baseUrl '/') are relative. Resolve relative
// request URLs against a test origin so those calls parse like they do in a
// real browser.
const BaseRequest = globalThis.Request
class RelativeAwareRequest extends BaseRequest {
  constructor(input: RequestInfo | URL, init?: RequestInit) {
    if (typeof input === 'string' && input.startsWith('/')) {
      input = `http://localhost${input}`
    }
    super(input, init)
  }
}
globalThis.Request = RelativeAwareRequest as typeof Request

// a11y assertions (`toHaveNoViolations`).
expect.extend(axeMatchers)

// MSW: intercept at the network boundary. Unhandled requests are an error so a
// test that hits an unexpected endpoint fails loudly instead of silently.
export const server = setupServer(...handlers)
beforeAll(() => server.listen({ onUnhandledRequest: 'error' }))
afterEach(() => server.resetHandlers())
afterAll(() => server.close())
