import { expect, test, type Page } from '@playwright/test'

/**
 * The service worker's push half (#16), in the only place it can be tested.
 *
 * `src/sw.ts` runs in a scope Vitest cannot enter — no `PushEvent`, no
 * `registration.showNotification`, no `clients` — so the decisions inside it
 * live in `src/lib/pushMessage.ts` (unit-tested at 100%) and *this* proves the
 * worker actually wires them up: a push arrives, and the device shows a
 * notification.
 *
 * The push is delivered through Chrome DevTools Protocol rather than through a
 * real push service, so it arrives already decrypted — which is exactly what
 * the worker sees after the browser decrypts it. The encryption itself is
 * proven against RFC 8291's own test vector in `src/webpush.rs`.
 *
 * What is asserted is the call to `showNotification`, recorded inside the
 * worker: headless Chromium accepts the call but keeps no notification for
 * `getNotifications` to return, and whether a *painted* notification appears is
 * a question only the real-device gate (research §14) can answer anyway.
 */

/** What the spy below records into the worker's own global scope. */
type Recorded = {
  __shown?: {
    title: string
    body?: string
    tag?: string
    renotify?: boolean
    url?: string
  }[]
}

/** Wait until a worker is in charge of this page. */
const controlled = (page: Page) =>
  page.waitForFunction(() => navigator.serviceWorker.controller !== null)

test.describe('Web Push', () => {
  test('shows a notification when a push arrives', async ({
    page,
    context,
  }) => {
    await context.grantPermissions(['notifications'])
    await page.goto('/')
    await controlled(page)

    // Record what the worker asks the platform to show. The spy goes in from
    // outside; nothing in the shipped worker knows about this test.
    const [worker] = context.serviceWorkers()
    await worker.evaluate(() => {
      const scope = globalThis as Recorded
      scope.__shown = []
      // The worker's own `registration`. Typed loosely because this file is a
      // DOM-scoped project: `ServiceWorkerGlobalScope` is not in its lib.
      const registration = (globalThis as unknown as {
        registration: ServiceWorkerRegistration
      }).registration
      const show = registration.showNotification.bind(registration)
      registration.showNotification = (title: string, options?: NotificationOptions) => {
        const shown = options as (NotificationOptions & {
          renotify?: boolean
          data?: { url?: string }
        }) | undefined
        scope.__shown?.push({
          title,
          body: shown?.body,
          tag: shown?.tag,
          renotify: shown?.renotify,
          url: shown?.data?.url,
        })
        return show(title, options)
      }
    })

    const cdp = await context.newCDPSession(page)
    // The registration to deliver to: enabling the domain replays what is
    // already registered, which after `controlled` is ours.
    const registration = new Promise<string>((resolve) => {
      cdp.on('ServiceWorker.workerRegistrationUpdated', ({ registrations }) => {
        const ours = registrations.find((entry) => entry.scopeURL.endsWith('/'))
        if (ours) resolve(ours.registrationId)
      })
    })
    await cdp.send('ServiceWorker.enable')
    const registrationId = await registration

    await cdp.send('ServiceWorker.deliverPushMessage', {
      origin: new URL(page.url()).origin,
      registrationId,
      data: JSON.stringify({
        id: 42,
        systemRef: 11,
        talkgroupRef: 54241,
        system: 'Fulton County',
        talkgroup: 'Fire Dispatch',
        count: 3,
      }),
    })

    await expect
      .poll(() => worker.evaluate(() => (globalThis as Recorded).__shown ?? []))
      .toEqual([
        {
          title: 'Fire Dispatch',
          body: '3 new calls · Fulton County',
          // The device's half of "no storms": the next notification for this
          // Talkgroup replaces this one rather than stacking under it.
          tag: 'rs-11-54241',
          renotify: true,
          url: '/?call=42',
        },
      ])
  })
})
