/**
 * "Tell me when this changed" — the subscription half of a handle.
 *
 * Both browser-facing handles in the client ([`createInstall`], and the service
 * worker's) are the same shape: the browser decides when something happens, we
 * hold no value the caller could poll for cheaply, and React needs to be told.
 * Written once so the two can't drift on what unsubscribing or tearing down
 * means.
 */
export interface Notifier {
  /** Be told on the next change; returns the release. */
  subscribe(listener: () => void): () => void
  /** Something changed — tell everyone. */
  changed(): void
  /** Forget every listener, for a handle being torn down. */
  clear(): void
}

export function notifier(): Notifier {
  const listeners = new Set<() => void>()
  return {
    subscribe(listener) {
      listeners.add(listener)
      return () => listeners.delete(listener)
    },
    changed() {
      for (const listener of listeners) listener()
    },
    clear() {
      listeners.clear()
    },
  }
}
