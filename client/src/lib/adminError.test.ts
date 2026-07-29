import { describe, expect, it } from 'vitest'

import { loginFailure, signInMessage, statusOf } from './adminError'

describe('loginFailure', () => {
  it('tells a wrong password apart from a lockout', () => {
    expect(loginFailure(401)).toMatch(/not accepted/i)
    expect(loginFailure(429, '900')).toMatch(/too many attempts/i)
  })

  it.each([
    ['900', '15 minutes'],
    ['60', '1 minute'],
    ['61', '2 minutes'],
    ['45', '45 seconds'],
    // A header the server did not send, or sent as nonsense: say roughly, not
    // wrongly.
    [undefined, 'a few minutes'],
    [null, 'a few minutes'],
    ['soon', 'a few minutes'],
    ['0', 'a few minutes'],
    ['-1', 'a few minutes'],
  ])('reads Retry-After %s as %s', (header, expected) => {
    expect(loginFailure(429, header)).toContain(expected)
  })

  it('says when the server never answered at all', () => {
    expect(loginFailure('FETCH_ERROR')).toMatch(/could not be reached/i)
  })

  it('falls back rather than showing a status code', () => {
    for (const status of [500, 418, 'PARSING_ERROR', undefined]) {
      expect(loginFailure(status)).toMatch(/signing in failed/i)
    }
  })
})

describe('signInMessage', () => {
  it('shows the message the response was turned into', () => {
    expect(signInMessage('That password was not accepted.')).toBe(
      'That password was not accepted.',
    )
  })

  it('falls back for a throw, which has no status to read', () => {
    for (const error of [undefined, { name: 'TypeError' }, { status: 401 }]) {
      expect(signInMessage(error)).toMatch(/signing in failed/i)
    }
  })
})

describe('statusOf', () => {
  // Our error bodies are plain text on purpose (they are wire contracts), so
  // this is the *normal* shape of a refusal, not an edge case.
  it('reads through a PARSING_ERROR to the status that was really returned', () => {
    expect(
      statusOf({ status: 'PARSING_ERROR', originalStatus: 429, data: 'nope\n' }),
    ).toBe(429)
  })

  it('reads a plain status when there is one', () => {
    expect(statusOf({ status: 401 })).toBe(401)
    expect(statusOf({ status: 'FETCH_ERROR' })).toBe('FETCH_ERROR')
  })

  it('has nothing to read from a throw', () => {
    for (const error of [undefined, null, 'boom', { name: 'TypeError' }]) {
      expect(statusOf(error)).toBeUndefined()
    }
  })
})
