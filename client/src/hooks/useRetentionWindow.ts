/**
 * Editing how long a System's or a channel's Calls are kept (#69, spec US 53).
 *
 * The rule lives here rather than in each form, because two forms reach it and
 * hold the identical three pieces of state: which of the three things the
 * setting is saying, the number it says when it is saying a number, and what
 * that pair means on the wire. Two copies is two chances for one of them to
 * spell *forever* differently — [`useStar`]'s argument, one setting along.
 *
 * What a form still owns is the one thing it cannot share: the disabled Save
 * button, because only the form has a button.
 */
import { useState } from 'react'

import { keepMode, keepWindow, type KeepMode } from '@/lib/curate'

export interface RetentionWindow {
  /** Spread straight onto [`RetentionField`] — the control owns its own
   *  refusal sentence, so a form that spreads these owes nothing else. */
  field: {
    mode: KeepMode
    days: string
    onMode: (mode: KeepMode) => void
    onDays: (days: string) => void
  }
  /** What to send, or `undefined` when the box does not hold a window — which
   *  is *refuse to submit*, never "leave it alone". */
  value: number | null | undefined
}

/** Open the control on what the row already says. */
export function useRetentionWindow(
  stored: number | null | undefined,
): RetentionWindow {
  const [mode, onMode] = useState(keepMode(stored))
  // Spelled against the mode rather than against the number's truthiness: `0` is
  // *forever*, so it is a mode and never a number in this box — and reading it
  // as an empty box because zero is falsy would be right by coincidence.
  const [days, onDays] = useState(
    keepMode(stored) === 'days' ? String(stored) : '',
  )

  return { field: { mode, days, onMode, onDays }, value: keepWindow(mode, days) }
}
