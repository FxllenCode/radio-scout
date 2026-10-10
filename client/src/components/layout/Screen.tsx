import type { ReactNode } from 'react'

import { useInstance } from '@/hooks/useInstance'

/** Consistent screen chrome: a mono title row (scanner-flavored) over content,
 *  with top safe-area padding for installed PWAs.
 *
 *  Headed by the **Instance**'s name when the Operator has given it one (#77),
 *  on every screen rather than only the Live one, because a Listener who opens
 *  a shared search link has never seen the Live screen and should still know
 *  whose scanner they are on. An unnamed Instance adds nothing: the product's
 *  own name on every screen would be chrome, not information. */
export function Screen({
  title,
  status,
  children,
}: {
  title: string
  status?: ReactNode
  children: ReactNode
}) {
  const { name } = useInstance()
  return (
    <div className="px-4 pt-[calc(env(safe-area-inset-top)+1.25rem)]">
      {name && (
        <p
          data-testid="instance-name"
          className="mb-1 truncate font-mono text-[10px] uppercase tracking-[0.2em] text-muted-foreground"
        >
          {name}
        </p>
      )}
      <header className="mb-5 flex items-baseline justify-between gap-3">
        <h1 className="font-mono text-lg font-semibold tracking-tight">
          {title}
        </h1>
        {status ? (
          <div className="font-mono text-xs text-muted-foreground">{status}</div>
        ) : null}
      </header>
      {children}
    </div>
  )
}
