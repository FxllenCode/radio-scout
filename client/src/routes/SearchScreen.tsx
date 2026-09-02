import {
  Download,
  FastForward,
  History,
  Link2,
  Pause,
  Play,
  RotateCcw,
  SkipBack,
  SkipForward,
  Square,
} from 'lucide-react'
import { useEffect, useMemo, useRef, useState, type ReactNode } from 'react'
import { Link, useSearchParams } from 'react-router-dom'

import { ActivityHeatmap } from '@/components/ActivityHeatmap'
import { CallFlags } from '@/components/CallFlags'
import { DateField, Field, controlClass } from '@/components/Field'
import { DensityRibbon } from '@/components/DensityRibbon'
import { Screen } from '@/components/layout/Screen'
import { StatusLed } from '@/components/StatusLed'
import { UnitLink } from '@/components/UnitLink'
import { Button } from '@/components/ui/button'
import { callCategory, systemName, talkgroupName } from '@/lib/call'
import { ledForCall } from '@/lib/led'
import { markName } from '@/lib/webhook'
import { sameSearch, type RunSearch } from '@/lib/run'
import { playingDetail } from '@/lib/strip'
import {
  downloadUrl,
  formatCallTime,
  formatDuration,
  pageSummary,
} from '@/lib/archive'
import { PRESETS, rangeOf } from '@/lib/dateRange'
import {
  RIBBON_BUCKETS,
  bucketOfOffset,
  offsetOfBucket,
  totalOf,
} from '@/lib/density'
import { HOUR_MS, heatmapWindow } from '@/lib/heatmap'
import { readSearchUrl, writeSearchUrl, type SearchUrl } from '@/lib/searchUrl'
import { dvrLink } from '@/lib/dvr'
import { useRunPageAhead } from '@/hooks/useRunPageAhead'
import { useShareLink } from '@/hooks/useShareLink'
import { cn } from '@/lib/utils'
import {
  useGetActivityQuery,
  useGetCallQuery,
  useGetFilterOptionsQuery,
  useLazySearchCallsQuery,
  useSearchCallsQuery,
} from '@/store/api'
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

/** The two views of the same numbers (#62): where it was busy, and when it
 *  usually is. */
const CHARTS = [
  { id: 'ribbon', label: 'Timeline' },
  { id: 'heatmap', label: 'By hour' },
] as const

/** Stands in for the page until the first search lands. */
const EMPTY_PAGE: SearchPage = {
  results: [],
  count: 0,
  limit: PAGE_SIZE,
  offset: 0,
  hasMore: false,
}

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
  const [params, setParams] = useSearchParams()
  /** The whole of what this screen is showing, read off the URL — the filters
   *  and the ordering (a **Run**'s identity, exactly), the window on screen,
   *  and a Call somebody linked to.
   *
   *  Memoized on the query *string*, so a value that has not changed keeps its
   *  identity: the filters are an RTK Query argument, an effect dependency and
   *  the thing a Run compares itself against, and a fresh object every render
   *  would re-issue all three. */
  const query = params.toString()
  const { search: filters, offset: windowOffset, call: linkedCall } = useMemo(
    () => readSearchUrl(new URLSearchParams(query)),
    [query],
  )

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

  /** How busy the archive was under these filters (#62, spec US 34–35).
   *
   *  Keyed on the filters alone — no `limit`, no `offset` — so paging through
   *  the results never refetches it, which is what makes a second aggregate per
   *  search affordable on a Pi. */
  const { data: activity } = useGetActivityQuery({
    ...filters,
    buckets: RIBBON_BUCKETS,
  })
  /** Which of the two views of that the Listener is looking at. */
  const [chart, setChart] = useState<'ribbon' | 'heatmap'>('ribbon')
  /** The heatmap's own request: hourly buckets over a bounded window, so the
   *  server never widens them underneath the fold into local hours. Skipped
   *  entirely while the ribbon is showing — a chart nobody is looking at is a
   *  round trip nobody asked for. */
  const { data: hourly } = useGetActivityQuery(
    { ...filters, ...(activity ? heatmapWindow(activity) : {}), bucketMs: HOUR_MS },
    { skip: chart !== 'heatmap' || !activity },
  )

  const mode = useAppSelector(selectPlaybackMode)
  const current = useAppSelector(selectCurrentCall)
  const interrupting = useAppSelector(selectIsInterrupting)
  const paused = useAppSelector(selectIsPaused)
  const position = useAppSelector(selectPlaybackPosition)
  const hasNext = useAppSelector(selectHasNext)
  const hasPrevious = useAppSelector(selectHasPrevious)

  const results = page.results

  /** Every link control on this screen, and what it says afterwards. */
  const link = useShareLink()

  /** Where the URL is going next. One writer, so the two things it decides
   *  cannot drift: a change of *anything* returns to the first page unless it
   *  says otherwise, and a deep-linked Call is dropped once it has been acted
   *  on — the address bar then describes what is on screen rather than what
   *  opened it. */
  const goTo = (next: Partial<SearchUrl>, replace = false) =>
    setParams(writeSearchUrl({ search: filters, offset: 0, ...next }), { replace })

  /**
   * Which control is being *typed* into, if any.
   *
   * A dropdown is one decision and earns one history entry. A number typed into
   * a box is not four, and a date is not sixteen — so the first keystroke in a
   * field pushes (back returns to the search before the typing) and the rest
   * replace (back does not walk it digit by digit). Anything that is not typing
   * clears this, so the next keystroke starts a new entry.
   */
  const typing = useRef<string | null>(null)

  /** Any filter change moves the window back to the first page. Unlike before
   *  #61 this does *not* tell the Run: the URL is the state, so the Run is told
   *  where the change is *observed* rather than at every control that could
   *  cause one — which is the only way the back button gets the same treatment
   *  as a dropdown (#92's argument, one layer up). */
  const updateFilters = (patch: Partial<RunSearch>, typedInto: string | null = null) => {
    goTo({ search: { ...filters, ...patch } }, typing.current === typedInto && typedInto !== null)
    typing.current = typedInto
  }

  /**
   * Tell the **Run** the search on screen changed.
   *
   * Skipped on the way in, because arriving at this screen is not a Listener
   * changing anything: a Run started from the per-Unit view (#47) and still
   * walking would otherwise be disarmed by nothing more than a tab switch.
   */
  const lastSearch = useRef(filters)
  useEffect(() => {
    // Compared with the predicate the Run itself uses (#89), not by reference:
    // `filters` is rebuilt whenever *any* of the URL changes, so turning a page
    // would otherwise mint a new Run for a search that had not moved.
    if (sameSearch(lastSearch.current, filters)) return
    lastSearch.current = filters
    dispatch(searchChanged(filters))
  }, [dispatch, filters])

  /** Put a link to `state` wherever this platform puts links (#61, US 30). */
  const send = (state: SearchUrl, title: string) =>
    link.share('/search', writeSearchUrl(state), title)

  /**
   * Play forward in time from a Call, whatever the list is sorted by (#61).
   *
   * A **Run** is one concept with an ordering (CONTEXT.md), so this is not a
   * new kind of playback — it is the same Run over a search anchored at this
   * Call and ordered the other way. The list on screen is untouched: browsing
   * is the screen's and the walk is the Run's, which is exactly the separation
   * #89 named the two offsets apart for.
   */
  const [fetchRun] = useLazySearchCallsQuery()
  async function playForward(call: Call) {
    const search: RunSearch = { ...filters, sort: 'oldest', after: call.timestamp }
    try {
      const onward = await fetchRun({ ...search, limit: PAGE_SIZE, offset: 0 }).unwrap()
      dispatch(startRun({ search, page: onward, index: 0 }))
    } catch {
      link.say('Could not play forward from there.')
    }
  }

  // The page the Run is about to need, fetched and handed over (#32, US 25).
  // Every screen that starts a Run owes this; #47's per-Unit view is the other.
  useRunPageAhead()

  const linkMissing = useDeepLinkedCall(linkedCall, filters)

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
        <DateField
          label="From"
          id="filter-after"
          ms={filters.after}
          onChange={(after) => updateFilters({ after }, 'after')}
        />
        <DateField
          label="To"
          id="filter-before"
          ms={filters.before}
          onChange={(before) => updateFilters({ before }, 'before')}
        />

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
              updateFilters(
                { unit: event.target.value ? Number(event.target.value) : undefined },
                'unit',
              )
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

        {/* The eight taps a date range costs, as one (#61, spec US 31). Each
            resolves to *instants* and fills the two inputs above, so what the
            URL carries is the range being searched rather than a word that
            would mean something different tomorrow (`lib/dateRange`).

            `type="button"`, all of them: `Button` renders a bare `<button>`,
            which inside a form submits it — the trap #49 got caught by. */}
        <div className="col-span-2 flex flex-wrap items-center gap-1.5">
          {PRESETS.map((preset) => (
            <Button
              key={preset.id}
              type="button"
              variant="outline"
              size="sm"
              className="h-7 px-2 font-mono text-[10px] uppercase tracking-wider"
              onClick={() => updateFilters(rangeOf(preset.id, Date.now()))}
            >
              {preset.label}
            </Button>
          ))}
          <Button
            type="button"
            variant="outline"
            size="sm"
            aria-label="Reset filters"
            className="h-7 gap-1 px-2 font-mono text-[10px] uppercase tracking-wider"
            onClick={() => setParams('')}
          >
            <RotateCcw className="size-3" aria-hidden />
            Reset
          </Button>
          {/* Into the **DVR** (#63) with what is already filtered. This is the
              gesture the feature is for — "rewind *this* channel" — and it is
              why the DVR is reached from here rather than from a fifth tab. A
              scope is always a Selection, so a Talkgroup filter becomes a
              one-entry matrix and anything else opens on the Listener's own
              scanner. */}
          <Button asChild variant="outline" size="sm" className="h-7 gap-1 px-2">
            <Link
              to={`/dvr?${dvrLink(filters)}`}
              aria-label="Open these results in the DVR"
            >
              <History className="size-3" aria-hidden />
              <span className="font-mono text-[10px] uppercase tracking-wider">
                DVR
              </span>
            </Link>
          </Button>
          <Button
            type="button"
            variant="outline"
            size="sm"
            aria-label="Copy link to this search"
            className="ml-auto h-7 px-2"
            onClick={() => send({ search: filters, offset: windowOffset }, 'Radio-Scout search')}
          >
            <Link2 className="size-3.5" aria-hidden />
          </Button>
        </div>

        {options?.dateStartMs !== undefined && (
          <p className="col-span-2 font-mono text-[11px] text-muted-foreground/70">
            Archive spans {formatCallTime(options.dateStartMs)} –{' '}
            {formatCallTime(options.dateStopMs)}
          </p>
        )}
      </form>

      {/* What a link control just did, and why the Call a link named is not
          here. Both are things that happened rather than states of the
          archive, so they share one live region under the filters. */}
      {(link.notice || linkMissing) && (
        <p
          role="status"
          className="mt-3 rounded-md border border-border bg-card px-3 py-2 font-mono text-[11px] text-muted-foreground"
        >
          {link.notice ?? 'That call is no longer in the archive.'}
        </p>
      )}

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

      {/* How busy the archive was under these filters, and when it usually is
          (#62, spec US 34–35). Drawn only when there is traffic to draw: an
          empty ribbon over "no calls match these filters" says nothing the
          sentence below it does not. */}
      {activity && totalOf(activity) > 0 && (
        <section aria-label="Activity" className="mt-5 space-y-2">
          <div className="flex items-baseline justify-between font-mono text-[11px] uppercase tracking-wider text-muted-foreground">
            <span>Activity</span>
            <div className="flex gap-1">
              {CHARTS.map((view) => (
                <Button
                  key={view.id}
                  type="button"
                  variant={chart === view.id ? 'secondary' : 'outline'}
                  size="sm"
                  aria-pressed={chart === view.id}
                  className="h-7 px-2 font-mono text-[10px] uppercase tracking-wider"
                  onClick={() => setChart(view.id)}
                >
                  {view.label}
                </Button>
              ))}
            </div>
          </div>
          {chart === 'ribbon' ? (
            <DensityRibbon
              series={activity}
              at={bucketOfOffset(activity.values, windowOffset, filters.sort)}
              label="Jump to a time in these results"
              onJump={(bucket) =>
                goTo(
                  {
                    offset: offsetOfBucket(
                      activity.values,
                      bucket,
                      filters.sort,
                      PAGE_SIZE,
                    ),
                  },
                  // Always replaces, unlike the paging buttons. A scrub is not
                  // a new view — the search has not changed — so Back should
                  // return to where the Listener was before they started
                  // dragging, not walk them back through every bucket the drag
                  // passed over.
                  true,
                )
              }
            />
          ) : hourly ? (
            <ActivityHeatmap series={hourly} />
          ) : (
            <p className="font-mono text-[11px] text-muted-foreground">
              Reading the archive…
            </p>
          )}
        </section>
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
              onPlayForward={() => playForward(call)}
              onCopyLink={() =>
                send({ search: {}, offset: 0, call: call.id }, talkgroupName(call))
              }
            />
          ))}
        </ul>
      )}

      <div className="mt-4 flex justify-between gap-2">
        <Button
          variant="outline"
          size="sm"
          disabled={windowOffset === 0}
          onClick={() => goTo({ offset: Math.max(0, windowOffset - PAGE_SIZE) })}
        >
          Previous page
        </Button>
        <Button
          variant="outline"
          size="sm"
          disabled={!page.hasMore}
          onClick={() => goTo({ offset: windowOffset + PAGE_SIZE })}
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

/**
 * Play the Call a link named (#61, spec US 30), and say so when it is gone.
 *
 * Resolved **by id** rather than searched for: a link names one Call, and
 * requiring it to also match the filters on screen would fail for the
 * commonest reason there is — the recipient was looking at something else. The
 * Run it starts is that Call alone, because a shared moment is a moment; the
 * row's own control is how a Listener asks to carry on from it.
 *
 * Started once per Call, so a re-render, a filter change or the back button
 * does not replay something already playing.
 */
function useDeepLinkedCall(id: number | undefined, search: RunSearch): boolean {
  const dispatch = useAppDispatch()
  const [, setParams] = useSearchParams()
  const { data: call, isError } = useGetCallQuery(id as number, { skip: id === undefined })

  useEffect(() => {
    if (!call) return
    dispatch(
      startRun({
        search,
        page: { results: [call], count: 1, limit: 1, offset: 0, hasMore: false },
        index: 0,
      }),
    )
    // Acted on, so it leaves the address bar — the filters stay, because they
    // describe what is on screen, and this described how it was opened.
    // *Replacing* rather than pushing, so back does not land on the link and
    // play it again; the shell then remembers a Search tab that is a search,
    // not one that re-plays a Call every time the Listener returns to it.
    setParams(
      (current) => {
        const next = new URLSearchParams(current)
        next.delete('call')
        return next
      },
      { replace: true },
    )
    // `search` is deliberately not a dependency: it is what the Run is
    // *labelled* with, and re-running because a filter moved would replay it.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [dispatch, call, setParams])

  // A Call that is *gone* is not consumed: nothing was acted on, so the address
  // still says what was asked for and the sentence below does not vanish.
  return isError
}

/** How a Call reads in a list: LED, Talkgroup, System·Tag·Group, time,
 *  duration. */
function ResultRow({
  call,
  isCurrent,
  onPlay,
  onPlayForward,
  onCopyLink,
}: {
  call: Call
  isCurrent: boolean
  onPlay: () => void
  onPlayForward: () => void
  onCopyLink: () => void
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
      {/* Linkable whether or not there is anything to play: an encrypted Call
          is still a thing worth pointing somebody at (spec US 9). */}
      <Button
        variant="outline"
        size="icon"
        aria-label={`Copy link to ${description}`}
        onClick={onCopyLink}
      >
        <Link2 className="size-4" aria-hidden />
      </Button>
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
          {/* The same gesture the other way round (#61): the list is newest
              first, so Play walks back through history and this walks forward
              from here. Offered only where there is an instant to anchor on —
              a Call whose time was never recorded would anchor a forward run at
              the beginning of the archive. */}
          {call.timestamp !== undefined && (
            <Button
              variant="outline"
              size="icon"
              aria-label={`Play forward from ${description}`}
              onClick={onPlayForward}
            >
              <FastForward className="size-4" aria-hidden />
            </Button>
          )}
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
          {/* The same sentence the mini-player says about the same Run (#56):
              two spellings would be two screens quietly disagreeing about
              where a Listener is. */}
          {playingDetail(interrupting ? 'interrupting' : 'archive', position)}
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
