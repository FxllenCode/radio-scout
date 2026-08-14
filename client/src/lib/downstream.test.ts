import { describe, expect, it } from 'vitest'

import type { AdminDownstream } from '@/types'

import {
  EVERYTHING,
  NOTHING,
  forwardsEverything,
  healthLine,
  isEditable,
  parseRefs,
  scopeOf,
  scopeRows,
} from './downstream'
import type { Selection } from './selection'

/** A healthy peer, for the health line's cases to vary one thing from. */
function peer(row: Partial<AdminDownstream> = {}): AdminDownstream {
  return {
    id: 1,
    label: 'county mirror',
    url: 'https://peer.example',
    scope: EVERYTHING,
    disabled: false,
    hasKey: true,
    queued: 0,
    lastSuccessMs: 1_700_000_000_000,
    lastFailureMs: null,
    lastFailure: null,
    consecutiveFailures: 0,
    createdAtMs: 1_600_000_000_000,
    ...row,
  }
}

describe('a Downstream scope, as a form edits it', () => {
  it('reads a whole System as one row with no Talkgroups', () => {
    const scope: Selection = { all: false, sel: { 11: { '*': true } } }

    expect(scopeRows(scope)).toEqual([{ systemRef: 11, talkgroupRefs: [] }])
  })

  it('reads named Talkgroups in ascending order, whatever order they were stored in', () => {
    const scope: Selection = {
      all: false,
      sel: { 11: { 300: true, 100: true, 200: true } },
    }

    expect(scopeRows(scope)).toEqual([
      { systemRef: 11, talkgroupRefs: [100, 200, 300] },
    ])
  })

  it('orders Systems by Ref, so the list does not reshuffle between renders', () => {
    const scope: Selection = {
      all: false,
      sel: { 22: { '*': true }, 11: { '*': true } },
    }

    expect(scopeRows(scope).map((row) => row.systemRef)).toEqual([11, 22])
  })

  /** The round trip is what makes the form safe to open on a saved scope. */
  it.each<[string, Selection]>([
    ['everything', EVERYTHING],
    ['nothing', NOTHING],
    ['a whole System', { all: false, sel: { 11: { '*': true } } }],
    ['named channels', { all: false, sel: { 11: { 100: true, 101: true } } }],
    [
      'two Systems, differently scoped',
      { all: false, sel: { 11: { '*': true }, 22: { 500: true } } },
    ],
  ])('round-trips %s', (_what, scope) => {
    expect(scopeOf(scopeRows(scope))).toEqual(
      forwardsEverything(scope) ? NOTHING : scope,
    )
    expect(isEditable(scope)).toBe(true)
  })

  /** **A scope the rows cannot draw is not silently narrowed.** The matrix can
   *  say "everything except this channel"; a row list cannot, and a Save built
   *  from those rows would forward less than the Operator asked for — or, on the
   *  `all` case, a great deal more. */
  it.each<[string, Selection]>([
    ['an exception', { all: false, sel: { 11: { '*': true, 100: false } } }],
    ['everything, narrowed', { all: true, sel: { 11: { 100: false } } }],
    [
      'a wildcard beside a named channel',
      { all: false, sel: { 11: { '*': true, 100: true } } },
    ],
  ])('reports %s as not editable here', (_what, scope) => {
    expect(isEditable(scope)).toBe(false)
  })

  it('is "everything" only when nothing has been carved out of it', () => {
    expect(forwardsEverything(EVERYTHING)).toBe(true)
    expect(forwardsEverything(NOTHING)).toBe(false)
    expect(forwardsEverything({ all: true, sel: { 11: { 100: false } } })).toBe(
      false,
    )
  })

  it('drops a half-typed row rather than keying a scope on NaN', () => {
    expect(scopeOf([{ systemRef: Number.NaN, talkgroupRefs: [100] }])).toEqual(
      NOTHING,
    )
  })

  /** Two rows for one System add up. The alternative is the second silently
   *  replacing the first, which looks like the form losing what was typed. */
  it('merges two rows naming the same System', () => {
    const scope = scopeOf([
      { systemRef: 11, talkgroupRefs: [100] },
      { systemRef: 11, talkgroupRefs: [101] },
    ])

    expect(scope).toEqual({ all: false, sel: { 11: { 100: true, 101: true } } })
  })

  it.each([
    ['100, 101', [100, 101]],
    ['100 101', [100, 101]],
    ['100,,101', [100, 101]],
    ['  ', []],
    ['100, fire', [100]],
  ])('reads %o as Talkgroup Refs', (raw, expected) => {
    expect(parseRefs(raw)).toEqual(expected)
  })
})

describe("a peer's health, in one line", () => {
  it('says a working peer is working, and nothing else', () => {
    const line = healthLine(peer())

    expect(line).toContain('https://peer.example')
    expect(line).toContain('last delivered')
    expect(line).not.toContain('queued')
    expect(line).not.toContain('failed')
    expect(line).not.toContain('needs its key')
  })

  /** The number that says whether anything is wrong *right now* — and the whole
   *  point of the durable queue: an hour of Calls waiting, not lost. */
  it('leads with the queue depth and names the last failure', () => {
    const line = healthLine(
      peer({
        queued: 12,
        consecutiveFailures: 4,
        lastFailure: 'peer-refused (503)',
        lastSuccessMs: null,
      }),
    )

    expect(line).toContain('12 queued')
    expect(line).toContain('4 failed · peer-refused (503)')
    expect(line).toContain('never delivered')
  })

  /** A peer restored from a backup has no credential, because a backup carries
   *  a peer's shape and never its key. Without this it is indistinguishable
   *  from a peer whose key is simply wrong. */
  it('calls out a peer that still needs its key', () => {
    expect(healthLine(peer({ hasKey: false, disabled: true }))).toContain(
      'needs its key',
    )
    expect(healthLine(peer({ disabled: true }))).toContain('disabled')
  })

  /** A failure with no slug behind it still reads as a sentence rather than
   *  trailing off — the server can leave the column null on a row whose
   *  failures predate it. */
  it('reports a failure count with no reason attached', () => {
    expect(healthLine(peer({ consecutiveFailures: 2 }))).toContain('2 failed')
  })
})
