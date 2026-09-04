/**
 * The **Star** on one Call, as a row control (#66, spec US 37).
 *
 * One component, drawn wherever a Call a Listener has **heard or found** is
 * listed — the archive's results, the Live screen's RECENT, a radio's history,
 * the DVR's transport, the player walking a **Run** — for [`UnitLink`]'s
 * reason: three hand-written buttons are three chances for one of them to
 * toggle the wrong thing or to stop saying what state it is in. The session
 * log reaches the same rule through its long-press sheet, because its row *is*
 * a button and a control inside one is what #47 got caught by.
 *
 * **Two surfaces deliberately have none.** The **listening queue** lists Calls
 * nobody has heard yet, and there is nothing to decide about one of those —
 * `enqueue` put it there and the Listener has not met it. And `MiniPlayer` is
 * two controls by its own rule ("three icon buttons in a docked strip on a
 * phone is a row of targets too small to hit"), which is the rule that already
 * costs it *Previous*; the way out is the one that rule names — Live is one tap
 * away, and #56 keeps the Call on its display after it ends.
 *
 * A **toggle**, so it carries `aria-pressed` and its name says what the tap
 * will do. There is no "starred by me" to distinguish from "starred": the mark
 * is the Instance's (`src/star.rs`), so a filled star means somebody kept this
 * Call and drawing two kinds would be drawing a distinction that does not
 * exist.
 */
import { useStar } from '@/hooks/useStar'
import { StarMark } from '@/components/StarMark'
import { Button } from '@/components/ui/button'
import type { Call } from '@/types'

export function StarButton({
  call,
  describedAs,
  disabled,
}: {
  /** `null` where the surface has a place for this control and nothing under
   *  it yet — the **DVR**'s transport before it has played anything. A row
   *  always has a Call; a player does not, and a transport that lost a button
   *  between Calls would be a worse answer than one that greys it. */
  call: Call | null
  /** How this Call reads in a label, so a screen reader hears which row's star
   *  is which — the same string every other control on the row is named with. */
  describedAs: string
  /** The *surface* refusing the tap, where `call: null` above is the surface
   *  having nothing to offer. Both end in a dead button and they are not the
   *  same sentence: the Live screen with its feed off kills every control but
   *  the way back (#88), and a starred row there must still *read* starred
   *  while it does — which is why this is a flag and not `call={null}`. */
  disabled?: boolean
}) {
  const { starred, verb, toggle } = useStar(call)

  return (
    <Button
      // `Button` renders a bare `<button>`, which inside a form submits it —
      // #49's trap, and the search filters are a form.
      type="button"
      variant="outline"
      size="icon"
      aria-pressed={starred}
      aria-label={`${verb} ${describedAs}`}
      disabled={disabled || !call}
      onClick={toggle}
    >
      <StarMark starred={starred} />
    </Button>
  )
}
