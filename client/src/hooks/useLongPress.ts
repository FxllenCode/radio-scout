/**
 * Tap and long-press on the same row, acting on the thing the finger went down
 * on (#58, its fifth acceptance criterion).
 *
 * # Why the subject is captured, and not read
 *
 * The two lists this serves both move under the thumb: the session log grows at
 * the top as Calls are heard, and the queue sheet re-orders whenever a
 * **Priority** Call arrives. So a press that read "which row is this?" when its
 * timer fired would open an *Avoid* menu over whichever Call had slid into that
 * place — a mis-tap that looks exactly like a deliberate one, on a control that
 * silences a channel.
 *
 * The subject is therefore taken at `pointerdown` and held until the press
 * resolves. React's own key-based reconciliation is the other half: a row keyed
 * by Call id keeps its DOM node, so the node the finger is on stays the node it
 * started on. This hook covers the half that keying cannot — the moment between
 * the finger going down and the timer firing, during which the component has
 * re-rendered and rebound every handler on the page.
 *
 * # Why a click is swallowed after a hold
 *
 * A long press ends with a `pointerup`, and the browser follows it with a
 * `click`. Without suppression, holding a row to reach *Avoid* would replay the
 * Call on the way there.
 */
import { useCallback, useEffect, useRef, type PointerEvent, type SyntheticEvent } from 'react'

/**
 * How long a press has to last to be a hold.
 *
 * The platform convention on both iOS and Android, which is the number a
 * Listener's thumb already knows — shorter turns a scroll-start into a menu,
 * longer reads as a control that did not respond.
 */
export const LONG_PRESS_MS = 500

/** What a row does when it is tapped, and when it is held. */
export interface LongPressActions<T> {
  onTap: (subject: T) => void
  onHold: (subject: T) => void
  /** Defaults to [`LONG_PRESS_MS`]. */
  delay?: number
}

/** The handlers a row spreads onto the element that carries `subject`. */
export interface LongPressHandlers {
  onPointerDown: () => void
  onPointerUp: () => void
  onPointerLeave: () => void
  onPointerCancel: (event: PointerEvent) => void
  onContextMenu: (event: SyntheticEvent) => void
  onClick: () => void
}

/**
 * Bind tap and hold to a row.
 *
 * Returns a function a row calls with what it is *about* —
 * `<button {...press(call)} />` — so the subject travels with the handlers
 * rather than being looked up later.
 */
export function useLongPress<T>({
  onTap,
  onHold,
  delay = LONG_PRESS_MS,
}: LongPressActions<T>): (subject: T) => LongPressHandlers {
  const timer = useRef<ReturnType<typeof setTimeout> | undefined>(undefined)
  /** A hold has fired and the `click` that follows its `pointerup` is not a
   *  tap. One flag rather than one per row: a finger presses one row at a
   *  time. */
  const fired = useRef(false)

  const cancel = useCallback(() => {
    if (timer.current !== undefined) clearTimeout(timer.current)
    timer.current = undefined
  }, [])

  // A row can be unmounted mid-press — the sheet closing, the Call playing —
  // and a timer that outlived it would fire into a component that is gone.
  useEffect(() => cancel, [cancel])

  return useCallback(
    (subject: T) => ({
      onPointerDown: () => {
        fired.current = false
        // A second press replaces the first: one gesture at a time, never two
        // timers racing to decide what the finger meant.
        cancel()
        timer.current = setTimeout(() => {
          timer.current = undefined
          fired.current = true
          onHold(subject)
        }, delay)
      },
      onPointerUp: cancel,
      onPointerLeave: cancel,
      onPointerCancel: cancel,
      // The platform's own long-press menu would cover ours, and on iOS it
      // starts a text selection over the row as well.
      onContextMenu: (event: SyntheticEvent) => event.preventDefault(),
      onClick: () => {
        if (fired.current) {
          fired.current = false
          return
        }
        onTap(subject)
      },
    }),
    [cancel, delay, onHold, onTap],
  )
}
