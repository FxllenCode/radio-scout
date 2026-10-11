import { useClock } from '@/hooks/useInstance'
import { formatCallTime, formatRowTime } from '@/lib/archive'

/**
 * When a Call happened, as a list row shows it (#78): the date only when it is
 * not today ([`formatRowTime`]), with the whole stamp one hover away in the
 * `title` and machine-readable in `dateTime` for anything that reads the page
 * rather than looking at it.
 *
 * `now` is the screen's clock (`useNow`), handed in so a list of two hundred
 * rows asks for it once.
 */
export function CallTime({
  ms,
  now,
  className,
}: {
  ms: number | undefined
  now: number
  className?: string
}) {
  const clock = useClock()
  if (ms === undefined || Number.isNaN(ms)) {
    return <span className={className}>—</span>
  }
  return (
    <time
      dateTime={new Date(ms).toISOString()}
      title={formatCallTime(ms, clock)}
      className={className}
    >
      {formatRowTime(ms, clock, now)}
    </time>
  )
}
