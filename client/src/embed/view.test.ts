import { describe, expect, it } from 'vitest'

import { when } from './view'

describe('when a call happened', () => {
  const now = new Date(2026, 9, 6, 15, 30, 0).getTime()

  /** A homepage reader is looking at today: the time of day is the whole of
   *  it. */
  it('is the time of day for today', () => {
    expect(when(new Date(2026, 9, 6, 9, 5, 7).getTime(), '24h', now)).toBe('09:05:07')
  })

  it('carries the date for anything older', () => {
    expect(when(new Date(2026, 9, 5, 23, 59, 1).getTime(), '24h', now)).toBe(
      '2026-10-05 23:59:01',
    )
  })

  /** A Call a Recorder stamped no time on says nothing rather than a guess. */
  it('says nothing for a call with no time', () => {
    expect(when(undefined, '24h', now)).toBe('')
  })

  /** On the Operator's clock (#77), like the app it opens. */
  it('is written on a twelve-hour clock when the Operator chose one', () => {
    expect(when(new Date(2026, 9, 6, 15, 5, 7).getTime(), '12h', now)).toBe('03:05:07 PM')
  })
})
