import { type LedColor, ledVar } from '@/lib/led'
import { cn } from '@/lib/utils'

/** The scanner status LED — a glowing dot colored by system/talkgroup. This is
 *  the app's primary carrier of meaning-through-color (docs/design/brief.md). */
export function StatusLed({
  color,
  size = 12,
  pulse = false,
  className,
}: {
  color: LedColor
  size?: number
  pulse?: boolean
  className?: string
}) {
  const value = ledVar(color)
  return (
    <span
      aria-hidden
      className={cn('inline-block rounded-full', pulse && 'animate-pulse', className)}
      style={{
        width: size,
        height: size,
        backgroundColor: value,
        boxShadow: `0 0 ${size * 0.75}px ${value}`,
      }}
    />
  )
}
