/**
 * Getting a link out of the app (#61, spec US 30).
 *
 * Spec US 30 wants "any find is a text message", and an installed PWA has no
 * URL bar to copy one out of — which is exactly the browser this project cares
 * most about (ADR-0005). So every linkable thing carries a control, and this is
 * what that control does.
 *
 * The platform is a **parameter**, not something reached for: `navigator.share`
 * exists on a phone and not on a desktop, `navigator.clipboard` needs a secure
 * context, and both can refuse. Passing them in is what makes each of those a
 * row in a table rather than a browser a test has to pretend to be.
 */

/** As much of `navigator` as sharing a link is about. */
export interface Sharer {
  share?: (data: { title?: string; url: string }) => Promise<void>
  clipboard?: { writeText: (text: string) => Promise<void> }
}

/** How it went, which is what the control says afterwards. */
export type ShareOutcome = 'shared' | 'copied' | 'dismissed' | 'failed'

/** An absolute link to a view of this instance. Absolute because a path is not
 *  something a Listener can paste into a message. */
export function linkTo(path: string, query: string, origin: string): string {
  return `${origin}${path}${query ? `?${query}` : ''}`
}

/**
 * Put `url` where the Listener can send it — the share sheet if the platform
 * has one, the clipboard otherwise.
 *
 * Cancelling the sheet is `dismissed` and stops there: it is a decision, and
 * copying anyway would ignore it while an error message would blame the
 * platform for something the Listener did. Every *other* refusal falls through
 * to the clipboard, because a share sheet that will not open is a reason to
 * offer the other way rather than to give up.
 */
export async function shareLink(
  url: string,
  title: string,
  platform: Sharer,
): Promise<ShareOutcome> {
  if (platform.share) {
    try {
      await platform.share({ title, url })
      return 'shared'
    } catch (error) {
      if (isDismissal(error)) return 'dismissed'
    }
  }

  if (platform.clipboard) {
    try {
      await platform.clipboard.writeText(url)
      return 'copied'
    } catch {
      // Denied, or no secure context. Nothing left to try.
    }
  }
  return 'failed'
}

/** The Listener closed the sheet — `AbortError`, per the Web Share API. */
const isDismissal = (error: unknown): boolean =>
  error instanceof Error && error.name === 'AbortError'

/**
 * What to tell the Listener, or `null` for the two outcomes that speak for
 * themselves.
 *
 * A share sheet that opened is its own confirmation and one waved away is the
 * Listener's own decision, so both say nothing. The clipboard is the opposite
 * case in both directions: copying has no visible result at all, and failing to
 * copy leaves somebody waiting for a link that is not coming.
 */
export function shareNotice(outcome: ShareOutcome): string | null {
  switch (outcome) {
    case 'copied':
      return 'Link copied.'
    case 'failed':
      return 'Could not copy the link.'
    case 'shared':
    case 'dismissed':
      return null
  }
}
