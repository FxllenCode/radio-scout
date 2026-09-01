/**
 * The way back from a **Selection** that arrived in a link (#61, spec US 30).
 *
 * Opening a link replaces what the Listener is listening to — which on a county
 * system may be six channels picked out of four hundred — and they did not
 * touch the panel to make it happen. That is the **Avoid** undo's case exactly
 * (#58), so it gets the same answer, in the same place, for the same reasons:
 * the offer has a deadline, and a Listener who opens a link and then goes to
 * look at what changed has spent none of it if the bar lives on the screen they
 * left.
 *
 * The deadline is a *moment* held in the store (`SELECTION_UNDO_MS`), so this
 * component is a timer and holds no policy: on a remount it waits out what is
 * left rather than starting again.
 */
import { Undo2 } from 'lucide-react'
import { useEffect } from 'react'

import { DockedBanner } from '@/components/layout/DockedBanner'
import { useAppDispatch, useAppSelector } from '@/store/hooks'
import {
  dismissSelectionUndo,
  selectSelectionUndo,
  undoSelectionLink,
  type SelectionUndo,
} from '@/store/live'

export function SelectionUndoBar() {
  const undo = useAppSelector(selectSelectionUndo)
  return undo ? <Offer undo={undo} /> : null
}

function Offer({ undo }: { undo: SelectionUndo }) {
  const dispatch = useAppDispatch()

  useEffect(() => {
    const left = Math.max(0, undo.expiresAt - Date.now())
    const timer = setTimeout(() => dispatch(dismissSelectionUndo()), left)
    return () => clearTimeout(timer)
  }, [dispatch, undo])

  return (
    <DockedBanner label="Selection undo">
      <p className="min-w-0 flex-1 truncate font-mono text-xs">
        Listening to a shared selection
      </p>
      <button
        type="button"
        onClick={() => dispatch(undoSelectionLink())}
        className="flex shrink-0 items-center gap-1.5 rounded-lg border border-border px-3 py-1.5 font-mono text-[11px] font-semibold uppercase tracking-wider transition-colors hover:bg-muted/40"
      >
        <Undo2 className="size-3.5" aria-hidden />
        Undo
      </button>
    </DockedBanner>
  )
}
