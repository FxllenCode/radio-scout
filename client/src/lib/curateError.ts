/** Turning a refused curation write into something an Operator can act on
 *  (#49).
 *
 *  The server answers every one of these as JSON — a stable `error` slug, a
 *  `detail` sentence, and the extra a particular refusal needs (`field`,
 *  `calls`). This reads that document back, and falls back to a sentence of its
 *  own when the failure was never a refusal at all: a dropped connection, or a
 *  5xx whose body is deliberately nothing but a request id (#29).
 *
 *  Why the sentence comes from the *server*: it is the side that knows the LED
 *  palette, how many Calls a delete would take, and which of three collisions
 *  happened. A client re-deriving those would be a second copy of the rules,
 *  drifting from the first. */

/** What a refused write turned out to be. */
export interface CurateFailure {
  /** The stable slug — `name-taken`, `talkgroup-has-calls`. Empty when the
   *  request never reached a refusal. */
  error: string
  /** The sentence to show. Always non-empty. */
  detail: string
  /** The input to mark, when the refusal named one. */
  field?: string
  /** How many Calls a refused delete would have taken. */
  calls?: number
}

/** Read an RTK Query error as a curation refusal. */
export function curateFailure(error: unknown): CurateFailure {
  const body = bodyOf(error)
  if (body && typeof body.detail === 'string' && body.detail !== '') {
    return {
      error: typeof body.error === 'string' ? body.error : '',
      detail: body.detail,
      field: typeof body.field === 'string' ? body.field : undefined,
      calls: typeof body.calls === 'number' ? body.calls : undefined,
    }
  }
  return { error: '', detail: fallback(error) }
}

/** Whether this failure is the one an Operator can answer by asking again with
 *  `force` — a delete refused because Calls are still behind it.
 *
 *  Matched on the slug rather than on the status, because 409 is also what a
 *  name or Ref collision answers with, and offering to force *those* would
 *  offer a button that does nothing. */
export function refusedForCalls(failure: CurateFailure): boolean {
  return failure.error.endsWith('-has-calls')
}

/** The JSON body of a refusal, if there was one.
 *
 *  `fetchBaseQuery` parses a JSON response into `data`; a non-JSON body — which
 *  the session guard's own refusals are, being the plain-text wire forms every
 *  other surface uses — arrives as a string instead, and falls through to the
 *  sentence below. */
function bodyOf(error: unknown): Record<string, unknown> | undefined {
  if (typeof error !== 'object' || error === null) return undefined
  const { data } = error as { data?: unknown }
  if (typeof data !== 'object' || data === null || Array.isArray(data))
    return undefined
  return data as Record<string, unknown>
}

/** What to say when there is no refusal to read. */
function fallback(error: unknown): string {
  if (typeof error !== 'object' || error === null) return SOMETHING_WENT_WRONG
  const { status, originalStatus } = error as {
    status?: number | string
    originalStatus?: number
  }
  const code = originalStatus ?? status
  if (code === 'FETCH_ERROR') return 'The server could not be reached.'
  // The session guard, reached because it lapsed while the form was open. Worth
  // its own sentence: "something went wrong" would send an Operator looking at
  // the value they typed rather than at the sign-in they need.
  if (code === 401 || code === 403)
    return 'Your admin session has expired. Sign in again.'
  return SOMETHING_WENT_WRONG
}

const SOMETHING_WENT_WRONG =
  'That change could not be saved. Check the server log.'
