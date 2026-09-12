/**
 * Letting go of an **Access code** grant the server has stopped honouring
 * (#68, spec US 52).
 *
 * The catalog is where the server says so, and this is the one place that acts
 * on it — see `components/AccessNotice` for what the Listener is then told.
 */
import { useEffect } from 'react'

import { accessOf, grantWentStale } from '@/lib/access'
import { grantExpired, selectGrant } from '@/store/access'
import { useGetCatalogQuery } from '@/store/api'
import { useAppDispatch, useAppSelector } from '@/store/hooks'

/**
 * Watch the catalog for the server saying this browser's grant is not a live
 * code, and let go of it when it does.
 *
 * Guarded on *holding* one, so a browser that has never unlocked anything never
 * dispatches — and the reducer guards it a second time, because two catalog
 * fetches answering the same thing must not raise the notice twice.
 */
export function useStaleGrantWatch(): void {
  const dispatch = useAppDispatch()
  const grant = useAppSelector(selectGrant)
  const { data } = useGetCatalogQuery(undefined, { skip: !grant })
  const stale = grantWentStale(accessOf(data))

  useEffect(() => {
    if (stale) dispatch(grantExpired(stale))
  }, [dispatch, stale])
}
