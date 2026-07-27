/**
 * Web Push, from the listener's side (#16, spec US 32, ADR-0005).
 *
 * The one thing this module exists to get right is **when the listener is
 * asked**. `Notification.requestPermission()` is a one-shot per origin: a page
 * that spends it on load asks about something nobody asked for, is refused, and
 * can never ask again — on iOS the only way back is deleting the home-screen
 * app. So it is spent on a tap ([`Push.enable`]) and nowhere else, and
 * everything before that tap is a state a switch can render.
 *
 * The browser half has no jsdom implementation at all, so it arrives as a port
 * ([`PushEnvironment`]) rather than being reached for directly — the same shape
 * `install.ts` and `serviceWorker.ts` use, and what lets the whole flow be
 * tested against MSW at the network boundary.
 */

import { notifier } from './notifier'
import type { Selection } from './selection'

/** What the app can offer right now. */
export type PushState =
  /** This browser cannot: no service worker, or no push in this context —
   *  which on iOS is what "not installed to the Home Screen" looks like. */
  | 'unsupported'
  /** The browser could, but the server has no VAPID identity. */
  | 'unavailable'
  /** The listener has refused, and only their browser settings can undo it. */
  | 'blocked'
  /** Available, and off. */
  | 'off'
  /** Subscribed. */
  | 'on'

/** The parts of `PushSubscription` this needs. */
export interface PushSubscriptionLike {
  endpoint: string
  toJSON(): { keys?: { p256dh?: string; auth?: string } }
  unsubscribe(): Promise<boolean>
}

/** The parts of `PushManager` this needs. */
export interface PushManagerLike {
  getSubscription(): Promise<PushSubscriptionLike | null>
  subscribe(options: {
    userVisibleOnly: boolean
    applicationServerKey: Uint8Array
  }): Promise<PushSubscriptionLike>
}

export interface PushRegistration {
  pushManager: PushManagerLike
}

/** The browser, as the two things this module actually needs from it. */
export interface PushEnvironment {
  /** The service-worker registration to subscribe through, or `undefined` in a
   *  browser (or context) that has none. */
  registration(): Promise<PushRegistration | undefined>
  permission(): NotificationPermission | undefined
  requestPermission(): Promise<NotificationPermission>
}

export interface Push {
  /** Settles once the server and the browser have been asked what they can do.
   *  Everything else is meaningful only after it. */
  readonly ready: Promise<void>
  readonly state: PushState
  /** The token the server issued for this device, which the live-feed socket
   *  presents so the server knows this listener is listening and holds its
   *  notifications (`src/push.rs`). Unguessable on purpose: it is also what
   *  unsubscribing proves itself with. */
  readonly token: string | undefined
  subscribe(listener: () => void): () => void
  /** Ask for permission and register. **Call this from a user gesture**; it is
   *  the only thing that ever spends the permission prompt. */
  enable(selection: Selection): Promise<PushState>
  disable(): Promise<void>
  /** Tell the server what this listener now hears, if they are subscribed. */
  sync(selection: Selection): Promise<void>
}

export interface PushOptions {
  environment?: PushEnvironment
}

/** The browser's own answer to [`PushEnvironment`]. */
const browser: PushEnvironment = {
  async registration() {
    const container = navigator.serviceWorker as
      | ServiceWorkerContainer
      | undefined
    // `PushManager` is exposed to an installed iOS web app and to nothing else,
    // so its absence *is* the install gate on the platform that has one.
    if (!container || typeof globalThis.PushManager === 'undefined') {
      return undefined
    }
    return (await container.ready) as unknown as PushRegistration
  },
  permission: () => globalThis.Notification?.permission,
  requestPermission: () => globalThis.Notification.requestPermission(),
}

export function createPush({
  environment = browser,
}: PushOptions = {}): Push {
  let state: PushState = 'unsupported'
  let token: string | undefined
  /** The Selection the server has for us, so a re-render is not a row write. */
  let saved: string | undefined
  let key: string | undefined
  let manager: PushManagerLike | undefined
  const changes = notifier()

  const settle = (next: PushState) => {
    if (state === next) return
    state = next
    changes.changed()
  }

  /**
   * Hand the subscription to the server, keeping the token it answers with.
   *
   * `selection` is optional, and its absence is meaningful: a reload
   * re-subscribes only to learn its token back, and sending a default
   * Selection there would overwrite what the listener chose on their last
   * visit — a phone notified about Talkgroups nobody picked.
   */
  const save = async (
    subscription: PushSubscriptionLike,
    selection?: Selection,
  ) => {
    const { keys } = subscription.toJSON()
    const response = await fetch('/api/push/subscribe', {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ endpoint: subscription.endpoint, keys, selection }),
    })
    if (!response.ok) return false
    const body = (await response.json()) as { token?: string }
    token = body.token
    if (selection) saved = JSON.stringify(selection)
    return true
  }

  const ready = (async () => {
    const [registration, served] = await Promise.all([
      environment.registration(),
      fetch('/api/push/key').then((response) =>
        response.ok
          ? (response.json() as Promise<{ key?: string }>)
          : undefined,
      ),
    ])
    if (!registration) return settle('unsupported')
    if (!served?.key) return settle('unavailable')

    manager = registration.pushManager
    key = served.key
    if (environment.permission() === 'denied') return settle('blocked')

    // A reload of an app that already had notifications on: the browser still
    // holds the subscription, but this page does not yet hold its token, so it
    // subscribes again — without a Selection, which the server already has.
    const existing = await manager.getSubscription()
    if (!existing) return settle('off')
    await save(existing)
    settle('on')
  })().catch(() => {
    // A server that cannot be reached at all is not a reason to render a
    // broken switch.
    settle('unavailable')
  })

  return {
    ready,
    subscribe: changes.subscribe,

    get state() {
      return state
    },

    get token() {
      return token
    },

    async enable(selection) {
      if (!manager || !key) return state
      const permission = await environment.requestPermission()
      if (permission === 'denied') {
        settle('blocked')
        return state
      }
      // "default" is the prompt dismissed — not a refusal, so the listener can
      // be asked again.
      if (permission !== 'granted') return state

      const subscription =
        (await manager.getSubscription()) ??
        (await manager.subscribe({
          // Required — and true of us: every push shows a notification.
          userVisibleOnly: true,
          applicationServerKey: decodeBase64Url(key),
        }))
      if (await save(subscription, selection)) settle('on')
      return state
    },

    async disable() {
      const subscription = await manager?.getSubscription()
      if (subscription) {
        // The server first: a browser-side unsubscribe that raced a failed
        // request would leave a row nothing can ever deliver to. The token is
        // what proves this device may forget itself.
        if (token) {
          await fetch('/api/push/unsubscribe', {
            method: 'POST',
            headers: { 'content-type': 'application/json' },
            body: JSON.stringify({ token }),
          })
        }
        await subscription.unsubscribe()
      }
      token = undefined
      saved = undefined
      settle('off')
    },

    async sync(selection) {
      if (state !== 'on' || !manager) return
      if (JSON.stringify(selection) === saved) return
      const subscription = await manager.getSubscription()
      if (subscription) await save(subscription, selection)
    },
  }
}

/**
 * base64url → bytes, for `applicationServerKey`.
 *
 * `atob` speaks standard base64 with padding; VAPID keys are base64url without
 * it, and the two differ in exactly the characters that appear in a random
 * 65-byte key — so a key run through the wrong one is wrong on most keys and
 * right on some, which is the worst kind of bug to own.
 */
export function decodeBase64Url(value: string): Uint8Array {
  const padded = value.replace(/-/g, '+').replace(/_/g, '/')
  const binary = atob(padded.padEnd(Math.ceil(padded.length / 4) * 4, '='))
  return Uint8Array.from(binary, (character) => character.charCodeAt(0))
}
