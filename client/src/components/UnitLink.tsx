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
 *
 * The name is `lib/call.ts`'s `unitName`, which falls back to the **bare Ref**
 * deliberately: the same number on three Calls says they are the same radio,
 * which is the whole thing rdio-scanner throws away by never showing units.
 *
 * The link's target is `routes/UnitScreen.tsx`, the history, and it is **two
 * requests on purpose** — the summary is a `GROUP BY` a search cannot answer
 * without reading the whole Archive, and the list below is an ordinary `?unit=`
 * search, so it pages, plays and downloads through the same **Run** the Search
 * screen uses and invents nothing.
 *
 * Two traps that shipped as bugs and are now pinned: **any screen that starts a
 * Run owes the page-ahead**, which is why it is `hooks/useRunPageAhead.ts`
 * rather than lines in one route (without it a Run plays to the fiftieth Call
 * and stops, which looks exactly like the radio having gone quiet); and the
 * recent list is a list item *holding* a button rather than a button holding
 * everything, because an anchor inside a button is neither valid HTML nor
 * reachable by a screen reader.
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
