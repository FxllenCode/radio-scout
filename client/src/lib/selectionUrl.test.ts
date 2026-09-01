import { describe, expect, it } from 'vitest'

import { EVERYTHING, isSelected, setSystem, setTalkgroups, type Selection } from './selection'
import { decodeSelection, encodeSelection } from './selectionUrl'

describe('a Selection as a link', () => {
  it('spells the default on its own when there is nothing to except', () => {
    expect(encodeSelection({ all: true, sel: {} })).toBe('1')
    expect(encodeSelection({ all: false, sel: {} })).toBe('0')
  })

  it('spells a Talkgroup turned off against an otherwise-on scanner', () => {
    expect(encodeSelection({ all: true, sel: { 100: { 5: false } } })).toBe('1_100.-5')
  })

  it("spells a System's wildcard, which is what carries Talkgroups nobody has heard of yet", () => {
    expect(
      encodeSelection({ all: false, sel: { 100: { '*': true, 5: false } } }),
    ).toBe('0_100.*.-5')
  })

  it('orders Systems and their entries, so one Selection is one link', () => {
    const scattered: Selection = {
      all: false,
      sel: { 200: { 7: true }, 100: { 12: true, '*': true, 5: true } },
    }
    expect(encodeSelection(scattered)).toBe('0_100.*.5.12_200.7')
  })

  it('uses only characters a query string carries unescaped', () => {
    const encoded = encodeSelection({ all: false, sel: { 100: { '*': true, 5: false } } })
    const params = new URLSearchParams()
    params.set('sel', encoded)
    expect(params.toString()).toBe(`sel=${encoded}`)
  })
})

describe('reading a Selection back off a link', () => {
  const CASES: Selection[] = [
    EVERYTHING,
    { all: false, sel: {} },
    { all: true, sel: { 100: { 5: false } } },
    { all: false, sel: { 100: { '*': true, 5: false }, 200: { 7: true } } },
    setSystem(setTalkgroups(EVERYTHING, [{ systemRef: 100, talkgroupRef: 5 }], false), 200, false),
  ]

  it.each(CASES)('survives the round trip: %j', (selection) => {
    expect(decodeSelection(encodeSelection(selection))).toEqual(selection)
  })

  it('answers the same question the sender was answering', () => {
    // The point of carrying the matrix rather than a list of what is on: a
    // Talkgroup neither side has heard of yet inherits the same default at both
    // ends (`lib/selection`, rule 2).
    const sent: Selection = { all: false, sel: { 100: { '*': true } } }
    const got = decodeSelection(encodeSelection(sent))!
    expect(isSelected(got, 100, 999)).toBe(isSelected(sent, 100, 999))
    expect(isSelected(got, 200, 999)).toBe(isSelected(sent, 200, 999))
  })
})

/**
 * A link is one statement, so a link that is not wholly readable is refused
 * whole — unlike a stored Selection, where a bad field costs the Listener that
 * field. Half of somebody else's scanner is not a thing anyone meant to send,
 * and applying it would replace a Selection they built by hand with a guess.
 */
describe('a link that says something unusable', () => {
  it.each([
    ['nothing at all', ''],
    ['no default', '_100.5'],
    ['a default that is neither', '2_100.5'],
    ['a System that is not a Ref', '1_alpha.5'],
    ['a Talkgroup that is not a Ref', '1_100.five'],
    ['a System with no entries', '1_100'],
    ['an empty entry', '1_100..5'],
    ['a bare minus', '1_100.-'],
    ['something from a different app entirely', '{"all":true}'],
  ])('refuses %s', (_what, encoded) => {
    expect(decodeSelection(encoded)).toBeUndefined()
  })
})
