import { type LedColor, ledVar } from '@/lib/led'
import { cn } from '@/lib/utils'

/** What both indicators are drawn from: a lit patch of color with a glow. */
function Light({
  color,
  size,
  pulse,
  shape,
  className,
  testId,
}: {
  color: LedColor
  size: number
  pulse: boolean
  shape: 'round' | 'square'
  className?: string
  testId: string
}) {
  const value = ledVar(color)
  return (
    <span
      aria-hidden
      data-testid={testId}
      data-shape={shape}
      className={cn(
        'inline-block shrink-0',
        shape === 'round' ? 'rounded-full' : 'rounded-[2px]',
        pulse && 'animate-pulse',
        className,
      )}
      style={{
        width: size,
        height: size,
        backgroundColor: value,
        boxShadow: `0 0 ${size * 0.75}px ${value}`,
      }}
    />
  )
}

/** A channel's LED — a round glowing dot colored by System/Talkgroup, the
 *  app's primary carrier of meaning-through-color (docs/design/brief.md). */
export function ChannelLed({
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
  return (
    <Light
      color={color}
      size={size}
      pulse={pulse}
      shape="round"
      className={className}
      testId="channel-led"
    />
  )
}

/**
 * A **status** lamp — the feed's link, the server's health, a recorder's
 * verdict — and deliberately not round (#78).
 *
 * Both used to be the same glowing dot, from palettes that overlap: a Fire
 * Talkgroup's red LED beside a red NO LINK said "alarm" twice, once wrongly.
 * Shape is the channel color cannot take, so it is what tells the two apart
 * at a glance — and for anybody who cannot tell red from green, it is the only
 * thing that does.
 */
export function StatusLamp({
  color,
  size = 8,
  pulse = false,
  className,
}: {
  color: LedColor
  size?: number
  pulse?: boolean
  className?: string
}) {
  return (
    <Light
      color={color}
      size={size}
      pulse={pulse}
      shape="square"
      className={className}
      testId="status-lamp"
    />
  )
}
