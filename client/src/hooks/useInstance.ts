import { DEFAULT_INSTANCE } from '@/lib/instance'
import { useGetInstanceQuery } from '@/store/api'
import type { Instance, TimeFormat } from '@/types'

/** The **Instance identity** (#77), or the defaults while it is on its way —
 *  which are what an unnamed Instance sends, so nothing is ever drawn that an
 *  uncurated Instance would not draw. */
export function useInstance(): Instance {
  return useGetInstanceQuery().data ?? DEFAULT_INSTANCE
}

/** How this Instance writes a time — what every `formatCallTime` is handed. */
export function useClock(): TimeFormat {
  return useInstance().timeFormat
}
