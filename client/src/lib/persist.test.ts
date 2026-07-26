import { describe, expect, it } from 'vitest'

import { EVERYTHING, setTalkgroups } from './selection'
import {
  loadSelection,
  namespaceOf,
  saveSelection,
  selectionKey,
} from './persist'

/** An in-memory `Storage`, so a test never depends on jsdom's shared one. */
function fakeStorage(seed: Record<string, string> = {}): Storage {
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
const hostileStorage: Storage = new Proxy(fakeStorage(), {
  get() {
    return () => {
      throw new DOMException('denied', 'SecurityError')
    }
  },
})

const NARROWED = setTalkgroups(EVERYTHING, [{ systemRef: 11, talkgroupRef: 100 }], false)

describe('the namespace (spec US 22)', () => {
  /** rdio reads `?id=`; a bookmarked second scanner keeps working here. */
  it('is named by ?id= in the URL', () => {
    expect(namespaceOf('?id=truck')).toBe('truck')
    expect(namespaceOf('?other=1&id=desk')).toBe('desk')
  })

  it('falls back to one shared scanner', () => {
    expect(namespaceOf('')).toBe('default')
    expect(namespaceOf('?id=')).toBe('default')
  })

  it('keeps two namespaces in separate keys', () => {
    expect(selectionKey('truck')).not.toBe(selectionKey('default'))
    expect(selectionKey('truck')).toContain('radio-scout')
  })
})

describe('persisting the selection', () => {
  it('survives a reload', () => {
    const storage = fakeStorage()

    saveSelection(storage, 'default', NARROWED)

    expect(loadSelection(storage, 'default')).toEqual(NARROWED)
  })

  it('is independent per namespace, so one browser runs two scanners', () => {
    const storage = fakeStorage()

    saveSelection(storage, 'truck', NARROWED)

    expect(loadSelection(storage, 'desk')).toBeUndefined()
    expect(loadSelection(storage, 'truck')).toEqual(NARROWED)
  })

  it('has nothing to say about a browser that has never chosen', () => {
    expect(loadSelection(fakeStorage(), 'default')).toBeUndefined()
  })

  /** rdio wipes *all* of local storage when it doesn't like what it reads. A
   *  selection we can't parse is one the listener has to make again — it is
   *  never a reason to touch anything else. */
  it.each([
    ['not json at all', 'wat'],
    ['json of the wrong shape', '{"all":"yes"}'],
    ['a matrix with a non-boolean entry', '{"all":true,"sel":{"11":{"100":"on"}}}'],
    ['null', 'null'],
    ['an array', '[]'],
    ['a matrix whose sel is null', '{"all":true,"sel":null}'],
    ['a matrix whose sel is an array', '{"all":true,"sel":[]}'],
    ['a matrix whose System entry is not an object', '{"all":true,"sel":{"11":true}}'],
    ['a matrix whose System entry is an array', '{"all":true,"sel":{"11":[]}}'],
  ])('ignores %s', (_case, stored) => {
    const storage = fakeStorage({ [selectionKey('default')]: stored, other: 'kept' })

    expect(loadSelection(storage, 'default')).toBeUndefined()
    expect(storage.getItem('other')).toBe('kept')
  })

  it('reads a wildcard entry back', () => {
    const stored = '{"all":false,"sel":{"11":{"*":true,"100":false}}}'
    const storage = fakeStorage({ [selectionKey('default')]: stored })

    expect(loadSelection(storage, 'default')).toEqual({
      all: false,
      sel: { '11': { '*': true, '100': false } },
    })
  })

  /** Storage can be denied outright; a listener with cookies blocked still gets
   *  a working scanner, just not a remembered one. */
  it('degrades to an unremembered scanner when storage is denied', () => {
    expect(loadSelection(hostileStorage, 'default')).toBeUndefined()
    expect(() => saveSelection(hostileStorage, 'default', NARROWED)).not.toThrow()
  })
})
