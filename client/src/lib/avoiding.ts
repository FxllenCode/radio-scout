/**
 * The **Avoid** sheet's rows, derived once (#58, spec US 25).
 *
 * Until now the only thing an Avoid was rendered as was a badge on a Talkgroups
 * panel row that was *already* named — so the Live screen could say how many
 * were in force and nothing could say *which*, which is exactly the gap spec
 * US 25 names ("a mis-tap doesn't silently mute a channel"). Naming one takes
 * the catalog, so the derivation goes here rather than in the component, and it
 * is a value a test constructs.
 *
 * # Why a Talkgroup with no catalog entry is still a row
 *
 * An Avoid outlives the catalog it was placed from: an Operator can renumber or
 * delete a Talkgroup, and a **Profile** restored on a phone that has not
 * fetched the catalog yet holds Avoids against nothing at all. A row that
 * vanished in either case would leave a channel silenced with no way to lift
 * it — the panel cannot show it either, because the panel draws the catalog.
 * So an unnamed Avoid is drawn under its Refs, which is enough to recognise and
 * enough to clear.
 */
import { talkgroupsOf, parseTalkgroupKey, talkgroupKey, type Avoids } from './selection'
import type { Catalog } from '@/types'

/** One silenced Talkgroup, as the sheet draws it. */
export interface AvoidedRow {
  /** The [`talkgroupKey`] that lifts it. */
  key: string
  label: string
  systemLabel: string
  talkgroupRef: number
  /** When it lapses — `0` for "until the Listener says otherwise". The deadline
   *  itself, because the countdown beside it is a subtraction from the clock
   *  and this module keeps none (`lib/panel`'s rule). */
  until: number
}

/**
 * Every Avoid in force, named where the catalog can name it.
 *
 * Ordered by System and then by label, so the list a Listener reads twice reads
 * the same way twice — the map itself is keyed by string and its iteration
 * order would put `11:1000` before `11:99`.
 */
export function avoidedRows(
  avoided: Avoids,
  catalog: Catalog | undefined,
): AvoidedRow[] {
  // Built once for the whole list rather than per row, which is the only reason
  // the private [`named`] exists beside the public [`nameOf`].
  const from = index(catalog)

  return Object.entries(avoided)
    .map(([key, until]) => ({ ...named(key, from), until }))
    .sort(
      (a, b) => a.systemLabel.localeCompare(b.systemLabel) || a.label.localeCompare(b.label),
    )
}

/** A silenced Talkgroup's identity, without the deadline — [`AvoidedRow`] minus
 *  the one field a caller naming a single key does not have. */
export type AvoidedName = Omit<AvoidedRow, 'until'>

/**
 * What to call one silenced Talkgroup — the same answer [`avoidedRows`] gives,
 * for a caller holding a single key rather than the map.
 *
 * The undo bar is that caller (#58): it names the Talkgroup an **Avoid** just
 * took, and it has no deadline to report — the offer's own clock is a different
 * number entirely. Asking `avoidedRows` for it would mean handing over a
 * fabricated deadline in order to get a label back, which is a lie in the shape
 * of a reuse.
 */
export function nameOf(key: string, catalog: Catalog | undefined): AvoidedName {
  return named(key, index(catalog))
}

function named(key: string, from: NamedTalkgroups): AvoidedName {
  const { systemRef, talkgroupRef } = parseTalkgroupKey(key)
  const entry = from.get(key)

  return {
    key,
    label: entry?.label ?? entry?.name ?? `Talkgroup ${talkgroupRef}`,
    systemLabel: entry?.systemLabel ?? `System ${systemRef}`,
    talkgroupRef,
  }
}

/** The catalog keyed for lookup. */
type NamedTalkgroups = Map<string, ReturnType<typeof talkgroupsOf>[number]>

const index = (catalog: Catalog | undefined): NamedTalkgroups =>
  new Map(
    (catalog ? talkgroupsOf(catalog) : []).map((entry) => [
      talkgroupKey(entry.systemRef, entry.talkgroupRef),
      entry,
    ]),
  )

/**
 * How many minutes a timed **Avoid** has left.
 *
 * Ceiled and floored at one, because a minute left must never read as none —
 * a row saying `0m` beside a channel that is still silent is the countdown
 * lying about the only thing it is for. Never asked of an indefinite Avoid,
 * which has no deadline to subtract from.
 *
 * One rounding rule, two vocabularies: the panel's badge says `30m` where the
 * sheet says "30 minutes left", and the number under both is this. Deliberately
 * not shared with `lib/panel`'s [`lastHeard`], which floors *elapsed* time and
 * names its own floor — the opposite rounding for the opposite direction.
 */
export function avoidMinutesLeft(now: number, until: number): number {
  return Math.max(1, Math.ceil((until - now) / 60_000))
}
