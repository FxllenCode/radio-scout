/**
 * **Catch-up**: draining the listening queue faster than real time (#59, spec
 * US 23).
 *
 * Pure — no React, no store, no clock, no element. Every rule below is a value a
 * test constructs, which is what lets `catchup.test.ts` assert the ticket's own
 * acceptance criterion ("a 40-call backlog drains measurably faster") as
 * arithmetic rather than as a stopwatch held against a browser.
 *
 * # The two levers, and why there are only two
 *
 * [ADR-0005](../../../docs/adr/0005-client-audio-media-session-background.md)
 * gives Radio-Scout **one `<audio>` element and no WebAudio** — iOS classifies
 * WebAudio output as ambient and mutes it the moment the app is backgrounded,
 * which is exactly why rdio-scanner has no working background audio. So the only
 * things that can make a deep queue shorter are `playbackRate` and
 * `currentTime`:
 *
 * - **Rate** shortens everything by the same fraction, and needs nothing from
 *   the server.
 * - **Trim** shortens only the parts nobody is talking in, and cannot be worked
 *   out here at all: a browser with no WebAudio cannot look at samples. Those
 *   are the **Quiet spans** the server's scanner found (`src/quiet/`), carried
 *   on the Call.
 *
 * A Call with no spans is not a failure — it is the ordinary case, and also what
 * an Instance with `[quiet] enabled = false` produces. Catch-up raises the rate
 * and trims nothing, which is the documented fallback.
 *
 * # Why a step rather than a plan
 *
 * The player asks *"I am here, now what"* several times a second and acts on one
 * answer. Handing it a schedule instead would mean this module tracking where
 * playback had got to — a second copy of a position the element already owns,
 * and the one thing guaranteed to disagree with it after a seek, a stall or a
 * lock-screen scrub.
 */
import type { Call } from '@/types'

/**
 * How much faster a queued Call plays while catching up.
 *
 * 1.5× is the speed at which dispatch traffic stays *comfortably* intelligible
 * — which is the acceptance criterion's "with speech intact", and it is a
 * criterion, not a preference. Every browser that matters keeps
 * `preservesPitch` on by default, so this is a time-stretch and not a
 * chipmunk; past about 2× the artefacts of that stretch start eating consonants,
 * which on a radio channel is the difference between a street name and a
 * different street name.
 *
 * A constant rather than a control, deliberately. The trim is where the large
 * win is on real recorder traffic — a call file spanning a grant is mostly hang
 * time — and a speed picker is a setting to explain, persist and support for a
 * second-order gain.
 */
export const CATCHUP_RATE = 1.5

/** Normal speed. Named, so the player never writes a bare `1` it might later
 *  mean something else by. */
export const NORMAL_RATE = 1

/**
 * A stretch of a Call where nobody is talking — `[startMs, endMs]`, exactly as
 * the server sends it (`crate::quiet::Span`).
 *
 * A pair rather than an object because it is two numbers and there will never be
 * a third, and because this rides on every Call in a search page.
 */
export type QuietSpan = [start: number, end: number]

/**
 * The shortest jump worth making, in milliseconds.
 *
 * A seek costs the element a re-buffer, and on iOS it is the one operation that
 * can stall a backgrounded page — so arriving 200 ms before the end of a span is
 * not worth acting on. It is also what stops the *last* `timeupdate` inside a
 * span from seeking to a point the element then reports as fractionally before
 * the span's end, and seeking again, forever.
 */
const MIN_SKIP_MS = 300

/**
 * How close to the end of a Call a span has to reach to count as its tail.
 *
 * The scanner measures a Call's length from its decoded samples and the element
 * reads it from the container header; on an MP3 those differ by up to a frame.
 * Anything within a quarter of a second is the same fact.
 */
const TAIL_MS = 250

/** What the player should do about where it is. `null` is "keep playing". */
export type CatchupStep =
  /** Jump to `toSeconds` — the far end of the gap the Listener is sitting in. */
  | { do: 'seek'; toSeconds: number }
  /** The rest of this Call is silence: take the next one. */
  | { do: 'advance' }

/**
 * What to do at `positionSeconds` of a Call `durationSeconds` long.
 *
 * **Advance rather than seek where the gap runs to the end**, because seeking to
 * exactly the duration is a request browsers answer differently — some fire
 * `ended`, some clamp and sit there playing nothing — and the queue already has
 * one way to move on. It is also the commonest gap there is: a recorder's call
 * file keeps recording after the last word.
 */
export function stepAt(
  spans: readonly QuietSpan[] | undefined,
  positionSeconds: number,
  durationSeconds: number,
): CatchupStep | null {
  const position = positionSeconds * 1000
  const inside = spans?.find(([start, end]) => position >= start && position < end)
  if (!inside) return null

  const [, end] = inside
  if (end - position < MIN_SKIP_MS) return null

  const duration = durationSeconds * 1000
  if (Number.isFinite(duration) && duration > 0 && duration - end <= TAIL_MS) {
    return { do: 'advance' }
  }
  return { do: 'seek', toSeconds: end / 1000 }
}

/**
 * How long a Listener will spend on what is waiting.
 *
 * Named for CONTEXT.md's own verb — **Catch-up** is *"draining the listening
 * queue faster than real time"* — and deliberately not `Backlog`, which that
 * glossary bans as a name for the **listening queue** itself.
 */
export interface Drain {
  /** Calls waiting. */
  calls: number
  /**
   * Seconds of listening left, at the rate given and with every known gap
   * trimmed out.
   *
   * Of the Calls whose length anybody measured. See [`Drain.unmeasured`] —
   * this is a floor, and the screen says so rather than inventing an average for
   * Calls nothing knows the length of.
   */
  seconds: number
  /**
   * How many waiting Calls carry no duration — every Call stored before #42, and
   * any whose recorder said nothing and whose header could not be read.
   *
   * Reported rather than guessed at, because the alternative is a countdown that
   * runs out with Calls still waiting, and a progress display that lies is worse
   * than one that hedges.
   */
  unmeasured: number
}

/**
 * What is left to hear, and roughly how long it will take.
 *
 * This is the "shows progress" half of the ticket, and it is also the assertion
 * the ticket's headline criterion is made of: ask it about the same forty Calls
 * twice, once catching up and once not, and the difference *is* the feature.
 *
 * **One parameter, not two.** The rate and the trim are the same decision —
 * catching up, or not — and a signature that let them be set separately would
 * allow the answer nobody wants: what a queue would cost if the Listener heard
 * the gaps at speed, or skipped them at normal pace. It also stops the display
 * quietly subtracting gaps a Listener with Catch-up off is going to sit through.
 *
 * Gaps are subtracted before the rate is applied, because that is the order they
 * happen in: the Listener skips the silence and then hears the rest faster. A
 * span reaching past the Call's own duration — which a **Replacement** could
 * leave behind for a moment — is clamped rather than allowed to make a Call
 * shorter than nothing.
 */
export function drain(queue: readonly Call[], catchingUp: boolean): Drain {
  const rate = catchingUp ? CATCHUP_RATE : NORMAL_RATE
  let seconds = 0
  let unmeasured = 0
  for (const call of queue) {
    if (!call.durationMs) {
      unmeasured += 1
      continue
    }
    const audible = catchingUp
      ? call.durationMs - quietMs(call, call.durationMs)
      : call.durationMs
    seconds += Math.max(0, audible) / 1000 / rate
  }
  return { calls: queue.length, seconds, unmeasured }
}

/** How much of a Call nobody is talking in, bounded by the Call itself.
 *
 *  `duration` is a parameter rather than read back off the Call, because the one
 *  caller has already established that it exists — reaching for it again would
 *  mean a `?? 0` no test could ever reach. */
function quietMs(call: Call, duration: number): number {
  return (call.quiet ?? []).reduce(
    (total, [start, end]) =>
      total + Math.max(0, Math.min(end, duration) - Math.min(start, duration)),
    0,
  )
}

/**
 * `m:ss`, for the queue-sheet readout.
 *
 * Rounded **up**, so "0:01" never means "already finished" — and so two hundred
 * milliseconds still reads as time remaining rather than as zero.
 */
export function formatRemaining(seconds: number): string {
  const whole = Math.ceil(Math.max(0, seconds))
  return `${Math.floor(whole / 60)}:${String(whole % 60).padStart(2, '0')}`
}
