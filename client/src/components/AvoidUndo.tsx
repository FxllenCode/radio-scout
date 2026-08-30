/**
 * The undo offer after an **Avoid** (#58, spec US 25).
 *
 * Avoid is the one control on the Live screen whose effect is *silence*, so a
 * mis-tap on it looks exactly like a channel that went quiet. Everything else
 * announces itself: a hold changes the header, a skip plays something else.
 * This is the announcement, and it carries the way back.
 *
 * # Where it lives, and why that is the shell
 *
 * In the docked column, beside the mini-player — not on the Live screen — for
 * the same reason #56 moved the transport readout there. The offer has a
 * deadline, and a Listener who avoids a channel and then switches to Talkgroups
 * to see what else is on has spent none of it. A bar drawn by the Live screen
 * would unmount on the way out and reappear on the way back, its window either
 * restarted or silently over.
 *
 * The window itself is the store's (`AVOID_UNDO_MS`), and the deadline is a
 * *moment* held in the offer, so this component is a timer and holds no policy:
 * on a remount it waits out what is left rather than starting again.
 */
import { Undo2 } from 'lucide-react'
import { useEffect } from 'react'

import { DockedBanner } from '@/components/layout/DockedBanner'
import { nameOf } from '@/lib/avoiding'
import { useGetCatalogQuery } from '@/store/api'
import { useAppDispatch, useAppSelector } from '@/store/hooks'
import { dismissAvoidUndo, selectAvoidUndo, undoAvoid, type AvoidUndo } from '@/store/live'

export function AvoidUndoBar() {
  const undo = useAppSelector(selectAvoidUndo)
  // The catalog query lives inside `Offer` so it is only ever issued while an
  // offer stands — this bar is mounted on every screen and absent almost all
  // of the time, and an app-wide fetch for a name nobody is reading is exactly
  // the kind of cost a phone pays for.
  return undo ? <Offer undo={undo} /> : null
}

function Offer({ undo }: { undo: AvoidUndo }) {
  const dispatch = useAppDispatch()
  const { data: catalog } = useGetCatalogQuery()
  // Named the way the Avoid sheet names them, so the sentence here and the list
  // there call the same channel by the same thing. Deliberately *not* through
  // `avoidedRows`: that would mean inventing a deadline to get a label back,
  // and this offer's clock is a different number from any Avoid's.
  const talkgroup = nameOf(undo.key, catalog)

  useEffect(() => {
    const left = Math.max(0, undo.expiresAt - Date.now())
    const timer = setTimeout(() => dispatch(dismissAvoidUndo()), left)
    return () => clearTimeout(timer)
  }, [dispatch, undo])

  return (
    <DockedBanner label="Avoid undo">
      <p className="min-w-0 flex-1 truncate font-mono text-xs">
        Avoiding <span className="font-semibold">{talkgroup.label}</span>
      </p>
      <button
        type="button"
        onClick={() => dispatch(undoAvoid())}
        className="flex shrink-0 items-center gap-1.5 rounded-lg border border-border px-3 py-1.5 font-mono text-[11px] font-semibold uppercase tracking-wider transition-colors hover:bg-muted/40"
      >
        <Undo2 className="size-3.5" aria-hidden />
        Undo
      </button>
    </DockedBanner>
  )
}
