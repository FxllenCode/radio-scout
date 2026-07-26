/**
 * Registering the service worker, and deciding when a new version takes over
 * (#15, spec US 28).
 *
 * # Why we register it ourselves
 *
 * `vite-plugin-pwa` will happily inject its own registration script. We turn
 * that off (`injectRegister: false`) and do it here, because *when a new
 * version takes over* is a product decision for this app, not a default:
 * Radio-Scout is a thing you leave running. A worker that calls `skipWaiting`
 * and reloads the page the moment a deploy lands would cut off a Call
 * mid-sentence — and, on a phone, end the backgrounded listening session this
 * whole epic exists to make work (ADR-0005).
 *
 * So: **a new version installs and waits.** Nothing reloads until the listener
 * asks for it, or until they next open the app cold.
 */

import { notifier } from './notifier'

/** Where the built worker lands, and therefore its scope: the whole app. */
const SERVICE_WORKER_URL = '/sw.js'

/** What [`applyUpdate`] sends the waiting worker. Workbox's generated worker
 *  answers exactly this message by calling `skipWaiting()` — it is the whole
 *  handover protocol, and the only thing that ever gives up control. */
export const SKIP_WAITING = { type: 'SKIP_WAITING' } as const

export interface ServiceWorkerOptions {
  /** How the page is reloaded once a new version takes over. Injected because
   *  jsdom cannot navigate — and so a test can prove we *didn't*. */
  reload?: () => void
}

export interface ServiceWorkerHandle {
  /** Whether a new version is installed and waiting for permission to run. */
  readonly updateReady: boolean
  /** Be told when [`updateReady`] changes; returns the release. */
  subscribe(listener: () => void): () => void
  /** Hand over to the waiting version and reload once it is in charge. The
   *  listener's decision — never ours. */
  applyUpdate(): void
  /** Stop listening. */
  destroy(): void
}

/** Register the worker. A browser without service workers gets a handle that
 *  does nothing, rather than an error — the app works uncached. */
export function registerServiceWorker({
  reload = () => location.reload(),
}: ServiceWorkerOptions = {}): ServiceWorkerHandle {
  const container = navigator.serviceWorker as
    | ServiceWorkerContainer
    | undefined
  if (!container) return inert

  let waiting: ServiceWorker | null = null
  let applying = false
  const changes = notifier()

  const announce = (worker: ServiceWorker | null) => {
    // No controller means this is the *first* worker this page has ever had,
    // not a new version of one — nothing to announce, and nothing to reload
    // for: it will take over on its own next load.
    if (!worker || waiting === worker || !container.controller) return
    waiting = worker
    changes.changed()
  }

  const tookOver = () => {
    // Only the handover we asked for reloads. A worker claiming clients on its
    // own must not throw a listener out of the app mid-Call.
    if (applying) reload()
  }
  container.addEventListener('controllerchange', tookOver)

  void container
    .register(SERVICE_WORKER_URL)
    .then((registration) => {
      // Already waiting when we got here — the tab was open across a deploy,
      // or this load didn't yield control.
      announce(registration.waiting)

      registration.addEventListener('updatefound', () => {
        const installing = registration.installing
        if (!installing) return
        installing.addEventListener('statechange', () => {
          if (installing.state === 'installed') announce(installing)
        })
      })
    })
    .catch(() => {
      // An insecure origin, a bad scope, or a load that raced the network.
      // The app runs uncached; nothing else about it changes.
    })

  return {
    get updateReady() {
      return waiting !== null
    },

    subscribe: changes.subscribe,

    applyUpdate() {
      if (!waiting) return
      applying = true
      waiting.postMessage(SKIP_WAITING)
    },

    destroy() {
      container.removeEventListener('controllerchange', tookOver)
      changes.clear()
    },
  }
}

/** The handle a browser without service workers gets. */
const inert: ServiceWorkerHandle = {
  updateReady: false,
  subscribe: () => () => {},
  applyUpdate() {},
  destroy() {},
}
