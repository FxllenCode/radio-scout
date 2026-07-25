import { useMemo } from 'react'

import { type LedColor, ledVar } from '@/lib/led'
import { WAVEFORM_BARS, barsLit, waveformBars } from '@/lib/waveform'
import { cn } from '@/lib/utils'

/**
 * The scanner display's waveform (#11, spec US 16): bars in the Talkgroup's LED
 * color, lit as far as the Call has played.
 *
 * The shape is deterministic per Call rather than sampled — see
 * `@/lib/waveform` for why real amplitude is a server-side job, not a WebAudio
 * one. Decorative: the readouts beside it carry the meaning.
 */
export function Waveform({
  seed,
  color,
  progress,
  live = true,
  className,
}: {
  /** The Call's id — same Call, same shape, every time. */
  seed: number
  color: LedColor
  /** How far through the Call, in `[0, 1]`. */
  progress: number
  /** False while paused, which stills the bars rather than dimming them out. */
  live?: boolean
  className?: string
}) {
  const bars = useMemo(() => waveformBars(seed), [seed])
  const lit = barsLit(progress, WAVEFORM_BARS)
  const value = ledVar(color)

  return (
    <div
      aria-hidden
      data-testid="waveform"
      className={cn('flex h-12 items-center gap-[2px]', className)}
    >
      {bars.map((height, bar) => (
        <span
          key={bar}
          data-lit={bar < lit}
          className="flex-1 rounded-[1px] transition-[height,opacity] duration-150"
          style={{
            height: `${Math.round(height * 100)}%`,
            backgroundColor: bar < lit ? value : 'var(--color-muted)',
            opacity: bar < lit ? (live ? 1 : 0.55) : 0.9,
          }}
        />
      ))}
    </div>
  )
}
