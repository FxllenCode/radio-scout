/// <reference lib="webworker" />
/**
 * The service worker (#15, #16).
 *
 * Ours rather than `vite-plugin-pwa`'s generated one, because two of the things
 * it does are product decisions this app has opinions about:
 *
 * - **A new version waits.** Radio-Scout is a thing you leave running; a worker
 *   that took over on install would cut off a Call, and on a phone end the
 *   backgrounded session ADR-0005 exists to make work. Only the listener's tap
 *   (`SKIP_WAITING`, from `lib/serviceWorker.ts`) hands over.
 * - **Web Push.** A generated worker has no `push` handler at all, and this is
 *   the only scope that can have one.
 *
 * Deliberately thin: everything with a decision in it is in `lib/pushMessage.ts`,
 * which is a plain module the Vitest suite can reach. What remains here is glue
 * against APIs that exist in no test environment, and is covered by the
 * Playwright layer and the real-device gate instead (ADR-0010's exclusion list).
 */
import { clientsClaim } from 'workbox-core'
import {
  cleanupOutdatedCaches,
  createHandlerBoundToURL,
  precacheAndRoute,
} from 'workbox-precaching'
import { NavigationRoute, registerRoute } from 'workbox-routing'

import { notificationFor, targetFor, type PushPayload } from './lib/pushMessage'
import { PUSH_OPEN } from './lib/pushOpen'

declare const self: ServiceWorkerGlobalScope

/** The app shell, injected at build time by `vite-plugin-pwa`. */
precacheAndRoute(self.__WB_MANIFEST)
cleanupOutdatedCaches()

/**
 * Client-side routing offline: any navigation is answered with the app shell —
 * except the server's own namespace. A cached API response would be stale, a
 * cached `/healthz` would lie about the server being up, and cached Call audio
 * would fill a phone with an archive nobody asked for.
 */
registerRoute(
  new NavigationRoute(createHandlerBoundToURL('/index.html'), {
    denylist: [/^\/api\//, /^\/healthz$/, /^\/rdio-scanner/],
  }),
)

// The first worker a page ever has takes charge at once (there is nothing to
// interrupt); a *replacement* still waits for the tap below.
clientsClaim()

self.addEventListener('message', (event: ExtendableMessageEvent) => {
  if (event.data?.type === 'SKIP_WAITING') void self.skipWaiting()
})

/**
 * A notification for every push, always.
 *
 * iOS revokes a subscription that receives a push and shows nothing, so an
 * unreadable payload still becomes a notification rather than being dropped —
 * `notificationFor` owns that fallback.
 */
self.addEventListener('push', (event: PushEvent) => {
  let payload: PushPayload | undefined
  try {
    payload = event.data?.json() as PushPayload
  } catch {
    payload = undefined
  }
  const { title, options } = notificationFor(payload)
  event.waitUntil(self.registration.showNotification(title, options))
})

/**
 * A tap resumes the feed: focus the app if it is already open — that tab has
 * the listener's queue and selection — and open it otherwise.
 */
self.addEventListener('notificationclick', (event: NotificationEvent) => {
  event.notification.close()
  const url = (event.notification.data as { url?: string } | null)?.url ?? '/'
  event.waitUntil(
    (async () => {
      const open = await self.clients.matchAll({
        type: 'window',
        includeUncontrolled: true,
      })
      const target = targetFor(open, self.location.origin)
      if (!target) {
        await self.clients.openWindow(url)
        return
      }
      await target.focus()
      // The tap is the gesture the page needs to start audio again, and it
      // happened here rather than there — so it is passed on.
      target.postMessage({ type: PUSH_OPEN, url })
    })(),
  )
})
