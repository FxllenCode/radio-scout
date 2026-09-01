import { barHeights } from '@/lib/density'
import { formatCallTime } from '@/lib/archive'
import { cn } from '@/lib/utils'
import type { Series } from '@/types'

/**
 * A bar per bucket, tallest full height (#62).
 *
 * The drawing half of every chart in the app that is a `Series`: the density
 * ribbon over search results and the operator's listener chart both render
 * this, and the ribbon adds a slider on top of it rather than a second set of
 * bars. One component because it is one picture, and because the two would
 * otherwise be two places to keep a rendering rule — the minimum bar height is
 * really a correctness rule, and having it in one of them and not the other is
 * a chart that lies on one screen.
 */
export function SeriesBars({
  series,
  marker,
  className,
}: {
  series: Series
  /** The bucket to pick out, when something is pointing at one. */
  marker?: number
  className?: string
}) {
  return (
    <div className={cn('flex w-full items-end gap-px', className)}>
      {barHeights(series.values).map((height, bucket) => (
        <span
          key={bucket}
          aria-hidden
          data-here={bucket === marker || undefined}
          className={cn(
            'min-h-px flex-1 rounded-[1px]',
            bucket === marker ? 'bg-foreground' : 'bg-muted-foreground/50',
          )}
          style={{ height: `${Math.round(height * 100)}%` }}
        />
      ))}
    </div>
  )
}

/**
 * What stretch of time a chart covers, at either end.
 *
 * Its own component for one reason, which is a rule rather than a style: `toMs`
 * is **exclusive**, so the label at the right-hand end is the last instant
 * covered and not the first one past it — a millisecond into tomorrow reads as
 * a whole extra day. Written once, so the two charts cannot come to disagree
 * about which instant they are naming.
 */
export function SeriesExtent({ series }: { series: Series }) {
  return (
    <div className="flex justify-between font-mono text-[10px] text-muted-foreground">
      <span>{formatCallTime(series.fromMs)}</span>
      <span>{formatCallTime(series.toMs - 1)}</span>
    </div>
  )
}
