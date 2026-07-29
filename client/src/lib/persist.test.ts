import { describe, expect, it } from 'vitest'

import { fakeStorage, hostileStorage } from '@/test/storage'

import { EVERYTHING, setTalkgroups } from './selection'
import {
  feedOffKey,
  loadFeedOff,
  loadSelection,
  namespaceOf,
  saveFeedOff,
  saveSelection,
  selectionKey,
} from './persist'

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

/** The feed-off switch (#80), remembered beside the Selection so a Listener who
 *  chose silence is not blasted with audio by a reload. */
describe('persisting the feed-off switch', () => {
  it('survives a reload, both ways round', () => {
    const storage = fakeStorage()

    saveFeedOff(storage, 'default', true)
    expect(loadFeedOff(storage, 'default')).toBe(true)

    saveFeedOff(storage, 'default', false)
    expect(loadFeedOff(storage, 'default')).toBe(false)
  })

  /**
   * Three answers, not two: `true`, `false`, and **never said**.
   *
   * The third is why this returns an optional rather than defaulting to `false`
   * itself. "Switched on deliberately" and "never touched" happen to lead to the
   * same place today, and collapsing them here would make that a coincidence the
   * caller could not undo — a future default-off Instance, say, could no longer
   * tell which it was looking at.
   */
  it('tells a remembered `false` apart from never having been told', () => {
    expect(
      loadFeedOff(fakeStorage({ [feedOffKey('default')]: 'false' }), 'default'),
    ).toBe(false)
    expect(loadFeedOff(fakeStorage(), 'default')).toBeUndefined()
  })

  /** A hand-edited or half-written value is not a boolean, and guessing at one
   *  would be worse than admitting we do not know. */
  it('has nothing to say about a value it did not write', () => {
    for (const junk of ['maybe', '1', 'TRUE', '', '{}']) {
      expect(
        loadFeedOff(fakeStorage({ [feedOffKey('default')]: junk }), 'default'),
      ).toBeUndefined()
    }
  })

  it('is independent per Profile', () => {
    const storage = fakeStorage()

    saveFeedOff(storage, 'truck', true)

    expect(loadFeedOff(storage, 'truck')).toBe(true)
    expect(loadFeedOff(storage, 'desk')).toBeUndefined()
  })

  it('degrades to an unremembered Profile when storage is denied', () => {
    expect(loadFeedOff(hostileStorage, 'default')).toBeUndefined()
    expect(() => saveFeedOff(hostileStorage, 'default', true)).not.toThrow()
  })
})
