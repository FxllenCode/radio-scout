/**
 * The **Access code** grant this browser holds (#68, spec US 52).
 *
 * A hook rather than four `useAppSelector(selectGrant)` calls, for the reason
 * `useStar` is one: the places that need it are a `<audio src>`, three download
 * links and an export link, and what they all want is the same one-line answer.
 * Every *fetch* gets it for nothing from the base query (`store/api`); this is
 * for the URLs a browser navigates to or plays, which never go through it.
 *
 * ## Design notes (moved verbatim from CLAUDE.md, #110)
 *
 * And **the client attaches the grant in one place per mechanism, never per call site**: the base query rewrites every `fetch`'s URL, `useGrant` serves the four URLs a browser *navigates to or plays* (an `<audio src>`, three download links, an export), and `connectLiveFeed` takes it as an option — with the socket re-opened when it changes, because the server resolves a connection's scope once. The one ordering bug worth remembering was found by a test: `invalidatesTags` fires on the fulfilled action, *before* any caller could store what came back, so the refetch went out without the grant and answered with exactly the channels the Listener had just unlocked being absent. Holding it first and invalidating by hand is the fix, and doing both inside the endpoint is what keeps a second caller from having to know.
 */
import { selectGrant } from '@/store/access'
import { useAppSelector } from '@/store/hooks'

/** What to put on a URL, or `undefined` — which is every Listener on an
 *  Instance that gates nothing. */
export function useGrant(): string | undefined {
  return useAppSelector(selectGrant)
}
