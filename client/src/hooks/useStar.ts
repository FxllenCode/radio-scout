/**
 * Starring the Call in hand (#66, spec US 37).
 *
 * The rule lives here rather than in the button, because two surfaces reach it
 * and they do not look alike: a list row draws an icon among five other icons,
 * and the Live display draws a **Control** in the grid beside Hold and Avoid
 * (#58's argument — the moment a Listener decides a transmission mattered is
 * while they are hearing it). Two components, one rule about what a Star *is*
 * and what tapping it does.
 */
import { useCallback } from 'react'

import { useSetStarMutation } from '@/store/api'
import { useAppSelector } from '@/store/hooks'
import { selectStarred } from '@/store/stars'
import type { Call } from '@/types'

export interface StarState {
  /** Whether this Call is starred — what this session said, else the server. */
  starred: boolean
  /** What a tap would *do*, for the three surfaces that have to name it — and
   *  the reason it is here rather than spelled at each of them: "Star" and
   *  "Unstar" is the one word the row control, the display control and the
   *  action sheet genuinely share, where the rest of each label is that
   *  surface's own ("Star Fire Dispatch at 14:32" against "Star this call"). */
  verb: 'Star' | 'Unstar'
  /** Put the Star on, or take it off. */
  toggle: () => void
}

export function useStar(call: Call | null): StarState {
  const starred = useAppSelector((state) => (call ? selectStarred(state, call) : false))
  const [setStar] = useSetStarMutation()
  // `null` is the Live display before the first Call, with the feed off, and in
  // playback mode — a real and common state, which is why the control there is
  // *disabled* rather than absent and why this guard is a rule rather than a
  // defensive line: a control with nothing under it does nothing.
  const toggle = useCallback(() => {
    if (call) void setStar({ id: call.id, starred: !starred })
  }, [call, setStar, starred])
  return { starred, verb: starred ? 'Unstar' : 'Star', toggle }
}
