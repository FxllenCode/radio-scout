import { useEffect, useRef, type ReactNode } from 'react'

import { useInstance } from '@/hooks/useInstance'

/** Consistent screen chrome: a mono title row (scanner-flavored) over content,
 *  with top safe-area padding for installed PWAs.
 *
 *  Headed by the **Instance**'s name when the Operator has given it one (#77),
 *  on every screen rather than only the Live one, because a Listener who opens
 *  a shared search link has never seen the Live screen and should still know
 *  whose scanner they are on. An unnamed Instance adds nothing: the product's
 *  own name on every screen would be chrome, not information.
 *
 *  `heading` is the title's level: 1 for the screen the page is about, 2 for
 *  the Live panel the desktop layout docks beside it (#78) — one page, one
 *  top-level heading.
 *
 *  **Arriving from the primary nav moves focus to the title** (#78), so a
 *  screen reader says where the Listener has landed instead of staying silent
 *  on the tab they pressed. Only from the nav: a link inside a screen, or the
 *  page loading, is not a Listener asking to be moved. */
export function Screen({
  title,
  status,
  heading = 1,
  children,
}: {
  title: string
  status?: ReactNode
  heading?: 1 | 2
  children: ReactNode
}) {
  const { name } = useInstance()
  const titled = useRef<HTMLHeadingElement>(null)
  useEffect(() => {
    if (heading !== 1) return
    if (document.activeElement?.closest('nav[aria-label="Primary"]')) titled.current?.focus()
  }, [heading])
  const Title = heading === 1 ? 'h1' : 'h2'
  return (
    <div className="px-4 pt-[calc(env(safe-area-inset-top)+1.25rem)]">
      {name && (
        <p
          data-testid="instance-name"
          className="mb-1 truncate font-mono text-[11px] uppercase tracking-[0.2em] text-muted-foreground"
        >
          {name}
        </p>
      )}
      <header className="mb-5 flex items-baseline justify-between gap-3">
        <Title
          ref={titled}
          tabIndex={-1}
          className="font-mono text-lg font-semibold tracking-tight outline-none"
        >
          {title}
        </Title>
        {status ? (
          <div className="font-mono text-xs text-muted-foreground">{status}</div>
        ) : null}
      </header>
      {children}
    </div>
  )
}
