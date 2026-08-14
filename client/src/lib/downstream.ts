/**
 * The **Downstream** screen's pure half (#52): a peer's scope as a form can edit
 * it, and its health as one line.
 *
 * The stored shape is a full [`Selection`] — the same matrix the live feed and
 * Web Push are scoped by, so one predicate decides all three and a patched Call
 * reaches a peer subscribed to the patched channel. That matrix can express more
 * than a form should ask for, though: `all` with per-System and per-Talkgroup
 * *exceptions* is exactly right for a Listener holding a Talkgroups panel, and
 * is a lot of screen for an Operator who wants "everything on System 11 except
 * the two sensitive channels".
 *
 * So the form edits **rows**, and this is the translation both ways. A row is a
 * System and either every Talkgroup in it or a named few; "everything" is the
 * whole thing in one checkbox.
 *
 * Two rules make the round trip safe, and both exist because the server is free
 * to hold a scope this form did not write — a hand-edited configuration
 * document, or a shape a later release adds:
 *
 * - Reading is **lossy by design and says so**: [`isEditable`] answers whether
 *   the rows below describe the scope *completely*, and the screen refuses to
 *   overwrite one they do not rather than silently narrowing it.
 * - Writing produces only shapes reading can round-trip, so a scope this form
 *   saved is always one it can re-open.
 */
import type { AdminDownstream } from '@/types'

import { formatCallTime } from './archive'
import { WILDCARD, type Selection } from './selection'

/** One line of the scope form: a System, and what of it goes. */
export interface ScopeRow {
  /** The System **Ref** — what an Operator reads on every other screen. */
  systemRef: number
  /** Talkgroup Refs to forward. Empty means **every Talkgroup in this System**,
   *  which is the wildcard the matrix spells `"*"`. */
  talkgroupRefs: number[]
}

/** A scope that forwards nothing — what a peer nobody has scoped holds, and the
 *  safe direction for anything unreadable. */
export const NOTHING: Selection = { all: false, sel: {} }

/** A scope that forwards every Call this Instance stores. */
export const EVERYTHING: Selection = { all: true, sel: {} }

/** Whether this scope forwards everything, with nothing carved out of it. */
export function forwardsEverything(scope: Selection): boolean {
  return scope.all && Object.keys(scope.sel).length === 0
}

/**
 * The rows a form shows for this scope.
 *
 * Only *enabled* entries become rows: an explicit `false` is an exception, which
 * the row shape has no way to draw — [`isEditable`] is what stops the form
 * pretending otherwise.
 *
 * Sorted by Ref so the list an Operator reads does not reshuffle between
 * renders; a `Selection`'s systems are object keys, whose order is the order
 * they were inserted in rather than anything meaningful.
 */
export function scopeRows(scope: Selection): ScopeRow[] {
  return Object.entries(scope.sel)
    .map(([systemRef, talkgroups]) => ({
      systemRef: Number(systemRef),
      talkgroupRefs: Object.entries(talkgroups)
        .filter(([talkgroupRef, on]) => on && talkgroupRef !== WILDCARD)
        .map(([talkgroupRef]) => Number(talkgroupRef))
        .sort((left, right) => left - right),
    }))
    .filter((row) => Number.isFinite(row.systemRef))
    .sort((left, right) => left.systemRef - right.systemRef)
}

/**
 * Whether [`scopeRows`] describes this scope **completely**.
 *
 * `false` means the stored scope says something the rows cannot: an exception
 * (`false` under a System), or `all` with Systems carved out of it. The screen
 * shows it read-only rather than offering a Save that would quietly forward less
 * than the Operator asked for — the same instinct that makes an unparseable
 * scope forward nothing on the server.
 */
export function isEditable(scope: Selection): boolean {
  if (forwardsEverything(scope)) return true
  // `all` with entries under it can only be narrowing something, and a row
  // cannot say "everything except".
  if (scope.all) return false
  return Object.values(scope.sel).every((talkgroups) => {
    const entries = Object.entries(talkgroups)
    // Every entry must be an inclusion, and a System that names its wildcard
    // must name *only* that — `{"*": true, "100": true}` is "all of it, and
    // also this one", which a row would redraw as just the wildcard.
    if (!entries.every(([, on]) => on)) return false
    const wildcard = entries.some(([ref]) => ref === WILDCARD)
    return !wildcard || entries.length === 1
  })
}

/**
 * The scope those rows mean.
 *
 * A row with no Talkgroups is the System's wildcard; a row with some is those
 * Refs. Rows naming no System at all are dropped, because a half-typed line in a
 * form must not become a scope entry keyed on `NaN`.
 */
export function scopeOf(rows: ScopeRow[]): Selection {
  const sel: Selection['sel'] = {}
  for (const row of rows) {
    if (!Number.isFinite(row.systemRef)) continue
    const talkgroups: Record<string, boolean> = {}
    const refs = row.talkgroupRefs.filter((ref) => Number.isFinite(ref))
    if (refs.length === 0) {
      talkgroups[WILDCARD] = true
    } else {
      for (const ref of refs) talkgroups[ref] = true
    }
    // Merged rather than replaced, so two rows an Operator typed for the same
    // System add up instead of the second silently winning.
    sel[row.systemRef] = { ...sel[row.systemRef], ...talkgroups }
  }
  return { all: false, sel }
}

/** Talkgroup Refs typed into a text input — `"100, 101"` — as numbers.
 *
 *  Anything that is not a number is dropped rather than refused: this runs on
 *  every keystroke, and refusing mid-word would fight the Operator's typing. The
 *  form shows what it understood, which is the honest feedback. */
export function parseRefs(raw: string): number[] {
  return raw
    .split(/[\s,]+/)
    .filter((part) => part !== '')
    .map(Number)
    .filter((ref) => Number.isFinite(ref))
}

/**
 * What an Operator needs to know about a peer at a glance, in one line.
 *
 * `queued` is the number that says whether anything is wrong *right now*, and it
 * is the **durable** depth — so a peer down for an hour reads as an hour of
 * Calls waiting rather than an hour of Calls lost, which is the difference this
 * whole feature exists to make.
 *
 * A peer with no key is called out by name: "refusing everything" and "never
 * given a credential" produce identical symptoms, and since the screen can never
 * show the key back, this is the only place the two can be told apart. It is
 * also what an imported backup looks like — a document carries a peer's shape
 * and never its credential.
 */
export function healthLine(row: AdminDownstream): string {
  const parts = [row.url]
  if (row.disabled) parts.push('disabled')
  if (!row.hasKey) parts.push('needs its key')
  if (row.queued > 0) parts.push(`${row.queued} queued`)
  if (row.consecutiveFailures > 0) {
    parts.push(
      `${row.consecutiveFailures} failed${
        row.lastFailure == null ? '' : ` · ${row.lastFailure}`
      }`,
    )
  }
  parts.push(
    row.lastSuccessMs == null
      ? 'never delivered'
      : `last delivered ${formatCallTime(row.lastSuccessMs)}`,
  )
  return parts.join(' · ')
}
