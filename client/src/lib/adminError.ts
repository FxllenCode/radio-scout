/** Turning a refused admin sign-in into words an operator can act on (#19,
 *  #30). */

/** Why the sign-in was refused.
 *
 *  rdio-scanner answers a locked-out address with the same 401 as a wrong
 *  password, so "I mistyped it" and "I am locked out for the next quarter of an
 *  hour" are indistinguishable from the browser. #19 made the server say which
 *  (429 + `Retry-After`); this is the half the operator reads. */
export function loginFailure(
  status: number | string | undefined,
  retryAfter?: string | null,
): string {
  switch (status) {
    case 401:
      return 'That password was not accepted.'
    case 429:
      return `Too many attempts. Try again in ${waitFor(retryAfter)}.`
    // What `fetchBaseQuery` reports when the request never got an answer.
    case 'FETCH_ERROR':
      return 'The server could not be reached.'
    default:
      return 'Signing in failed. Check the server log.'
  }
}

/** A `Retry-After` header as a human interval. A locked-out address is told
 *  roughly how long, not to the second: the cooldown runs from the *last*
 *  attempt, so a precise countdown would be a lie the moment they try again. */
function waitFor(retryAfter?: string | null): string {
  const seconds = Number(retryAfter)
  if (!Number.isFinite(seconds) || seconds <= 0) return 'a few minutes'
  if (seconds < 60) return `${Math.ceil(seconds)} seconds`
  const minutes = Math.ceil(seconds / 60)
  return minutes === 1 ? '1 minute' : `${minutes} minutes`
}

/** The status a `fetchBaseQuery` error really carries.
 *
 *  It parses every response as JSON, so a body that isn't — which ours
 *  deliberately aren't, being plain-text wire contracts — arrives as
 *  `PARSING_ERROR` with the real status tucked into `originalStatus`. Reading
 *  only `status` would turn every refusal into "something went wrong", which is
 *  precisely the confusion [`loginFailure`] exists to end. */
export function statusOf(error: unknown): number | string | undefined {
  if (typeof error !== 'object' || error === null) return undefined
  const { status, originalStatus } = error as {
    status?: number | string
    originalStatus?: number
  }
  return originalStatus ?? status
}

/** The message to show for whatever a failed sign-in produced.
 *
 *  `transformErrorResponse` turns every *response* into one of the strings
 *  above, but RTK Query's error type also admits a `SerializedError` — a throw
 *  rather than an answer — which has no status to read. That lands on the same
 *  fallback: an operator needs a sentence, not a discriminated union. */
export function signInMessage(error: unknown): string {
  return typeof error === 'string' ? error : loginFailure(undefined)
}
