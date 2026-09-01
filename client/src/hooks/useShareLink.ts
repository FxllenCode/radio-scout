/**
 * Handing a link to the Listener, wherever they are (#61, spec US 30).
 *
 * Three controls mint links — this search, one Call, the **Selection** — on two
 * screens, and every one of them owes the same three things: reach the platform
 * (`lib/share`), say what happened, and take the saying back down again. A
 * notice is a thing that *just happened*, not a thing that is true, so one left
 * standing becomes furniture under the filters for the rest of the session.
 *
 * One hook rather than that shape written twice, because it already had been:
 * the Talkgroups copy carried the notice and not the timer, so "Link copied."
 * sat under its panel bar forever — a surface's *shape* may differ, the policy
 * underneath may not (#92's rule, one layer up).
 */
import { useEffect, useState } from 'react'

import { linkTo, shareLink, shareNotice } from '@/lib/share'

/** How long a link control's confirmation stands. Long enough to read on a
 *  phone, short enough that "Link copied" is not still on screen by the time a
 *  Listener has pasted it. */
export const NOTICE_MS = 3_000

export interface ShareLink {
  /** What the last link control did, or `null` — the two outcomes that speak
   *  for themselves say nothing ([`shareNotice`]). */
  notice: string | null
  /** Offer a link to `path?query`, named `title` where a share sheet asks. */
  share: (path: string, query: string, title: string) => Promise<void>
  /** Say something else happened — the one failure that is not a share's. */
  say: (notice: string) => void
}

export function useShareLink(): ShareLink {
  const [notice, setNotice] = useState<string | null>(null)

  useEffect(() => {
    if (!notice) return
    const timer = setTimeout(() => setNotice(null), NOTICE_MS)
    return () => clearTimeout(timer)
  }, [notice])

  return {
    notice,
    share: async (path, query, title) => {
      setNotice(
        shareNotice(
          await shareLink(linkTo(path, query, window.location.origin), title, navigator),
        ),
      )
    },
    say: setNotice,
  }
}
