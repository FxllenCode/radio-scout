import { useState } from 'react'

import { useFoldMembersMutation, usePreviewFoldMutation } from '@/store/api'
import type { MemberDelta, MergeReport } from '@/types'

/**
 * **Preview, then commit** — the one road every fold takes (#50, spec US 17).
 *
 * A fold is the only curation act that rewrites the *archive* rather than the
 * configuration, so nothing is ever performed straight from a click: build a
 * delta, `?dryRun` it, render what came back, and only then send the identical
 * delta for real.
 *
 * A hook rather than three copies, because the row editor, the unfold button and
 * the bulk bar are the same flow against different deltas — and because "the
 * preview always precedes the write" is a promise about *all* of them. A second
 * copy is how one of them would come to skip it.
 */

/** A merge that has been previewed and is waiting on the Operator.
 *
 *  **`apply` lives here rather than beside `preview`**, so committing something
 *  that was never previewed is not expressible: there is no `apply` to call
 *  until a preview has come back and produced one of these. A guard on a
 *  top-level `apply()` would say the same thing and could be forgotten. */
export interface Pending {
  delta: MemberDelta
  report: MergeReport
  apply: () => void
}

/** Everything a merge flow needs, wherever it is rendered from. */
export interface Merge {
  pending?: Pending
  error: unknown
  preview: (delta: MemberDelta) => void
  cancel: () => void
}

export function useMerge(id: number, onApplied: () => void): Merge {
  const [previewFold, previewing] = usePreviewFoldMutation()
  const [foldMembers, folding] = useFoldMembersMutation()
  const [pending, setPending] = useState<
    { delta: MemberDelta; report: MergeReport } | undefined
  >(undefined)

  return {
    pending: pending && {
      ...pending,
      // **The identical delta**, not one rebuilt from the report: what was
      // shown is what is sent, which is the whole meaning of a confirmation.
      apply: () =>
        void foldMembers({ id, delta: pending.delta })
          .unwrap()
          .then(() => {
            setPending(undefined)
            onApplied()
          })
          // Rendered by `MergeConfirmation` off the mutation's own error. The
          // preview goes with it: whatever the server refused, what was on
          // screen is no longer a promise about the run that follows.
          .catch(() => setPending(undefined)),
    },
    error: previewing.error ?? folding.error,
    preview: (delta) =>
      void previewFold({ id, delta })
        .unwrap()
        .then((report) => setPending({ delta, report }))
        .catch(() => setPending(undefined)),
    cancel: () => setPending(undefined),
  }
}
