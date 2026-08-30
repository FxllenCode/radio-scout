import { describe, expect, it } from 'vitest'

import { fakeStorage, hostileStorage } from '@/test/storage'

import { EVERYTHING, setTalkgroups } from './selection'
import {
  feedOffKey,
  loadFeedOff,
  loadPriority,
  loadSelection,
  namespaceOf,
  priorityKey,
  saveFeedOff,
  savePriority,
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

/**
 * **Priority** is part of a **Profile** (#58, spec US 27): a Listener who
 * picked their dispatch channels out of four hundred Talkgroups has done real
 * work, and it must not cost them a reload.
 *
 * Its own key, for [`avoidsKey`]'s reason: a list an older build cannot read
 * costs the Listener that list alone, never their Selection.
 */
describe('persisting Priority (#58)', () => {
  it('survives a reload', () => {
    const storage = fakeStorage()

    savePriority(storage, 'default', ['11:100', '12:900'])

    expect(loadPriority(storage, 'default')).toEqual(['11:100', '12:900'])
  })

  it('is independent per Profile', () => {
    const storage = fakeStorage()

    savePriority(storage, 'truck', ['11:100'])

    expect(loadPriority(storage, 'truck')).toEqual(['11:100'])
    expect(loadPriority(storage, 'desk')).toBeUndefined()
  })

  /** Every entry names a Talkgroup, or the ordering would be keyed by something
   *  no Call can ever match — a **Pin**'s rule, and here it decides what plays
   *  next rather than only what a row looks like. */
  it.each([
    ['not a list', '{"11:100":true}'],
    ['a key that is not a Talkgroup', '["oops"]'],
    ['a key that is not even a string', '[7]'],
    ['half-written JSON', '["11:'],
  ])('has nothing to say about %s', (_what, stored) => {
    expect(
      loadPriority(fakeStorage({ [priorityKey('default')]: stored }), 'default'),
    ).toBeUndefined()
  })

  it('reads an empty list back as one, which is a Listener who cleared theirs', () => {
    expect(
      loadPriority(fakeStorage({ [priorityKey('default')]: '[]' }), 'default'),
    ).toEqual([])
  })

  it('runs unremembered on a browser that denies storage', () => {
    expect(() => savePriority(hostileStorage, 'default', ['11:100'])).not.toThrow()
    expect(loadPriority(hostileStorage, 'default')).toBeUndefined()
  })
})
