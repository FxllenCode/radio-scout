/**
 * Handing somebody a **Share link** to one Call (#64, spec US 32).
 *
 * Two steps that are one gesture: mint the link, then put it wherever this
 * platform puts links. A hook rather than lines in a route because the pair is
 * the unit — a screen that minted and then forgot to hand the result anywhere
 * would have created a public URL nobody was ever given — and because "what to
 * say when it fails" is a policy rather than a screen's business. One screen
 * offers it today; #67's Events share the same way, and so will the session
 * log.
 *
 * **The link's spelling is the server's**, never rebuilt here. What comes back
 * is a path with its query already on it, which is handed to `linkTo` as an
 * opaque path and made absolute against this browser's origin — the side that
 * actually knows which address the Listener reached this Instance on. Spelling
 * `/s?t=…` a second time in TypeScript is the divergence
 * `lib/selectionEncoding.json` exists to prevent, one feature along.
 */
import { useShareCallMutation } from '@/store/api'
import type { Call } from '@/types'

import { talkgroupName } from '@/lib/call'
import type { ShareControl } from '@/hooks/useShareLink'

export function usePublicShare(link: ShareControl): (call: Call) => Promise<void> {
  const [mint] = useShareCallMutation()

  return async (call) => {
    try {
      const minted = await mint(call.id).unwrap()
      await link.share(minted.url, '', talkgroupName(call))
    } catch {
      // The one failure worth a sentence: the Call is gone, or the Operator has
      // closed sharing since this page loaded. Either way nothing was copied,
      // and a Listener waiting to paste a link deserves to know.
      link.say('Could not create a share link.')
    }
  }
}
