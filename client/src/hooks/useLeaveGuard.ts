import { useEffect, useRef } from 'react'

/**
 * Ask before the tab closes while something is listening (#78) — rdio's
 * "Live Feed is ON, do you really want to leave?".
 *
 * A scanner left running is one somebody means to keep listening to, and an
 * accidental Cmd-W on a quiet channel would otherwise end the night's
 * listening without a word — the Listener finds out an hour later, by noticing
 * the silence. Browsers decide what the question says and show it only to a
 * page somebody has touched, which a Listener who started audio always has.
 *
 * Returns the way to stand aside for a leaving the app itself chose — the
 * reload that takes an update — so the guard does not argue with the button
 * that was just pressed.
 */
export function useLeaveGuard(armed: boolean): () => void {
  const standingAside = useRef(false)

  useEffect(() => {
    if (!armed) return
    const hold = (event: BeforeUnloadEvent) => {
      if (standingAside.current) return
      event.preventDefault()
      // The legacy half of the same request, which some browsers still need.
      event.returnValue = ''
    }
    window.addEventListener('beforeunload', hold)
    return () => window.removeEventListener('beforeunload', hold)
  }, [armed])

  return () => {
    standingAside.current = true
  }
}
