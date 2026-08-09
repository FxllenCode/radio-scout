import { skipToken } from '@reduxjs/toolkit/query'
import { useEffect } from 'react'

import { prefetchAudio } from '@/lib/prefetch'
import { useSearchCallsQuery } from '@/store/api'
import { useAppDispatch, useAppSelector } from '@/store/hooks'
import { runPaged, selectIsRolling, selectWantedPage } from '@/store/playback'

/**
 * Keep the page a **Run** is about to need on hand, and hand it over at the
 * boundary (#32's page-ahead, US 25's roll-on).
 *
 * **Any screen that starts a Run owes this**, and that is why it is a hook
 * rather than lines in one route. A Run walks a page at a time; without
 * somebody subscribing to the page it names and giving it back, it plays to the
 * end of the loaded page and stops — which looks exactly like the search having
 * run out. #47's per-Unit view was the second screen to start one, and stalled
 * silently at fifty Calls until this moved here.
 *
 * `RunView.wanted` is the single answer to *which* page: the boundary being
 * crossed, or the one being waited at (#89). `skipToken` means a Run with
 * nothing to page onto asks for nothing at all.
 */
export function useRunPageAhead() {
  const dispatch = useAppDispatch()
  const wanted = useAppSelector(selectWantedPage)
  const rolling = useAppSelector(selectIsRolling)
  // `currentData`, never `data`: RTK Query keeps the *previous* argument's
  // answer in `data` while the next one is in flight, so a Run that re-armed
  // onto a new search would be handed the page-ahead of the search the Listener
  // just left — and go on playing Calls from it. `currentData` is undefined
  // until the page for the argument now asked for is really in hand.
  const { currentData: ahead } = useSearchCallsQuery(wanted ?? skipToken)

  // Hand it over the moment the Run is actually at the boundary. Gated on
  // `rolling` rather than on the data alone because the page-ahead's whole
  // point is that the page is *already there* — RTK Query serves a cached one
  // without a fulfilled action, so waiting for one to arrive would miss exactly
  // the case #32 exists for.
  //
  // `wanted` rides along as the request this page answers, which the Run checks
  // against the one it named. That check is only ever as true as the caller: it
  // is `currentData` above that makes the claim an honest one here, because
  // with `data` the page could belong to a request nobody is making any more
  // while `wanted` said otherwise.
  useEffect(() => {
    if (rolling && wanted && ahead) {
      dispatch(runPaged({ window: wanted, page: ahead }))
    }
  }, [rolling, wanted, ahead, dispatch])

  // ...and warm the first Call of that page, which is the audio the Run arrives
  // at. #14's prefetch stops at the end of the loaded page, because what
  // follows the last Call of one is not in the store yet.
  //
  // The gate is a boolean rather than `wanted` itself, and not for tidiness:
  // `wanted` is rebuilt whenever the Run changes at all, so depending on it
  // would abort and restart the warm on every Call the Run advances through.
  const pagingAhead = wanted !== null
  useEffect(() => {
    if (!pagingAhead) return
    const first = ahead?.results[0]
    if (!first) return
    const controller = new AbortController()
    void prefetchAudio(first.audioUrl, controller.signal)
    return () => controller.abort()
  }, [pagingAhead, ahead?.results])
}
