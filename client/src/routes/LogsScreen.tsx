import { useState, type ReactNode } from 'react'

import { Screen } from '@/components/layout/Screen'
import { Button } from '@/components/ui/button'
import { signInMessage } from '@/lib/adminError'
import { dateTimeLocalToMs, formatCallTime, pageSummary } from '@/lib/archive'
import { cn } from '@/lib/utils'
import {
  useAdminLoginMutation,
  useAdminLogoutMutation,
  useGetAdminSessionQuery,
  useGetLogsQuery,
} from '@/store/api'
import type { LogEvent, LogQuery } from '@/types'

/** Events per page. Bigger than the archive's, because a log line is a row of
 *  text rather than a Call with audio behind it. */
const PAGE_SIZE = 100

const controlClass =
  'w-full rounded-md border border-border bg-background px-2 py-1.5 font-mono text-xs text-foreground'

/**
 * Settings → Logs (#30, ADR-0011) — what the server has been saying, for an
 * operator who has no shell to read `journalctl` from.
 *
 * rdio-scanner has this page and operators expect it; this one differs in three
 * ways that matter at 2am. `level` is a **floor** ("warnings and worse"), not
 * one level at a time. An event shows its **structured fields** and its
 * **correlation ref**, because ours stores an event's parts rather than a
 * rendered sentence — so the `internal error (ref: …)` a listener read out over
 * the phone is findable here. And what is stored obeys ADR-0011 rule 5: no
 * listener's address is in this table at any level, which is enforced by the
 * sink refusing to run below INFO rather than by anything on this page.
 */
export function LogsScreen() {
  const session = useGetAdminSessionQuery()
  const signedIn = session.isSuccess

  const [filters, setFilters] = useState<LogQuery>({})
  const [offset, setOffset] = useState(0)
  /** The instant paging was started from — see [`pageForward`]. */
  const [pinned, setPinned] = useState<number | undefined>(undefined)

  const page = useGetLogsQuery(
    { ...filters, before: filters.before ?? pinned, limit: PAGE_SIZE, offset },
    { skip: !signedIn },
  )

  const [logout] = useAdminLogoutMutation()

  /** Any filter change invalidates the page window we were on — and the pin,
   *  because a new question deserves a window of its own. */
  function updateFilters(patch: Partial<LogQuery>) {
    setFilters((current) => ({ ...current, ...patch }))
    setOffset(0)
    setPinned(undefined)
  }

  /** Move to the next page, **pinning the window on the way out**.
   *
   *  Offset paging assumes the rows below the offset do not move, and here
   *  they do: reading this page is itself an admin request, which the server
   *  logs — so between rendering page 1 and pressing this, at least one new
   *  event has landed at the head, and a raw `offset` would show page 1's last
   *  rows again. Pinning `before` to the newest event we have already seen
   *  freezes the window; the pin lasts until a filter changes or paging
   *  returns to the top. (The archive search does not need this: Calls arrive
   *  from a recorder, not from the act of looking at them.)
   */
  function pageForward(newest: number | undefined) {
    if (pinned === undefined && newest !== undefined) {
      // Exclusive upper bound, so the event it was taken from is still in.
      setPinned(newest + 1)
    }
    setOffset(offset + PAGE_SIZE)
  }

  /** ...and back. Returning to the top releases the pin, so the first page is
   *  live again — which is the page an operator watching a problem wants. */
  function pageBack() {
    const previous = Math.max(0, offset - PAGE_SIZE)
    setOffset(previous)
    if (previous === 0) setPinned(undefined)
  }

  if (!signedIn) {
    return (
      <Screen title="Logs">
        {session.isLoading ? (
          <Placeholder>Checking your session…</Placeholder>
        ) : (
          <SignIn />
        )}
      </Screen>
    )
  }

  const results = page.data?.results ?? []

  return (
    <Screen
      title="Logs"
      status={
        <Button
          variant="outline"
          size="sm"
          onClick={() => logout(session.data?.csrf_token ?? '')}
        >
          Sign out
        </Button>
      }
    >
      <form
        role="search"
        aria-label="Log filters"
        className="grid grid-cols-2 gap-2"
        onSubmit={(event) => event.preventDefault()}
      >
        <Field label="Level" htmlFor="filter-level">
          <select
            id="filter-level"
            className={controlClass}
            value={filters.level ?? ''}
            onChange={(event) =>
              updateFilters({ level: event.target.value || undefined })
            }
          >
            {/* A floor, so each option includes everything above it. */}
            <option value="">Everything</option>
            <option value="info">Info and above</option>
            <option value="warn">Warnings and errors</option>
            <option value="error">Errors only</option>
          </select>
        </Field>

        <Field label="Page" htmlFor="log-page-summary">
          <p
            id="log-page-summary"
            aria-live="polite"
            className="font-mono text-xs text-muted-foreground"
          >
            {pageSummary(offset, results.length, page.data?.count ?? 0, 'events')}
          </p>
        </Field>

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
      </form>

      {page.isError ? (
        <Placeholder role="alert">
          The log could not be read. Check that the server is reachable.
        </Placeholder>
      ) : results.length === 0 ? (
        <Placeholder>
          {page.isFetching ? 'Reading the log…' : 'No log events match.'}
        </Placeholder>
      ) : (
        <ul
          aria-label="Log events"
          className="mt-3 divide-y divide-border rounded-xl border border-border bg-card"
        >
          {results.map((event) => (
            <EventRow key={event.id} event={event} />
          ))}
        </ul>
      )}

      <div className="mt-4 flex justify-between gap-2">
        <Button
          variant="outline"
          size="sm"
          disabled={offset === 0}
          onClick={pageBack}
        >
          Previous page
        </Button>
        <Button
          variant="outline"
          size="sm"
          disabled={!page.data?.hasMore}
          onClick={() => pageForward(results[0]?.atMs)}
        >
          Next page
        </Button>
      </div>
    </Screen>
  )
}

/** The colour a level reads at a glance. Restrained on purpose — the LEDs are
 *  where colour lives in this app (docs/design/brief.md), so a level is a word
 *  with a tint, not a badge. */
const levelClass: Record<string, string> = {
  ERROR: 'text-red-400',
  WARN: 'text-amber-400',
  INFO: 'text-muted-foreground',
}

/** One event: when, how loud, what happened, and the detail behind it. */
function EventRow({ event }: { event: LogEvent }) {
  const fields = Object.entries(event.fields ?? {})

  return (
    <li className="flex flex-col gap-1 px-3 py-2.5">
      <div className="flex items-baseline gap-3">
        <span
          className={cn(
            'shrink-0 font-mono text-[11px] font-medium uppercase tracking-wider',
            levelClass[event.level] ?? 'text-muted-foreground',
          )}
        >
          {event.level}
        </span>
        <time className="shrink-0 font-mono text-[11px] text-muted-foreground">
          {formatCallTime(event.atMs)}
        </time>
        <span
          data-testid="log-message"
          className="min-w-0 flex-1 truncate font-mono text-sm"
        >
          {event.message}
        </span>
      </div>
      {(fields.length > 0 || event.requestId) && (
        <p className="truncate pl-1 font-mono text-[11px] text-muted-foreground">
          {fields.map(([key, value]) => `${key}=${format(value)}`).join(' ')}
          {event.requestId && (
            <span className="text-muted-foreground/70">
              {fields.length > 0 ? ' · ' : ''}
              ref {event.requestId}
            </span>
          )}
        </p>
      )}
      <p className="truncate pl-1 font-mono text-[10px] text-muted-foreground/60">
        {event.target}
      </p>
    </li>
  )
}

/** A field value as it reads in a log line — a string bare, anything else as
 *  its JSON, so a nested object is still legible. */
function format(value: unknown): string {
  return typeof value === 'string' ? value : JSON.stringify(value)
}

/** The password gate. Deliberately part of this screen rather than a route of
 *  its own: the Logs view is the only thing behind it so far, and an admin area
 *  with its own navigation is a later ticket's to design. */
function SignIn() {
  const [password, setPassword] = useState('')
  const [login, attempt] = useAdminLoginMutation()

  return (
    <form
      className="mt-3 flex flex-col gap-3 rounded-xl border border-border bg-card px-4 py-5"
      onSubmit={(event) => {
        event.preventDefault()
        login(password)
      }}
    >
      <p className="font-mono text-xs text-muted-foreground">
        The log is admin-only: an instance's log names its recorders and its
        failures.
      </p>
      <div className="flex flex-col gap-1">
        <label
          htmlFor="admin-password"
          className="font-mono text-[10px] uppercase tracking-wider text-muted-foreground"
        >
          Admin password
        </label>
        <input
          id="admin-password"
          type="password"
          autoComplete="current-password"
          className={controlClass}
          value={password}
          onChange={(event) => setPassword(event.target.value)}
        />
      </div>
      {attempt.isError && (
        <p role="alert" className="font-mono text-xs text-red-400">
          {signInMessage(attempt.error)}
        </p>
      )}
      <Button type="submit" size="sm" disabled={attempt.isLoading}>
        {attempt.isLoading ? 'Signing in…' : 'Sign in'}
      </Button>
    </form>
  )
}

/** The empty / loading / failed states the list stands in for. */
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
