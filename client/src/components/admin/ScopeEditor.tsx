import { useState } from 'react'

import { Field, controlClass } from '@/components/admin/AdminUi'
import { Button } from '@/components/ui/button'
import {
  EVERYTHING,
  NOTHING,
  forwardsEverything,
  isEditable,
  parseRefs,
  scopeOf,
  scopeRows,
} from '@/lib/downstream'
import type { Selection } from '@/lib/selection'

/** One line of the form, as the Operator is *typing* it.
 *
 *  Text, not numbers, and that is the whole reason this state exists. A field
 *  rendered from the parsed value fights the typing: `"100, "` parses to `[100]`
 *  and re-renders as `"100"`, eating the separator on every keystroke, so a list
 *  of channels cannot be typed at all. Holding the raw text and parsing on the
 *  way *out* is the standard shape and the only one that works here.
 *
 *  `key` is stable and per row rather than the array index, so removing a row
 *  does not leave the row below it holding the removed one's text. */
interface EditableRow {
  key: number
  systemRef: string
  talkgroups: string
}

/**
 * Which Calls a **Downstream** peer gets (#52).
 *
 * The stored value is the live feed's own **Selection** matrix, so one predicate
 * scopes the feed and forwarding alike — and a patched Call reaches a
 * peer subscribed to the channel it was patched onto, which is the bug rdio has
 * from scoping forwarding with a rule of its own.
 *
 * The *form* is deliberately narrower than the matrix: a checkbox for
 * everything, else a row per System naming either all of it or a few channels.
 * That is the whole of what rdio's equivalent can express, and the shape an
 * Operator actually wants. The matrix can also say "everything **except**", and
 * this refuses to edit a scope that does rather than redrawing it as something
 * smaller — see [`isEditable`]. A hand-edited configuration document is where
 * one comes from, and silently narrowing it would be the worst kind of quiet.
 */
export function ScopeEditor({
  id,
  scope,
  onChange,
}: {
  /** Unique per mounted editor, so two on one screen do not share input ids. */
  id: string
  scope: Selection
  onChange: (scope: Selection) => void
}) {
  // Seeded once, from the scope this editor was opened on. It is not resynced
  // from the prop afterwards, and must not be: the only thing that changes the
  // scope while this is mounted is this component, so a resync could only ever
  // undo what is being typed.
  const [rows, setRows] = useState<EditableRow[]>(() =>
    scopeRows(scope).map((row, index) => ({
      key: index,
      systemRef: String(row.systemRef),
      talkgroups: row.talkgroupRefs.join(', '),
    })),
  )
  const [nextKey, setNextKey] = useState(() => scopeRows(scope).length)

  const everything = forwardsEverything(scope)
  const editable = isEditable(scope)

  /** Show `next`, and tell the form what it now means. */
  const replace = (next: EditableRow[]) => {
    setRows(next)
    onChange(
      scopeOf(
        next.map((row) => ({
          systemRef: Number(row.systemRef),
          talkgroupRefs: parseRefs(row.talkgroups),
        })),
      ),
    )
  }

  const edit = (key: number, change: Partial<EditableRow>) =>
    replace(rows.map((row) => (row.key === key ? { ...row, ...change } : row)))

  return (
    <fieldset className="flex flex-col gap-2 rounded-lg border border-border px-3 py-2">
      <legend className="px-1 font-mono text-[10px] uppercase tracking-wider text-muted-foreground">
        What to forward
      </legend>

      {!editable && (
        // Read-only rather than a Save that would quietly forward less.
        <p role="status" className="font-mono text-[11px] text-muted-foreground">
          This peer's scope carries exceptions, which this form cannot show. Edit
          it in the configuration document, or replace it here by choosing
          everything.
        </p>
      )}

      <label className="flex items-center gap-2 text-sm">
        <input
          type="checkbox"
          checked={everything}
          onChange={(event) => {
            if (event.target.checked) {
              setRows([])
              onChange(EVERYTHING)
            } else {
              onChange(NOTHING)
            }
          }}
        />
        Forward everything
      </label>

      {!everything && editable && (
        <>
          {rows.map((row) => (
            <div
              key={row.key}
              className="grid grid-cols-[6rem_1fr_auto] items-end gap-2"
            >
              <Field label="System ref" htmlFor={`${id}-scope-system-${row.key}`}>
                <input
                  id={`${id}-scope-system-${row.key}`}
                  type="number"
                  className={controlClass}
                  value={row.systemRef}
                  onChange={(event) =>
                    edit(row.key, { systemRef: event.target.value })
                  }
                />
              </Field>
              <Field
                label="Talkgroup refs (blank = all)"
                htmlFor={`${id}-scope-talkgroups-${row.key}`}
              >
                <input
                  id={`${id}-scope-talkgroups-${row.key}`}
                  className={controlClass}
                  placeholder="all of them"
                  value={row.talkgroups}
                  onChange={(event) =>
                    edit(row.key, { talkgroups: event.target.value })
                  }
                />
              </Field>
              <Button
                type="button"
                variant="outline"
                size="sm"
                onClick={() =>
                  replace(rows.filter((existing) => existing.key !== row.key))
                }
              >
                Remove
              </Button>
            </div>
          ))}
          <div>
            <Button
              type="button"
              variant="outline"
              size="sm"
              onClick={() => {
                // A row with no System yet contributes nothing — `scopeOf`
                // drops it — so adding one changes what is on screen without
                // changing what would be saved.
                replace([
                  ...rows,
                  { key: nextKey, systemRef: '', talkgroups: '' },
                ])
                setNextKey(nextKey + 1)
              }}
            >
              Add a system
            </Button>
          </div>
        </>
      )}
    </fieldset>
  )
}
