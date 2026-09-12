import { describe, expect, it } from 'vitest'

import {
  accessReducer,
  grantExpired,
  grantHeld,
  grantReleased,
  initialAccessState,
  selectGrant,
  selectStaleGrant,
  staleNoticeDismissed,
  type AccessState,
} from './access'

const GRANT = `rsg_${'a1b2c3d4'.repeat(4)}`

const after = (state: AccessState, ...actions: Parameters<typeof accessReducer>[1][]) =>
  actions.reduce(accessReducer, state)

describe('holding a grant', () => {
  it('starts holding nothing, which is where every Listener starts', () => {
    expect(initialAccessState).toEqual({})
    expect(selectGrant({ access: initialAccessState })).toBeUndefined()
  })

  it('holds what an unlock handed back', () => {
    const state = after(initialAccessState, grantHeld(GRANT))

    expect(selectGrant({ access: state })).toBe(GRANT)
  })

  /** A Listener who has just unlocked is not owed a notice about the grant they
   *  replaced. */
  it('clears an outstanding notice when a new grant arrives', () => {
    const state = after(
      initialAccessState,
      grantHeld(GRANT),
      grantExpired('expired'),
      grantHeld(GRANT),
    )

    expect(selectGrant({ access: state })).toBe(GRANT)
    expect(selectStaleGrant({ access: state })).toBeUndefined()
  })
})

describe('letting one go', () => {
  /** Dropped rather than kept and ignored, so nothing sends a credential that
   *  does not work — and the *reason* is kept, because that is the whole of
   *  what the Listener is told. */
  it('drops a grant the server says is not a live code, and remembers why', () => {
    const state = after(initialAccessState, grantHeld(GRANT), grantExpired('unknown'))

    expect(selectGrant({ access: state })).toBeUndefined()
    expect(selectStaleGrant({ access: state })).toBe('unknown')
  })

  /** The catalog is fetched more than once, and two answers saying the same
   *  thing must not raise the notice twice — the second finds nothing held. */
  it('says nothing about a grant it was not holding', () => {
    const never = after(initialAccessState, grantExpired('expired'))
    expect(selectStaleGrant({ access: never })).toBeUndefined()

    const twice = after(
      initialAccessState,
      grantHeld(GRANT),
      grantExpired('expired'),
      staleNoticeDismissed(),
      grantExpired('expired'),
    )
    expect(
      selectStaleGrant({ access: twice }),
      'the notice was read; a second catalog answer must not raise it again',
    ).toBeUndefined()
  })

  /** Locking up again is one reducer for both halves, because they are one
   *  thing: stop holding this, and stop saying anything about it. */
  it('releases the grant and the notice together', () => {
    const state = after(
      initialAccessState,
      grantHeld(GRANT),
      grantExpired('expired'),
      grantReleased(),
    )

    expect(state).toEqual({ grant: undefined, stale: undefined })
  })

  /** Dismissing the notice leaves whatever is held alone — a Listener who
   *  unlocked again while the bar was up keeps what they unlocked with. */
  it('dismissing the notice does not take the grant with it', () => {
    const state = after(
      initialAccessState,
      grantHeld(GRANT),
      grantExpired('unknown'),
      grantHeld(GRANT),
      staleNoticeDismissed(),
    )

    expect(selectGrant({ access: state })).toBe(GRANT)
    expect(selectStaleGrant({ access: state })).toBeUndefined()
  })
})

/** The base query reads this out of whatever store it finds itself in, and a
 *  store assembled without the slice — which is what a focused test of another
 *  slice builds — must mean *no grant* rather than a crash on its first
 *  request. */
describe('a store without the slice', () => {
  it('reads as holding nothing rather than throwing', () => {
    expect(selectGrant({})).toBeUndefined()
    expect(selectStaleGrant({})).toBeUndefined()
  })
})
