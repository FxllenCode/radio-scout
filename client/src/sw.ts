/// <reference lib="webworker" />
/**
 * The service worker (#15).
 *
 * Ours rather than `vite-plugin-pwa`'s generated one, because of a product
 * decision this app has an opinion about: **a new version waits.** Radio-Scout
 * is a thing you leave running; a worker that took over on install would cut off
 * a Call, and on a phone end the backgrounded session ADR-0005 exists to make
 * work. Only the listener's tap (`SKIP_WAITING`, from `lib/serviceWorker.ts`)
 * hands over.
 *
 * Until #107 there was a second reason, and it was the louder one: a generated
 * worker cannot have a `push` handler, and this was the only scope that could.
 * Notifications are gone (ADR-0014) and the conclusion outlived its argument —
 * worth saying, so the `injectManifest` strategy in `vite.config.ts` does not
 * read as unmotivated.
 *
 * Deliberately thin: what remains is glue against APIs that exist in no test
 * environment, covered by the Playwright layer rather than by jsdom (ADR-0010's
 * exclusion list).
 */
import { clientsClaim } from 'workbox-core'
import {
  cleanupOutdatedCaches,
  createHandlerBoundToURL,
  precacheAndRoute,
} from 'workbox-precaching'
import { NavigationRoute, registerRoute } from 'workbox-routing'

declare const self: ServiceWorkerGlobalScope

/** The app shell, injected at build time by `vite-plugin-pwa`. */
precacheAndRoute(self.__WB_MANIFEST)
cleanupOutdatedCaches()

/**
 * Client-side routing offline: any navigation is answered with the app shell —
 * except the server's own namespace. A cached API response would be stale, a
 * cached `/healthz` would lie about the server being up, and cached Call audio
 * would fill a phone with an archive nobody asked for.
 *
 * **`/s` is on that list, and it is the one entry that is not an API** (#64). A
 * **Share link** is a page the *server* renders — the whole promise is that it
 * plays without the app — so a worker that answered it with the app shell would
 * break every share link opened in a browser that has this instance installed,
 * and only in that browser. Anchored so the three client-side routes that begin
 * with an `s` (`/search`, `/session`, `/settings`) still get the shell: what is
 * denied is `/s` exactly, and anything under `/s/`.
 */
registerRoute(
  new NavigationRoute(createHandlerBoundToURL('/index.html'), {
    denylist: [/^\/api\//, /^\/healthz$/, /^\/rdio-scanner/, /^\/s(\/|\?|$)/],
  }),
)

// The first worker a page ever has takes charge at once (there is nothing to
// interrupt); a *replacement* still waits for the tap below.
clientsClaim()

self.addEventListener('message', (event: ExtendableMessageEvent) => {
  if (event.data?.type === 'SKIP_WAITING') void self.skipWaiting()
})
