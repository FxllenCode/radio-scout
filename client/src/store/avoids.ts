/**
 * The store's clock for **Avoid** (#91, spec US 14).
 *
 * An Avoid is a *deadline*, not a countdown: [`Avoids`] holds the moment each
 * one lapses. Every reader decides for itself whether one is still in force by
 * comparing that moment to the clock — which is what makes an Avoid survive a
 * reload, because a timestamp persists where a running timer cannot.
 *
 * Something still has to *act* when a moment passes, though, and that is what
 * this is. The Avoids a Listener is holding are subtracted from the
 * subscription matrix (ADR-0004), so the server stops sending the Talkgroup
 * altogether — which means the Talkgroup's own traffic can never be what
 * notices its Avoid is up. Without this, an Avoid on the busiest Talkgroup on a
 * quiet system would simply never come back, and coming back on its own is the
 * whole of what spec US 14 promises.
 *
 * # Improving on what it replaced
 *
 * This used to be a five-second `setInterval` in the live-feed socket
 * component — a poll that ran whether or not any Avoid existed, on a
 * Pi-class phone, and that was up to five seconds late when one did. Here
 * there is **one timer**, scheduled for the earliest deadline in the map and
 * rescheduled whenever the map changes, so:
 *
 * - nothing runs at all while no timed Avoid is in force;
 * - reactivation is exact rather than up to a sweep late;
 * - it belongs to the *store*, so it does not depend on any component being
 *   mounted, and it is provable without rendering anything.
 *
 * A background tab can still have its timers throttled by the browser, so the
 * deadline is compared lazily as well — see `received` in `./live`.
 */
import { createListenerMiddleware } from '@reduxjs/toolkit'

import type { Avoids } from '@/lib/selection'

import { expireAvoids, type LiveState } from './live'

/** The next moment worth waking for: the earliest deadline in force, or
 *  `undefined` when every Avoid is indefinite and there is nothing to wait
 *  for. `0` is "until the listener says otherwise" and is never a deadline. */
export function nextDeadline(avoided: Avoids): number | undefined {
  const deadlines = Object.values(avoided).filter((until) => until !== 0)
  return deadlines.length > 0 ? Math.min(...deadlines) : undefined
}

/** The slice this clock watches. */
interface WithLive {
  live: LiveState
}

/**
 * A clock that wakes once per deadline to let the Avoids that reached theirs
 * lapse.
 *
 * One per store, because a listener middleware holds the running effects it can
 * cancel: shared, two stores in one process would cancel each other's wait.
 * `makeStore` calls this, and nothing else needs to.
 *
 * It is woken by the Avoid map changing, and by `expireAvoids` itself — the
 * second is what lets a store that was *hydrated* holding an Avoid schedule for
 * it, since hydrated state arrives without an action to notice. The effect
 * cancels the run before it, so the map changing mid-wait reschedules rather
 * than accumulating timers, and it dispatches `expireAvoids(deadline)` rather
 * than `expireAvoids(Date.now())`: the Avoid it waited for then lapses even if
 * the timer fired a moment early, which is what stops the reschedule from being
 * able to spin.
 */
export function createAvoidClock() {
  const clock = createListenerMiddleware<WithLive>()

  clock.startListening({
    predicate: (action, current, previous) =>
      expireAvoids.match(action) || current.live.avoided !== previous.live.avoided,

    effect: async (_action, api) => {
      api.cancelActiveListeners()
      const deadline = nextDeadline(api.getState().live.avoided)
      if (deadline === undefined) return

      await api.delay(Math.max(0, deadline - Date.now()))
      api.dispatch(expireAvoids(deadline))
    },
  })

  return clock
}
