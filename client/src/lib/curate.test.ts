import { describe, expect, it } from 'vitest'

import { parseRefs, splitList } from './curate'

describe('splitList', () => {
  /** An empty box is the **empty set**, not "leave it alone" — `groups`
   *  replaces, the CSV importer's rule (#18), so this is how a channel loses
   *  its last Group. */
  it.each([
    ['', []],
    ['   ', []],
    ['Fire', ['Fire']],
    ['Fire, Dispatch', ['Fire', 'Dispatch']],
    ['  Fire ,Dispatch  ', ['Fire', 'Dispatch']],
    // A stray comma is not a Group called "" — the server drops those too, so
    // the count shown before saving is the count that gets saved.
    ['Fire,,Dispatch', ['Fire', 'Dispatch']],
    [',', []],
    ['Fire Dispatch', ['Fire Dispatch']],
  ])('reads %o as %o', (raw, expected) => {
    expect(splitList(raw)).toEqual(expected)
  })
})

describe('parseRefs', () => {
  /** The one field where dropping a typo would be silent: rdio stores a
   *  System's blacklist as free text and ignores whatever will not parse, so a
   *  mistyped ref refuses nothing and says nothing. */
  it.each([
    ['', []],
    ['100', [100]],
    ['100, 200', [100, 200]],
    ['  100 ,200 ', [100, 200]],
    ['100,,200', [100, 200]],
  ])('reads %o as %o', (raw, expected) => {
    expect(parseRefs(raw)).toEqual(expected)
  })

  /** Anything that is not a whole number is refused as a whole, rather than
   *  quietly contributing nothing — the entry an Operator typed is the one they
   *  believe is blacklisted. */
  it.each(['fire', '100, fire', '1.5', '1e', 'NaN', '  x  '])(
    'refuses %o',
    (raw) => {
      expect(parseRefs(raw)).toBeUndefined()
    },
  )

  /** A ref may be negative in principle — the recorder's numbering is not ours
   *  to constrain here, and the server is the one that decides. */
  it('accepts a negative ref rather than second-guessing the recorder', () => {
    expect(parseRefs('-1')).toEqual([-1])
  })
})
