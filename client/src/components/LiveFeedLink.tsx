import { useEffect, useRef } from 'react'

import {
  connectLiveFeed,
  type LiveFeedHandle,
  type LiveStatus,
  type Subscription,
} from '@/lib/liveFeed'
import { usePush } from '@/hooks/usePush'
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
  // Notifications (#16): the socket presents the subscription so the server
  // knows this listener is listening, and re-registers the matrix so what wakes
  // their phone is what they actually hear.
  const { push, token: pushToken } = usePush()

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
      // Read at send time: notifications can be switched on while this socket
      // is already open, and the next `sub` frame is what tells the server.
      pushToken: () => push?.token,
    })
    feed.current = handle
    return () => handle.close()
  }, [dispatch, store, push])

  // Re-sent on both counts: when the listener changes what they hear, and when
  // a push subscription appears or goes while this socket is already open —
  // otherwise someone who just switched notifications on would keep being
  // notified about their own listening until the next reconnect.
  useEffect(() => {
    const selection = JSON.parse(matrix) as Subscription
    feed.current?.subscribe(selection)
    // The server's copy of the Selection decides which Calls are worth waking a
    // phone for, so it moves with the listener's. A no-op while notifications
    // are off, and a no-op when the server already has this one.
    void push?.sync(selection)
  }, [matrix, push, pushToken])

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
