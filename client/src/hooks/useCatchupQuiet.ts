import { skipToken } from '@reduxjs/toolkit/query'
import { useEffect, useMemo } from 'react'

import { useGetQuietSpansQuery } from '@/store/api'
import { useAppDispatch, useAppSelector } from '@/store/hooks'
import {
  quietFound,
  selectIsCatchingUp,
  selectLiveCall,
  selectQueue,
} from '@/store/live'

/**
 * How far down the queue to ask about at once (#59).
 *
 * Catch-up plays a Call every few seconds, so the answer needs to be in hand
 * before the head arrives — but asking about all hundred would fetch spans for
 * Calls a Listener will drop, jump past, or never reach. Eight is roughly a
 * minute of catching up: several Calls of lead time, and one request rather
 * than one per Call.
 */
const WINDOW = 8

/**
 * Keep the **Quiet spans** of the Calls Catch-up is about to play on hand (#59,
 * spec US 23).
 *
 * **This exists because a live frame cannot carry them.** The frame is published
 * at ingest, before the scanner has looked at the audio, and nothing republishes
 * one (#46) — so the spans are on the server minutes before the Call reaches the
 * head of a Listener's queue, and no push delivers them. A Call read back from
 * the Archive needs none of this: it carries them on itself.
 *
 * Pull rather than push, and that is the whole design decision: a Listener who
 * is *caught up* — nearly all of them, nearly all of the time — can never use a
 * span, so a frame per Call carrying one would be traffic every Pi pays for and
 * almost nobody spends. With Catch-up off this hook sends nothing at all.
 *
 * `useRunPageAhead`'s shape, and its `currentData` rule for its reason: RTK
 * Query keeps the previous argument's answer in `data` while the next is in
 * flight, and writing *those* spans onto *these* Calls would seek a Listener to
 * gaps measured in somebody else's audio.
 */
export function useCatchupQuiet() {
  const dispatch = useAppDispatch()
  const catchingUp = useAppSelector(selectIsCatchingUp)
  const queue = useAppSelector(selectQueue)
  // **The Call already playing counts.** Engaging Catch-up is something a
  // Listener does *while* a Call is on the air, and that Call is not in the
  // queue — so asking about the queue alone leaves the first Call of every
  // drain untrimmed, which is the one a Listener watches to decide whether this
  // works at all.
  const playing = useAppSelector(selectLiveCall)

  // Only the Calls nothing has answered for yet. A Call whose spans arrived —
  // including the empty answer that means "looked up, no gaps" — is never asked
  // about again, which is what stops the ordinary Call, the one with nothing to
  // trim, from being re-requested on every pass forever.
  const wanted = useMemo(() => {
    if (!catchingUp) return null
    const ids = [...(playing ? [playing] : []), ...queue.slice(0, WINDOW)]
      .filter((call) => call.quiet === undefined)
      .map((call) => call.id)
    return ids.length > 0 ? ids : null
  }, [catchingUp, playing, queue])

  const { currentData: spans } = useGetQuietSpansQuery(wanted ?? skipToken)

  useEffect(() => {
    if (wanted && spans) dispatch(quietFound({ ids: wanted, spans }))
  }, [wanted, spans, dispatch])
}
