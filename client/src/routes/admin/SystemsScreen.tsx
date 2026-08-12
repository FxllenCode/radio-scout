import { useState } from 'react'

import {
  AdminGate,
  FailureNote,
  Field,
  Placeholder,
  RowCard,
  RowList,
  SignOutButton,
  controlClass,
} from '@/components/admin/AdminUi'
import { Screen } from '@/components/layout/Screen'
import { useAdminSession } from '@/hooks/useAdminSession'
import { Button } from '@/components/ui/button'
import { parseRefs } from '@/lib/curate'
import {
  useCreateSystemMutation,
  useDeleteSystemMutation,
  useGetSystemsQuery,
  useUpdateSystemMutation,
} from '@/store/api'
import type { AdminSystem } from '@/types'

/**
 * Settings → Admin → Systems (#49).
 *
 * The entity everything else hangs off, and so the one whose delete is worth
 * being careful about: a System holds a night of Archive, and the server refuses
 * to take it until an Operator asks on purpose. rdio deletes the System and
 * leaves its calls behind as rows nothing can label.
 *
 * Two fields here reach nothing else in Radio-Scout. `autoPopulate` is the
 * per-System toggle (#8) that lets one System keep discovering channels while
 * the instance-wide switch is off; `enhancement` is the nullable scope (#20)
 * whose whole point is the third state — **inherit** — that a checkbox cannot
 * express, so it is a three-way select.
 */
export function SystemsScreen() {
  const signedIn = useAdminSession()
  const listing = useGetSystemsQuery(undefined, { skip: !signedIn })
  const [create, creating] = useCreateSystemMutation()
  const [remove, removing] = useDeleteSystemMutation()
  const [editing, setEditing] = useState<number | undefined>(undefined)
  const [draftRef, setDraftRef] = useState('')
  const [draftLabel, setDraftLabel] = useState('')

  const rows = listing.data?.results ?? []

  return (
    <Screen title="Systems" status={<SignOutButton />}>
      <AdminGate>
        <form
          className="mt-3 grid grid-cols-[6rem_1fr_auto] items-end gap-2"
          onSubmit={async (event) => {
            event.preventDefault()
            try {
              await create({
                // Blank mints the lowest free Ref, which is the same answer #8
                // gives a recorder that named a System without numbering it.
                ref: draftRef.trim() === '' ? undefined : Number(draftRef),
                label: draftLabel.trim() === '' ? undefined : draftLabel.trim(),
              }).unwrap()
              setDraftRef('')
              setDraftLabel('')
            } catch {
              /* rendered below */
            }
          }}
        >
          <Field label="Ref" htmlFor="new-system-ref">
            <input
              id="new-system-ref"
              type="number"
              placeholder="auto"
              className={controlClass}
              value={draftRef}
              onChange={(event) => setDraftRef(event.target.value)}
            />
          </Field>
          <Field label="New system" htmlFor="new-system-label">
            <input
              id="new-system-label"
              className={controlClass}
              value={draftLabel}
              onChange={(event) => setDraftLabel(event.target.value)}
            />
          </Field>
          <Button type="submit" size="sm" disabled={creating.isLoading}>
            Add
          </Button>
        </form>
        {creating.error != null && <FailureNote error={creating.error} />}

        {listing.isError ? (
          <Placeholder role="alert">
            The systems could not be read. Check that the server is reachable.
          </Placeholder>
        ) : rows.length === 0 ? (
          <Placeholder>
            {listing.isLoading
              ? 'Reading…'
              : 'No systems yet. One appears the first time a recorder uploads.'}
          </Placeholder>
        ) : (
          <RowList label="Systems">
            {rows.map((row) => (
              <RowCard
                key={row.id}
                title={row.label ?? `System ${row.ref}`}
                subtitle={`ref ${row.ref} · ${row.talkgroups} talkgroups · ${row.units} units · ${row.calls} calls`}
                actions={
                  <>
                    <Button
                      variant="outline"
                      size="sm"
                      onClick={() =>
                        setEditing(editing === row.id ? undefined : row.id)
                      }
                    >
                      {editing === row.id ? 'Close' : 'Edit'}
                    </Button>
                    <Button
                      variant="outline"
                      size="sm"
                      onClick={() => remove({ id: row.id })}
                    >
                      Delete
                    </Button>
                  </>
                }
              >
                {editing === row.id && (
                  <SystemForm row={row} onSaved={() => setEditing(undefined)} />
                )}
              </RowCard>
            ))}
          </RowList>
        )}
        {removing.error != null && (
          <FailureNote
            error={removing.error}
            // The one refusal with an answer. `originalArgs` rather than a
            // remembered id: it is what the refused request actually named, so
            // the retry cannot land on a different row than the one refused.
            onForce={() =>
              removing.originalArgs &&
              remove({ id: removing.originalArgs.id, force: true })
            }
          />
        )}
      </AdminGate>
    </Screen>
  )
}

/** One System's editable fields. */
function SystemForm({
  row,
  onSaved,
}: {
  row: AdminSystem
  onSaved: () => void
}) {
  const [update, updating] = useUpdateSystemMutation()
  const [label, setLabel] = useState(row.label ?? '')
  const [autoPopulate, setAutoPopulate] = useState(row.autoPopulate)
  const [enhancement, setEnhancement] = useState(inherit(row.enhancement))
  const [blacklist, setBlacklist] = useState(row.blacklist.join(', '))
  const [ref, setRef] = useState(String(row.ref))
  const refs = parseRefs(blacklist)

  return (
    <form
      aria-label={`Edit ${row.label ?? `system ${row.ref}`}`}
      className="flex flex-col gap-2"
      onSubmit={async (event) => {
        event.preventDefault()
        try {
          await update({
            id: row.id,
            patch: {
              // A renumbered System keeps its Talkgroups, its Units and its
              // Archive: those hang off the **Id**, and the Ref is what the
              // radio network calls it (CONTEXT.md). Editing it is how an
              // Operator corrects a number a recorder guessed at.
              ref: Number(ref),
              // `null` clears; an absent field could only ever leave it alone,
              // which would make a label impossible to remove.
              label: label.trim() === '' ? null : label.trim(),
              autoPopulate,
              enhancement: enhancement === '' ? null : enhancement === 'on',
              blacklist: refs ?? [],
            },
          }).unwrap()
          onSaved()
        } catch {
          /* rendered below */
        }
      }}
    >
      <div className="grid grid-cols-[6rem_1fr] gap-2">
        <Field label="Ref" htmlFor={`system-ref-${row.id}`}>
          <input
            id={`system-ref-${row.id}`}
            type="number"
            required
            className={controlClass}
            value={ref}
            onChange={(event) => setRef(event.target.value)}
          />
        </Field>
        <Field label="Label" htmlFor={`system-label-${row.id}`}>
          <input
            id={`system-label-${row.id}`}
            className={controlClass}
            value={label}
            onChange={(event) => setLabel(event.target.value)}
          />
        </Field>
      </div>
      <label className="flex items-center gap-2 font-mono text-xs">
        <input
          type="checkbox"
          checked={autoPopulate}
          onChange={(event) => setAutoPopulate(event.target.checked)}
        />
        Discover new talkgroups and units on this system
      </label>
      <Field label="Enhancement" htmlFor={`system-enhance-${row.id}`}>
        <select
          id={`system-enhance-${row.id}`}
          className={controlClass}
          value={enhancement}
          onChange={(event) => setEnhancement(event.target.value)}
        >
          <option value="">Follow the instance setting</option>
          <option value="on">Always enhance</option>
          <option value="off">Never enhance</option>
        </select>
      </Field>
      <Field
        label="Blacklisted talkgroup refs"
        htmlFor={`system-blacklist-${row.id}`}
      >
        <input
          id={`system-blacklist-${row.id}`}
          className={controlClass}
          placeholder="100, 200"
          value={blacklist}
          onChange={(event) => setBlacklist(event.target.value)}
        />
      </Field>
      {refs === undefined ? (
        // Refused here rather than dropped on the way to the server, because
        // this is the one field where a typo would be *silently* discarded:
        // rdio stores the list as free text and ignores whatever will not
        // parse, so a mistyped ref refuses nothing and says nothing.
        <p role="alert" className="font-mono text-xs text-red-400">
          Blacklist entries must be talkgroup refs — numbers, separated by
          commas.
        </p>
      ) : (
        <p className="font-mono text-[11px] text-muted-foreground">
          Refs listed here are never ingested, even before their talkgroup
          exists.
        </p>
      )}
      {updating.error != null && <FailureNote error={updating.error} />}
      <Button type="submit" size="sm" disabled={refs === undefined}>
        Save
      </Button>
    </form>
  )
}

/** The select's value for a nullable flag: `''` is "inherit", which is the state
 *  a checkbox has no way to hold. */
function inherit(value: boolean | null | undefined): string {
  if (value === true) return 'on'
  if (value === false) return 'off'
  return ''
}
