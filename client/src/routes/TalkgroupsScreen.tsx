import { Search } from 'lucide-react'
import { useEffect, useState, type ReactNode } from 'react'

import { Screen } from '@/components/layout/Screen'
import { StatusLed } from '@/components/StatusLed'
import { Button } from '@/components/ui/button'
import { ledForCall } from '@/lib/led'
import {
  categoryViews,
  countOn,
  isSelected,
  stateOf,
  summarize,
  talkgroupsOf,
  type CatalogEntry,
  type CategoryView,
  type Selection,
  type TalkgroupKey,
  type TriState,
} from '@/lib/selection'
import { cn } from '@/lib/utils'
import { useGetCatalogQuery } from '@/store/api'
import { useAppDispatch, useAppSelector } from '@/store/hooks'
import {
  chooseEverything,
  chooseSystem,
  chooseTalkgroups,
  selectAudibleSelection,
  selectAvoidUntil,
} from '@/store/live'
import type { Catalog } from '@/types'

const EMPTY_CATALOG: Catalog = { systems: [] }

/** How often a running avoid's countdown is redrawn. Finer than the minute it
 *  displays, coarse enough that a phone with the panel open isn't rendering
 *  for nothing. */
const COUNTDOWN_TICK_MS = 30_000

/**
 * Talkgroups (Select) — the live-feed selection surface (#12, spec US 19–22).
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
 */
export function TalkgroupsScreen() {
  const dispatch = useAppDispatch()
  // What the listener will actually *hear*: the selection with their avoids
  // (spec US 14) laid over it, so a muted Talkgroup reads off in the rows, the
  // category chips and the summary alike.
  const selection = useAppSelector(selectAudibleSelection)
  const { data, isLoading, isError } = useGetCatalogQuery()
  const [filter, setFilter] = useState('')

  const catalog = data ?? EMPTY_CATALOG
  const { on, total } = summarize(catalog, selection)
  const matches = matching(talkgroupsOf(catalog), filter)
  const filtered = filter.trim().length > 0

  return (
    <Screen
      title="Talkgroups"
      status={
        total > 0 ? (
          <span
            data-testid="selection-summary"
            className="rounded-md border border-border bg-muted/40 px-2 py-1 font-mono text-[11px] tabular-nums"
          >
            {on} of {total} on
          </span>
        ) : null
      }
    >
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
          <Filter value={filter} onChange={setFilter} />

          <Categories
            catalog={catalog}
            selection={selection}
            onToggle={(category) =>
              dispatch(
                chooseTalkgroups({
                  keys: category.keys,
                  on: category.state !== 'all',
                }),
              )
            }
          />

          {catalog.systems.map((system) => {
            const talkgroups = matches.filter((tg) => tg.systemRef === system.ref)
            if (talkgroups.length === 0) return null
            return (
              <SystemSection
                key={system.ref}
                label={system.label ?? `System ${system.ref}`}
                talkgroups={talkgroups}
                selection={selection}
                // The button acts on what it is next to. Unfiltered, that is
                // the System itself — one wildcard, which also covers the
                // Talkgroups this browser has never heard of (spec US 21).
                // Filtered, it is the rows on screen and nothing else.
                onAll={(next) =>
                  dispatch(
                    filtered
                      ? chooseTalkgroups({ keys: talkgroups, on: next })
                      : chooseSystem({ systemRef: system.ref, on: next }),
                  )
                }
                onToggle={(key, next) =>
                  dispatch(chooseTalkgroups({ keys: [key], on: next }))
                }
              />
            )
          })}

          {matches.length === 0 && <Notice>No talkgroups match “{filter}”.</Notice>}

          <div className="mt-6 flex items-center gap-2 border-t border-border pt-4">
            <span className="flex-1 font-mono text-[11px] tabular-nums text-muted-foreground">
              {on} of {total} on
            </span>
            <Button
              variant="outline"
              size="sm"
              aria-label="Turn everything on"
              className="font-mono text-[11px] uppercase tracking-wider"
              onClick={() => dispatch(chooseEverything(true))}
            >
              All on
            </Button>
            <Button
              variant="outline"
              size="sm"
              aria-label="Turn everything off"
              className="font-mono text-[11px] uppercase tracking-wider"
              onClick={() => dispatch(chooseEverything(false))}
            >
              All off
            </Button>
          </div>
        </>
      )}
    </Screen>
  )
}

/** Talkgroups whose label, name, tag, group or TGID contains `filter`. */
function matching(talkgroups: CatalogEntry[], filter: string): CatalogEntry[] {
  const needle = filter.trim().toLowerCase()
  if (!needle) return talkgroups
  return talkgroups.filter((talkgroup) =>
    [
      talkgroup.label,
      talkgroup.name,
      talkgroup.tag,
      ...talkgroup.groups,
      String(talkgroup.ref),
    ].some((field) => field?.toLowerCase().includes(needle)),
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
    <div className="relative mb-4">
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

/** The Group and Tag category rows (spec US 20). */
function Categories({
  catalog,
  selection,
  onToggle,
}: {
  catalog: Catalog
  selection: Selection
  onToggle: (category: CategoryView) => void
}) {
  const rows = [
    { heading: 'Groups', categories: categoryViews(catalog, selection, 'group') },
    { heading: 'Tags', categories: categoryViews(catalog, selection, 'tag') },
  ]

  return (
    <div className="mb-5 flex flex-col gap-3">
      {rows.map(({ heading, categories }) =>
        categories.length === 0 ? null : (
          <section key={heading} aria-label={heading}>
            <h2 className="mb-1.5 font-mono text-[9px] font-semibold uppercase tracking-[0.2em] text-muted-foreground/70">
              {heading}
            </h2>
            <div className="grid grid-cols-2 gap-1.5">
              {categories.map((category) => (
                <CategoryChip
                  key={category.label}
                  category={category}
                  onClick={() => onToggle(category)}
                />
              ))}
            </div>
          </section>
        ),
      )}
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
  category: CategoryView
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

function SystemSection({
  label,
  talkgroups,
  selection,
  onAll,
  onToggle,
}: {
  label: string
  talkgroups: CatalogEntry[]
  selection: Selection
  onAll: (on: boolean) => void
  onToggle: (key: TalkgroupKey, on: boolean) => void
}) {
  const on = countOn(selection, talkgroups)
  const allOn = stateOf(selection, talkgroups) === 'all'

  return (
    <fieldset className="mb-5">
      <legend className="mb-1.5 flex w-full items-center gap-2">
        <span className="font-mono text-[9px] font-semibold uppercase tracking-[0.2em] text-muted-foreground/70">
          {label}
        </span>
        <span className="flex-1 font-mono text-[9px] tabular-nums text-muted-foreground/50">
          {on}/{talkgroups.length}
        </span>
        <button
          type="button"
          aria-label={`Turn ${label} all ${allOn ? 'off' : 'on'}`}
          onClick={() => onAll(!allOn)}
          className="font-mono text-[10px] font-semibold uppercase tracking-wider text-muted-foreground transition-colors hover:text-foreground"
        >
          {allOn ? 'All off' : 'All on'}
        </button>
      </legend>
      <ul className="divide-y divide-border rounded-xl border border-border bg-card">
        {talkgroups.map((talkgroup) => (
          <TalkgroupRow
            key={talkgroup.ref}
            talkgroup={talkgroup}
            selection={selection}
            onToggle={onToggle}
          />
        ))}
      </ul>
    </fieldset>
  )
}

function TalkgroupRow({
  talkgroup,
  selection,
  onToggle,
}: {
  talkgroup: CatalogEntry
  selection: Selection
  onToggle: (key: TalkgroupKey, on: boolean) => void
}) {
  const key: TalkgroupKey = {
    systemRef: talkgroup.systemRef,
    talkgroupRef: talkgroup.ref,
  }
  const avoidedUntil = useAppSelector((state) =>
    selectAvoidUntil(state, key.systemRef, key.talkgroupRef),
  )
  // `selection` already has the avoids laid over it, so a muted Talkgroup reads
  // off here exactly as it does in the counts above.
  const selected = isSelected(selection, key.systemRef, key.talkgroupRef)

  return (
    <li>
      <button
        type="button"
        role="switch"
        aria-checked={selected}
        onClick={() => onToggle(key, !selected)}
        className="flex w-full items-center gap-3 px-3 py-2.5 text-left transition-colors hover:bg-muted/40"
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
          <StatusLed
            color={ledForCall({
              systemRef: talkgroup.systemRef,
              talkgroupRef: talkgroup.ref,
              led: talkgroup.led,
            })}
            size={8}
            className="shrink-0"
          />
        ) : (
          <span
            aria-hidden
            className="size-2 shrink-0 rounded-full bg-muted-foreground/30"
          />
        )}
        <span
          className={cn(
            'min-w-0 flex-1 truncate font-mono text-sm',
            !selected && 'text-muted-foreground',
          )}
        >
          {talkgroup.label ?? talkgroup.name ?? `Talkgroup ${talkgroup.ref}`}
        </span>
        {avoidedUntil !== undefined && <AvoidBadge until={avoidedUntil} />}
        <span
          aria-hidden
          className="shrink-0 font-mono text-[10px] tabular-nums text-muted-foreground/60"
        >
          {talkgroup.ref}
        </span>
      </button>
    </li>
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
function AvoidBadge({ until }: { until: number }) {
  const timed = until > 0
  const now = useNow(timed)
  const minutes = Math.max(1, Math.ceil((until - now) / 60_000))

  return (
    <span className="flex shrink-0 items-center gap-1 rounded border border-led-orange/40 bg-led-orange/10 px-1.5 py-0.5 font-mono text-[9px] font-bold uppercase tracking-wider text-led-orange">
      <span
        aria-hidden
        data-testid={timed ? 'avoid-blink' : undefined}
        className={cn('size-1 rounded-full bg-current', timed && 'animate-pulse')}
      />
      {timed ? `${minutes}m` : 'avoid'}
    </span>
  )
}

/** The clock, as far as a countdown needs it. Only ticks while something is
 *  counting down — the store changes when an avoid *lapses*, never while one is
 *  merely running, so nothing else would redraw the number. */
function useNow(ticking: boolean): number {
  const [now, setNow] = useState(() => Date.now())

  useEffect(() => {
    if (!ticking) return
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
