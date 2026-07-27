import { http, HttpResponse } from 'msw'
import { afterEach, beforeEach, describe, expect, it } from 'vitest'

import { server } from '@/test/setup'
import { ORIGIN, VAPID_PUBLIC_KEY } from '@/test/handlers'
import { FAKE_ENDPOINT, FAKE_KEYS, fakePush } from '@/test/push'

import { createPush } from './push'
import { EVERYTHING, type Selection } from './selection'

/** Every subscribe the app posted, in order. */
let posted: unknown[] = []
let unsubscribed: unknown[] = []

beforeEach(() => {
  posted = []
  unsubscribed = []
  server.use(
    http.post(`${ORIGIN}/api/push/subscribe`, async ({ request }) => {
      posted.push(await request.json())
      return HttpResponse.json({ token: TOKEN })
    }),
    http.post(`${ORIGIN}/api/push/unsubscribe`, async ({ request }) => {
      unsubscribed.push(await request.json())
      return new HttpResponse(null, { status: 204 })
    }),
  )
})

/** A push handle, settled — `createPush` asks the server what it supports. */
async function push(env = fakePush()) {
  const handle = createPush({ environment: env })
  await handle.ready
  return handle
}

const ONE_TALKGROUP: Selection = { all: false, sel: { 11: { 54241: true } } }

/** The handle the server issues — unguessable, so one device cannot silence
 *  another's notifications. */
const TOKEN = 'a-subscription-token'

describe('turning notifications on', () => {
  it('is offered once the server and the browser can both do it', async () => {
    expect((await push()).state).toBe('off')
  })

  // The permission prompt is a one-shot per origin: spend it on page load and
  // the listener is asked about something they never asked for, and can never
  // be asked again. It happens on a tap, or not at all.
  it('asks for permission only when the listener asks for it', async () => {
    const env = fakePush()

    const handle = await push(env)

    expect(env.asked).toBe(0)
    await handle.enable(EVERYTHING)
    expect(env.asked).toBe(1)
  })

  it('subscribes with the server key and registers the Selection', async () => {
    const env = fakePush()
    const handle = await push(env)

    const state = await handle.enable(ONE_TALKGROUP)

    expect(state).toBe('on')
    expect(handle.token).toBe(TOKEN)
    expect(posted).toEqual([
      {
        endpoint: FAKE_ENDPOINT,
        keys: FAKE_KEYS,
        selection: ONE_TALKGROUP,
      },
    ])
    // The raw key bytes, not the base64url text — `subscribe` rejects a string
    // on some browsers and mis-reads it on others.
    expect(env.applicationServerKey).toBeInstanceOf(Uint8Array)
    expect((env.applicationServerKey as Uint8Array).length).toBe(65)
  })

  it('reports a refused permission as blocked, and registers nothing', async () => {
    const handle = await push(fakePush({ answer: 'denied' }))

    expect(await handle.enable(EVERYTHING)).toBe('blocked')
    expect(posted).toEqual([])
  })

  // Dismissing the prompt is not a no — the listener can be asked again.
  it('stays off when the listener dismisses the prompt', async () => {
    const handle = await push(fakePush({ answer: 'default' }))

    expect(await handle.enable(EVERYTHING)).toBe('off')
    expect(posted).toEqual([])
  })
})

describe('what a listener is told when they cannot have it', () => {
  it('says unsupported when the browser has no service worker', async () => {
    expect((await push(fakePush({ unsupported: true }))).state).toBe(
      'unsupported',
    )
  })

  // A server with no VAPID identity (`RADIO_SCOUT_VAPID_PRIVATE_KEY` unset and
  // unwritable) answers 404. Offering a switch that could only fail is worse
  // than saying so.
  it('says unavailable when the server has no identity', async () => {
    server.use(
      http.get(
        `${ORIGIN}/api/push/key`,
        () => new HttpResponse(null, { status: 404 }),
      ),
    )

    expect((await push()).state).toBe('unavailable')
  })

  it('says blocked when permission was already refused', async () => {
    expect((await push(fakePush({ permission: 'denied' }))).state).toBe(
      'blocked',
    )
  })
})

describe('a reload of an app that already had notifications on', () => {
  it('finds itself on, and asks the server for its token back', async () => {
    const handle = await push(fakePush({ permission: 'granted', subscribed: true }))

    expect(handle.state).toBe('on')
    expect(handle.token).toBe(TOKEN)
    expect(posted).toHaveLength(1)
  })

  // The Selection the listener chose lives on the server between visits. A
  // reload that sent one would overwrite it — and the default any page could
  // invent is "everything", which is a phone woken by Talkgroups nobody picked.
  it('does not overwrite the Selection it left there', async () => {
    await push(fakePush({ permission: 'granted', subscribed: true }))

    expect(posted).toEqual([
      { endpoint: FAKE_ENDPOINT, keys: FAKE_KEYS },
    ])
  })
})

describe('keeping the server in step with the Selection', () => {
  it('re-registers when the listener changes what they hear', async () => {
    const handle = await push()
    await handle.enable(EVERYTHING)

    await handle.sync(ONE_TALKGROUP)

    expect(posted).toHaveLength(2)
    expect(posted[1]).toMatchObject({ selection: ONE_TALKGROUP })
  })

  it('says nothing to the server while notifications are off', async () => {
    const handle = await push()

    await handle.sync(ONE_TALKGROUP)

    expect(posted).toEqual([])
  })

  // Every subscribe is a row write on a Pi; a screen that re-renders is not
  // news.
  it('does not repeat a Selection the server already has', async () => {
    const handle = await push()
    await handle.enable(ONE_TALKGROUP)

    await handle.sync(ONE_TALKGROUP)

    expect(posted).toHaveLength(1)
  })
})

describe('turning notifications off', () => {
  it('tells the server and the browser both', async () => {
    const env = fakePush()
    const handle = await push(env)
    await handle.enable(EVERYTHING)

    await handle.disable()

    expect(handle.state).toBe('off')
    expect(handle.token).toBeUndefined()
    // By token, not by endpoint: forgetting a device is something only that
    // device may do.
    expect(unsubscribed).toEqual([{ token: TOKEN }])
    expect(env.unsubscribed).toBe(true)
  })
})

describe('the change notifier', () => {
  it('tells a subscriber when the state changes, until released', async () => {
    const handle = await push()
    let changes = 0
    const release = handle.subscribe(() => (changes += 1))

    await handle.enable(EVERYTHING)
    expect(changes).toBe(1)

    release()
    await handle.disable()
    expect(changes).toBe(1)
  })
})

describe('the application server key', () => {
  it('decodes the base64url the server serves', async () => {
    const env = fakePush()
    const handle = await push(env)

    await handle.enable(EVERYTHING)

    // The uncompressed-point marker, and the last byte — the two that would
    // survive a base64url/base64 mix-up unnoticed if only the length were
    // checked.
    const bytes = env.applicationServerKey as Uint8Array
    expect(bytes[0]).toBe(0x04)
    expect(bytes[64]).toBe(0x0f)
    expect(VAPID_PUBLIC_KEY.endsWith('7A8')).toBe(true)
  })
})

describe('the browser it reaches for when nothing is injected', () => {
  /** jsdom has neither `PushManager` nor `navigator.serviceWorker`, so the
   *  browser's own answers to the port are only reachable by standing them up.
   *  What is asserted is the *decision*: a context without push support is
   *  `unsupported`, which on iOS is what "not installed" looks like. */
  const define = (name: string, value: unknown) =>
    Object.defineProperty(globalThis, name, { value, configurable: true })

  afterEach(() => {
    Reflect.deleteProperty(globalThis, 'PushManager')
    Reflect.deleteProperty(navigator, 'serviceWorker')
    Reflect.deleteProperty(globalThis, 'Notification')
  })

  it('is unsupported where the browser has no PushManager', async () => {
    const handle = createPush()
    await handle.ready

    expect(handle.state).toBe('unsupported')
  })

  it('reads the registration and the permission when it does', async () => {
    const registration = { pushManager: { getSubscription: async () => null } }
    define('PushManager', class {})
    Object.defineProperty(navigator, 'serviceWorker', {
      value: { ready: Promise.resolve(registration) },
      configurable: true,
    })
    define('Notification', { permission: 'denied' })

    const handle = createPush()
    await handle.ready

    // Permission was refused before this page loaded — read from the browser,
    // not from a fake.
    expect(handle.state).toBe('blocked')
  })

  it('asks the browser itself for permission', async () => {
    const registration = { pushManager: { getSubscription: async () => null } }
    define('PushManager', class {})
    Object.defineProperty(navigator, 'serviceWorker', {
      value: { ready: Promise.resolve(registration) },
      configurable: true,
    })
    let asked = 0
    define('Notification', {
      permission: 'default',
      requestPermission: async () => {
        asked += 1
        return 'denied'
      },
    })
    const handle = createPush()
    await handle.ready

    expect(await handle.enable(EVERYTHING)).toBe('blocked')
    expect(asked).toBe(1)
  })

  it('reports unavailable when the server cannot be reached at all', async () => {
    server.use(http.get(`${ORIGIN}/api/push/key`, () => HttpResponse.error()))

    const handle = createPush({ environment: fakePush() })
    await handle.ready

    expect(handle.state).toBe('unavailable')
  })
})
