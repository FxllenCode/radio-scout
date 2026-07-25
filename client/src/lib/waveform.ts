/**
 * The scanner display's waveform (#11, spec US 16).
 *
 * **It is a shape, not a measurement** — deterministic per Call rather than
 * sampled from the audio. Real amplitude would mean one of two things, and both
 * were rejected for #11:
 *
 * - A WebAudio `AnalyserNode`, which needs `createMediaElementSource` — that
 *   routes playback *through* WebAudio, and iOS then treats our audio as
 *   ambient and mutes it in the background ([ADR-0005](../../../docs/adr/0005-client-audio-media-session-background.md)).
 *   Trading the product's headline feature for a visualization is not a trade.
 * - Peaks extracted server-side at ingest and stored per Call, which is the
 *   honest way to get a true envelope with a plain `<audio>` element — a decode
 *   dependency, a column, and CPU on a Pi. Deferred to its own ticket.
 *
 * So this draws *that* audio is playing and how far in, in the Call's LED color.
 * rdio-scanner draws no waveform at all.
 */

/** Bars across the display. Enough to read as speech, few enough to be cheap. */
export const WAVEFORM_BARS = 48

/**
 * Bar heights in `(0, 1]` for the Call with this id — stable, so a Call redraws
 * identically and replaying one looks like the same transmission.
 */
export function waveformBars(seed: number, count = WAVEFORM_BARS): number[] {
  const random = mulberry32(seed)
  return Array.from({ length: count }, (_, bar) => {
    // A transmission opens and closes quieter than it runs.
    const envelope = Math.sin(((bar + 0.5) / count) * Math.PI)
    const noise = 0.35 + 0.65 * random()
    return clamp(0.08 + 0.92 * noise * envelope, 0.08, 1)
  })
}

/** How many bars the playhead has passed, given progress in `[0, 1]`. Nonsense
 *  progress (a `NaN` duration before metadata loads, a stale `currentTime` past
 *  the end) reads as "none" and "all" rather than as a broken display. */
export function barsLit(progress: number, count = WAVEFORM_BARS): number {
  if (!Number.isFinite(progress)) return 0
  return Math.round(clamp(progress, 0, 1) * count)
}

/** A small, fast, seeded PRNG (mulberry32) — the same seed always replays the
 *  same sequence, which is the whole point here. */
function mulberry32(seed: number): () => number {
  let state = seed >>> 0
  return () => {
    state = (state + 0x6d2b79f5) >>> 0
    let drawn = Math.imul(state ^ (state >>> 15), 1 | state)
    drawn = (drawn + Math.imul(drawn ^ (drawn >>> 7), 61 | drawn)) ^ drawn
    return ((drawn ^ (drawn >>> 14)) >>> 0) / 4294967296
  }
}

function clamp(value: number, low: number, high: number): number {
  return Math.min(high, Math.max(low, value))
}
