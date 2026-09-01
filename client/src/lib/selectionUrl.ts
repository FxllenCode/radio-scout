/**
 * A **Selection** as something you can send somebody (#61, spec US 30).
 *
 * # What travels, and why it is the matrix
 *
 * Not "the Talkgroups that are on" — the ADR-0004 matrix itself, defaults and
 * wildcards included. A list of what is on is a snapshot of one catalog, and the
 * recipient's has different Talkgroups in it: auto-populate (#8) mints one the
 * moment a recorder finds it, so a link sent this morning would arrive naming
 * channels that no longer exhaust the System. The matrix says what to *do* about
 * a Talkgroup nobody has decided on, so the link answers that question the same
 * way at both ends — which is `lib/selection`'s second rule, reaching one more
 * place than it was written for.
 *
 * # The spelling
 *
 * `<default>` then a `_`-separated System per exception, each `<systemRef>`
 * followed by `.`-separated entries — `*` for the System's wildcard, a Ref
 * otherwise, prefixed `-` when it is off:
 *
 * ```text
 * 1                    everything on, no exceptions
 * 0_100.*.-5_200.7     all off; System 100 on except Talkgroup 5; 200's 7 on
 * ```
 *
 * Only `[0-9*._-]`, which is exactly what `URLSearchParams` carries unescaped —
 * so the link a Listener copies is the link they read, rather than forty
 * percent `%` signs. Ordered, so one Selection is one link and two people
 * sharing the same scanner share the same URL.
 */
import { WILDCARD, type Selection } from './selection'

/** This Selection as a URL value. */
export function encodeSelection(selection: Selection): string {
  const systems = Object.entries(selection.sel)
    // Numeric, so the order is a Ref order rather than a string one — `1000`
    // before `99` would still be *a* deterministic link, and a baffling one.
    .sort(([a], [b]) => Number(a) - Number(b))
    .map(([systemRef, talkgroups]) => {
      const entries = Object.entries(talkgroups)
        // The wildcard first: it is what the Refs beside it are exceptions to,
        // so the link reads in the order the matrix resolves.
        .sort(([a], [b]) => rank(a) - rank(b))
        .map(([ref, on]) => `${on ? '' : '-'}${ref}`)
      return [systemRef, ...entries].join('.')
    })

  return [selection.all ? '1' : '0', ...systems].join('_')
}

/** Where an entry sorts: the wildcard ahead of every Ref. */
const rank = (key: string) => (key === WILDCARD ? -1 : Number(key))

/**
 * The Selection a link carries, or `undefined` if it does not carry one.
 *
 * All or nothing, unlike the per-field guards `lib/persist` reads storage with.
 * A stored value is this browser's own and a bad field costs it that field; a
 * link is one statement made by somebody else, and applying half of it would
 * replace a Selection the Listener built by hand with a guess at what was sent.
 */
export function decodeSelection(encoded: string): Selection | undefined {
  const [all, ...systems] = encoded.split('_')
  if (all !== '0' && all !== '1') return undefined

  const sel: Selection['sel'] = {}
  for (const system of systems) {
    const [systemRef, ...entries] = system.split('.')
    // A System named with nothing to say about it is not an exception to
    // anything — and is how a trailing `_` would read.
    if (!isRef(systemRef) || entries.length === 0) return undefined

    const talkgroups: Record<string, boolean> = {}
    for (const entry of entries) {
      const on = !entry.startsWith('-')
      const key = on ? entry : entry.slice(1)
      if (key !== WILDCARD && !isRef(key)) return undefined
      talkgroups[key] = on
    }
    sel[Number(systemRef)] = talkgroups
  }

  return { all: all === '1', sel }
}

/** A Ref is a run of digits and nothing else — `Number('')` is `0` and
 *  `Number(' 5 ')` is `5`, either of which would put a Talkgroup nobody named
 *  into somebody else's scanner. */
const isRef = (value: string) => /^\d+$/.test(value)
