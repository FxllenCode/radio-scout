import { useEffect, useRef, useState } from 'react'

import {
  registerServiceWorker,
  type ServiceWorkerHandle,
} from '@/lib/serviceWorker'

/** Whether a new version is waiting, and how to take it. */
export interface AppUpdate {
  ready: boolean
  apply: () => void
}

/**
 * Registers the service worker and reports when a new version is waiting.
 *
 * Lives as a hook because the shell has one docked banner slot and has to
 * choose what goes in it — and because registration belongs to the shell's
 * lifetime, not to a banner that is absent almost always.
 */
export function useAppUpdate(): AppUpdate {
  const [ready, setReady] = useState(false)
  const worker = useRef<ServiceWorkerHandle>(null)

  useEffect(() => {
    const registered = registerServiceWorker()
    worker.current = registered
    const sync = () => setReady(registered.updateReady)
    const release = registered.subscribe(sync)
    sync()
    return () => {
      release()
      registered.destroy()
      worker.current = null
    }
  }, [])

  return { ready, apply: () => worker.current?.applyUpdate() }
}
