import {
  Download,
  Pause,
  Play,
  SkipBack,
  SkipForward,
  Square,
} from 'lucide-react'
import { useEffect, useMemo, useState, type ReactNode } from 'react'

import { CallFlags } from '@/components/CallFlags'
import { Screen } from '@/components/layout/Screen'
import { StatusLed } from '@/components/StatusLed'
import { Button } from '@/components/ui/button'
import { callCategory, systemName, talkgroupName } from '@/lib/call'
import { ledForCall } from '@/lib/led'
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
  playResults,
  previous,
  selectCurrentCall,
  selectHasNext,
  selectHasPrevious,
  selectIsExhausted,
  selectIsInterrupting,
  selectIsNearingPageEnd,
  selectPlaybackMode,
  selectPlaybackPosition,
  stop,
} from '@/store/playback'
import { selectIsPaused, togglePause } from '@/store/transport'
import type { Call, SearchQuery } from '@/types'

/** Results per page. Small enough to stay snappy on a Pi over a phone
 *  connection, large enough that scrolling beats paging. */
const PAGE_SIZE = 50

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
  const [filters, setFilters] = useState<SearchQuery>({ sort: 'newest' })
  const [offset, setOffset] = useState(0)

  const { data: options } = useGetFilterOptionsQuery(filters)
  /** The search behind the results on screen. Named because #32's page-ahead has
   *  to be able to say "the run playing is walking *this* page". */
  const pageQuery = useMemo(
    () => ({ ...filters, limit: PAGE_SIZE, offset }),
    [filters, offset],
  )
  const { data: page, isFetching, isError } = useSearchCallsQuery(pageQuery)

  const mode = useAppSelector(selectPlaybackMode)
  const current = useAppSelector(selectCurrentCall)
  const interrupting = useAppSelector(selectIsInterrupting)
  const paused = useAppSelector(selectIsPaused)
  const exhausted = useAppSelector(selectIsExhausted)
  const position = useAppSelector(selectPlaybackPosition)
  const hasNext = useAppSelector(selectHasNext)
  const hasPrevious = useAppSelector(selectHasPrevious)
  const nearingPageEnd = useAppSelector(selectIsNearingPageEnd)

  // Stable across renders while the page is, so the resume effect below can
  // depend on it without re-firing every render.
  const results = useMemo(() => page?.results ?? [], [page?.results])

  /** Set while waiting for the page playback rolled onto (US 25). */
  const [resumeOnNextPage, setResumeOnNextPage] = useState(false)

  /** The search whose page the run currently playing is walking (#32).
   *
   *  Playback's index counts into the result set it was *started* from, so it
   *  only says anything about the page on screen while that is still the same
   *  page. A filter change replaces the set; the paging buttons move the window
   *  out from under it. In both cases the index keeps reporting "nearly at the
   *  end" of a page nobody is on, and a page-ahead decided from it would fetch
   *  — and warm the audio of — a boundary the listener is nowhere near. Holding
   *  the *query* rather than a flag makes both cases one comparison; a boolean
   *  caught the filter one and missed the paging one entirely.
   *
   *  **This is not RTK Query's cache key.** That one sorts its keys
   *  (`defaultSerializeQueryArgs`); this is a plain `JSON.stringify`, so it is
   *  key-*order* sensitive and only sound because `filters` is only ever
   *  extended (`{ ...current, ...patch }`), never rebuilt or deleted from.
   *  Something that rebuilds the object — a "clear filters" button — would
   *  reorder the keys and leave this permanently false. That fails safe (no
   *  page-ahead, never a wrong one) but it fails *silently*, so rebuild the
   *  filters and you have to compare something order-independent instead. */
  const [playingQuery, setPlayingQuery] = useState<string | null>(null)
  const walkingThisPage = playingQuery === JSON.stringify(pageQuery)

  /** Any filter change invalidates the page window the listener was on — and
   *  any playback that was rolling onto the next page of the *old* filters. */
  function updateFilters(patch: Partial<SearchQuery>) {
    setFilters((current) => ({ ...current, ...patch }))
    setOffset(0)
    setResumeOnNextPage(false)
  }

  /** Start playing the `index`-th loaded result, recording which search those
   *  results came from so the page-ahead knows what playback is walking. US 25's
   *  roll onto the next page records the same thing where it resumes. */
  function play(index: number) {
    setPlayingQuery(JSON.stringify(pageQuery))
    dispatch(
      playResults({
        results,
        index,
        offset: page?.offset ?? 0,
        total: page?.count ?? results.length,
      }),
    )
  }

  // Page-ahead (#32). The page boundary is the only transition that costs a
  // search *and* a cold audio fetch, so it is the one a listener notices — and
  // #14's prefetch stopped exactly there, because `selectNextCall` can only see
  // the loaded page. Asking for the next page as a *second subscription* is what
  // makes it free: this is the same cache key `offset + PAGE_SIZE` will use, so
  // when playback rolls on, RTK Query already holds the answer and the roll-on
  // effect below resumes without a round trip. `skip` keeps it to the cases that
  // want it — nothing while the live feed owns the audio (the selector's job),
  // nothing when there is no page to fetch, and nothing while what is playing
  // belongs to a set the filters have since replaced.
  const pagingAhead =
    walkingThisPage && nearingPageEnd && page?.hasMore === true
  const { data: nextPage } = useSearchCallsQuery(
    { ...pageQuery, offset: offset + PAGE_SIZE },
    { skip: !pagingAhead },
  )

  // ...and warm the first Call of it, which is the audio playback arrives at.
  //
  // `pagingAhead` is a dependency, and not for tidiness: RTK Query **keeps its
  // `data` when `skip` flips to true**, so `nextPage` holds its identity when
  // the page-ahead stands down. Without this the effect would never re-run,
  // the cleanup would never fire, and an audio warm nobody wants any more would
  // run to completion. Depending on the decision rather than only on its result
  // is what makes the abort reach the case it exists for.
  useEffect(() => {
    if (!pagingAhead) return
    const first = nextPage?.results[0]
    if (!first) return
    const controller = new AbortController()
    void prefetchAudio(first.audioUrl, controller.signal)
    return () => controller.abort()
  }, [pagingAhead, nextPage?.results])

  // Playback ran off the end of the loaded page. US 25 asks for sequential
  // playback through the *filtered results*, not through one page of them, so
  // roll onto the next page and keep going. `stop()` consumes the cue straight
  // away, so this can't fire twice for one exhaustion.
  useEffect(() => {
    if (!exhausted) return
    if (page?.hasMore) {
      setOffset((current) => current + PAGE_SIZE)
      setResumeOnNextPage(true)
    }
    dispatch(stop())
  }, [exhausted, page?.hasMore, dispatch])

  // ...and pick up at the top of that page once it lands — which re-arms the
  // page-ahead against the page now playing, so a long run keeps warming each
  // boundary rather than only the first.
  useEffect(() => {
    if (!resumeOnNextPage || isFetching || results.length === 0) return
    setResumeOnNextPage(false)
    setPlayingQuery(JSON.stringify(pageQuery))
    dispatch(
      playResults({
        results,
        index: 0,
        offset: page?.offset ?? 0,
        total: page?.count ?? results.length,
      }),
    )
  }, [
    resumeOnNextPage,
    isFetching,
    results,
    pageQuery,
    page?.offset,
    page?.count,
    dispatch,
  ])

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
        <span>{pageSummary(offset, results.length, page?.count ?? 0)}</span>
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
              onPlay={() => play(index)}
            />
          ))}
        </ul>
      )}

      <div className="mt-4 flex justify-between gap-2">
        <Button
          variant="outline"
          size="sm"
          disabled={offset === 0}
          onClick={() => setOffset(Math.max(0, offset - PAGE_SIZE))}
        >
          Previous page
        </Button>
        <Button
          variant="outline"
          size="sm"
          disabled={!page?.hasMore}
          onClick={() => setOffset(offset + PAGE_SIZE)}
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
