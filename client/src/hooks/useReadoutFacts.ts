import { useOnline } from '@/hooks/useOnline'
import type { ReadoutFacts } from '@/lib/feed'
import { useGetCatalogQuery } from '@/store/api'
import { useAppSelector } from '@/store/hooks'
import { selectRefused } from '@/store/live'

/**
 * Everything the feed's readout is told beyond its standing (#78) — gathered
 * once, for the two readers that must not come to disagree: the Live screen,
 * which draws it, and the announcer, which says it.
 *
 * `firstRun` is unknown until the catalog lands, and read as the common case
 * meanwhile: a working Instance between Calls.
 */
export function useReadoutFacts(): ReadoutFacts {
  return {
    // Why the server is turning us away, when it is (#77).
    refused: useAppSelector(selectRefused),
    // Whose fault a dead link is: no network is the Listener's to fix.
    offline: !useOnline(),
    // Whether there is anything to listen *to* yet.
    firstRun: useGetCatalogQuery().data?.systems.length === 0,
  }
}
