import { useState } from 'react'

import { FailureNote, Field, controlClass } from '@/components/admin/AdminUi'
import { Button } from '@/components/ui/button'
import { useGetRangesQuery, useSetRangesMutation } from '@/store/api'
import type { AdminUnit, Span } from '@/types'

/**
 * **A Unit's Ranges** (#50, spec US 16) — the blocks a fleet numbers its radios
 * in, editable at last from somewhere other than a CSV.
 *
 * **No preview here, and that is not an oversight.** A Call names the radios it
 * heard by *Ref*, never by a Unit's id — which is why deleting a Unit takes no
 * Call with it — so editing a span moves nothing in the archive. What changes is
 * what an apparatus is *called*, and removing a span puts the bare number back
 * exactly as it read before anybody curated one. There is nothing to confirm
 * that an undo would not answer better.
 *
 * What it does owe is atomicity: the server refuses an overlapping span rather
 * than applying the half of the edit that fits, because a Ref inside two Ranges
 * belongs to whichever row the query returns first — one radio's Calls
 * attributing to two different apparatus depending on the day.
 */
export function RangesEditor({ row }: { row: AdminUnit }) {
  const ranges = useGetRangesQuery(row.id)
  const [set, setting] = useSetRangesMutation()
  const [from, setFrom] = useState('')
  const [to, setTo] = useState('')

  const held = ranges.data?.results ?? []

  return (
    <section
      aria-label="Ranges"
      className="mt-2 flex flex-col gap-2 border-t border-border pt-2"
    >
      <p className="font-mono text-[10px] uppercase tracking-wider text-muted-foreground">
        Also answers to
      </p>
      {held.length === 0 ? (
        <p className="font-mono text-xs text-muted-foreground">
          {ranges.isFetching ? 'Reading…' : 'Only its own radio id.'}
        </p>
      ) : (
        <ul aria-label="Ranges" className="flex flex-col gap-1">
          {held.map((span) => (
            <li
              key={`${span.from}-${span.to}`}
              className="flex items-center justify-between gap-2 font-mono text-xs"
            >
              <span>{label(span)}</span>
              <Button
                type="button"
                variant="outline"
                size="sm"
                onClick={() => set({ id: row.id, delta: { add: [], remove: [span] } })}
              >
                {`Remove ${label(span)}`}
              </Button>
            </li>
          ))}
        </ul>
      )}

      <form
        aria-label={`Add a range to ${row.label ?? row.ref}`}
        className="flex items-end gap-2"
        onSubmit={async (event) => {
          event.preventDefault()
          // A blank box is not zero. Both inputs are `required`, so a browser
          // refuses this before it reaches here — but a form is submittable
          // from script, and posting `0-0` would claim a span nobody asked for.
          if (from === '' || to === '') return
          const span = { from: Number(from), to: Number(to) }
          try {
            await set({ id: row.id, delta: { add: [span], remove: [] } }).unwrap()
            setFrom('')
            setTo('')
          } catch {
            /* rendered below */
          }
        }}
      >
        <Field label="From" htmlFor={`unit-from-${row.id}`}>
          <input
            id={`unit-from-${row.id}`}
            type="number"
            required
            className={controlClass}
            value={from}
            onChange={(event) => setFrom(event.target.value)}
          />
        </Field>
        <Field label="To" htmlFor={`unit-to-${row.id}`}>
          <input
            id={`unit-to-${row.id}`}
            type="number"
            required
            className={controlClass}
            value={to}
            onChange={(event) => setTo(event.target.value)}
          />
        </Field>
        <Button type="submit" size="sm" disabled={setting.isLoading}>
          Add range
        </Button>
      </form>
      {setting.error != null && <FailureNote error={setting.error} />}
    </section>
  )
}

/** `1201–1299`, with an en dash — a span is a range of numbers, not a
 *  subtraction, and the hyphen reads as one at this size. */
function label(span: Span): string {
  return `${span.from}–${span.to}`
}
