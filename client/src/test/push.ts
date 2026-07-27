/**
 * A fake `PushManager` for the #16 tests.
 *
 * The browser half of Web Push has no jsdom implementation at all — no
 * `PushManager`, no `Notification` — so `createPush` takes its world as a port
 * ([`PushEnvironment`](../lib/push.ts)) and this is what a test hands it. It is
 * a *fake*, not a mock: it holds a real subscription and answers from it, so a
 * test asserts on what the app did rather than on which method it called.
 */
import type {
  PushEnvironment,
  PushRegistration,
  PushSubscriptionLike,
} from '@/lib/push'

/** The endpoint a fake subscription reports. */
export const FAKE_ENDPOINT = 'https://push.example.net/p/abc'

export const FAKE_KEYS = {
  p256dh:
    'BCVxsr7N_eNgVRqvHtD0zTZsEc6-VV-JvLexhqUzORcxaOzi6-AYWXvTBHm4bjyPjs7Vd8pZGH6SRpkNtoIAiw4',
  auth: 'BTBZMqHH6r4Tts7J_aSIgg',
}

export interface FakePush extends PushEnvironment {
  /** The `applicationServerKey` the app subscribed with, if it did. */
  applicationServerKey?: BufferSource | string | null
  /** Whether the live subscription has been unsubscribed. */
  unsubscribed: boolean
  /** How many times permission was asked for — the "one gesture" rule. */
  asked: number
}

export interface FakePushOptions {
  /** What the browser answers when asked for permission. */
  answer?: NotificationPermission
  /** The permission before anything is asked. */
  permission?: NotificationPermission
  /** Start already subscribed, as a reload of an app that has notifications on
   *  would find it. */
  subscribed?: boolean
  /** No service worker at all — a browser that cannot do this. */
  unsupported?: boolean
}

export function fakePush({
  answer = 'granted',
  permission = 'default',
  subscribed = false,
  unsupported = false,
}: FakePushOptions = {}): FakePush {
  let current: PushSubscriptionLike | null = null
  const env: FakePush = {
    asked: 0,
    unsubscribed: false,

    permission: () => permission,

    async requestPermission() {
      env.asked += 1
      permission = answer
      return answer
    },

    async registration() {
      if (unsupported) return undefined
      const registration: PushRegistration = {
        pushManager: {
          async getSubscription() {
            return current
          },
          async subscribe(options) {
            env.applicationServerKey = options.applicationServerKey
            current = subscription()
            return current
          },
        },
      }
      return registration
    },
  }

  function subscription(): PushSubscriptionLike {
    return {
      endpoint: FAKE_ENDPOINT,
      toJSON: () => ({ endpoint: FAKE_ENDPOINT, keys: FAKE_KEYS }),
      async unsubscribe() {
        env.unsubscribed = true
        current = null
        return true
      },
    }
  }

  if (subscribed) current = subscription()
  return env
}
