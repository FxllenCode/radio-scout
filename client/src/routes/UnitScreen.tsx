import { ArrowLeft, Download, Play } from 'lucide-react'
import type { ReactNode } from 'react'
import { Link, useParams } from 'react-router-dom'

import { CallFlags } from '@/components/CallFlags'
import { Screen } from '@/components/layout/Screen'
import { StatusLed } from '@/components/StatusLed'
import { Button } from '@/components/ui/button'
import { useRunPageAhead } from '@/hooks/useRunPageAhead'
import { downloadUrl, formatCallTime, formatDuration } from '@/lib/archive'
import { talkgroupName } from '@/lib/call'
import { ledForCall } from '@/lib/led'
import { useGetUnitHistoryQuery, useSearchCallsQuery } from '@/store/api'
import { useAppDispatch } from '@/store/hooks'
import { startRun } from '@/store/playback'
import type { Call, RefSpan, SearchPage, UnitHistory } from '@/types'

/** How many of this radio's Calls the page carries. Enough to be a night's
 *  traffic for one apparatus; the archive search is where a longer look goes. */
const PAGE_SIZE = 50

/**
 * One radio's history (#47, spec US 44) — "who said that, and where else".
 *
 * Reached by tapping a unit label anywhere it renders: the scanner display, the
 * recent list, a search row. rdio-scanner stores unit rows and has no way to
 * show one at all, so a listener there sees a number three times and can never
 * ask whether it was the same radio.
 *
 * Two requests, and deliberately: the summary above is a `GROUP BY` a search
 * cannot answer without reading the whole archive, and the list below is an
 * ordinary archive search with `unit` set — the same one the Search screen
 * runs, so it pages, plays and downloads identically and this screen invents
 * nothing.
 */
export function UnitScreen() {
  const params = useParams<{ systemRef: string; ref: string }>()
  const systemRef = Number(params.systemRef)
  const unitRef = Number(params.ref)

  const { data: unit, isError } = useGetUnitHistoryQuery({ systemRef, ref: unitRef })
  const search = { system: systemRef, unit: unitRef }
  const { data: page } = useSearchCallsQuery({ ...search, limit: PAGE_SIZE, offset: 0 })
  // A Run started here walks this radio's Calls, and owes the same page-ahead
  // the Search screen does — without it, it would stop dead at the boundary and
  // look exactly like the radio having gone quiet.
  useRunPageAhead()

  return (
    <Screen
      title={unit?.label ?? `Unit ${unitRef}`}
      status={
        <Link
          to="/search"
          aria-label="Back to search"
          className="inline-flex items-center gap-1"
        >
          <ArrowLeft className="size-3.5" aria-hidden />
          Search
        </Link>
      }
    >
      {isError ? (
        <p
          role="alert"
          className="rounded-xl border border-border bg-card px-6 py-8 text-center font-mono text-sm text-muted-foreground"
        >
          No radio {unitRef} on system {systemRef}.
        </p>
      ) : unit ? (
        <>
          <Summary unit={unit} />
          <Talkgroups unit={unit} />
          <Calls calls={page?.results ?? []} search={search} page={page} />
        </>
      ) : null}
    </Screen>
  )
}

/** The apparatus: what it is called, what it answers to, and how long it has
 *  been around. */
function Summary({ unit }: { unit: UnitHistory }) {
  return (
    <section
      aria-label="Unit summary"
      className="rounded-xl border border-border bg-card px-4 py-4"
    >
      <p className="truncate font-mono text-xs text-muted-foreground">
        {unit.systemLabel ?? `System ${unit.systemRef}`} · unit {unit.ref}
      </p>
      <dl className="mt-3 grid grid-cols-2 gap-x-4 gap-y-2 font-mono text-[11px]">
        <Stat name="Calls" testId="unit-calls">
          {unit.callCount}
        </Stat>
        <Stat name="Also" testId="unit-members">
          {spans(unit.memberRefs)}
        </Stat>
        <Stat name="First heard" testId="unit-first">
          {formatCallTime(unit.firstHeardMs)}
        </Stat>
        <Stat name="Last heard" testId="unit-last">
          {formatCallTime(unit.lastHeardMs)}
        </Stat>
      </dl>
    </section>
  )
}

/** The Ranges and lone member Refs an apparatus also answers to (#45), written
 *  the way an operator writes them in the CSV. */
function spans(members: RefSpan[] | undefined): string {
  if (!members || members.length === 0) return '—'
  return members
    .map((span) => (span.from === span.to ? `${span.from}` : `${span.from}-${span.to}`))
    .join('; ')
}

/** Where this radio talks, busiest first — the question the whole view exists
 *  for, which is why the server orders it rather than the screen. */
function Talkgroups({ unit }: { unit: UnitHistory }) {
  if (unit.talkgroups.length === 0) {
    return (
      <p className="mt-5 rounded-xl border border-border bg-card px-6 py-8 text-center font-mono text-sm text-muted-foreground">
        This radio has not been heard yet.
      </p>
    )
  }

  return (
    <section className="mt-5">
      <h2 className="mb-2 font-mono text-xs uppercase tracking-wider text-muted-foreground">
        Talkgroups
      </h2>
      <ul
        aria-label="Talkgroups used"
        className="divide-y divide-border rounded-xl border border-border bg-card"
      >
        {unit.talkgroups.map((talkgroup) => (
          <li
            key={talkgroup.ref}
            className="flex items-center gap-3 px-3 py-2.5 font-mono text-sm"
          >
            <span className="min-w-0 flex-1 truncate">
              {talkgroup.label ?? `Talkgroup ${talkgroup.ref}`}
            </span>
            <span className="shrink-0 text-[11px] tabular-nums text-muted-foreground">
              {talkgroup.calls}
            </span>
            <time className="shrink-0 text-[11px] text-muted-foreground">
              {formatCallTime(talkgroup.lastHeardMs)}
            </time>
          </li>
        ))}
      </ul>
    </section>
  )
}

/** This radio's Calls, newest first — playable and downloadable exactly as a
 *  search result is, because that is what they are. */
function Calls({
  calls,
  search,
  page,
}: {
  calls: Call[]
  search: { system: number; unit: number }
  page: SearchPage | undefined
}) {
  const dispatch = useAppDispatch()
  if (calls.length === 0 || !page) return null

  return (
    <section className="mt-5">
      <h2 className="mb-2 font-mono text-xs uppercase tracking-wider text-muted-foreground">
        Calls
      </h2>
      <ul
        aria-label="Unit calls"
        className="divide-y divide-border rounded-xl border border-border bg-card"
      >
        {calls.map((call, index) => (
          <li key={call.id} className="flex items-center gap-3 px-3 py-2.5">
            <StatusLed color={ledForCall(call)} size={10} />
            <div className="min-w-0 flex-1">
              <p className="flex items-center gap-1.5 truncate font-mono text-sm">
                <span className="truncate">{talkgroupName(call)}</span>
                <CallFlags call={call} />
              </p>
              <time className="font-mono text-[11px] text-muted-foreground">
                {formatCallTime(call.timestamp)}
              </time>
            </div>
            <span className="w-12 shrink-0 text-right font-mono text-[11px] tabular-nums text-muted-foreground">
              {formatDuration(call.durationMs)}
            </span>
            {/* An encrypted Call has no audio at all — no `audioUrl` — so it
                gets no controls rather than controls that 404 (spec US 9). */}
            {call.audioUrl && (
              <>
                <Button
                  variant="outline"
                  size="icon"
                  aria-label={`Play ${talkgroupName(call)} at ${formatCallTime(call.timestamp)}`}
                  onClick={() => dispatch(startRun({ search, page, index }))}
                >
                  <Play className="size-4" aria-hidden />
                </Button>
                <a
                  href={downloadUrl(call.id)}
                  download
                  aria-label={`Download ${talkgroupName(call)} at ${formatCallTime(call.timestamp)}`}
                  className="inline-flex size-9 shrink-0 items-center justify-center rounded-md border border-border text-muted-foreground transition-colors hover:text-foreground"
                >
                  <Download className="size-4" aria-hidden />
                </a>
              </>
            )}
          </li>
        ))}
      </ul>
    </section>
  )
}

function Stat({
  name,
  testId,
  children,
}: {
  name: string
  testId: string
  children: ReactNode
}) {
  return (
    <div className="flex items-baseline justify-between gap-2">
      <dt className="text-[10px] uppercase tracking-wider text-muted-foreground">
        {name}
      </dt>
      <dd data-testid={testId} className="truncate tabular-nums">
        {children}
      </dd>
    </div>
  )
}
