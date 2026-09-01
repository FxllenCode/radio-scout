import { useRef } from 'react'

import {
  bucketAt,
  bucketOfOffset,
  bucketStartMs,
  barHeights,
  type Ordering,
} from '@/lib/density'
import { formatCallTime } from '@/lib/archive'
import { cn } from '@/lib/utils'
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
 * rdio-scanner has nothing here at all: time travel there is the Previous
 * button, one page at a time.
 *
 * Everything it decides is in `@/lib/density`; this draws and listens.
 */
export function DensityRibbon({
  series,
  ordering,
  offset,
  onJump,
}: {
  series: Series
  /** Which end of the archive the results are walked from. */
  ordering: Ordering
  /** Where the window on screen starts, in rows — which is what the marker
   *  below is placed from, so the ribbon says where the Listener *is* and not
   *  merely where they last dragged to. */
  offset: number
  /** Take the window to the results in this bucket. */
  onJump: (bucket: number) => void
}) {
  const track = useRef<HTMLDivElement>(null)
  const dragging = useRef(false)

  const heights = barHeights(series.values)
  const here = bucketOfOffset(series.values, offset, ordering)
  const last = Math.max(0, series.values.length - 1)

  /** Which bucket a pointer is over, in the ribbon's own coordinates. */
  const bucketUnder = (clientX: number): number => {
    const box = track.current?.getBoundingClientRect()
    if (!box || box.width === 0) return here
    return bucketAt(series.values.length, (clientX - box.left) / box.width)
  }

  const jumpTo = (bucket: number) => {
    if (bucket !== here) onJump(bucket)
  }

  return (
    <div className="space-y-1">
      <div
        ref={track}
        role="slider"
        tabIndex={0}
        aria-label="Jump to a time in these results"
        aria-valuemin={0}
        aria-valuemax={last}
        aria-valuenow={here}
        aria-valuetext={`${formatCallTime(bucketStartMs(series, here))}, ${
          series.values[here] ?? 0
        } calls`}
        data-testid="density-ribbon"
        className="flex h-14 w-full touch-none items-end gap-px rounded-md border border-border bg-card px-1 py-1 outline-none focus-visible:ring-2 focus-visible:ring-ring"
        onPointerDown={(event) => {
          dragging.current = true
          event.currentTarget.setPointerCapture(event.pointerId)
          jumpTo(bucketUnder(event.clientX))
        }}
        onPointerMove={(event) => {
          if (dragging.current) jumpTo(bucketUnder(event.clientX))
        }}
        onPointerUp={() => {
          dragging.current = false
        }}
        onPointerCancel={() => {
          dragging.current = false
        }}
        onKeyDown={(event) => {
          const step = KEYS[event.key]
          if (step === undefined) return
          event.preventDefault()
          jumpTo(
            step === 'first' ? 0 : step === 'last' ? last : here + step,
          )
        }}
      >
        {heights.map((height, bucket) => (
          <span
            key={bucket}
            aria-hidden
            data-here={bucket === here || undefined}
            className={cn(
              'min-h-px flex-1 rounded-[1px]',
              bucket === here ? 'bg-foreground' : 'bg-muted-foreground/50',
            )}
            style={{ height: `${Math.round(height * 100)}%` }}
          />
        ))}
      </div>
      <div className="flex justify-between font-mono text-[10px] text-muted-foreground">
        <span>{formatCallTime(series.fromMs)}</span>
        {/* The last instant the ribbon covers, not the first one past it —
            `toMs` is exclusive, and a label a millisecond into tomorrow reads
            as a whole extra day. */}
        <span>{formatCallTime(series.toMs - 1)}</span>
      </div>
    </div>
  )
}

/** Keyboard scrubbing: the ARIA slider contract, so the ribbon is reachable
 *  without a pointer at all. */
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
