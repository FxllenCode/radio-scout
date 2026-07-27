/**
 * What a Web Push becomes on the device (#16, spec US 32).
 *
 * Pure on purpose. The service worker (`src/sw.ts`) is a different global scope
 * that neither Vitest nor `tsc -b`'s app project can reach comfortably, so
 * everything with a decision in it lives here and the worker stays glue.
 *
 * Two rules shape it, both from the platform rather than from taste:
 *
 * 1. **Every push must show a notification.** iOS revokes a subscription that
 *    receives one and shows nothing (research §6), so a payload we cannot read
 *    still becomes a notification — losing one notification's detail beats
 *    losing every future notification.
 * 2. **One notification per Talkgroup.** The server already coalesced by time;
 *    `tag` is the device's half of the same rule, so a busy shift replaces one
 *    line on the lock screen instead of burying it.
 */

/** The payload the server sends (`src/push.rs`). Deliberately small — a push
 *  service will not carry more than about 4 KB. */
export interface PushPayload {
  /** The Call to open on tap. */
  id: number
  systemRef: number
  talkgroupRef: number
  /** The System's label, if it has one. */
  system?: string
  /** The Talkgroup's label, if it has one. */
  talkgroup?: string
  /** How many Calls this notification stands for, the ones the server's
   *  coalescing window folded in included. */
  count: number
}

/** A notification, ready for `registration.showNotification`. */
export interface Notification {
  title: string
  options: NotificationOptions & {
    /** Not in TypeScript's DOM lib (it is a working-draft addition), and the
     *  thing that makes a replacement notification alert again rather than
     *  swap in silently. */
    renotify?: boolean
    tag?: string
    data?: { url: string }
  }
}

/** The mark on the lock screen — the app's own icon, already built for the
 *  manifest (#15). */
const ICON = '/icon-192.png'

export function notificationFor(payload: PushPayload | undefined): Notification {
  if (!isPayload(payload)) {
    // Rule 1: something, rather than nothing.
    return {
      title: 'Radio-Scout',
      options: { body: 'New activity', icon: ICON, data: { url: '/' } },
    }
  }

  const calls =
    payload.count > 1 ? `${payload.count} new calls` : 'New call'
  return {
    title: payload.talkgroup ?? `Talkgroup ${payload.talkgroupRef}`,
    options: {
      body: payload.system ? `${calls} · ${payload.system}` : calls,
      icon: ICON,
      tag: `rs-${payload.systemRef}-${payload.talkgroupRef}`,
      renotify: true,
      data: { url: `/?call=${payload.id}` },
    },
  }
}

/** Is this the payload we send, rather than a push from something else — or a
 *  body that never decoded? */
function isPayload(payload: PushPayload | undefined): payload is PushPayload {
  return (
    typeof payload?.talkgroupRef === 'number' &&
    typeof payload.systemRef === 'number' &&
    typeof payload.count === 'number'
  )
}

/** The parts of a `WindowClient` this decision needs. */
export interface OpenWindow {
  url: string
}

/**
 * The already-open window a tap should focus, or `undefined` to open a new one.
 *
 * Focusing beats opening: a listener who taps a notification while the app is
 * open in a tab should land in *that* tab, with its queue and its selection,
 * not in a second copy of the app competing for the same audio.
 */
export function targetFor<T extends OpenWindow>(
  open: readonly T[],
  origin: string,
): T | undefined {
  return open.find((client) => client.url.startsWith(origin))
}
