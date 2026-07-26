import { expect, test, type Page } from '@playwright/test'

/** Wait until a worker is actually in charge of this page — `ready` only says
 *  one is active, which is not the same as it serving our requests. */
const controlled = (page: Page) =>
  page.waitForFunction(() => navigator.serviceWorker.controller !== null)

test.describe('PWA', () => {
  test('is installable: a manifest with the fields and icons a browser demands', async ({
    page,
    request,
  }) => {
    await page.goto('/')

    const href = await page.locator('link[rel=manifest]').getAttribute('href')
    expect(href, 'index.html links a manifest').toBeTruthy()

    const response = await request.get(href!)
    expect(response.status()).toBe(200)
    expect(response.headers()['content-type']).toContain('manifest+json')

    const manifest = JSON.parse(await response.text()) as {
      name: string
      display: string
      start_url: string
      icons: { src: string; sizes: string; purpose?: string }[]
    }
    expect(manifest.name).toBe('Radio-Scout')
    // Standalone is not cosmetic: iOS offers Web Push (#16) to nothing else,
    // and background audio needs the app out of a browser tab (ADR-0005).
    expect(manifest.display).toBe('standalone')
    expect(manifest.start_url).toBe('/')

    const sizes = manifest.icons.map((icon) => icon.sizes)
    expect(sizes).toContain('192x192')
    expect(sizes).toContain('512x512')
    expect(
      manifest.icons.some((icon) => icon.purpose?.includes('maskable')),
      'an icon Android can mask without cropping the mark',
    ).toBe(true)

    for (const icon of manifest.icons) {
      expect((await request.get(icon.src)).status(), icon.src).toBe(200)
    }

    // iOS is the platform this whole epic is for, and it does not take its
    // home-screen icon from the manifest — it reads this link.
    const apple = await page
      .locator('link[rel="apple-touch-icon"]')
      .getAttribute('href')
    expect(apple, 'index.html links an apple-touch-icon').toBeTruthy()
    expect((await request.get(apple!)).status()).toBe(200)
  })

  test('registers a service worker and lets it take charge', async ({
    page,
  }) => {
    await page.goto('/')

    await controlled(page)

    const scope = await page.evaluate(
      async () => (await navigator.serviceWorker.ready).scope,
    )
    // The whole app, not a subdirectory of it.
    expect(new URL(scope).pathname).toBe('/')
  })

  test('still serves the app when the network is gone', async ({
    page,
    context,
  }) => {
    await page.goto('/')
    await controlled(page)

    await context.setOffline(true)
    await page.reload()

    // The shell renders from the precache; the live feed and API are of course
    // dead, which the app already has a state for.
    await expect(page.getByRole('navigation', { name: 'Primary' })).toBeVisible()
    await expect(page.getByRole('heading', { name: 'LIVE' })).toBeVisible()
  })

  test('never answers an API request out of its cache', async ({
    page,
    context,
  }) => {
    await page.goto('/')
    await controlled(page)
    await context.setOffline(true)

    // Offline and uncached, these must *fail*. A worker that fell back to the
    // app shell for them would hand JSON parsers a page of HTML, and one that
    // cached Call audio would fill a phone with an archive it never asked for.
    for (const path of ['/api/catalog', '/api/call/1/audio', '/healthz']) {
      const failed = await page.evaluate(
        (url) => fetch(url).then(() => false).catch(() => true),
        path,
      )
      expect(failed, `${path} must not be served from the cache`).toBe(true)
    }
  })
})
