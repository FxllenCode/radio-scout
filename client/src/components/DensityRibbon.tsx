import { useRef, useState } from 'react'

import { SeriesBars, SeriesExtent } from '@/components/SeriesBars'
import { bucketAt, bucketStartMs } from '@/lib/density'
import { formatCallTime } from '@/lib/archive'
import type { Series } from '@/types'

/**
 * The **density ribbon** over search results (#62, spec US 34–35).
 *
 * A bar per bucket across the whole of what the current search reaches, with a
 * marker showing where the page on screen is. Dragging or arrowing along it
 * jumps the window to that moment — the search is untouched, so a **Run** that
 * is playing keeps playing (#89's separation of the Run's window from the
 * screen's).
 *
 * It is also the per-talkgroup activity chart spec US 34 asks for: set the
 * Talkgroup filter and the bars are that channel's, and tapping one is landing
 * in the archive at that moment. One control, because it is one picture — a
 * second chart of the same numbers on the same screen would be two things to
 * keep in step and one of them would eventually be wrong.
 *
 * # A drag moves the marker; releasing moves the window
 *
 * The bucket under the thumb is *local* state until the pointer comes up, and
 * only then does the window move. A ribbon that jumped live would issue a fresh
 * `GET /api/calls` for every bucket the thumb crossed — a slow drag across a
 * hundred and twenty bars is a hundred and twenty page queries, on the Pi this
 * project is written for. It also reads better: the results stop thrashing
 * under the thumb.
 *
 * That is why the readout beneath the bars matters rather than being decoration
 * — it is the only thing saying *where* a drag is going, and "jump-to-date"
 * with no date on screen is a gesture a Listener has to perform blind.
 *
 * rdio-scanner has nothing here at all: time travel there is the Previous
 * button, one page at a time.
 *
 * # Where the marker sits is the screen's to say
 *
 * Two screens scrub this and they are in two different places (#63). The
 * Search screen's position is a *row* — which page of results is up — and it
 * reads `bucketOfOffset`. The **DVR**'s position is an *instant* — where the
 * playhead is — and it reads `bucketOfInstant`. So `at` arrives already
 * resolved rather than being derived here from an offset and an ordering, and
 * this component keeps knowing nothing about what a position means.
 *
 * Everything it decides is in `@/lib/density`; this draws and listens.
 */
export function DensityRibbon({
  series,
  at,
  label,
  onJump,
}: {
  series: Series
  /** Which bucket the Listener is actually in — what the marker returns to
   *  once a drag has ended, so the ribbon says where they *are* and not merely
   *  where they last dragged to. */
  at: number
  /** What this scrubber does, for a screen reader — the two screens do
   *  genuinely different things with it. */
  label: string
  /** Take the Listener to this bucket. */
  onJump: (bucket: number) => void
}) {
  const track = useRef<HTMLDivElement>(null)
  /** Where the Listener has pointed, while they are still pointing at it.
   *
   *  A slider's thumb stays where it was put — it does not snap back — and here
   *  it *has* to: on the Search screen the window is a page, so the offset a
   *  jump produces is page-granular and cannot represent "bucket 41" at all.
   *  Reverting to the page's own bucket after every keypress would make
   *  arrowing along the ribbon look broken for the thirty presses it takes to
   *  cross a page. */
  const [scrubbing, setScrubbing] = useState<number | undefined>(undefined)
  /** Whether a pointer is down on the track, as opposed to a thumb merely
   *  resting somewhere from an earlier gesture. */
  const dragging = useRef(false)

  const here = scrubbing ?? at
  const last = Math.max(0, series.values.length - 1)
  // No `?? 0`: every producer of an `at` clamps into the array (`density`'s
  // own `clampIndex`), as does `bucketAt`, so `here` is always a bucket that
  // exists — and a fallback nothing can reach is a branch no test could kill.
  const calls = series.values[here]
  const readout = `${formatCallTime(bucketStartMs(series, here))} · ${calls} ${
    calls === 1 ? 'call' : 'calls'
  }`

  /** Which bucket a pointer is over, in the ribbon's own coordinates. */
  const bucketUnder = (clientX: number): number => {
    const box = track.current?.getBoundingClientRect()
    if (!box || box.width === 0) return here
    return bucketAt(series.values.length, (clientX - box.left) / box.width)
  }

  const jumpTo = (bucket: number) => {
    if (bucket !== at) onJump(bucket)
  }

  return (
    <div className="space-y-1">
      <div
        ref={track}
        role="slider"
        tabIndex={0}
        aria-label={label}
        aria-valuemin={0}
        aria-valuemax={last}
        aria-valuenow={here}
        aria-valuetext={readout}
        data-testid="density-ribbon"
        className="h-14 w-full touch-none rounded-md border border-border bg-card px-1 py-1 outline-none focus-visible:ring-2 focus-visible:ring-ring"
        onPointerDown={(event) => {
          event.currentTarget.setPointerCapture(event.pointerId)
          dragging.current = true
          setScrubbing(bucketUnder(event.clientX))
        }}
        onPointerMove={(event) => {
          if (dragging.current) setScrubbing(bucketUnder(event.clientX))
        }}
        onPointerUp={() => {
          if (dragging.current && scrubbing !== undefined) jumpTo(scrubbing)
          dragging.current = false
        }}
        // A cancelled pointer is the gesture being taken away — a browser
        // claiming it for a scroll, a phone call arriving — not a decision, so
        // the window stays where it was and the thumb goes back to it.
        onPointerCancel={() => {
          dragging.current = false
          setScrubbing(undefined)
        }}
        // Leaving the control puts the thumb back on the window, so a ribbon
        // nobody is touching always says where the Listener actually is.
        onBlur={() => setScrubbing(undefined)}
        onKeyDown={(event) => {
          const step = KEYS[event.key]
          if (step === undefined) return
          event.preventDefault()
          const next =
            step === 'first' ? 0 : step === 'last' ? last : clamp(here + step, last)
          setScrubbing(next)
          jumpTo(next)
        }}
      >
        <SeriesBars series={series} marker={here} className="h-full" />
      </div>
      {/* Where the marker is, live while dragging — the only thing on screen
          that says what "jump to date" is about to jump to. `aria-live` off:
          the slider's own `aria-valuetext` is what a screen reader hears, and
          announcing both would say everything twice. */}
      <p
        data-testid="ribbon-at"
        className="text-center font-mono text-[10px] text-foreground"
        aria-hidden
      >
        {readout}
      </p>
      <SeriesExtent series={series} />
    </div>
  )
}

const clamp = (bucket: number, last: number) => Math.min(Math.max(0, bucket), last)

/** Keyboard scrubbing: the ARIA slider contract, so the ribbon is reachable
 *  without a pointer at all. Each press is one decision, so unlike a drag it
 *  moves the window straight away. */
const KEYS: Record<string, number | 'first' | 'last' | undefined> = {
  ArrowLeft: -1,
  ArrowRight: 1,
  ArrowDown: -1,
  ArrowUp: 1,
  PageDown: -10,
  PageUp: 10,
  Home: 'first',
  End: 'last',
}
