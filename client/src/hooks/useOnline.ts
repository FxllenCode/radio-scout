import { useSyncExternalStore } from 'react'

function subscribe(changed: () => void): () => void {
  window.addEventListener('online', changed)
  window.addEventListener('offline', changed)
  return () => {
    window.removeEventListener('online', changed)
    window.removeEventListener('offline', changed)
  }
}

const online = () => navigator.onLine

/**
 * Whether this device has a network, as the browser reports it (#78).
 *
 * What tells **Feed down**'s two situations apart: no network is the
 * Listener's to fix, a server that is not answering is not. Trusted only in
 * the direction the specification makes it trustworthy — `false` means the
 * browser will not try the network at all, while `true` promises nothing — so
 * nothing reads it to decide the feed is up, only to say why it is down.
 *
 * A hook over the browser rather than a field in the store, because no
 * reducer has any business with it: it changes what the screen *says*, never
 * what plays.
 */
export function useOnline(): boolean {
  return useSyncExternalStore(subscribe, online)
}
