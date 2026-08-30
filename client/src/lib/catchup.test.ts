import { describe, expect, it } from 'vitest'

import type { Call } from '@/types'

import {
  CATCHUP_RATE,
  drain,
  formatRemaining,
  stepAt,
  type QuietSpan,
} from './catchup'

const call = (id: number, durationMs?: number, quiet?: QuietSpan[]): Call => ({
  id,
  systemRef: 11,
  talkgroupRef: 54241,
  durationMs,
  quiet,
})

describe('stepAt', () => {
  it('keeps playing where nobody said there was a gap', () => {
    expect(stepAt(undefined, 2, 10)).toBeNull()
    expect(stepAt([], 2, 10)).toBeNull()
    expect(stepAt([[4000, 6000]], 2, 10)).toBeNull()
  })

  it('jumps to the far end of the gap it is sitting in', () => {
    expect(stepAt([[4000, 6000]], 4.5, 10)).toEqual({ do: 'seek', toSeconds: 6 })
    // The moment the gap opens, and the moment before it closes enough to be
    // worth a jump — both act.
    expect(stepAt([[4000, 6000]], 4, 10)).toEqual({ do: 'seek', toSeconds: 6 })
    expect(stepAt([[4000, 6000]], 5.699, 10)).toEqual({ do: 'seek', toSeconds: 6 })
  })

  /** A seek costs a re-buffer, and on iOS it can stall a backgrounded page. It
   *  is also what stops the last `timeupdate` inside a gap from seeking to a
   *  point the element reports as fractionally short of it, and seeking again. */
  it('does not jump the last fraction of a gap', () => {
    expect(stepAt([[4000, 6000]], 5.8, 10)).toBeNull()
    expect(stepAt([[4000, 6000]], 6, 10)).toBeNull()
  })

  /** The commonest gap there is: a recorder's call file keeps recording after
   *  the last word. Advancing rather than seeking, because seeking to exactly
   *  the duration is a request browsers answer differently. */
  it('takes the next Call when the gap runs to the end', () => {
    expect(stepAt([[6000, 10000]], 6.5, 10)).toEqual({ do: 'advance' })
    // Within the tolerance the container header and the decoder disagree by.
    expect(stepAt([[6000, 9900]], 6.5, 10)).toEqual({ do: 'advance' })
    // Outside it, there is real audio after the gap and the Call is not over.
    expect(stepAt([[6000, 9000]], 6.5, 10)).toEqual({ do: 'seek', toSeconds: 9 })
  })

  /** A duration the element does not know yet is not a reason to stop trimming
   *  — only a reason not to claim a gap reaches the end. */
  it('still jumps when the element has no duration yet', () => {
    expect(stepAt([[4000, 6000]], 4.5, NaN)).toEqual({ do: 'seek', toSeconds: 6 })
    expect(stepAt([[4000, 6000]], 4.5, 0)).toEqual({ do: 'seek', toSeconds: 6 })
  })

  it('picks the gap it is in, out of several', () => {
    const spans: QuietSpan[] = [
      [1000, 3000],
      [5000, 8000],
    ]
    expect(stepAt(spans, 1.5, 20)).toEqual({ do: 'seek', toSeconds: 3 })
    expect(stepAt(spans, 4, 20)).toBeNull()
    expect(stepAt(spans, 5.5, 20)).toEqual({ do: 'seek', toSeconds: 8 })
  })
})

describe('drain', () => {
  it('counts what is waiting and how long it will take', () => {
    const queue = [call(1, 10_000), call(2, 6_000)]

    expect(drain(queue, false)).toEqual({
      calls: 2,
      seconds: 16,
      unmeasured: 0,
    })
  })

  /** The gaps come out only when the Listener is skipping them: with Catch-up
   *  off they are seconds they are about to sit through, and a readout that had
   *  already subtracted them would be counting down to the wrong moment. */
  it('takes the gaps out before the rate goes on, and only when catching up', () => {
    const queue = [call(1, 10_000, [[2000, 6000]])]

    expect(drain(queue, false).seconds).toBeCloseTo(10)

    // Six seconds of talking, at one and a half times.
    expect(drain(queue, true).seconds).toBeCloseTo(4)
  })

  /** A countdown that runs out with Calls still waiting is worse than one that
   *  hedges, so a Call nobody measured is reported rather than averaged. */
  it('says how many Calls nobody measured rather than guessing', () => {
    const queue = [call(1, 10_000), call(2), call(3)]

    expect(drain(queue, false)).toEqual({
      calls: 3,
      seconds: 10,
      unmeasured: 2,
    })
    expect(drain([], false)).toEqual({ calls: 0, seconds: 0, unmeasured: 0 })
  })

  /** A span outstaying its Call — which a **Replacement** can leave behind for a
   *  moment — is clamped, never allowed to make a Call shorter than nothing. */
  it('never makes a Call shorter than nothing', () => {
    expect(drain([call(1, 4_000, [[0, 99_000]])], true).seconds).toBe(0)
    expect(drain([call(1, 4_000, [[3000, 99_000]])], true).seconds).toBeCloseTo(2)
  })

  /**
   * **The ticket's own acceptance criterion**: a 40-call backlog drains
   * measurably faster, with speech intact.
   *
   * Forty Calls of a shape a Trunk Recorder really produces — a keyup, the hang
   * time, another keyup, and the tail after the last word — which is why the
   * trim is the larger of the two levers here and the rate is the smaller. The
   * assertion is on the *ratio* rather than on a wall-clock number, because what
   * the ticket promises is a difference and what a machine can measure is
   * arithmetic.
   *
   * "Speech intact" is the reason nothing here goes faster than
   * [`CATCHUP_RATE`]: every second of talking in the backlog is still played,
   * only sooner.
   */
  it('drains a 40-call backlog measurably faster', () => {
    const queue = Array.from({ length: 40 }, (_, id) =>
      call(id, 12_000, [
        [1500, 4500],
        [6000, 12_000],
      ]),
    )

    const before = drain(queue, false)
    const after = drain(queue, true)

    // Eight real minutes of sitting through it...
    expect(before.seconds).toBeCloseTo(40 * 12)
    // ...against two of listening: nine seconds of the twelve is nobody
    // talking, and the three that are left play at one and a half times.
    expect(after.seconds).toBeCloseTo(40 * 2)
    expect(after.seconds).toBeLessThan(before.seconds / 3)
    // Every second of speech survives — the rate is the only thing shortening
    // it, and it is applied to the talking rather than instead of it.
    expect(after.seconds * CATCHUP_RATE).toBeCloseTo(40 * 3)
  })

  /** With no spans anywhere — an Instance with `[quiet] enabled = false`, or one
   *  whose scanner has not caught up — the rate alone is still the fallback, and
   *  it still works. */
  it('still shortens a backlog with no spans at all', () => {
    const queue = Array.from({ length: 40 }, (_, id) => call(id, 12_000))

    expect(drain(queue, true).seconds).toBeCloseTo(
      drain(queue, false).seconds / CATCHUP_RATE,
    )
  })
})

describe('formatRemaining', () => {
  it.each([
    [0, '0:00'],
    [0.2, '0:01'],
    [59, '0:59'],
    [60, '1:00'],
    [125.4, '2:06'],
    [-3, '0:00'],
  ])('renders %s seconds as %s', (seconds, expected) => {
    expect(formatRemaining(seconds)).toBe(expected)
  })
})
