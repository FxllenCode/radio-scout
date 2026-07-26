/**
 * `Storage` stand-ins, so a test never depends on jsdom's shared one — and can
 * say "this browser refuses to remember anything" out loud.
 */

/** An in-memory `Storage`. */
export function fakeStorage(seed: Record<string, string> = {}): Storage {
  const map = new Map(Object.entries(seed))
  return {
    get length() {
      return map.size
    },
    clear: () => map.clear(),
    getItem: (key) => map.get(key) ?? null,
    key: (index) => [...map.keys()][index] ?? null,
    removeItem: (key) => void map.delete(key),
    setItem: (key, value) => void map.set(key, value),
  }
}

/** A `Storage` that refuses everything — Safari in private mode, or a browser
 *  with site data blocked. */
export const hostileStorage: Storage = new Proxy(fakeStorage(), {
  get() {
    return () => {
      throw new DOMException('denied', 'SecurityError')
    }
  },
})
