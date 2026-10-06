import { createServer, type Server } from 'node:http'
import type { AddressInfo } from 'node:net'

import { expect, test } from '@playwright/test'

/**
 * **The Embed page, framed by somebody else's site** (#75, spec US 59).
 *
 * The one claim no other layer can make: the built page renders, and *plays*,
 * inside an `<iframe>` on a different origin — a different **site**, even, since
 * the host here is `127.0.0.1` and the embed is `localhost`, which a browser
 * treats as two sites. The host is a real server rather than a routed page:
 * Chrome's Local Network Access check refuses to let a *public* page frame a
 * local address at all, and a page Playwright fulfils has no address, so it
 * counts as public — which is also why `docs/operating.md` says an embedded
 * Instance must be one the host's readers can reach. What the page
 * decides is Vitest's (`src/embed/`); what headers the binary frames it with is
 * the Rust suite's (`tests/embed.rs`), because there is no Rust process under
 * this runner. Everything the page reads is routed at the network boundary.
 */

/** The host's own page: nothing but the snippet an Operator handed them —
 *  served for real, from another site on the same loopback. */
let host: Server
let HOST = ''
let hostPage = ''

test.beforeAll(async () => {
  host = createServer((_, response) => {
    response.setHeader('content-type', 'text/html')
    response.end(hostPage)
  })
  await new Promise<void>((resolve) => host.listen(0, '127.0.0.1', resolve))
  HOST = `http://127.0.0.1:${(host.address() as AddressInfo).port}/`
})

test.afterAll(() => host.close())

/** The snippet, framing the built page by its token. */
function framing(baseURL: string | undefined): string {
  return (
    '<!doctype html><title>County Fire</title><h1>County Fire Department</h1>' +
    `<iframe src="${baseURL}/embed.html?t=${TOKEN}" title="Fire dispatch" ` +
    'width="100%" height="420" style="border:0;max-width:480px" ' +
    'allow="autoplay"></iframe>'
  )
}
const TOKEN = 'a1b2c3'

/** A short, real, decodable WAV — Chromium has to play it, not pretend to. */
function wav(seconds: number): Buffer {
  const hz = 8_000
  const samples = Math.round(hz * seconds)
  const bytes = Buffer.alloc(44 + samples * 2)
  bytes.write('RIFF', 0, 'ascii')
  bytes.writeUInt32LE(36 + samples * 2, 4)
  bytes.write('WAVEfmt ', 8, 'ascii')
  bytes.writeUInt32LE(16, 16)
  bytes.writeUInt16LE(1, 20)
  bytes.writeUInt16LE(1, 22)
  bytes.writeUInt32LE(hz, 24)
  bytes.writeUInt32LE(hz * 2, 28)
  bytes.writeUInt16LE(2, 32)
  bytes.writeUInt16LE(16, 34)
  bytes.write('data', 36, 'ascii')
  bytes.writeUInt32LE(samples * 2, 40)
  for (let i = 0; i < samples; i += 1) {
    bytes.writeInt16LE(Math.round(Math.sin((2 * Math.PI * 440 * i) / hz) * 8000), 44 + i * 2)
  }
  return bytes
}

function call(id: number) {
  return {
    id,
    systemRef: 11,
    talkgroupRef: 100,
    talkgroupLabel: `Fire ${id}`,
    timestamp: Date.now(),
    durationMs: 400,
    audioUrl: `/api/call/${id}/audio`,
  }
}

test('plays inside a frame on another site', async ({ page, context, baseURL }) => {
  hostPage = framing(baseURL)
  await context.route('**/api/embed?*', (route) =>
    route.fulfill({
      json: {
        name: 'Fire dispatch',
        selection: { all: false, sel: { 11: { 100: true } } },
        calls: [call(1)],
      },
    }),
  )
  await context.route('**/api/call/*/audio', (route) =>
    route.fulfill({ contentType: 'audio/wav', body: wav(0.4) }),
  )
  // The live feed: greet, wait for the page's subscription, then send two Calls.
  const subscriptions: unknown[] = []
  await context.routeWebSocket('**/api/live', (socket) => {
    socket.send(JSON.stringify({ t: 'hello', protocol: 2, heartbeatMs: 30_000 }))
    socket.onMessage((message) => {
      subscriptions.push(JSON.parse(String(message)))
      socket.send(JSON.stringify({ t: 'call', seq: 1, call: call(7) }))
      socket.send(JSON.stringify({ t: 'call', seq: 2, call: call(8) }))
    })
  })

  await page.goto(HOST)
  const frame = page.frameLocator('iframe[title="Fire dispatch"]')

  await expect(frame.getByRole('heading', { name: 'Fire dispatch' })).toBeVisible()
  await expect(frame.getByText('Fire 1')).toBeVisible()

  await frame.getByRole('button', { name: 'Listen live' }).click()

  expect(await frame.locator('body').evaluate(() => location.origin)).toBe(
    new URL(baseURL!).origin,
  )
  await expect.poll(() => subscriptions).toEqual([
    { t: 'sub', all: false, sel: { 11: { 100: true } } },
  ])
  await expect(frame.getByRole('status')).toHaveText('Live')
  // The first live Call really plays — decoded and moving, not merely asked —
  // and the second follows it on the same element without another press.
  const audio = frame.locator('audio')
  await expect
    .poll(() => audio.evaluate((element: HTMLAudioElement) => element.src))
    .toContain('/api/call/7/audio')
  await expect
    .poll(() =>
      audio.evaluate(
        (element: HTMLAudioElement) => !element.paused && element.currentTime > 0,
      ),
    )
    .toBe(true)
  await expect
    .poll(() => audio.evaluate((element: HTMLAudioElement) => element.src), {
      timeout: 5_000,
    })
    .toContain('/api/call/8/audio')
  await expect
    .poll(() => audio.evaluate((element: HTMLAudioElement) => element.currentTime > 0))
    .toBe(true)
})

test('says so in the frame when the embed is gone', async ({ page, context, baseURL }) => {
  hostPage = framing(baseURL)
  await context.route('**/api/embed?*', (route) =>
    route.fulfill({ status: 404, body: 'embed not found\n' }),
  )

  await page.goto(HOST)

  await expect(
    page
      .frameLocator('iframe[title="Fire dispatch"]')
      .getByText('This feed is no longer available.'),
  ).toBeVisible()
})
