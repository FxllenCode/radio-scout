/** Helpers the archive screen shares with the RTK Query layer: turning filters
 *  into a request, and turning a Call into something a listener can read.
 *  Mirrors the backend's `src/archive.rs`. */
import type { LogQuery, SearchQuery } from '@/types'

/** Serialize filters into a query string, dropping anything unset. Keys are
 *  sorted so the same filters always produce the same URL — stable to assert
 *  on, friendly to HTTP caches, and (since RTK Query keys its cache by the URL)
 *  what keeps two orderings of the same filters one cache entry.
 *
 *  Shared by the archive search (#13) and the operator log (#30): both back
 *  ends read a blank value as "no filter", so both fronts may drop it. */
export function searchParams(query: LogQuery | SearchQuery): string {
  const params = new URLSearchParams()
  for (const [key, value] of Object.entries(query)) {
    if (value === undefined || value === null || value === '') continue
    params.set(key, String(value))
  }
  params.sort()
  return params.toString()
}

/** Where a Call's audio downloads from, named after the Call rather than its
 *  object key (spec US 27). */
export function downloadUrl(callId: number): string {
  return `/api/call/${callId}/download`
}

/** Read an `<input type="datetime-local">` value as unix milliseconds. The
 *  input is in the listener's own timezone — which the server has no way to
 *  know — so the conversion happens here and the wire carries only ms. */
export function dateTimeLocalToMs(value: string): number | undefined {
  if (!value.trim()) return undefined
  const ms = new Date(value).getTime()
  return Number.isNaN(ms) ? undefined : ms
}

/** A Call's time in the listener's timezone, in a fixed `YYYY-MM-DD HH:MM:SS`
 *  shape — a scanner log reads better aligned than localized. */
export function formatCallTime(ms: number | undefined): string {
  if (ms === undefined) return '—'
  const at = new Date(ms)
  if (Number.isNaN(at.getTime())) return '—'
  const pad = (value: number) => String(value).padStart(2, '0')
  return (
    `${at.getFullYear()}-${pad(at.getMonth() + 1)}-${pad(at.getDate())}` +
    ` ${pad(at.getHours())}:${pad(at.getMinutes())}:${pad(at.getSeconds())}`
  )
}

/** How long a transmission was (#42, spec US 8), in the shape a scanner
 *  operator reads at a glance: bare seconds under a minute, `m:ss` past it.
 *
 *  A Call that carries no `durationMs` — every Call ingested before #42, and
 *  anything whose container header could not be read — shows a dash. Absent is
 *  not zero: rendering an unknown length as `0.0s` would make an unmeasured
 *  Call look like the kerchunk the duration column exists to spot. */
export function formatDuration(ms: number | undefined): string {
  if (ms === undefined || Number.isNaN(ms) || ms < 0) return '—'
  const seconds = ms / 1000
  if (seconds < 60) return `${seconds.toFixed(1)}s`
  const minutes = Math.floor(seconds / 60)
  const rest = Math.floor(seconds % 60)
  return `${minutes}:${String(rest).padStart(2, '0')}`
}

/** The `1–100 of 421` readout under a result list. `count` is the true total,
 *  so it stays honest across pages. `noun` names what is being counted, since
 *  the operator log (#30) pages the same way over something that isn't Calls. */
export function pageSummary(
  offset: number,
  shown: number,
  count: number,
  noun = 'calls',
): string {
  if (count === 0 || shown === 0) return `No ${noun}`
  return `${offset + 1}–${offset + shown} of ${count}`
}
