/**
 * The **Tone profile** form's pure half (#55, spec US 20).
 *
 * A profile is a station's paging sequence — Quick Call II's two tones, a
 * single long group tone, or an A-B-then-group run of three. Everything here is
 * about telling an Operator that what they typed could never fire *before* they
 * save it, because the failure it prevents is the one that produces no error
 * anywhere: a pager that is not being watched looks exactly like a pager that
 * has not gone off.
 *
 * The rule is deliberately a **second** implementation of `tone::unusable`
 * rather than a substitute for it — the server refuses the same thing, and
 * would refuse an imported document that carried it too. It is here because a
 * form that only found out on submit is a round trip an Operator has to
 * interpret. Keeping the two honest is `looksPostable`'s arrangement (#54).
 */
import type { ToneStep } from '@/types'

/** The band tone-out detection listens to, matching `detect::BAND_LOW_HZ` and
 *  `BAND_HIGH_HZ`. Quick Call II runs 288.5–2468.2 Hz; the margin covers the
 *  sets that go higher, and the floor keeps mains hum out. */
export const BAND_LOW_HZ = 200
export const BAND_HIGH_HZ = 3300

/** Matching `tone::MIN_STEP_MS`, `MAX_STEPS`, `MAX_TOLERANCE_PCT`,
 *  `MAX_GAP_MS`. */
export const MIN_STEP_MS = 100
export const MAX_STEPS = 8
export const MAX_TOLERANCE_PCT = 25
export const MAX_GAP_MS = 10_000

/** What a profile defaults to — twice the ±1% a Quick Call set is specified to,
 *  which is what every field tool defaults to. */
export const DEFAULT_TOLERANCE_PCT = 2
export const DEFAULT_GAP_MAX_MS = 300

/**
 * Why this profile could never page anything — or `null` if it could.
 *
 * The order of the checks is the order an Operator fills the form in, so the
 * first thing they are told about is the first thing they got to.
 */
export function unusable(
  steps: ToneStep[],
  tolerancePct: number,
  gapMaxMs: number,
): string | null {
  if (steps.length === 0) return 'a tone profile needs at least one tone'
  if (steps.length > MAX_STEPS) {
    return `a tone profile may name at most ${MAX_STEPS} tones`
  }
  for (const step of steps) {
    if (!(step.hz >= BAND_LOW_HZ && step.hz <= BAND_HIGH_HZ)) {
      return `${step.hz} Hz is outside the ${BAND_LOW_HZ}-${BAND_HIGH_HZ} Hz range tone-out detection listens to`
    }
    if (!(step.minMs >= MIN_STEP_MS)) {
      return `a tone must be held for at least ${MIN_STEP_MS} ms (got ${step.minMs} ms)`
    }
  }
  if (!(tolerancePct > 0 && tolerancePct <= MAX_TOLERANCE_PCT)) {
    return `tolerance must be between 0 and ${MAX_TOLERANCE_PCT}%, as a percentage of each tone`
  }
  if (!(gapMaxMs >= 0 && gapMaxMs <= MAX_GAP_MS)) {
    return `the gap between tones must be between 0 and ${MAX_GAP_MS} ms`
  }
  return null
}

/**
 * The sequence in one line — `1122.5 Hz for 0.8s → 1465.6 Hz for 2.0s`.
 *
 * Seconds rather than milliseconds because a tone's length is a thing an
 * Operator counts out loud, where the *frequency* is a number off a tone-set
 * chart and is shown as one.
 */
export function sequenceLine(steps: ToneStep[]): string {
  if (steps.length === 0) return 'no tones'
  return steps
    .map((step) => `${step.hz} Hz for ${(step.minMs / 1000).toFixed(1)}s`)
    .join(' → ')
}

/** What an Operator reads about a profile at a glance, in one line — the
 *  `webhookHealthLine` shape (#54). A disabled profile says so, because it is
 *  otherwise indistinguishable from one that is simply not being paged. */
export function toneProfileLine(profile: {
  steps: ToneStep[]
  tolerancePct: number
  disabled: boolean
}): string {
  const parts = [sequenceLine(profile.steps), `±${profile.tolerancePct}%`]
  if (profile.disabled) parts.push('disabled')
  return parts.join(' · ')
}
