/**
 * "Resumes the feed on tap" (#16, spec US 32), for the tab that was already
 * open.
 *
 * A tap on a notification is a user gesture — but it happens in the service
 * worker, not in the page, so the page has to be told. The worker focuses the
 * existing tab and posts `push-open`; this is the page's end of that message,
 * and what it triggers is a plain `resume`: the listener came back, so the
 * transport should not be sitting paused behind a play button they never
 * pressed.
 *
 * A cold open needs none of this — a fresh page starts unpaused and the live
 * feed connects on its own.
 */

/** The message a tapped notification sends (`src/sw.ts`). */
export const PUSH_OPEN = 'push-open'

/** The part of `ServiceWorkerContainer` this needs. */
export interface MessageSource {
  addEventListener(type: 'message', listener: (event: MessageEvent) => void): void
  removeEventListener(
    type: 'message',
    listener: (event: MessageEvent) => void,
  ): void
}

/**
 * Call `resumed` whenever a tapped notification hands this page back to the
 * listener. Returns the release.
 *
 * A browser with no service worker gets a no-op release rather than an error —
 * the app runs without any of this.
 */
export function onPushOpen(
  resumed: () => void,
  source: MessageSource | undefined = navigator.serviceWorker,
): () => void {
  if (!source) return () => {}
  const listener = (event: MessageEvent) => {
    if ((event.data as { type?: string } | null)?.type === PUSH_OPEN) resumed()
  }
  source.addEventListener('message', listener)
  return () => source.removeEventListener('message', listener)
}
