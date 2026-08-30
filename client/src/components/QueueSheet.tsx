/**
 * What is waiting, and what a Listener can do about it (#58, spec US 24).
 *
 * The `Q` count has been on the display since #11 and has never been anything
 * but a number. This is the ticket's own sentence — "the queue is a tool
 * instead of a number" — so every row offers the two things a number cannot:
 * hear this one now, or let it go.
 *
 * # Three rules worth knowing
 *
 * **Every control names a Call id**, never a position. The list re-orders under
 * the Listener's thumb whenever a **Priority** Call arrives, and a control
 * keyed on where a row *sat* would act on whichever Call had slid into that
 * place. `key={call.id}` is the other half: React moves the row's DOM node
 * rather than rewriting it, so the button a finger is on stays the button it
 * went down on.
 *
 * **Playing and jumping close the sheet; dropping does not.** The first two are
 * decisions about what to hear next and the sheet has served its purpose;
 * dropping is pruning, and closing after each one would make clearing four
 * Calls cost eight taps.
 *
 * **Only the jump is counted as missed.** The ticket asks for that explicitly
 * — "counted as missed, never silent" — and the asymmetry with the per-row drop
 * is deliberate: a Listener who read a row and let it go is not someone traffic
 * was kept from, which is what `missed` means (`store/live`).
 */
import { Play, SkipForward, X } from 'lucide-react'

import { formatCallTime } from '@/lib/archive'
import { talkgroupName, systemName } from '@/lib/call'
import { ledForCall } from '@/lib/led'
import { useAppDispatch, useAppSelector } from '@/store/hooks'
import { dropQueued, jumpToNewest, playQueued, selectQueue } from '@/store/live'

import { Sheet, SheetAction } from './Sheet'
import { StatusLed } from './StatusLed'
import { UnitLink } from './UnitLink'

export function QueueSheet({ onClose }: { onClose: () => void }) {
  const dispatch = useAppDispatch()
  const queue = useAppSelector(selectQueue)

  const play = (id: number) => {
    dispatch(playQueued(id))
    onClose()
  }

  return (
    <Sheet title={`Queue — ${queue.length} waiting`} onClose={onClose}>
      {queue.length === 0 ? (
        <p className="py-6 text-center font-mono text-xs text-muted-foreground">
          Nothing waiting — the feed is caught up.
        </p>
      ) : (
        <>
          {/* The way out of a backlog, at the top where a Listener who opened
              this sheet *because* it said 40 will look first. */}
          <SheetAction
            className="mb-3"
            onClick={() => {
              dispatch(jumpToNewest())
              onClose()
            }}
          >
            <SkipForward className="size-3.5" aria-hidden />
            Jump to newest
            {queue.length > 1 && (
              <span className="text-muted-foreground">
                — {queue.length - 1} counted missed
              </span>
            )}
          </SheetAction>

          <ul
            aria-label="Queued calls"
            className="divide-y divide-border rounded-xl border border-border"
          >
            {queue.map((call) => (
              <li key={call.id} className="flex items-center gap-2 px-3 py-2">
                <StatusLed color={ledForCall(call)} size={10} />
                <span className="flex min-w-0 flex-1 flex-col">
                  <span className="truncate font-mono text-sm leading-tight">
                    {talkgroupName(call)}
                  </span>
                  <span className="truncate font-mono text-[10px] text-muted-foreground">
                    {systemName(call)} · {formatCallTime(call.timestamp)}
                  </span>
                </span>
                {/* Not inside a row-wide button: an anchor within a button is
                    neither valid HTML nor reachable by a screen reader (#47). */}
                <UnitLink
                  call={call}
                  className="max-w-20 shrink-0 font-mono text-[10px] text-muted-foreground"
                />
                <button
                  type="button"
                  aria-label={`Play ${talkgroupName(call)} now`}
                  onClick={() => play(call.id)}
                  className="shrink-0 rounded p-1.5 text-muted-foreground transition-colors hover:text-foreground"
                >
                  <Play className="size-4" aria-hidden />
                </button>
                <button
                  type="button"
                  aria-label={`Drop ${talkgroupName(call)}`}
                  onClick={() => dispatch(dropQueued(call.id))}
                  className="shrink-0 rounded p-1.5 text-muted-foreground transition-colors hover:text-led-red"
                >
                  <X className="size-4" aria-hidden />
                </button>
              </li>
            ))}
          </ul>
        </>
      )}
    </Sheet>
  )
}
