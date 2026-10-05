/**
 * Editing how long a System's or a channel's Calls wait before they are
 * published — the **Delay** (#73, spec US 62).
 *
 * [`useRetentionWindow`]'s shape and for its reason: two forms hold the
 * identical state — which thing the setting is saying, the number it says, and
 * what that pair means on the wire — and two copies is two chances for one of
 * them to send *none* as a different value. The form still owns its disabled
 * Save button, because only the form has one.
 *
 * **Whether there is a level above to follow is said once, here** — as the
 * option's label, or its absence — and the control is handed both that and
 * whether the box holds a Delay, so the option it draws and the refusal it
 * shows cannot disagree with the value the form sends.
 */
import { useState } from 'react'

import { delayMode, delayValue, type DelayMode } from '@/lib/curate'

export interface DelayForm {
  /** Spread straight onto [`DelayField`]. */
  field: {
    mode: DelayMode
    minutes: string
    onMode: (mode: DelayMode) => void
    onMinutes: (minutes: string) => void
    /** What the *follow* option says, or absent where there is nothing above
     *  to follow — a System. */
    inheritsFrom?: string
    /** The box holds no Delay, so the form must not submit. */
    refused: boolean
  }
  /** What to send, or `undefined` when the box does not hold a Delay — *refuse
   *  to submit*, never "leave it alone". */
  value: number | null | undefined
}

/** Open the control on what the row already says. `inheritsFrom` is the label
 *  of the *follow* option on an entity that has a level above it — a
 *  Talkgroup's System — and absent on one that has none. */
export function useDelay(
  stored: number | null | undefined,
  inheritsFrom?: string,
): DelayForm {
  const inherits = inheritsFrom !== undefined
  const opened = delayMode(stored, inherits)
  const [mode, onMode] = useState(opened)
  const [minutes, onMinutes] = useState(
    opened === 'minutes' ? String(stored) : '',
  )
  const value = delayValue(mode, minutes, inherits)
  return {
    field: {
      mode,
      minutes,
      onMode,
      onMinutes,
      inheritsFrom,
      refused: value === undefined,
    },
    value,
  }
}
