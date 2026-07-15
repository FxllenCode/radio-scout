import type { ReactNode } from 'react'

/** Consistent screen chrome: a mono title row (scanner-flavored) over content,
 *  with top safe-area padding for installed PWAs. */
export function Screen({
  title,
  status,
  children,
}: {
  title: string
  status?: ReactNode
  children: ReactNode
}) {
  return (
    <div className="px-4 pt-[calc(env(safe-area-inset-top)+1.25rem)]">
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
