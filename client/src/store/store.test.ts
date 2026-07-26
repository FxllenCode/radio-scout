import { describe, expect, it } from 'vitest'

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

  /** A browser with site data blocked (and this jsdom, which has no local
   *  storage at all) still gets a working scanner — just not a remembered one. */
  it('runs unremembered when the browser has no storage', () => {
    const store = makeStore({ storage: undefined, namespace: 'default' })

    store.dispatch(
      chooseTalkgroups({ keys: [{ systemRef: 11, talkgroupRef: 100 }], on: false }),
    )

    expect(selectSelection(store.getState())).toEqual(NARROWED)
  })

  it('defaults to this browser and this tab’s scanner', () => {
    expect(selectSelection(makeStore().getState())).toEqual(EVERYTHING)
  })
})
