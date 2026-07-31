import { Lock, TriangleAlert } from 'lucide-react'
import type { ReactNode } from 'react'

import { cn } from '@/lib/utils'
import type { Call } from '@/types'

/**
 * The two things a recorder can say about a transmission that change how a
 * listener should read it (#42, spec US 5 and US 9): the emergency bit the
 * radio set, and that the talkgroup was encrypted.
 *
 * One component, used by the scanner display, the RECENT list and the archive
 * rows, so an emergency looks the same everywhere it appears. Both render
 * nothing at all when the flag is unset, which is nearly always — a badge that
 * is usually present is a badge nobody sees.
 *
 * **The icon carries the meaning, not the colour.** Red-on-amber is not a
 * distinction every listener can make, so each badge is a shape *and* a
 * `title`, and the title is what the tests and a screen reader read.
 */
export function CallFlags({
  call,
  className,
}: {
  call: Pick<Call, 'emergency' | 'encrypted'>
  className?: string
}) {
  if (!call.emergency && !call.encrypted) return null
  return (
    <span className={cn('flex shrink-0 items-center gap-1', className)}>
      {call.emergency && (
        <Flag label="Emergency" className="text-[var(--led-red,#ef4444)]">
          <TriangleAlert className="size-3.5" aria-hidden />
        </Flag>
      )}
      {call.encrypted && (
        <Flag label="Encrypted" className="text-muted-foreground">
          <Lock className="size-3.5" aria-hidden />
        </Flag>
      )}
    </span>
  )
}

/** One badge. The `title` and the `aria-label` carry the meaning; the icon
 *  inside is decorative, so a listener who cannot tell the two colours apart
 *  still gets the word on hover and in a screen reader. */
function Flag({
  label,
  className,
  children,
}: {
  label: string
  className?: string
  children: ReactNode
}) {
  return (
    <span
      role="img"
      title={label}
      aria-label={label}
      className={cn('inline-flex items-center', className)}
    >
      {children}
    </span>
  )
}
