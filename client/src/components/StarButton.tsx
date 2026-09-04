/**
 * The **Star** on one Call, as a row control (#66, spec US 37).
 *
 * One component, drawn wherever a Call is listed — the archive's results, a
 * radio's history, this session's log — for [`UnitLink`]'s reason: "star from
 * any row" is a promise about *every* row, and three hand-written buttons are
 * three chances for one of them to toggle the wrong thing or to stop saying
 * what state it is in.
 *
 * A **toggle**, so it carries `aria-pressed` and its name says what the tap
 * will do. There is no "starred by me" to distinguish from "starred": the mark
 * is the Instance's (`src/star.rs`), so a filled star means somebody kept this
 * Call and drawing two kinds would be drawing a distinction that does not
 * exist.
 */
import { Star } from 'lucide-react'

import { useStar } from '@/hooks/useStar'
import { Button } from '@/components/ui/button'
import { cn } from '@/lib/utils'
import type { Call } from '@/types'

export function StarButton({
  call,
  describedAs,
}: {
  /** `null` where the surface has a place for this control and nothing under
   *  it yet — the **DVR**'s transport before it has played anything. A row
   *  always has a Call; a player does not, and a transport that lost a button
   *  between Calls would be a worse answer than one that greys it. */
  call: Call | null
  /** How this Call reads in a label, so a screen reader hears which row's star
   *  is which — the same string every other control on the row is named with. */
  describedAs: string
}) {
  const { starred, toggle } = useStar(call)

  return (
    <Button
      // `Button` renders a bare `<button>`, which inside a form submits it —
      // #49's trap, and the search filters are a form.
      type="button"
      variant="outline"
      size="icon"
      aria-pressed={starred}
      aria-label={`${starred ? 'Unstar' : 'Star'} ${describedAs}`}
      disabled={!call}
      onClick={toggle}
    >
      <Star
        className={cn('size-4', starred && 'fill-current text-amber-400')}
        aria-hidden
      />
    </Button>
  )
}
