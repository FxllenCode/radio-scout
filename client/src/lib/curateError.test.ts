import { describe, expect, it } from 'vitest'

import { curateFailure, refusedForCalls } from './curateError'

describe('curateFailure', () => {
  /** A refusal is read back whole: the slug a client branches on, the sentence
   *  it shows, and the extra a particular refusal needs. The sentence comes
   *  from the server because that is the side that knows the LED palette and
   *  how many Calls a delete would have taken. */
  it('reads a refusal the server sent', () => {
    const failure = curateFailure({
      status: 409,
      data: {
        error: 'system-has-calls',
        detail: 'this system still has 12 calls in the archive',
        calls: 12,
      },
    })

    expect(failure).toEqual({
      error: 'system-has-calls',
      detail: 'this system still has 12 calls in the archive',
      field: undefined,
      calls: 12,
    })
  })

  /** ...including the field to mark, which is what puts the message under the
   *  input rather than at the top of the form. */
  it('carries the field a validation refusal named', () => {
    const failure = curateFailure({
      status: 400,
      data: { error: 'field-required', detail: 'name is required', field: 'name' },
    })

    expect(failure.field).toBe('name')
  })

  /** A 5xx body is deliberately nothing but a request id (#29), and a dropped
   *  connection has no body at all — neither is a refusal, and both still owe
   *  the Operator a sentence. */
  it.each([
    [{ status: 'FETCH_ERROR', error: 'boom' }, 'The server could not be reached.'],
    [{ status: 500, data: 'internal error (request id: abc)' }, /could not be saved/],
    [{ status: 401, data: 'admin session required\n' }, /session has expired/],
    [{ status: 403, data: 'csrf token required\n' }, /session has expired/],
    [{ originalStatus: 401 }, /session has expired/],
    [undefined, /could not be saved/],
    [null, /could not be saved/],
    ['a bare string', /could not be saved/],
    [{ status: 400, data: ['not', 'an', 'object'] }, /could not be saved/],
    [{ status: 400, data: { error: 'x' } }, /could not be saved/],
    [{ status: 400, data: { error: 'x', detail: '' } }, /could not be saved/],
  ])('falls back for %o', (error, expected) => {
    expect(curateFailure(error).detail).toMatch(expected)
  })

  /** A refusal whose `error` is not a string still yields a usable sentence —
   *  the detail is what the Operator reads, and the slug is only for branching. */
  it('keeps the sentence when the slug is not a string', () => {
    const failure = curateFailure({ status: 400, data: { error: 7, detail: 'nope' } })

    expect(failure).toEqual({
      error: '',
      detail: 'nope',
      field: undefined,
      calls: undefined,
    })
  })
})

describe('refusedForCalls', () => {
  /** A delete refused for Calls is the one refusal with an answer, so it is the
   *  only one that may offer to force. */
  it.each([
    ['system-has-calls', true],
    ['talkgroup-has-calls', true],
    // A collision is also a 409, and forcing would do nothing about it — an
    // offer to force here would be a button that lies.
    ['name-taken', false],
    ['system-ref-taken', false],
    ['field-required', false],
    ['', false],
  ])('%s -> %s', (error, expected) => {
    expect(refusedForCalls({ error, detail: 'x' })).toBe(expected)
  })
})
