/**
 * Which Talkgroups are silenced, and one tap to let any of them back in (#58,
 * spec US 25).
 *
 * Before this the Live screen could say *how many* Avoids stood and offered one
 * control over them: clear every one. So a Listener who avoided a chatty
 * tactical channel for two hours and then mis-tapped a second had to give up
 * both to fix one — and a Listener who could not remember what they had
 * silenced had nowhere to look but a four-hundred-row panel.
 *
 * The names come from the catalog, which the Talkgroups panel has already
 * fetched and RTK Query has cached — and an Avoid the catalog cannot name is
 * still a row (`lib/avoiding`), because an Avoid outlives the Talkgroup it was
 * placed on and a channel with no way to un-silence it is the worst outcome
 * here.
 */
import { Ban } from 'lucide-react'

import { avoidMinutesLeft, avoidedRows } from '@/lib/avoiding'
import { useAppDispatch, useAppSelector } from '@/store/hooks'
import { useGetCatalogQuery } from '@/store/api'
import { clearAvoid, clearAvoids, selectAvoids } from '@/store/live'

import { Sheet } from './Sheet'

export function AvoidSheet({ onClose }: { onClose: () => void }) {
  const dispatch = useAppDispatch()
  const avoided = useAppSelector(selectAvoids)
  const { data: catalog } = useGetCatalogQuery()
  const rows = avoidedRows(avoided, catalog)
  // Read once per render rather than per row, so two rows of the same Avoid
  // cannot disagree by a millisecond. `lib/avoiding` keeps no clock of its own
  // (`lib/panel`'s rule), and this list is short enough that a ticking
  // countdown would be motion for nothing.
  const now = Date.now()

  return (
    <Sheet title={`Avoiding — ${rows.length}`} onClose={onClose}>
      {rows.length === 0 ? (
        <p className="py-6 text-center font-mono text-xs text-muted-foreground">
          Nothing is avoided.
        </p>
      ) : (
        <>
          <ul
            aria-label="Avoided talkgroups"
            className="divide-y divide-border rounded-xl border border-border"
          >
            {rows.map((row) => (
              <li key={row.key} className="flex items-center gap-2 px-3 py-2">
                <Ban className="size-3.5 shrink-0 text-led-orange" aria-hidden />
                <span className="flex min-w-0 flex-1 flex-col">
                  <span className="truncate font-mono text-sm leading-tight">
                    {row.label}
                  </span>
                  <span className="truncate font-mono text-[10px] text-muted-foreground">
                    {row.systemLabel} · {row.talkgroupRef}
                  </span>
                </span>
                <span className="shrink-0 font-mono text-[10px] uppercase tracking-wider text-led-orange">
                  {row.until === 0
                    ? 'until cleared'
                    : `${avoidMinutesLeft(now, row.until)} min left`}
                </span>
                <button
                  type="button"
                  aria-label={`Stop avoiding ${row.label}`}
                  onClick={() => dispatch(clearAvoid(row.key))}
                  className="shrink-0 rounded px-2 py-1 font-mono text-[10px] font-semibold uppercase tracking-wider text-muted-foreground transition-colors hover:text-foreground"
                >
                  Unmute
                </button>
              </li>
            ))}
          </ul>
          {/* Still offered, below the list rather than instead of it: clearing
              the lot is a reasonable thing to want and a terrible thing to be
              the only option. */}
          <button
            type="button"
            onClick={() => {
              dispatch(clearAvoids())
              onClose()
            }}
            className="mt-3 w-full rounded-lg border border-border py-2 font-mono text-[11px] uppercase tracking-wider transition-colors hover:bg-muted/40"
          >
            Clear all
          </button>
        </>
      )}
    </Sheet>
  )
}
