/**
 * The **Access code** grant this browser holds (#68, spec US 52).
 *
 * A hook rather than four `useAppSelector(selectGrant)` calls, for the reason
 * `useStar` is one: the places that need it are a `<audio src>`, three download
 * links and an export link, and what they all want is the same one-line answer.
 * Every *fetch* gets it for nothing from the base query (`store/api`); this is
 * for the URLs a browser navigates to or plays, which never go through it.
 */
import { selectGrant } from '@/store/access'
import { useAppSelector } from '@/store/hooks'

/** What to put on a URL, or `undefined` — which is every Listener on an
 *  Instance that gates nothing. */
export function useGrant(): string | undefined {
  return useAppSelector(selectGrant)
}
