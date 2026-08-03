import {
  Download,
  Pause,
  Play,
  SkipBack,
  SkipForward,
  Square,
} from 'lucide-react'
import { skipToken } from '@reduxjs/toolkit/query'
import { useEffect, useState, type ReactNode } from 'react'

import { CallFlags } from '@/components/CallFlags'
import { Screen } from '@/components/layout/Screen'
import { StatusLed } from '@/components/StatusLed'
import { Button } from '@/components/ui/button'
import { callCategory, systemName, talkgroupName } from '@/lib/call'
import { ledForCall } from '@/lib/led'
import type { RunSearch } from '@/lib/run'
import {
  dateTimeLocalToMs,
  downloadUrl,
  formatCallTime,
  formatDuration,
  pageSummary,
} from '@/lib/archive'
import { prefetchAudio } from '@/lib/prefetch'
import { cn } from '@/lib/utils'
import { useGetFilterOptionsQuery, useSearchCallsQuery } from '@/store/api'
import { useAppDispatch, useAppSelector } from '@/store/hooks'
import {
  enterLiveFeed,
  enterPlaybackMode,
  next,
  previous,
  runPaged,
  searchChanged,
  selectCurrentCall,
  selectHasNext,
  selectHasPrevious,
  selectIsInterrupting,
  selectIsRolling,
  selectPlaybackMode,
  selectPlaybackPosition,
  selectWantedPage,
  startRun,
  stop,
} from '@/store/playback'
import { selectIsPaused, togglePause } from '@/store/transport'
import type { Call, SearchPage, SearchQuery } from '@/types'

/** Results per page. Small enough to stay snappy on a Pi over a phone
 *  connection, large enough that scrolling beats paging. */
const PAGE_SIZE = 50

/** Stands in for the page until the first search lands. */
const EMPTY_PAGE: SearchPage = {
  results: [],
  count: 0,
  limit: PAGE_SIZE,
  offset: 0,
  hasMore: false,
}

const controlClass =
  'w-full rounded-md border border-border bg-background px-2 py-1.5 font-mono text-xs text-foreground'

/**
 * Search (Archive) — filter stored Calls, then play or download them
 * (#13, spec US 24–27).
 *
 * Filters cascade: every dropdown offers only values that have Calls behind
 * them *given the others already chosen*, computed server-side. rdio-scanner
 * builds the same dropdowns from its whole Talkgroup config, so it offers
 * options that search to nothing.
 */
export function SearchScreen() {
  const dispatch = useAppDispatch()
  /** The filters and the ordering — never the window, which is why this is a
   *  [`RunSearch`] rather than a whole `SearchQuery`. A Run's identity is
   *  exactly this value, so a `limit` or `offset` finding its way in here would
   *  be a Run that ends whenever the Listener turned a page. */
  const [filters, setFilters] = useState<RunSearch>({ sort: 'newest' })
  /** Where the window **on screen** starts. Deliberately not the Run's own
   *  offset (`Run.from`), which is where the Calls being *played* start: the
   *  Listener may browse ahead with the paging buttons while a Run walks page
   *  one, and neither should move the other (#89). */
  const [windowOffset, setWindowOffset] = useState(0)

  const { data: options } = useGetFilterOptionsQuery(filters)
  const { data, isFetching, isError } = useSearchCallsQuery({
    ...filters,
    limit: PAGE_SIZE,
    offset: windowOffset,
  })
  /** The window on screen. Empty rather than absent before the first page
   *  lands, so a row that exists always has the page it came from to start a
   *  Run with — `isFetching` and `isError` are what tell the two apart, and
   *  they already do. */
  const page = data ?? EMPTY_PAGE

  const mode = useAppSelector(selectPlaybackMode)
  const current = useAppSelector(selectCurrentCall)
  const interrupting = useAppSelector(selectIsInterrupting)
  const paused = useAppSelector(selectIsPaused)
  const position = useAppSelector(selectPlaybackPosition)
  const hasNext = useAppSelector(selectHasNext)
  const hasPrevious = useAppSelector(selectHasPrevious)

  const results = page.results

  /** Any filter change moves the window back to the first page, and tells the
   *  Run — which decides for itself whether it is still walking the search on
   *  screen. */
  function updateFilters(patch: Partial<RunSearch>) {
    const changed = { ...filters, ...patch }
    setFilters(changed)
    setWindowOffset(0)
    dispatch(searchChanged(changed))
  }

  /** The page of the Archive the Run needs on hand — the boundary it is about
   *  to cross (#32's page-ahead) or the one it is waiting at (US 25's roll-on).
   *
   *  Both used to be worked out here, from playback's index plus two pieces of
   *  local state that had to agree with it. Now there is one answer and one
   *  subscription: RTK Query holds the page before the Run arrives, so crossing
   *  the boundary costs no round trip, and `skipToken` means a Run with nothing
   *  to page onto asks for nothing at all. */
  const wanted = useAppSelector(selectWantedPage)
  const rolling = useAppSelector(selectIsRolling)
  // `currentData`, never `data`: RTK Query keeps the *previous* argument's
  // answer in `data` while the next one is in flight, so a Run that re-armed
  // onto a new search would be handed the page-ahead of the search the Listener
  // just left — and go on playing Calls from it. `currentData` is undefined
  // until the page for the argument now asked for is really in hand.
  const { currentData: ahead } = useSearchCallsQuery(wanted ?? skipToken)

  // Hand it over the moment the Run is actually at the boundary. Gated on
  // `rolling` rather than on the data alone because the page-ahead's whole
  // point is that the page is *already there* — RTK Query serves a cached one
  // without a fulfilled action, so waiting for one to arrive would miss exactly
  // the case #32 exists for.
  //
  // `wanted` rides along as the request this page answers, which the Run checks
  // against the one it named. That check is only ever as true as the caller: it
  // is `currentData` above that makes the claim an honest one here, because
  // with `data` the page could belong to a request nobody is making any more
  // while `wanted` said otherwise.
  useEffect(() => {
    if (rolling && wanted && ahead) {
      dispatch(runPaged({ window: wanted, page: ahead }))
    }
  }, [rolling, wanted, ahead, dispatch])

  // ...and warm the first Call of that page, which is the audio the Run arrives
  // at. #14's prefetch stops at the end of the loaded page, because what
  // follows the last Call of one is not in the store yet.
  //
  // The gate is a boolean rather than `wanted` itself, and not for tidiness:
  // `wanted` is rebuilt whenever the Run changes at all, so depending on it
  // would abort and restart the warm on every Call the Run advances through.
  const pagingAhead = wanted !== null
  useEffect(() => {
    if (!pagingAhead) return
    const first = ahead?.results[0]
    if (!first) return
    const controller = new AbortController()
    void prefetchAudio(first.audioUrl, controller.signal)
    return () => controller.abort()
  }, [pagingAhead, ahead?.results])

  return (
    <Screen
      title="Search"
      status={
        <button
          type="button"
          aria-pressed={mode === 'playback'}
          onClick={() =>
            dispatch(mode === 'playback' ? enterLiveFeed() : enterPlaybackMode())
          }
          className={cn(
            'rounded-md border px-2 py-1 font-mono text-[11px] uppercase tracking-wider transition-colors',
            mode === 'playback'
              ? 'border-foreground text-foreground'
              : 'border-border text-muted-foreground',
          )}
        >
          Playback mode
        </button>
      }
    >
      <form
        role="search"
        aria-label="Archive filters"
        className="grid grid-cols-2 gap-2"
        onSubmit={(event) => event.preventDefault()}
      >
        <Field label="From" htmlFor="filter-after">
          <input
            id="filter-after"
            type="datetime-local"
            className={controlClass}
            onChange={(event) =>
              updateFilters({ after: dateTimeLocalToMs(event.target.value) })
            }
          />
        </Field>
        <Field label="To" htmlFor="filter-before">
          <input
            id="filter-before"
            type="datetime-local"
            className={controlClass}
            onChange={(event) =>
              updateFilters({ before: dateTimeLocalToMs(event.target.value) })
            }
          />
        </Field>

        <Field label="System" htmlFor="filter-system">
          <select
            id="filter-system"
            className={controlClass}
            value={filters.system ?? ''}
            onChange={(event) =>
              updateFilters({
                system: event.target.value
                  ? Number(event.target.value)
                  : undefined,
                // A Talkgroup Ref only means something inside its System.
                talkgroup: undefined,
              })
            }
          >
            <option value="">Any system</option>
            {options?.systems.map((system) => (
              <option key={system.ref} value={system.ref}>
                {system.label ?? system.ref}
              </option>
            ))}
          </select>
        </Field>

        <Field label="Talkgroup" htmlFor="filter-talkgroup">
          <select
            id="filter-talkgroup"
            className={controlClass}
            value={
              filters.talkgroup === undefined
                ? ''
                : `${filters.system ?? ''}:${filters.talkgroup}`
            }
            onChange={(event) => {
              if (!event.target.value) {
                updateFilters({ talkgroup: undefined })
                return
              }
              // Picking a Talkgroup pins its System too, since a Ref is unique
              // only within one — rdio makes you choose the System first.
              const [systemRef, talkgroupRef] = event.target.value.split(':')
              updateFilters({
                system: Number(systemRef),
                talkgroup: Number(talkgroupRef),
              })
            }}
          >
            <option value="">Any talkgroup</option>
            {options?.talkgroups.map((talkgroup) => (
              <option
                key={`${talkgroup.systemRef}:${talkgroup.ref}`}
                value={`${talkgroup.systemRef}:${talkgroup.ref}`}
              >
                {talkgroup.label ?? talkgroup.ref}
              </option>
            ))}
          </select>
        </Field>

        <Field label="Group" htmlFor="filter-group">
          <select
            id="filter-group"
            className={controlClass}
            value={filters.group ?? ''}
            onChange={(event) =>
              updateFilters({ group: event.target.value || undefined })
            }
          >
            <option value="">Any group</option>
            {options?.groups.map((group) => (
              <option key={group} value={group}>
                {group}
              </option>
            ))}
          </select>
        </Field>

        <Field label="Tag" htmlFor="filter-tag">
          <select
            id="filter-tag"
            className={controlClass}
            value={filters.tag ?? ''}
            onChange={(event) =>
              updateFilters({ tag: event.target.value || undefined })
            }
          >
            <option value="">Any tag</option>
            {options?.tags.map((tag) => (
              <option key={tag} value={tag}>
                {tag}
              </option>
            ))}
          </select>
        </Field>

        {/* The kerchunk filter (#42, spec US 8), in whole seconds because that
            is the unit a listener thinks in. "Any" clears the parameter rather
            than sending 0 — a Call whose length was never measured matches no
            threshold, so a zero would silently hide the pre-#42 archive. */}
        <Field label="Min duration" htmlFor="filter-min-duration">
          <select
            id="filter-min-duration"
            className={controlClass}
            value={filters.minDuration ?? ''}
            onChange={(event) =>
              updateFilters({
                minDuration: event.target.value
                  ? Number(event.target.value)
                  : undefined,
              })
            }
          >
            <option value="">Any duration</option>
            <option value="1">1s or longer</option>
            <option value="3">3s or longer</option>
            <option value="5">5s or longer</option>
            <option value="15">15s or longer</option>
          </select>
        </Field>

        <Field label="Sort" htmlFor="filter-sort">
          <select
            id="filter-sort"
            className={controlClass}
            value={filters.sort ?? 'newest'}
            onChange={(event) =>
              updateFilters({ sort: event.target.value as SearchQuery['sort'] })
            }
          >
            <option value="newest">Newest first</option>
            <option value="oldest">Oldest first</option>
          </select>
        </Field>

        {options?.dateStartMs !== undefined && (
          <p className="col-span-2 font-mono text-[11px] text-muted-foreground/70">
            Archive spans {formatCallTime(options.dateStartMs)} –{' '}
            {formatCallTime(options.dateStopMs)}
          </p>
        )}
      </form>

      {current && (
        <NowPlaying
          call={current}
          interrupting={interrupting}
          paused={paused}
          position={position}
          hasNext={hasNext}
          hasPrevious={hasPrevious}
          onPrevious={() => dispatch(previous())}
          onNext={() => dispatch(next())}
          onTogglePause={() => dispatch(togglePause())}
          onStop={() => dispatch(stop())}
        />
      )}

      <div
        className="mt-5 flex items-baseline justify-between font-mono text-[11px] uppercase tracking-wider text-muted-foreground"
        aria-live="polite"
      >
        <span>{pageSummary(windowOffset, results.length, page.count)}</span>
        {isFetching && <span>Searching…</span>}
      </div>

      {isError ? (
        <Placeholder role="alert">
          Search failed. Check that the server is reachable.
        </Placeholder>
      ) : results.length === 0 ? (
        <Placeholder>
          {isFetching
            ? 'Searching the archive…'
            : 'No calls match these filters.'}
        </Placeholder>
      ) : (
        <ul
          aria-label="Search results"
          className="mt-3 divide-y divide-border rounded-xl border border-border bg-card"
        >
          {results.map((call, index) => (
            <ResultRow
              key={call.id}
              call={call}
              isCurrent={current?.id === call.id}
              onPlay={() => dispatch(startRun({ search: filters, page, index }))}
            />
          ))}
        </ul>
      )}

      <div className="mt-4 flex justify-between gap-2">
        <Button
          variant="outline"
          size="sm"
          disabled={windowOffset === 0}
          onClick={() => setWindowOffset(Math.max(0, windowOffset - PAGE_SIZE))}
        >
          Previous page
        </Button>
        <Button
          variant="outline"
          size="sm"
          disabled={!page.hasMore}
          onClick={() => setWindowOffset(windowOffset + PAGE_SIZE)}
        >
          Next page
        </Button>
      </div>
    </Screen>
  )
}

/** The empty / loading / failed states the result list stands in for
 *  (docs/design/brief.md states 22–24). */
function Placeholder({
  role,
  children,
}: {
  role?: 'alert'
  children: ReactNode
}) {
  return (
    <p
      role={role}
      className="mt-3 rounded-xl border border-border bg-card px-6 py-8 text-center font-mono text-sm text-muted-foreground"
    >
      {children}
    </p>
  )
}

function Field({
  label,
  htmlFor,
  children,
}: {
  label: string
  htmlFor: string
  children: ReactNode
}) {
  return (
    <div className="flex flex-col gap-1">
      <label
        htmlFor={htmlFor}
        className="font-mono text-[10px] uppercase tracking-wider text-muted-foreground"
      >
        {label}
      </label>
      {children}
    </div>
  )
}

/** How a Call reads in a list: LED, Talkgroup, System·Tag·Group, time,
 *  duration. */
function ResultRow({
  call,
  isCurrent,
  onPlay,
}: {
  call: Call
  isCurrent: boolean
  onPlay: () => void
}) {
  const name = talkgroupName(call)
  const system = systemName(call)
  const description = `${name} on ${system} at ${formatCallTime(call.timestamp)}`

  return (
    <li
      className={cn(
        'flex items-center gap-3 px-3 py-2.5',
        isCurrent && 'bg-muted/40',
      )}
    >
      <StatusLed
        color={ledForCall(call)}
        size={10}
      />
      <div className="min-w-0 flex-1">
        <p className="flex items-center gap-1.5 truncate font-mono text-sm">
          <span className="truncate">{name}</span>
          <CallFlags call={call} />
        </p>
        <p className="truncate font-mono text-[11px] text-muted-foreground">
          {[system, callCategory(call)].filter(Boolean).join(' · ')}
        </p>
      </div>
      <time className="shrink-0 font-mono text-[11px] text-muted-foreground">
        {formatCallTime(call.timestamp)}
      </time>
      {/* Right-aligned and fixed-width so a column of lengths reads as a
          column — which is what makes a kerchunk visible next to a dispatch
          without reading any of the numbers (#42, spec US 8). */}
      <span className="w-12 shrink-0 text-right font-mono text-[11px] tabular-nums text-muted-foreground">
        {formatDuration(call.durationMs)}
      </span>
      {/* An encrypted Call has no audio at all — the server sends no
          `audioUrl` for one — so it gets no controls rather than controls
          that 404 (spec US 9). */}
      {call.audioUrl && (
        <>
          <Button
            variant="outline"
            size="icon"
            aria-label={`Play ${description}`}
            onClick={onPlay}
          >
            <Play className="size-4" aria-hidden />
          </Button>
          <a
            href={downloadUrl(call.id)}
            download
            aria-label={`Download ${description}`}
            className="inline-flex size-9 shrink-0 items-center justify-center rounded-md border border-border text-muted-foreground transition-colors hover:text-foreground"
          >
            <Download className="size-4" aria-hidden />
          </a>
        </>
      )}
    </li>
  )
}

/** The bar shown while an archived Call is playing. With the live feed on this
 *  is an *interruption*: it says so, and finishing hands straight back to the
 *  live feed with its listening queue untouched (spec US 26). */
function NowPlaying({
  call,
  interrupting,
  paused,
  position,
  hasNext,
  hasPrevious,
  onPrevious,
  onNext,
  onTogglePause,
  onStop,
}: {
  call: Call
  interrupting: boolean
  paused: boolean
  position: { index: number; total: number }
  hasNext: boolean
  hasPrevious: boolean
  onPrevious: () => void
  onNext: () => void
  onTogglePause: () => void
  onStop: () => void
}) {
  return (
    <section
      aria-label="Now playing"
      className="mt-4 flex items-center gap-3 rounded-xl border border-border bg-card px-3 py-2.5"
    >
      {/* docs/design/brief.md state 6: a paused Call blinks, a playing one is
          steady. */}
      <StatusLed
        color={ledForCall(call)}
        size={12}
        pulse={paused}
      />
      <div className="min-w-0 flex-1">
        <p className="truncate font-mono text-sm">{talkgroupName(call)}</p>
        <p className="font-mono text-[11px] text-muted-foreground">
          {interrupting
            ? 'Interrupting live feed'
            : `${position.index + 1} of ${position.total}`}
        </p>
      </div>
      <Button
        variant="outline"
        size="icon"
        aria-label="Previous call"
        disabled={!hasPrevious}
        onClick={onPrevious}
      >
        <SkipBack className="size-4" aria-hidden />
      </Button>
      {/* The same pause the lock-screen button applies (#14, spec US 15). */}
      <Button
        variant="outline"
        size="icon"
        aria-label={paused ? 'Resume' : 'Pause'}
        onClick={onTogglePause}
      >
        {paused ? (
          <Play className="size-4" aria-hidden />
        ) : (
          <Pause className="size-4" aria-hidden />
        )}
      </Button>
      <Button
        variant="outline"
        size="icon"
        // Finishing an interruption hands back to the live feed, so during one
        // the control is named for what it actually does.
        aria-label={interrupting ? 'Back to live feed' : 'Next call'}
        disabled={!hasNext && !interrupting}
        onClick={onNext}
      >
        <SkipForward className="size-4" aria-hidden />
      </Button>
      <Button variant="outline" size="icon" aria-label="Stop" onClick={onStop}>
        <Square className="size-4" aria-hidden />
      </Button>
    </section>
  )
}
