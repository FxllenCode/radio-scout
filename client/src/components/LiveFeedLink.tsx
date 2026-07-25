import { useEffect, useRef } from 'react'

import {
  connectLiveFeed,
  type LiveFeedHandle,
  type LiveStatus,
  type Subscription,
} from '@/lib/liveFeed'
import { useAppDispatch, useAppSelector, useAppStore } from '@/store/hooks'
import {
  connected,
  connecting,
  disconnected,
  expireAvoids,
  lagged,
  received,
  selectSince,
} from '@/store/live'
import { selectSubscription } from '@/store/transport'

/** How often lapsed avoids are swept up. Avoids are timed in tens of minutes
 *  (spec US 14), so this only has to be finer than a listener would notice. */
const AVOID_SWEEP_MS = 5_000

const STATUS_ACTION: Record<LiveStatus, () => { type: string }> = {
  connecting,
  connected,
  offline: disconnected,
}

/**
 * Holds the live-feed socket open and keeps the server's copy of the
 * subscription matrix in step with the listener's (#11).
 *
 * It renders nothing and lives in the shell, above the router, for the same
 * reason the `<audio>` element does: moving between tabs must not drop the feed
 * or the queue behind it.
 */
export function LiveFeedLink() {
  const dispatch = useAppDispatch()
  const store = useAppStore()
  const feed = useRef<LiveFeedHandle>(null)

  // Serialized, so this re-runs when the *matrix* changes rather than on every
  // render that rebuilds an equal one.
  const matrix = useAppSelector((state) =>
    JSON.stringify(selectSubscription(state)),
  )

  useEffect(() => {
    const handle = connectLiveFeed({
      onStatus: (status) => dispatch(STATUS_ACTION[status]()),
      onCall: (call, catchup) => dispatch(received({ call, catchup })),
      onLagged: (skipped) => dispatch(lagged(skipped)),
      // Read at send time, not subscribe time: the cursor moves with every Call
      // and only matters when the socket comes back (ADR-0004).
      since: () => selectSince(store.getState()),
    })
    feed.current = handle
    return () => handle.close()
  }, [dispatch, store])

  useEffect(() => {
    feed.current?.subscribe(JSON.parse(matrix) as Subscription)
  }, [matrix])

  // A timed avoid has to reactivate on its own (spec US 14), and nothing else
  // is guaranteed to wake the store when its moment comes.
  useEffect(() => {
    const sweep = setInterval(
      () => dispatch(expireAvoids(Date.now())),
      AVOID_SWEEP_MS,
    )
    return () => clearInterval(sweep)
  }, [dispatch])

  return null
}
