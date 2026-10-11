import { useEffect, useState } from 'react'

/**
 * The clock, for a screen that draws something measured from it.
 *
 * It ticks for the *display* alone — nothing that decides what plays reads it
 * (an **Avoid**'s deadline is the store's own clock, `store/avoids`). One timer
 * per screen rather than one per row: a county panel has four hundred rows,
 * and a last-heard age on each would otherwise be four hundred intervals. It
 * runs only while `ticking`, because a parked phone must not redraw itself for
 * nothing.
 *
 * Read once on arming as well as on each tick, because a clock started an hour
 * after mount would otherwise begin by drawing every age an hour stale.
 */
export function useNow(ticking: boolean, everyMs: number): number {
  const [now, setNow] = useState(() => Date.now())

  useEffect(() => {
    if (!ticking) return
    setNow(Date.now())
    const tick = setInterval(() => setNow(Date.now()), everyMs)
    return () => clearInterval(tick)
  }, [ticking, everyMs])

  return now
}
