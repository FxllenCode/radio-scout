import { Lock, RadioTower, TriangleAlert, Waypoints } from 'lucide-react'
import type { ReactNode } from 'react'

import { cn } from '@/lib/utils'
import type { Call } from '@/types'

/**
 * The four things known about a transmission that change how a listener should
 * read it: the **Emergency** bit the radio set (#42, spec US 5), that the
 * talkgroup was encrypted (spec US 9), that a **Tone profile** on this channel
 * was paged in the audio (#55, spec US 20), and that it arrived on a **Patch**
 * (#56, spec US 54).
 *
 * The first and the third are **Marks** (CONTEXT.md) and are shown identically
 * here on purpose — one is proved by the wire and the other by this Instance's
 * own signal processing, but a Listener reading a list is asking the same
 * question of both. Neither wakes anybody: Radio-Scout does not notify
 * (ADR-0014).
 *
 * One component, used by the scanner display, the RECENT list and the archive
 * rows, so an emergency looks the same everywhere it appears. Both render
 * nothing at all when the flag is unset, which is nearly always — a badge that
 * is usually present is a badge nobody sees.
 *
 * The **Patch** chip is the odd one out and deliberately kept here anyway: it
 * is provenance rather than a **Mark**, so it never fires a Webhook and is not
 * in `MARKS`. But a Listener reading a row is asking the same *kind* of
 * question of it — "is there something about this one?" — and putting it
 * anywhere else would be a second badge strip beside this one.
 *
 * **The icon carries the meaning, not the colour.** Red-on-amber is not a
 * distinction every listener can make, so each badge is a shape *and* a
 * `title`, and the title is what the tests and a screen reader read.
 */
export function CallFlags({
  call,
  className,
}: {
  call: Pick<Call, 'emergency' | 'encrypted' | 'tone' | 'tones' | 'patches'>
  className?: string
}) {
  const patches = call.patches ?? []
  if (!call.emergency && !call.encrypted && !call.tone && patches.length === 0)
    return null
  return (
    <span className={cn('flex shrink-0 items-center gap-1', className)}>
      {call.emergency && (
        <Flag label="Emergency" className="text-[var(--led-red,#ef4444)]">
          <TriangleAlert className="size-3.5" aria-hidden />
        </Flag>
      )}
      {/* Amber rather than the emergency red, because a page-out is a thing to
          look at and an emergency is a thing to act on — and, per the note
          above, the *icon* is what carries that distinction for a Listener who
          cannot tell the two colours apart. */}
      {call.tone && (
        <Flag label={pagedLabel(call.tones)} className="text-[var(--led-amber,#f59e0b)]">
          <RadioTower className="size-3.5" aria-hidden />
        </Flag>
      )}
      {call.encrypted && (
        <Flag label="Encrypted" className="text-muted-foreground">
          <Lock className="size-3.5" aria-hidden />
        </Flag>
      )}
      {/* Muted, like the encryption lock and unlike the two Marks above: a
          patch is a fact about *routing*, not a thing to look at or act on.
          Last in the strip for the same reason. */}
      {patches.length > 0 && (
        <Flag label={patchedLabel(patches)} className="text-muted-foreground">
          <Waypoints className="size-3.5" aria-hidden />
        </Flag>
      )}
    </span>
  )
}

/** Which channels this transmission was also on.
 *
 *  Named rather than counted, because "patched" alone leaves a Listener with a
 *  question they have no way to answer: a Call filed under FD Dispatch that also
 *  went out on TAC 3 is a different thing from one patched to a neighbouring
 *  county, and only the Refs say which. Refs rather than labels because a
 *  patch's members are Refs on the wire (CONTEXT.md) — some of them minted for
 *  the patch event itself, so there is frequently no Talkgroup to have a label.
 *
 *  Takes the resolved list rather than the Call, because the caller has already
 *  had to decide whether there is a patch at all — the same answer gates the
 *  whole strip — and re-asking here would be a branch nothing can reach. */
function patchedLabel(patches: number[]): string {
  return `Patched to ${patches.join(', ')}`
}

/** What the tone-out badge is *called*, which is where the station name lives.
 *
 *  The badge's title and `aria-label` are the only thing a listener reads —
 *  the icon is decorative — so naming the station here is what turns "a page
 *  happened" into "Station 12 was paged" on a list, a live frame and a screen
 *  reader alike, with no second request and no screen to open.
 *
 *  Falls back to the bare word when nothing named it: a Call marked by a
 *  profile that has since been read back unreadably still says a page happened,
 *  which is the true half of what is known. */
function pagedLabel(tones: Call['tones']): string {
  const named = (tones ?? []).map((page) => page.label.trim()).filter(Boolean)
  return named.length === 0 ? 'Tone-out' : `Tone-out: ${named.join(', ')}`
}

/** One badge. The `title` and the `aria-label` carry the meaning; the icon
 *  inside is decorative, so a listener who cannot tell the two colours apart
 *  still gets the word on hover and in a screen reader. */
function Flag({
  label,
  className,
  children,
}: {
  label: string
  className?: string
  children: ReactNode
}) {
  return (
    <span
      role="img"
      title={label}
      aria-label={label}
      className={cn('inline-flex items-center', className)}
    >
      {children}
    </span>
  )
}
