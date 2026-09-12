import { describe, expect, it } from 'vitest'

import {
  accessOf,
  clearGrant,
  isGrant,
  loadGrant,
  lockedCount,
  lockedSentence,
  saveGrant,
  staleNotice,
  unlockFailure,
  withGrant,
} from './access'

import type { Catalog } from '@/types'

const GRANT = `rsg_${'a1b2c3d4'.repeat(4)}`

/** A storage that remembers, and one that refuses — both of which a real
 *  browser is (Safari in private mode throws on `setItem`). */
function memory(): Storage {
  const held = new Map<string, string>()
  return {
    getItem: (key) => held.get(key) ?? null,
    setItem: (key, value) => void held.set(key, value),
    removeItem: (key) => void held.delete(key),
    clear: () => held.clear(),
    key: () => null,
    length: 0,
  } as Storage
}

function denied(): Storage {
  return {
    getItem: () => {
      throw new Error('denied')
    },
    setItem: () => {
      throw new Error('denied')
    },
    removeItem: () => {
      throw new Error('denied')
    },
    clear: () => {},
    key: () => null,
    length: 0,
  } as Storage
}

function catalog(talkgroups: { ref: number; locked?: boolean }[]): Catalog {
  return {
    systems: [{ ref: 11, talkgroups: talkgroups.map((tg) => ({ ...tg, groups: [] })) }],
    activityWindowMs: 86_400_000,
    sharing: true,
    export: { enabled: true, maxCalls: 1000 },
    starred: { kept: false, keptDays: 0 },
  }
}

describe('withGrant', () => {
  it('puts the grant on the query string, whatever else is already there', () => {
    expect(withGrant('/api/calls', GRANT)).toBe(`/api/calls?grant=${GRANT}`)
    expect(withGrant('/api/calls?sort=oldest', GRANT)).toBe(
      `/api/calls?sort=oldest&grant=${GRANT}`,
    )
  })

  /** Holding nothing is the ordinary state — every Listener starts there, and
   *  on an Instance that gates nothing they stay there. */
  it('leaves a url alone when there is no grant', () => {
    expect(withGrant('/api/calls')).toBe('/api/calls')
    expect(withGrant('/api/calls', '')).toBe('/api/calls')
  })

  /** Relative paths are the point: a `<audio src>` and a `fetch` both take one,
   *  and `new URL` would need a base a worker does not have. */
  it('works on a websocket url too, which is the one absolute case', () => {
    expect(withGrant('ws://scanner.local/api/live', GRANT)).toBe(
      `ws://scanner.local/api/live?grant=${GRANT}`,
    )
  })
})

describe('isGrant', () => {
  it('accepts what this Instance mints and nothing else', () => {
    expect(isGrant(GRANT)).toBe(true)
    // A hand-edited or half-written value would otherwise be sent on every
    // request forever and answered with a `stale` the app would act on.
    for (const wrong of [
      '',
      'rsg_',
      'rsg_nothex0000000000000000000000zz',
      `rsg_${'a'.repeat(31)}`,
      `rsg_${'a'.repeat(33)}`,
      'rsk_' + 'a'.repeat(32),
      undefined,
      null,
      42,
      { grant: GRANT },
    ]) {
      expect(isGrant(wrong), String(wrong)).toBe(false)
    }
  })
})

describe('remembering a grant', () => {
  it('round-trips, and forgetting really forgets', () => {
    const storage = memory()

    expect(loadGrant(storage)).toBeUndefined()
    saveGrant(storage, GRANT)
    expect(loadGrant(storage)).toBe(GRANT)

    clearGrant(storage)
    expect(loadGrant(storage)).toBeUndefined()
  })

  /** A stored value we did not write is ignored rather than sent — `persist`'s
   *  own rule, and here it is the difference between an unusable credential and
   *  a request that quietly answers with less than it should. */
  it('ignores a stored value that is not a grant', () => {
    const storage = memory()
    storage.setItem('radio-scout:access:grant', 'hunter2')

    expect(loadGrant(storage)).toBeUndefined()
  })

  /** Storage being denied is a supported state, not an error: the scanner still
   *  runs, just unremembered. */
  it('survives a browser that refuses to remember anything', () => {
    const storage = denied()

    expect(() => saveGrant(storage, GRANT)).not.toThrow()
    expect(() => clearGrant(storage)).not.toThrow()
    expect(loadGrant(storage)).toBeUndefined()
    expect(loadGrant(undefined)).toBeUndefined()
  })
})

describe('what the catalog says', () => {
  /** An Instance that gates nothing sends no `access` block at all, which is
   *  what makes its catalog document byte-identical to the one it served before
   *  this feature existed. */
  it('reads an absent block as an Instance that gates nothing', () => {
    expect(accessOf(undefined)).toEqual({ gating: false })
    expect(accessOf(catalog([{ ref: 100 }]))).toEqual({ gating: false })
  })

  it('counts the locked rows, which is the sentence that makes the control worth tapping', () => {
    expect(lockedCount(undefined)).toBe(0)
    expect(lockedCount(catalog([{ ref: 100 }, { ref: 200 }]))).toBe(0)
    expect(
      lockedCount(
        catalog([{ ref: 100 }, { ref: 200, locked: true }, { ref: 300, locked: true }]),
      ),
    ).toBe(2)
  })
})

describe('what a Listener is told', () => {
  /** A plural that reads "1 channels" is a bug nobody writes a component test
   *  for, which is why the sentence is a value. */
  it('counts the locked channels in words that agree with the number', () => {
    expect(lockedSentence(1)).toMatch(/^One channel /)
    expect(lockedSentence(0)).toMatch(/^0 channels /)
    expect(lockedSentence(2)).toMatch(/^2 channels /)
  })

  /** Two sentences, because they are two different things to go and do: an
   *  expired code is a new code from the same person, and an unknown one has
   *  been revoked. */
  it('tells an expired grant apart from a revoked one', () => {
    expect(staleNotice('expired')).not.toBe(staleNotice('unknown'))
    expect(staleNotice('expired')).toMatch(/expired/i)
    expect(staleNotice('unknown')).toMatch(/revoked|changed/i)
  })

  /** Keyed on the status, because the bodies are plain text a Listener should
   *  never be shown and the three outcomes an unlock has are three statuses. */
  it('renders every refusal an unlock can answer with', () => {
    expect(unlockFailure(401)).toMatch(/doesn't work/i)
    expect(unlockFailure(410)).toMatch(/expired/i)
    expect(unlockFailure(429)).toMatch(/too many/i)
    // A 500, a dropped connection, anything else: the honest answer is that we
    // could not check, which is not the same as saying the code was wrong.
    expect(unlockFailure(500)).toMatch(/try again/i)
    expect(unlockFailure(undefined)).toMatch(/try again/i)
  })
})
