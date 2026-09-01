import { ChevronDown, ChevronRight, Link2, Pin, Search, Zap } from 'lucide-react'
import { useEffect, useMemo, useRef, useState, type ReactNode } from 'react'
import { useSearchParams } from 'react-router-dom'

import { Screen } from '@/components/layout/Screen'
import { StatusLed } from '@/components/StatusLed'
import { Button } from '@/components/ui/button'
import { useWindowedRows } from '@/hooks/useWindowedRows'
import { avoidMinutesLeft } from '@/lib/avoiding'
import {
  lastHeard,
  panelOf,
  windowLabel,
  type Choice,
  type PanelCategory,
  type PanelRow,
  type PanelSort,
  type PanelSystem,
} from '@/lib/panel'
import type { TriState } from '@/lib/selection'
import { decodeSelection, encodeSelection } from '@/lib/selectionUrl'
import { linkTo, shareLink, shareNotice } from '@/lib/share'
import { cn } from '@/lib/utils'
import { useGetCatalogQuery } from '@/store/api'
import { useAppDispatch, useAppSelector } from '@/store/hooks'
import {
  applyLinkedSelection,
  chooseEverything,
  chooseSystem,
  chooseTalkgroups,
  selectAudibleSelection,
  selectAvoids,
  selectPriority,
  selectSelection,
  togglePriority,
} from '@/store/live'
import {
  selectExpandedSystems,
  selectPanelSort,
  selectPinned,
  showSystem,
  sortPanel,
  togglePin,
} from '@/store/panel'
import type { Catalog } from '@/types'

const EMPTY_CATALOG: Catalog = { systems: [], activityWindowMs: 0 }

/** How often the clock this screen keeps is redrawn. Finer than the minute a
 *  countdown or a last-heard age displays, coarse enough that a phone with the
 *  panel open isn't rendering for nothing. */
const COUNTDOWN_TICK_MS = 30_000

/** How tall a Talkgroup row stands. A number rather than a class, because it is
 *  also what [`useWindowedRows`] measures the list in — one source of truth, so
 *  the height the browser lays out and the height the window arithmetic assumes
 *  cannot drift apart. */
const ROW_HEIGHT = 44

/**
 * Talkgroups (Select) — the live-feed selection surface (#12, spec US 19–22),
 * at county scale (#57, spec US 29).
 *
 * What is chosen here *is* the subscription matrix the server is sent
 * (ADR-0004), so turning a Talkgroup off stops it reaching the device at all
 * rather than arriving and being thrown away — bandwidth and battery, which is
 * the point of server-side filtering.
 *
 * Laid out from `docs/design/mockups` screen 2c: category chips over per-System
 * lists, with a running summary and a global all-on/all-off. Three things
 * rdio's select panel doesn't have, and a 300-Talkgroup system needs: **Tag**
 * categories as well as Group ones (rdio's Tag category type is never
 * constructed), a **filter** so a Talkgroup can be found without scrolling a
 * wall of buttons, and **counts** so a category says how much of it is on
 * before you tap it.
 *
 * The panel is derived once, by [`panelOf`], and this file renders it (#91).
 * Nothing below decides what a tap means, what a row is called, how much of a
 * System is on, what order the rows are in or which of them are folded away —
 * it is handed all of that. Every input to that memo holds its identity between
 * dispatches, which is what lets 400+ rows scroll while audio plays (#57):
 * playback progress redraws nothing here.
 *
 * # What makes it a panel rather than a scroll (#57)
 *
 * - **The controls are on screen.** The summary, the filter, the sort and the
 *   global all-on/all-off sit in a sticky bar rather than at the foot of four
 *   hundred rows. A System's own controls are reachable because a System that
 *   size starts folded away, which is the pair the ticket asks for.
 * - **The rows are windowed** ([`useWindowedRows`]), so what is in the DOM is
 *   what is on screen and a little either side — not four hundred buttons
 *   React has to reconcile every time the Selection changes.
 * - **Pins float to the top**, across Systems, because a daily channel in the
 *   third System is exactly what a 400-row scroll buries.
 *
 * The section headers are deliberately *not* sticky. Their offset would have to
 * be the sticky bar's height, and a bar whose controls wrap on a narrow phone
 * has no height a constant can name — so the fold is what keeps them reachable,
 * and a Listener deep inside one open System scrolls up rather than reading a
 * header pinned at the wrong place.
 */
/**
 * Listen to what a link says to listen to (#61, spec US 30).
 *
 * Applied once, then **taken out of the address bar**, which is the opposite of
 * what a Call deep link does and deliberately so: a `?call=` is a request that
 * this Call be played and stays true however often it is opened, where a `?sel=`
 * is folded into state the browser goes on remembering — so a URL still
 * claiming it would re-offer the undo on every back-and-forth and would be a
 * lie the moment the Listener touched a row.
 *
 * A link that does not carry a Selection is ignored whole
 * (`lib/selectionUrl`), because half of somebody else's scanner is not
 * something they meant to send.
 */
function useLinkedSelection() {
  const dispatch = useAppDispatch()
  const [params, setParams] = useSearchParams()
  const encoded = params.get('sel')
  const applied = useRef<string>(undefined)

  useEffect(() => {
    if (encoded === null || applied.current === encoded) return
    applied.current = encoded
    const selection = decodeSelection(encoded)
    if (selection) dispatch(applyLinkedSelection({ selection, at: Date.now() }))
    setParams(
      (current) => {
        const next = new URLSearchParams(current)
        next.delete('sel')
        return next
      },
      { replace: true },
    )
  }, [dispatch, encoded, setParams])
}

export function TalkgroupsScreen() {
  const dispatch = useAppDispatch()
  // What the listener will actually *hear*: the selection with their avoids
  // (spec US 14) laid over it, so a muted Talkgroup reads off in the rows, the
  // category chips and the summary alike.
  const selection = useAppSelector(selectAudibleSelection)
  const avoided = useAppSelector(selectAvoids)
  const pinned = useAppSelector(selectPinned)
  // From the `live` slice, not the panel's: **Priority** decides what plays
  // next, and `store/panel`'s rule is that nothing in it can (#58).
  const priority = useAppSelector(selectPriority)
  const expanded = useAppSelector(selectExpandedSystems)
  const sort = useAppSelector(selectPanelSort)
  // Refetched when the Listener comes back to this screen and the answer is
  // more than a minute old (#57): the last-heard ages are measured from the
  // moment the catalog was read, so a panel opened after lunch would otherwise
  // show a busy channel as having been quiet for three hours. Deliberately not
  // a poll — one query a minute per parked phone is a cost the Pi pays for a
  // screen nobody is looking at.
  const { data, isLoading, isError } = useGetCatalogQuery(undefined, {
    refetchOnMountOrArgChange: 60,
  })
  const [filter, setFilter] = useState('')
  // The Selection itself, not the audible one: a link carries what the Listener
  // *chose*, where `selectAudibleSelection` has their **Avoids** and any
  // **Hold** laid over it — reading of the moment, not of the scanner.
  const chosen = useAppSelector(selectSelection)
  const [notice, setNotice] = useState<string | null>(null)
  useLinkedSelection()

  const catalog = data ?? EMPTY_CATALOG
  const panel = useMemo(
    () =>
      panelOf({ catalog, selection, avoided, priority, filter, pinned, expanded, sort }),
    [catalog, selection, avoided, priority, filter, pinned, expanded, sort],
  )
  const { on, total } = panel
  // Only while something drawn is a subtraction from the clock — see [`useNow`].
  const now = useNow(panel.ticking)

  /** Do what this control is for. Two action creators, chosen by the shape the
   *  panel handed us — never by this file working out which one applies. */
  const choose = (choice: Choice) =>
    dispatch('systemRef' in choice ? chooseSystem(choice) : chooseTalkgroups(choice))

  /** What every row needs and no row decides — passed down whole, because the
   *  four of them travel together to every list on the screen. */
  const controls: RowControls = {
    now,
    sort,
    onChoose: choose,
    onPin: (key: string) => dispatch(togglePin(key)),
    onPriority: (key: string) => dispatch(togglePriority(key)),
  }

  return (
    <Screen title="Talkgroups">
      {isError ? (
        <Notice role="alert">
          Could not load talkgroups — the server may be unreachable.
        </Notice>
      ) : isLoading ? (
        <Notice>Loading talkgroups…</Notice>
      ) : total === 0 ? (
        <Empty />
      ) : (
        <>
          {/* Everything that acts on the whole panel, kept on screen: at 400
              rows a global "All off" at the foot of the list is a control that
              exists and cannot be reached. */}
          <div className="sticky top-0 z-20 -mx-4 mb-4 border-b border-border bg-background/95 px-4 pb-2.5 pt-2 backdrop-blur">
            <Filter value={filter} onChange={setFilter} />
            <div className="mt-2 flex items-center gap-1.5">
              {/* The one summary there is. It used to sit in the screen's title
                  row, which scrolls away — and a count of what is on is exactly
                  what a Listener wants while they are four hundred rows down. */}
              <span
                data-testid="selection-summary"
                className="mr-auto font-mono text-[11px] tabular-nums text-muted-foreground"
              >
                {on} of {total} on
              </span>
              <SortToggle
                sort={sort}
                windowMs={catalog.activityWindowMs}
                onSort={(next) => dispatch(sortPanel(next))}
              />
              <Button
                variant="outline"
                size="sm"
                aria-label="Turn everything on"
                className="h-7 px-2 font-mono text-[10px] uppercase tracking-wider"
                onClick={() => dispatch(chooseEverything(true))}
              >
                All on
              </Button>
              <Button
                variant="outline"
                size="sm"
                aria-label="Turn everything off"
                className="h-7 px-2 font-mono text-[10px] uppercase tracking-wider"
                onClick={() => dispatch(chooseEverything(false))}
              >
                All off
              </Button>
              {/* What is selected, as something you can send somebody (#61,
                  spec US 30) — in the bar with the other whole-panel controls,
                  because that is what it acts on. */}
              <Button
                variant="outline"
                size="sm"
                aria-label="Copy link to this selection"
                className="h-7 px-2"
                onClick={async () =>
                  setNotice(
                    shareNotice(
                      await shareLink(
                        linkTo(
                          '/talkgroups',
                          `sel=${encodeSelection(chosen)}`,
                          window.location.origin,
                        ),
                        'Radio-Scout selection',
                        navigator,
                      ),
                    ),
                  )
                }
              >
                <Link2 className="size-3.5" aria-hidden />
              </Button>
            </div>
            {notice && (
              <p
                role="status"
                className="mt-1.5 font-mono text-[11px] text-muted-foreground"
              >
                {notice}
              </p>
            )}
          </div>

          {panel.pinned.length > 0 && (
            <section aria-label="Pinned" className="mb-5">
              <h2 className="mb-1.5 font-mono text-[9px] font-semibold uppercase tracking-[0.2em] text-muted-foreground/70">
                Pinned
              </h2>
              <RowList rows={panel.pinned} named {...controls} />
            </section>
          )}

          <div className="mb-5 flex flex-col gap-3">
            {panel.categories.map(({ heading, categories }) => (
              <section key={heading} aria-label={heading}>
                <h2 className="mb-1.5 font-mono text-[9px] font-semibold uppercase tracking-[0.2em] text-muted-foreground/70">
                  {heading}
                </h2>
                <div className="grid grid-cols-2 gap-1.5">
                  {categories.map((category) => (
                    <CategoryChip
                      key={category.label}
                      category={category}
                      onClick={() => choose(category.choice)}
                    />
                  ))}
                </div>
              </section>
            ))}
          </div>

          {panel.systems.map((system) => (
            <SystemSection
              key={system.key}
              system={system}
              onShow={(shown) => dispatch(showSystem({ systemRef: system.key, shown }))}
              {...controls}
            />
          ))}

          {panel.empty && <Notice>No talkgroups match “{filter}”.</Notice>}
        </>
      )}
    </Screen>
  )
}

function Filter({
  value,
  onChange,
}: {
  value: string
  onChange: (value: string) => void
}) {
  return (
    <div className="relative">
      <Search
        className="pointer-events-none absolute left-3 top-1/2 size-3.5 -translate-y-1/2 text-muted-foreground"
        aria-hidden
      />
      <input
        type="search"
        aria-label="Filter talkgroups"
        placeholder="Filter by name, tag or TGID"
        value={value}
        onChange={(event) => onChange(event.target.value)}
        className="h-9 w-full rounded-lg border border-border bg-card pl-9 pr-3 font-mono text-xs outline-none placeholder:text-muted-foreground/60 focus-visible:border-foreground/40 focus-visible:ring-[3px] focus-visible:ring-ring/30"
      />
    </div>
  )
}

/** Catalog order, or busiest first (#57). Two buttons rather than a select,
 *  because it is a two-state choice a thumb should not have to open a menu for
 *  — and the window it sorts over is named from the catalog's own answer, so
 *  the label cannot come to describe a window that has moved. */
function SortToggle({
  sort,
  windowMs,
  onSort,
}: {
  sort: PanelSort
  windowMs: number
  onSort: (sort: PanelSort) => void
}) {
  const options: { value: PanelSort; label: string; name: string }[] = [
    { value: 'name', label: 'A–Z', name: 'Sort by name' },
    {
      value: 'active',
      label: 'Active',
      name: `Sort by most active in the last ${windowLabel(windowMs)}`,
    },
  ]

  return (
    <div className="flex overflow-hidden rounded-md border border-border">
      {options.map((option) => (
        <button
          key={option.value}
          type="button"
          aria-label={option.name}
          aria-pressed={sort === option.value}
          onClick={() => onSort(option.value)}
          className={cn(
            'h-7 px-2 font-mono text-[10px] uppercase tracking-wider transition-colors',
            sort === option.value
              ? 'bg-foreground text-background'
              : 'text-muted-foreground hover:text-foreground',
          )}
        >
          {option.label}
        </button>
      ))}
    </div>
  )
}

/** The glyph the mockup gives each of the three states. */
const STATE_GLYPH: Record<TriState, string> = { all: '●', some: '◐', none: '○' }

/** A three-state toggle is exactly what `aria-pressed="mixed"` is for, so the
 *  partial state is announced rather than left to the glyph. */
const PRESSED: Record<TriState, 'true' | 'mixed' | 'false'> = {
  all: 'true',
  some: 'mixed',
  none: 'false',
}

function CategoryChip({
  category,
  onClick,
}: {
  category: PanelCategory
  onClick: () => void
}) {
  const { state } = category
  return (
    <button
      type="button"
      aria-pressed={PRESSED[state]}
      onClick={onClick}
      className={cn(
        'flex items-center justify-between gap-2 rounded-lg border px-3 py-2 font-mono text-[11px] tracking-wide transition-colors',
        state === 'all' && 'border-foreground bg-foreground text-background',
        state === 'some' && 'border-muted-foreground bg-muted/50 text-foreground',
        state === 'none' && 'border-border text-muted-foreground',
      )}
    >
      <span className="flex min-w-0 items-center gap-2">
        <span aria-hidden>{STATE_GLYPH[state]}</span>
        <span className="truncate">{category.label}</span>
      </span>
      <span className="shrink-0 tabular-nums opacity-70">
        {category.on}/{category.total}
      </span>
    </button>
  )
}

/** What every row needs and no row decides. */
interface RowControls {
  now: number
  sort: PanelSort
  onChoose: (choice: Choice) => void
  onPin: (key: string) => void
  onPriority: (key: string) => void
}

function SystemSection({
  system,
  onShow,
  ...controls
}: { system: PanelSystem; onShow: (shown: boolean) => void } & RowControls) {
  const { label, on, total, allOn, collapsed, rows } = system

  return (
    <section role="group" aria-label={label} className="mb-5">
      <div className="mb-1.5 flex w-full items-center gap-2">
        <button
          type="button"
          aria-expanded={!collapsed}
          onClick={() => onShow(collapsed)}
          className="flex items-center gap-1 font-mono text-[9px] font-semibold uppercase tracking-[0.2em] text-muted-foreground/70 transition-colors hover:text-foreground"
        >
          {collapsed ? (
            <ChevronRight className="size-3" aria-hidden />
          ) : (
            <ChevronDown className="size-3" aria-hidden />
          )}
          {label}
        </button>
        <span className="flex-1 font-mono text-[9px] tabular-nums text-muted-foreground/50">
          {on}/{total}
        </span>
        <button
          type="button"
          aria-label={`Turn ${label} all ${allOn ? 'off' : 'on'}`}
          onClick={() => controls.onChoose(system.all)}
          className="font-mono text-[10px] font-semibold uppercase tracking-wider text-muted-foreground transition-colors hover:text-foreground"
        >
          {allOn ? 'All off' : 'All on'}
        </button>
      </div>
      {!collapsed && <RowList rows={rows} {...controls} />}
    </section>
  )
}

/**
 * A list of Talkgroup rows, windowed (#57).
 *
 * The padding stands in for the rows that are not drawn, so the list is exactly
 * as tall as it would be whole and the page's scroll height never moves under
 * the Listener's thumb. Below the threshold in `lib/window` nothing is skipped
 * and the padding is zero, which is every list on a small instance.
 */
function RowList({
  rows,
  named,
  ...controls
}: { rows: PanelRow[]; named?: boolean } & RowControls) {
  const { ref, view } = useWindowedRows(rows.length, ROW_HEIGHT)

  return (
    <ul
      ref={ref}
      style={{ paddingTop: view.padTop, paddingBottom: view.padBottom }}
      className="divide-y divide-border rounded-xl border border-border bg-card"
    >
      {rows.slice(view.start, view.end).map((row) => (
        <TalkgroupRow key={row.key} row={row} named={named} {...controls} />
      ))}
    </ul>
  )
}

function TalkgroupRow({
  row,
  named,
  now,
  sort,
  onChoose,
  onPin,
  onPriority,
}: { row: PanelRow; named?: boolean } & RowControls) {
  const { selected, avoidedUntil, pinned, priority } = row

  return (
    <li className="flex items-center" style={{ height: ROW_HEIGHT }}>
      <button
        type="button"
        role="switch"
        aria-checked={selected}
        onClick={() => onChoose(row.choice)}
        className="flex h-full min-w-0 flex-1 items-center gap-3 pl-3 pr-1 text-left transition-colors hover:bg-muted/40"
      >
        <span
          aria-hidden
          className={cn(
            'flex size-[18px] shrink-0 items-center justify-center rounded-[5px] border text-[11px] font-bold',
            selected
              ? 'border-foreground bg-foreground text-background'
              : 'border-muted-foreground/50 text-transparent',
          )}
        >
          ✓
        </span>
        {selected ? (
          <StatusLed color={row.led} size={8} className="shrink-0" />
        ) : (
          <span
            aria-hidden
            className="size-2 shrink-0 rounded-full bg-muted-foreground/30"
          />
        )}
        <span className="flex min-w-0 flex-1 flex-col">
          <span
            className={cn(
              'truncate font-mono text-sm leading-tight',
              !selected && 'text-muted-foreground',
            )}
          >
            {row.label}
          </span>
          {/* A pinned row has left its section, so it says where it came from
              — otherwise two Systems' "Dispatch" are one name twice. */}
          {named && (
            <span className="truncate font-mono text-[9px] uppercase tracking-wider text-muted-foreground/60">
              {row.systemLabel}
            </span>
          )}
        </span>
        {avoidedUntil !== undefined && <AvoidBadge until={avoidedUntil} now={now} />}
        <Activity row={row} sort={sort} now={now} />
        <span
          aria-hidden
          className="shrink-0 font-mono text-[10px] tabular-nums text-muted-foreground/60"
        >
          {row.talkgroupRef}
        </span>
      </button>
      {/* Siblings of the switch, never inside it: a button within a button is
          neither valid HTML nor reachable by a screen reader (#47).

          Priority first, because it is the one that changes what a Listener
          *hears* — a **Pin** only moves the row. They look alike deliberately:
          two per-Talkgroup preferences a Listener sets from the same place. */}
      <button
        type="button"
        aria-label={`${priority ? 'Clear priority on' : 'Give priority to'} ${row.label}`}
        aria-pressed={priority}
        onClick={() => onPriority(row.key)}
        className={cn(
          'flex h-full shrink-0 items-center pl-1.5 pr-1 transition-colors',
          priority
            ? 'text-led-amber'
            : 'text-muted-foreground/30 hover:text-muted-foreground',
        )}
      >
        <Zap className={cn('size-3.5', priority && 'fill-current')} aria-hidden />
      </button>
      <button
        type="button"
        aria-label={`${pinned ? 'Unpin' : 'Pin'} ${row.label}`}
        aria-pressed={pinned}
        onClick={() => onPin(row.key)}
        className={cn(
          'flex h-full shrink-0 items-center pl-1 pr-2.5 transition-colors',
          pinned
            ? 'text-foreground'
            : 'text-muted-foreground/30 hover:text-muted-foreground',
        )}
      >
        <Pin className={cn('size-3.5', pinned && 'fill-current')} aria-hidden />
      </button>
    </li>
  )
}

/**
 * How busy this Talkgroup has been (#57).
 *
 * One cell, and what it says follows the sort: sorted by name it is the
 * last-heard age, which is what a Listener scanning an alphabetical list wants;
 * sorted by most active it is the count the order is *by*, so the ordering is
 * legible instead of mysterious. Both at once would cost a phone's row width at
 * exactly the scale this ticket is about.
 *
 * The visible text is short and hidden from assistive technology, with the
 * whole sentence beside it: "23h" read aloud is not an answer.
 */
function Activity({ row, sort, now }: { row: PanelRow; sort: PanelSort; now: number }) {
  if (sort === 'active') {
    const calls = row.recentCalls
    return (
      <Measure said={`${calls} recent ${calls === 1 ? 'call' : 'calls'}`} shown={calls} />
    )
  }

  if (row.lastCallAtMs === undefined) return null
  const ago = lastHeard(now, row.lastCallAtMs)
  return <Measure said={`last heard ${ago} ago`} shown={ago} />
}

/** One number, twice: short enough for a 400-row list, and spelled out for
 *  whoever is listening to the page rather than looking at it.
 *
 *  The spelled-out half carries its own leading space. The row's accessible
 *  name is these text nodes run together, and without it a screen reader says
 *  "Channel 0033 recent calls". */
function Measure({ said, shown }: { said: string; shown: string | number }) {
  return (
    <>
      {' '}
      <span className="sr-only">{said}</span>
      <span
        aria-hidden
        className="shrink-0 font-mono text-[10px] tabular-nums text-muted-foreground/70"
      >
        {shown}
      </span>
    </>
  )
}

/**
 * A Talkgroup muted right now (spec US 14).
 *
 * A **timed** avoid blinks and counts down: it is coming back on its own, which
 * is a different thing from one the listener switched off — rdio's select panel
 * shows both the same way. An **indefinite** one sits steady, because there is
 * nothing to wait for.
 */
function AvoidBadge({ until, now }: { until: number; now: number }) {
  const timed = until > 0
  // The rounding rule is `lib/avoiding`'s, shared with the Avoid sheet so a
  // badge and a row cannot say a channel has a different number of minutes
  // left (#58).
  const minutes = avoidMinutesLeft(now, until)

  return (
    <span className="flex shrink-0 items-center gap-1 rounded border border-led-orange/40 bg-led-orange/10 px-1.5 py-0.5 font-mono text-[9px] font-bold uppercase tracking-wider text-led-orange">
      <span
        aria-hidden
        data-testid={timed ? 'avoid-blink' : undefined}
        className={cn('size-1 rounded-full bg-current', timed && 'animate-pulse')}
      />
      {/* Announced as a sentence and shown as a glyph. `30m` beside a label is
          read as part of it — "Alpha Fire30m" — and is not an answer besides. */}
      {' '}
      <span className="sr-only">
        {timed ? `avoided, ${minutes} minutes left` : 'avoided'}
      </span>
      <span aria-hidden>{timed ? `${minutes}m` : 'avoid'}</span>
    </span>
  )
}

/**
 * The clock, as far as this screen needs it.
 *
 * The one thing here that ticks (#91), and it ticks for the *display* alone:
 * whether an **Avoid** is still in force is decided from its deadline, by the
 * store's own clock (`store/avoids`) and by every Call that arrives.
 *
 * One timer for the screen rather than one per row — a county panel has four
 * hundred rows, and a last-heard age on each would otherwise be four hundred
 * intervals — and it runs only while [`Panel.ticking`] says something drawn is
 * measured from a clock. A panel of switches and counts needs none, and a
 * parked phone must not redraw itself twice a minute for nothing.
 *
 * Read once on arming as well as on each tick, because a clock started an hour
 * after mount would otherwise begin by drawing every age an hour stale.
 */
function useNow(ticking: boolean): number {
  const [now, setNow] = useState(() => Date.now())

  useEffect(() => {
    if (!ticking) return
    setNow(Date.now())
    const tick = setInterval(() => setNow(Date.now()), COUNTDOWN_TICK_MS)
    return () => clearInterval(tick)
  }, [ticking])

  return now
}

function Notice({ children, role }: { children: ReactNode; role?: 'alert' }) {
  return (
    <p
      role={role}
      className="rounded-xl border border-border bg-card px-6 py-8 text-center font-mono text-sm text-muted-foreground"
    >
      {children}
    </p>
  )
}

/** Zero-config first run: nothing has been heard yet, so there is nothing to
 *  choose between. */
function Empty() {
  return (
    <div className="rounded-xl border border-border bg-card px-6 py-12 text-center">
      <p className="font-mono text-sm text-muted-foreground">No systems yet.</p>
      <p className="mt-1 text-xs text-muted-foreground/70">
        Systems and talkgroups appear here automatically as calls arrive.
      </p>
    </div>
  )
}
