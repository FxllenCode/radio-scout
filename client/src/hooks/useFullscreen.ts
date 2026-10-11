import { useSyncExternalStore } from 'react'

function subscribe(changed: () => void): () => void {
  document.addEventListener('fullscreenchange', changed)
  return () => document.removeEventListener('fullscreenchange', changed)
}

const active = () => document.fullscreenElement != null

/**
 * The whole app full screen, and back (#78) — rdio's double-tap, and one of the
 * lost affordances the critique named.
 *
 * `available` is the browser's own answer, because a control that would be
 * refused is not offered: iPhone Safari has no Fullscreen API at all, and an
 * installed app there is already as full screen as it gets. The *document*
 * goes full screen rather than the readout, so the controls come with it.
 */
export function useFullscreen(): { available: boolean; active: boolean; toggle: () => void } {
  const isActive = useSyncExternalStore(subscribe, active)
  return {
    available: document.fullscreenEnabled === true,
    active: isActive,
    toggle: () => {
      // A refusal (no user gesture, a frame that forbids it) changes nothing,
      // and there is nothing to say about it that the unchanged screen does
      // not already say.
      if (isActive) void document.exitFullscreen().catch(() => {})
      else void document.documentElement.requestFullscreen().catch(() => {})
    },
  }
}
