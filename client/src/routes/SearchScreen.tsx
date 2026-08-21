import {
  Download,
  Pause,
  Play,
  SkipBack,
  SkipForward,
  Square,
} from 'lucide-react'
import { useState, type ReactNode } from 'react'

import { CallFlags } from '@/components/CallFlags'
import { Screen } from '@/components/layout/Screen'
import { StatusLed } from '@/components/StatusLed'
import { UnitLink } from '@/components/UnitLink'
import { Button } from '@/components/ui/button'
import { callCategory, systemName, talkgroupName } from '@/lib/call'
import { ledForCall } from '@/lib/led'
import { markName } from '@/lib/webhook'
import type { RunSearch } from '@/lib/run'
import {
  dateTimeLocalToMs,
  downloadUrl,
  formatCallTime,
  formatDuration,
  pageSummary,
} from '@/lib/archive'
import { useRunPageAhead } from '@/hooks/useRunPageAhead'
import { cn } from '@/lib/utils'
import { useGetFilterOptionsQuery, useSearchCallsQuery } from '@/store/api'
import { useAppDispatch, useAppSelector } from '@/store/hooks'
import {
  enterLiveFeed,
  enterPlaybackMode,
  next,
  previous,
  searchChanged,
  selectCurrentCall,
  selectHasNext,
  selectHasPrevious,
  selectIsInterrupting,
  selectPlaybackMode,
  selectPlaybackPosition,
  startRun,
  stop,
} from '@/store/playback'
import { selectIsPaused, togglePause } from '@/store/transport'
import { MARKS, type Call, type Mark, type SearchPage, type SearchQuery } from '@/types'

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

  // The page the Run is about to need, fetched and handed over (#32, US 25).
  // Every screen that starts a Run owes this; #47's per-Unit view is the other.
  useRunPageAhead()

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

        {/* The **Marks** filter (#42, #55). One control over the whole closed
            vocabulary rather than a checkbox per mark, because they are
            mutually exclusive as a *question* — "show me the emergencies" and
            "show me the page-outs" are two searches, not one with two boxes
            ticked — and because a mark added later is one entry in `MARKS`
            rather than another control here. */}
        <Field label="Mark" htmlFor="filter-mark">
          <select
            id="filter-mark"
            className={controlClass}
            value={filters.mark ?? ''}
            onChange={(event) =>
              updateFilters({
                mark: (event.target.value || undefined) as Mark | undefined,
              })
            }
          >
            <option value="">Any call</option>
            {MARKS.map((mark) => (
              <option key={mark} value={mark}>
                {markName(mark)}
              </option>
            ))}
          </select>
        </Field>

        {/* Search by radio (#47, spec US 44). A typed Ref rather than a
            dropdown: a county has tens of thousands of radios, so offering them
            as options would put an unbounded list in every filter response —
            and the way a Listener actually arrives here is by tapping a unit
            label, which sets this. */}
        <Field label="Unit" htmlFor="filter-unit">
          <input
            id="filter-unit"
            type="number"
            inputMode="numeric"
            placeholder="Any unit"
            className={controlClass}
            value={filters.unit ?? ''}
            onChange={(event) =>
              updateFilters({
                unit: event.target.value ? Number(event.target.value) : undefined,
              })
            }
          />
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
      {/* Who keyed it (#47, spec US 42/44) — its own column beside the length,
          for the reason that one is a column: a name in the same place on every
          row is readable down the page without reading any of it, where a name
          appended to a variable-length line is not. Truncating rather than
          wrapping, so the column stays a column on a phone; the whole name is
          one tap away on the radio's own history, which is what it links to. */}
      <span className="w-20 shrink-0 truncate text-right font-mono text-[11px] text-muted-foreground">
        <UnitLink call={call} />
      </span>
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
