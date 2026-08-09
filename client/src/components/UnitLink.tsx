import { Link } from 'react-router-dom'

import { unitHref, unitName } from '@/lib/call'
import { cn } from '@/lib/utils'
import type { Call } from '@/types'

/**
 * The radio a Call was heard under, as a link to its history (#47, spec
 * US 42/44).
 *
 * One component, and that is the point: "reachable from any rendered unit
 * label" is a promise about *every* place a unit appears — the scanner display,
 * the recent list, a search row — and three hand-written spans would be three
 * chances for one of them to render a name that goes nowhere.
 *
 * Renders nothing at all when no radio was heard, which is most of an archive
 * fed by a recorder that sends no source: a dash on every row would be clutter
 * bought for nothing, and the display asks for one explicitly where a stat line
 * does need a placeholder.
 */
export function UnitLink({ call, className }: { call: Call; className?: string }) {
  const name = unitName(call)
  const href = unitHref(call)
  if (name === undefined || href === undefined) return null

  return (
    <Link
      to={href}
      aria-label={`History for unit ${name}`}
      className={cn('truncate underline decoration-dotted underline-offset-2', className)}
    >
      {name}
    </Link>
  )
}
