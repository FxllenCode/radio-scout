import '@testing-library/jest-dom'

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
