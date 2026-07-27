import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

import { selectionKey } from '@/lib/persist'
import { EVERYTHING, setTalkgroups } from '@/lib/selection'

import { chooseTalkgroups, received, selectSelection } from './live'
import { makeStore } from './store'

/** An in-memory `Storage` that also counts what was written, so a test can say
 *  "nothing was persisted" and mean it. */
function fakeStorage(seed: Record<string, string> = {}) {
  const map = new Map(Object.entries(seed))
  const writes: string[] = []
  const storage: Storage = {
    get length() {
      return map.size
    },
    clear: () => map.clear(),
    getItem: (key) => map.get(key) ?? null,
    key: (index) => [...map.keys()][index] ?? null,
    removeItem: (key) => void map.delete(key),
    setItem: (key, value) => {
      writes.push(value)
      map.set(key, value)
    },
  }
  return { storage, writes }
}

const NARROWED = setTalkgroups(EVERYTHING, [{ systemRef: 11, talkgroupRef: 100 }], false)

describe('makeStore', () => {
  /** Whether `globalThis.localStorage` exists at all differs by Node version —
   *  and two of these tests deliberately take the *default* storage rather than
   *  passing one. Left ambient, they assert something different on a laptop than
   *  on a runner, which is how CI found `{ storage: undefined }` silently
   *  falling back to the browser's. Pinned, they assert the same thing
   *  everywhere. */
  let ambient: ReturnType<typeof fakeStorage>

  beforeEach(() => {
    ambient = fakeStorage()
    vi.stubGlobal('localStorage', ambient.storage)
  })

  afterEach(() => {
    vi.unstubAllGlobals()
  })

  it('starts a listener who has never chosen on everything', () => {
    const { storage } = fakeStorage()

    const store = makeStore({ storage, namespace: 'default' })

    expect(selectSelection(store.getState())).toEqual(EVERYTHING)
  })

  it('starts from the selection this browser last made (spec US 22)', () => {
    const { storage } = fakeStorage({
      [selectionKey('default')]: JSON.stringify(NARROWED),
    })

    const store = makeStore({ storage, namespace: 'default' })

    expect(selectSelection(store.getState())).toEqual(NARROWED)
  })

  it('remembers a selection change as it happens', () => {
    const { storage } = fakeStorage()
    const store = makeStore({ storage, namespace: 'truck' })

    store.dispatch(
      chooseTalkgroups({ keys: [{ systemRef: 11, talkgroupRef: 100 }], on: false }),
    )

    expect(storage.getItem(selectionKey('truck'))).toBe(JSON.stringify(NARROWED))
    expect(storage.getItem(selectionKey('default'))).toBeNull()
  })

  /** A Call arrives every few seconds; writing local storage on each one would
   *  cost a phone battery for nothing. */
  it('writes nothing when a Call arrives', () => {
    const { storage, writes } = fakeStorage()
    const store = makeStore({ storage, namespace: 'default' })

    store.dispatch(
      received({
        call: { id: 1, systemRef: 11, talkgroupRef: 100, audioUrl: '/api/call/1/audio' },
      }),
    )

    expect(writes).toEqual([])
  })

  /** A browser with site data blocked — or a sandboxed context — still gets a
   *  working scanner, just not a remembered one.
   *
   *  Saying so out loud (`storage: undefined`) has to *mean* it. It used to fall
   *  through to a destructuring default and quietly take the browser's storage
   *  instead, so this test asserted nothing about the case it names and, worse,
   *  persisted a narrowed selection that the next test then read back. Hence the
   *  second assertion: unremembered means nothing was written anywhere. */
  it('runs unremembered when the browser has no storage', () => {
    const store = makeStore({ storage: undefined, namespace: 'default' })

    store.dispatch(
      chooseTalkgroups({ keys: [{ systemRef: 11, talkgroupRef: 100 }], on: false }),
    )

    expect(selectSelection(store.getState())).toEqual(NARROWED)
    expect(ambient.writes).toEqual([])
  })

  it('defaults to this browser and this tab’s scanner', () => {
    expect(selectSelection(makeStore().getState())).toEqual(EVERYTHING)
  })
})
