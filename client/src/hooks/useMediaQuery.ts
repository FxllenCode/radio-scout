import { useSyncExternalStore } from 'react'

/**
 * Whether a media query matches, kept current (#78).
 *
 * For the layout decisions CSS cannot make on its own — *whether a component
 * mounts at all*. The desktop layout's Live panel is a whole second scanner
 * screen, and hiding it on a phone with a class would still render it, and
 * re-render it on every store dispatch, on exactly the hardware that can least
 * afford it. A browser without `matchMedia` is answered as not matching.
 */
export function useMediaQuery(query: string): boolean {
  return useSyncExternalStore(
    (changed) => {
      const list = globalThis.matchMedia?.(query)
      list?.addEventListener('change', changed)
      return () => list?.removeEventListener('change', changed)
    },
    () => globalThis.matchMedia?.(query).matches === true,
  )
}
