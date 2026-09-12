/**
 * Telling a Listener their **Access code** stopped working (#68, spec US 52).
 *
 * The server's answer to a grant it no longer honours is to **degrade**: every
 * read answers with the open channels, nothing is refused, and the app carries
 * on. That is the right behaviour and it has one cost — done silently, a
 * Listener would simply find some channels missing and no reason given. This is
 * the reason, said once.
 *
 * It lives in the shell beside the undo bars, for the reason they do (#58, #61):
 * the thing that changed is *what can be heard*, which is not a fact about the
 * screen anybody happens to be on.
 *
 * What *causes* the notice is `hooks/useStaleGrantWatch`, which is where it
 * belongs: reading the catalog and letting go of the grant is a dispatch rather
 * than a render, and keeping it out of here leaves a component that is usually
 * drawing nothing drawing nothing.
 */
import { KeyRound } from 'lucide-react'

import { DockedBanner } from '@/components/layout/DockedBanner'
import { staleNotice } from '@/lib/access'
import { selectStaleGrant, staleNoticeDismissed } from '@/store/access'
import { useAppDispatch, useAppSelector } from '@/store/hooks'

import { Button } from './ui/button'

/** The sentence itself, drawn only while there is one to say. */
export function AccessNoticeBar() {
  const dispatch = useAppDispatch()
  const stale = useAppSelector(selectStaleGrant)
  if (!stale) return null

  return (
    <DockedBanner>
      <KeyRound className="size-4 shrink-0 text-muted-foreground" aria-hidden />
      <p role="status" className="flex-1 font-mono text-xs leading-snug">
        {staleNotice(stale)}
      </p>
      <Button
        variant="outline"
        size="sm"
        className="h-7 shrink-0 px-2 font-mono text-[10px] uppercase tracking-wider"
        onClick={() => dispatch(staleNoticeDismissed())}
      >
        OK
      </Button>
    </DockedBanner>
  )
}
