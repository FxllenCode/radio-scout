/**
 * A bottom sheet: a titled panel over the screen, dismissed by Escape, by the
 * backdrop, or by its own Close (#58).
 *
 * One component for the two lists #58 opens over the Live screen — what is
 * waiting, and what is silenced — because they are the same object with
 * different rows in it, and two hand-rolled overlays would be two chances for
 * one of them to be unreachable by keyboard or unnamed to a screen reader.
 *
 * It renders inline rather than through a portal. The app is one column inside
 * `AppShell` and the sheet is `fixed`, so a portal would buy nothing but a
 * second place for the stacking order to be decided; rendering in place also
 * keeps it inside the tree a test renders, which is where its rows are asserted
 * on.
 *
 * # What it is not
 *
 * Not a focus trap. A trap needs a tab-cycle implementation that is right in
 * every browser, and what it protects against — tabbing to the page behind — is
 * a desktop concern on a screen whose whole content is the sheet. Focus is
 * *moved* to the panel on open, which is the part a screen reader needs, and
 * Escape is always the way out.
 */
import { X } from 'lucide-react'
import { useEffect, useId, useRef, type ReactNode } from 'react'

import { cn } from '@/lib/utils'

export function Sheet({
  title,
  onClose,
  children,
}: {
  /** Names the dialog, for a screen reader and for the header. */
  title: string
  onClose: () => void
  children: ReactNode
}) {
  const panel = useRef<HTMLDivElement>(null)
  const heading = useId()

  // Focus lands on the panel rather than on its first control: a sheet that
  // opens with *Drop* focused is one Enter away from discarding a Call the
  // Listener opened the sheet to look at.
  useEffect(() => panel.current?.focus(), [])

  return (
    <div className="fixed inset-0 z-50 flex flex-col justify-end">
      {/* Tapping away is how a sheet is dismissed on a phone. Hidden from the
          accessibility tree and out of the tab order — Escape and the Close
          button are the reachable ways out, and a second unlabelled "close"
          in the tree would only be noise to read past. */}
      <button
        type="button"
        aria-hidden
        tabIndex={-1}
        onClick={onClose}
        className="absolute inset-0 bg-background/80 backdrop-blur-sm"
      />
      <div
        ref={panel}
        role="dialog"
        aria-modal="true"
        aria-labelledby={heading}
        tabIndex={-1}
        onKeyDown={(event) => {
          if (event.key === 'Escape') onClose()
        }}
        className="relative mx-auto flex max-h-[80dvh] w-full max-w-2xl flex-col rounded-t-2xl border-t border-border bg-card pb-[env(safe-area-inset-bottom)] outline-none"
      >
        <header className="flex items-center justify-between gap-3 border-b border-border px-4 py-3">
          <h2
            id={heading}
            className="font-mono text-xs font-semibold uppercase tracking-wider"
          >
            {title}
          </h2>
          <button
            type="button"
            aria-label="Close"
            onClick={onClose}
            className="-mr-1 rounded p-1 text-muted-foreground transition-colors hover:text-foreground"
          >
            <X className="size-4" aria-hidden />
          </button>
        </header>
        <div className="min-h-0 flex-1 overflow-y-auto overscroll-contain px-4 py-3">
          {children}
        </div>
      </div>
    </div>
  )
}

/**
 * A sheet's full-width action — *Jump to newest*, *Clear all*.
 *
 * Here rather than copied into each sheet because the two were the same string
 * of a dozen utility classes twice over, which is how one of them comes to be a
 * different size than the other after a tweak nobody re-read.
 */
export function SheetAction({
  onClick,
  className,
  children,
}: {
  onClick: () => void
  className?: string
  children: ReactNode
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      className={cn(
        'flex w-full items-center justify-center gap-2 rounded-lg border border-border py-2 font-mono text-[11px] uppercase tracking-wider transition-colors hover:bg-muted/40',
        className,
      )}
    >
      {children}
    </button>
  )
}
