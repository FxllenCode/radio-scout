import type { ReactNode } from 'react'

import { cn } from '@/lib/utils'

/**
 * One card in the docked column above the tab bar (design brief 19A).
 *
 * The shell decides what goes in that column and in what order — since #56 it
 * can hold a mini-player under the banner — so what lives here is the *card*:
 * the geometry every docked thing shares, rather than being copied into each
 * one that wants the spot.
 *
 * `label` names it for a screen reader, which is also what makes it a region:
 * a `<section>` without an accessible name is exposed as nothing at all, so the
 * two banners stay unnamed generic boxes and only the mini-player becomes
 * something a screen reader can jump to.
 */
export function DockedBanner({
  label,
  className,
  children,
}: {
  label?: string
  className?: string
  children: ReactNode
}) {
  return (
    <section
      aria-label={label}
      className={cn(
        'pointer-events-auto flex w-full items-center gap-3 rounded-xl border border-border bg-card px-4 py-3.5 shadow-lg',
        className,
      )}
    >
      {children}
    </section>
  )
}
