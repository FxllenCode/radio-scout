import { useEffect, useRef } from 'react'

import {
  connectLiveFeed,
  type LiveFeedHandle,
  type LiveStatus,
} from '@/lib/liveFeed'
import { useAppDispatch, useAppSelector, useAppStore } from '@/store/hooks'
import {
  connected,
  connecting,
  disconnected,
  gapped,
  lagged,
  received,
  selectFeedStatus,
  selectSince,
} from '@/store/live'
import { selectGrant } from '@/store/access'
import { selectSubscription } from '@/store/transport'

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

  // Compared by reference: the selector holds its identity while nothing it
  // reads has changed (#91), so this re-runs when the *matrix* changes and not
  // on every dispatch that would have rebuilt an equal one.
  const matrix = useAppSelector(selectSubscription)
  const feedOff = useAppSelector(selectFeedStatus) === 'off'
  // **A dependency, so unlocking re-opens the socket** (#68). The server
  // resolves a connection's scope once, at connect — an expiry is re-read on the
  // heartbeat and a *new* grant is not — so a Listener who unlocks a channel
  // while connected has to be given a new socket, or they would hear nothing
  // from it until the next network blip.
  const grant = useAppSelector(selectGrant)

  useEffect(() => {
    // Feed off is a **hard** off (#80): no socket at all, so bandwidth and
    // battery go to zero. Nothing takes over — Radio-Scout does not notify
    // (ADR-0014) — so switching it back on is the only way back in.
    //
    // The one place that reads the *particular* cause rather than
    // `feedPlays` (#88), because the two silences differ here and nowhere else:
    // playback mode keeps the connection up and sends an empty subscription, so
    // a listener browsing the archive is still reachable the moment they come
    // back.
    //
    // Guarded here rather than by closing after connecting, so a listener whose
    // browser remembered the choice never opens one: no round trip, and no
    // burst of backfilled audio arriving before the choice is applied.
    if (feedOff) {
      // Nothing is connected, and the display must not keep saying otherwise.
      dispatch(disconnected())
      return
    }
    const handle = connectLiveFeed(
      {
        onStatus: (status) => dispatch(STATUS_ACTION[status]()),
        onCall: (call, seq) => dispatch(received(call, seq)),
        onLagged: (skipped) => dispatch(lagged(skipped)),
        onGap: () => dispatch(gapped()),
        // Read at send time, not subscribe time: the cursor moves with every
        // Call and only matters when the socket comes back (ADR-0004). Turning
        // the feed off clears it, so coming back subscribes from now.
        since: () => selectSince(store.getState()),
      },
      { grant },
    )
    feed.current = handle
    // Tell it what to send *here*, not only from the matrix effect below.
    //
    // A handle starts with no subscription and `connectLiveFeed` sends nothing
    // until it has one, so whoever creates a handle owns subscribing it. That
    // used to be free — one handle per mount, paired with the matrix effect's
    // first run — but a handle is now created again whenever the feed comes back
    // on (#80), and `turnFeedOn` does not change the matrix, so the matrix
    // effect does not re-run. Without this the listener would get a connected
    // socket the server has nothing selected on: a green light and permanent
    // silence until they next touched the Selection.
    handle.subscribe(selectSubscription(store.getState()))
    return () => {
      feed.current = null
      handle.close()
    }
  }, [dispatch, store, feedOff, grant])

  // Re-sent when the listener changes what they hear.
  useEffect(() => {
    feed.current?.subscribe(matrix)
  }, [matrix])

  return null
}
