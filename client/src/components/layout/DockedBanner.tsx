import type { ReactNode } from 'react'

/**
 * The one banner slot, docked above the tab bar (design brief 19A).
 *
 * There is exactly one of these on screen at a time — the shell decides what
 * goes in it — so its geometry lives here rather than being copied into each
 * thing that wants the spot.
 */
export function DockedBanner({ children }: { children: ReactNode }) {
  return (
    <div className="pointer-events-none fixed inset-x-0 bottom-16 z-40 flex justify-center px-3">
      <div className="pointer-events-auto flex w-full max-w-2xl items-center gap-3 rounded-xl border border-border bg-card px-4 py-3.5 shadow-lg">
        {children}
      </div>
    </div>
  )
}
