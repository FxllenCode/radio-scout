/**
 * **Access codes**, from the browser's side (#68, spec US 52).
 *
 * The server gates *channels*; this is the whole of what the client has to know
 * about it. Three things, and they are deliberately small:
 *
 * - the **grant** an unlock handed back, remembered per browser,
 * - how a grant reaches the server — on the **query string**, which is the only
 *   part of a URL an `<audio src>`, a `WebSocket` and a `fetch` can all carry,
 *   and the part `http_log` never writes down (ADR-0008), and
 * - what to do when the grant stops working, which is the case the whole design
 *   turns on: every read **degrades to open listening** rather than refusing, so
 *   the app keeps working and the catalog is what says why.
 *
 * Nothing here decides *what* is locked — that is the server's, and the client
 * reads it off the catalog as [`CatalogTalkgroup.locked`]. A client that decided
 * for itself would be a second implementation of the gate, and the wrong one.
 */
import { readStored, writeStored } from './persist'

import type { Catalog } from '@/types'

/** The query parameter a grant rides in — `crate::access::GRANT_PARAM`. */
export const GRANT_PARAM = 'grant'

/** Where this browser's grant is kept.
 *
 *  **Not namespaced by `?id=`**, unlike a Selection: two scanners in one browser
 *  are two arrangements of the same listener's access, and making somebody
 *  unlock twice because they opened a second tab would be a lock in the wrong
 *  place. It is also deliberately `localStorage` rather than a cookie — the
 *  unlock is the Listener's, and clearing site data is how they take it back. */
const GRANT_KEY = 'radio-scout:access:grant'

/** A grant looks like this or it is not one. Checked because a hand-edited or
 *  half-written value would otherwise be sent up on every request forever, and
 *  answered with a `stale` the app would then act on. */
const GRANT_SHAPE = /^rsg_[0-9a-f]{32}$/

/** Whether `value` is something this Instance could have minted. */
export function isGrant(value: unknown): value is string {
  return typeof value === 'string' && GRANT_SHAPE.test(value)
}

/** The grant this browser is holding, or `undefined`. */
export function loadGrant(storage: Storage | undefined): string | undefined {
  const stored = readStored(storage, GRANT_KEY)
  return isGrant(stored) ? stored : undefined
}

/** Remember a grant. */
export function saveGrant(storage: Storage | undefined, grant: string): void {
  writeStored(storage, GRANT_KEY, grant)
}

/** Forget it — what a **stale** grant earns, and what "lock again" does. */
export function clearGrant(storage: Storage | undefined): void {
  try {
    storage?.removeItem(GRANT_KEY)
  } catch {
    // Storage denied (private mode, blocked site data), same as `writeStored`.
  }
}

/**
 * `url` with the grant on it, or `url` unchanged when there is none.
 *
 * **One function, every destination.** A `fetch`, an `<audio src>`, a download
 * link and a `WebSocket` URL are four things a component builds and one thing
 * the server reads, so the appending happens here rather than four times — and
 * a caller that forgets is a caller whose request quietly answers with the open
 * channels only, which looks exactly like a channel having gone quiet.
 *
 * Written by hand rather than through `URL`, because these are **relative**
 * paths (`/api/call/3/audio`) and `new URL` needs a base; handing it
 * `location.origin` would work in a browser and throw in a worker.
 */
export function withGrant(url: string, grant?: string): string {
  if (!grant) return url
  const joiner = url.includes('?') ? '&' : '?'
  return `${url}${joiner}${GRANT_PARAM}=${encodeURIComponent(grant)}`
}

/** Why the grant this browser sent is not being honoured. The server's own
 *  vocabulary, so there is nothing to translate. */
export type Stale = 'unknown' | 'expired'

/** Where this browser stands, read off the catalog. */
export interface Access {
  /** Whether this Instance restricts anything at all. `false` means there is no
   *  unlock control to draw — which is every Instance until an Operator marks a
   *  channel. */
  gating: boolean
  /** The label of the code being held, when one is live. */
  label?: string
  /** When it runs out, if it does. */
  expiresAtMs?: number
  /** Set when the grant that was sent is not a live code — the browser should
   *  stop sending it and say so. */
  stale?: Stale
}

/** What the catalog says about access, defaulted for an Instance that gates
 *  nothing and therefore sends no `access` block at all. */
export function accessOf(catalog: Catalog | undefined): Access {
  return catalog?.access ?? { gating: false }
}

/** Whether the catalog is telling this browser to let go of what it holds.
 *
 *  The one place the degrade-rather-than-refuse decision is *acted* on: every
 *  read has already answered with the open channels, so nothing is broken — what
 *  is left is to stop sending a credential that does not work and to say so
 *  once. */
export function grantWentStale(access: Access): Stale | undefined {
  return access.stale
}

/** What to tell a Listener whose grant stopped working.
 *
 *  Two sentences rather than one, because they are two different things to go
 *  and do: an expired code is a new code from the same person, and an unknown
 *  one has been revoked — or was never this Instance's. */
export function staleNotice(stale: Stale): string {
  return stale === 'expired'
    ? 'Your access code has expired. Ask for a new one to hear those channels again.'
    : 'Your access code no longer works. It may have been changed or revoked.'
}

/** How many channels on this catalog this Listener cannot hear.
 *
 *  Drawn beside the unlock control, because "3 channels are locked" is the
 *  sentence that makes the control worth tapping — an Operator gated the
 *  channel, not the fact that it exists (which is why the rows are still
 *  listed at all). */
export function lockedCount(catalog: Catalog | undefined): number {
  return (catalog?.systems ?? []).reduce(
    (count, system) =>
      count + system.talkgroups.filter((talkgroup) => talkgroup.locked).length,
    0,
  )
}

/** What the unlock sheet opens with — the sentence that says why it is worth
 *  filling in.
 *
 *  A value rather than a ternary in the markup, for the reason every other
 *  sentence in `lib/` is one: a plural that reads "1 channels" is a bug nobody
 *  writes a component test for, and this is a table row. */
export function lockedSentence(locked: number): string {
  return locked === 1
    ? 'One channel on this scanner is restricted.'
    : `${locked} channels on this scanner are restricted.`
}

/** A refusal from `POST /api/unlock`, as something to put under the input.
 *
 *  Keyed on the **status**, which is the whole of what distinguishes them here:
 *  the bodies are plain text a Listener should never be shown, and the three
 *  outcomes an unlock has are exactly three statuses. */
export function unlockFailure(status: number | string | undefined): string {
  if (status === 410) return 'That code has expired.'
  if (status === 429) return 'Too many tries. Wait a few minutes and try again.'
  if (status === 401) return "That code doesn't work here."
  // Anything else — a 500, `FETCH_ERROR`, a dropped connection. The honest
  // answer is that we could not check, which is not the same as saying the
  // code was wrong.
  return 'Could not check that code. Try again in a moment.'
}
