import { useState } from 'react'

import { AdminGate, Placeholder, SignOutButton } from '@/components/admin/AdminUi'
import { Screen } from '@/components/layout/Screen'
import { Button } from '@/components/ui/button'
import { formatCallTime } from '@/lib/archive'
import { barHeights, bucketStartMs } from '@/lib/density'
import { useAdminSession } from '@/hooks/useAdminSession'
import {
  LISTENER_RANGES,
  listenerQuery,
  peakOf,
  type ListenerRange,
} from '@/lib/listenerChart'
import { useGetListenerHistoryQuery } from '@/store/api'

/**
 * Settings → Admin → Listeners (#62, spec US 41) — how many people have been
 * listening, and when.
 *
 * **Counts, and nothing else.** The server records one number per interval and
 * has nothing beside it to record (ADR-0011 rule 5), so this screen can only
 * ever answer "how many, and when" — which is the whole question. There is no
 * per-Talkgroup breakdown here and there is not going to be one: on a quiet
 * channel with one listener that would be a record of who was listening.
 *
 * Behind the admin gate, unlike every other chart in the app: the Archive is
 * open because listening is open, and how many people take that up is the
 * Operator's own business rather than a fact a Listener gets to publish for
 * them.
 *
 * rdio-scanner has the opposite of this — it logs every listener's IP and
 * access-code ident at INFO and keeps no history at all, so an operator there
 * has a permanent record of *who* and no answer to *how many*.
 */
export function ListenersScreen() {
  const signedIn = useAdminSession()
  const [range, setRange] = useState<ListenerRange>(LISTENER_RANGES[0])
  // The clock is read once per range change rather than on every render, so the
  // request argument is stable and RTK Query is not re-fetching a chart whose
  // window has crept a millisecond.
  const [asked, setAsked] = useState(() => listenerQuery(LISTENER_RANGES[0], Date.now()))
  const { data: series, isFetching } = useGetListenerHistoryQuery(asked, {
    skip: !signedIn,
  })

  const choose = (next: ListenerRange) => {
    setRange(next)
    setAsked(listenerQuery(next, Date.now()))
  }

  const peak = series && peakOf(series)

  return (
    <Screen title="Listeners" status={<SignOutButton />}>
      <AdminGate>
        <div className="mt-3 flex gap-1">
          {LISTENER_RANGES.map((one) => (
            <Button
              key={one.id}
              type="button"
              variant={one.id === range.id ? 'secondary' : 'outline'}
              size="sm"
              aria-pressed={one.id === range.id}
              className="h-7 px-2 font-mono text-[10px] uppercase tracking-wider"
              onClick={() => choose(one)}
            >
              {one.label}
            </Button>
          ))}
        </div>

        {series === undefined ? (
          <Placeholder>
            {isFetching ? 'Reading the counts…' : 'No listener history yet.'}
          </Placeholder>
        ) : (
          <div className="mt-3 space-y-2 rounded-xl border border-border bg-card px-4 py-4">
            <p className="font-mono text-xs text-muted-foreground">
              {peak
                ? `Peak ${peak.listeners} ${peak.listeners === 1 ? 'listener' : 'listeners'}, ${formatCallTime(peak.atMs)}`
                : 'Nobody has listened in this window.'}
            </p>
            <div
              className="flex h-24 w-full items-end gap-px"
              role="img"
              aria-label={
                peak
                  ? `Peak listeners over the last ${range.label}: highest ${peak.listeners} at ${formatCallTime(peak.atMs)}`
                  : `Peak listeners over the last ${range.label}: nobody`
              }
            >
              {barHeights(series.values).map((height, bucket) => (
                <span
                  key={bucket}
                  title={`${formatCallTime(bucketStartMs(series, bucket))} — ${series.values[bucket]}`}
                  className="min-h-px flex-1 rounded-[1px] bg-muted-foreground/70"
                  style={{ height: `${Math.round(height * 100)}%` }}
                />
              ))}
            </div>
            <div className="flex justify-between font-mono text-[10px] text-muted-foreground">
              <span>{formatCallTime(series.fromMs)}</span>
              {/* `toMs` is exclusive, so the label is the last instant covered —
                  a millisecond later reads as a whole extra bucket. */}
              <span>{formatCallTime(series.toMs - 1)}</span>
            </div>
          </div>
        )}

        <p className="mt-4 px-1 font-mono text-[11px] text-muted-foreground">
          Counts only — no addresses, no sessions, no per-talkgroup breakdown.
          How often they are recorded is [listeners] interval_secs, and how long
          they are kept is [retention] listener_days.
        </p>
      </AdminGate>
    </Screen>
  )
}
