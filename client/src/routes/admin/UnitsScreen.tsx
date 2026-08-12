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
import { pageSummary } from '@/lib/archive'
import {
  useCreateUnitMutation,
  useDeleteUnitMutation,
  useGetAdminUnitsQuery,
  useGetSystemsQuery,
  useUpdateUnitMutation,
} from '@/store/api'
import type { AdminUnit } from '@/types'

const PAGE_SIZE = 50

/**
 * Settings → Admin → Units (#49, spec US 42).
 *
 * An Instance rosters radios itself — every Call names the ones that keyed, and
 * #47 made either alias enough to create a row — so what this screen is for is
 * the half a recorder can never supply: the apparatus's *name*.
 *
 * Which is why the filter that matters is **unnamed**. An Operator sitting down
 * to name a fleet wants the radios nobody has named, and no text search can ask
 * for the absence of a name. rdio-scanner shows units nowhere at all.
 */
export function UnitsScreen() {
  const signedIn = useAdminSession()
  const systems = useGetSystemsQuery(undefined, { skip: !signedIn })
  const [system, setSystem] = useState('')
  const [text, setText] = useState('')
  const [unnamed, setUnnamed] = useState(false)
  const [offset, setOffset] = useState(0)

  const page = useGetAdminUnitsQuery(
    {
      system: system === '' ? undefined : Number(system),
      q: text === '' ? undefined : text,
      unnamed: unnamed ? true : undefined,
      limit: PAGE_SIZE,
      offset,
    },
    { skip: !signedIn },
  )
  const [remove, removing] = useDeleteUnitMutation()
  const [editing, setEditing] = useState<number | undefined>(undefined)

  const rows = page.data?.results ?? []
  /** Any filter change invalidates the window it was read in. */
  function refilter(apply: () => void) {
    apply()
    setOffset(0)
  }

  return (
    <Screen title="Units" status={<SignOutButton />}>
      <AdminGate>
        <form
          role="search"
          aria-label="Unit filters"
          className="mt-3 grid grid-cols-2 gap-2"
          onSubmit={(event) => event.preventDefault()}
        >
          <Field label="System" htmlFor="unit-filter-system">
            <select
              id="unit-filter-system"
              className={controlClass}
              value={system}
              onChange={(event) =>
                refilter(() => setSystem(event.target.value))
              }
            >
              <option value="">Every system</option>
              {(systems.data?.results ?? []).map((row) => (
                <option key={row.id} value={row.ref}>
                  {row.label ?? `System ${row.ref}`}
                </option>
              ))}
            </select>
          </Field>
          <Field label="Search" htmlFor="unit-filter-q">
            <input
              id="unit-filter-q"
              className={controlClass}
              placeholder="name or radio id"
              value={text}
              onChange={(event) => refilter(() => setText(event.target.value))}
            />
          </Field>
          <label className="col-span-2 flex items-center gap-2 font-mono text-xs">
            <input
              type="checkbox"
              checked={unnamed}
              onChange={(event) =>
                refilter(() => setUnnamed(event.target.checked))
              }
            />
            Only radios nobody has named
          </label>
        </form>

        <NewUnitForm systems={systems.data?.results ?? []} />

        {page.isError ? (
          <Placeholder role="alert">
            The units could not be read. Check that the server is reachable.
          </Placeholder>
        ) : rows.length === 0 ? (
          <Placeholder>
            {page.isFetching ? 'Reading…' : 'No units match.'}
          </Placeholder>
        ) : (
          <RowList label="Units">
            {rows.map((row) => (
              <RowCard
                key={row.id}
                title={row.label ?? `Radio ${row.ref}`}
                subtitle={`${row.systemLabel ?? `system ${row.systemRef}`} · radio ${row.ref}`}
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
                      onClick={() => remove(row.id)}
                    >
                      Delete
                    </Button>
                  </>
                }
              >
                {editing === row.id && (
                  <UnitForm row={row} onSaved={() => setEditing(undefined)} />
                )}
              </RowCard>
            ))}
          </RowList>
        )}
        {removing.error != null && <FailureNote error={removing.error} />}

        <div className="mt-4 flex items-center justify-between gap-2">
          <Button
            variant="outline"
            size="sm"
            disabled={offset === 0}
            onClick={() => setOffset(Math.max(0, offset - PAGE_SIZE))}
          >
            Previous
          </Button>
          <p
            aria-live="polite"
            className="font-mono text-xs text-muted-foreground"
          >
            {pageSummary(offset, rows.length, page.data?.count ?? 0, 'units')}
          </p>
          <Button
            variant="outline"
            size="sm"
            disabled={!page.data?.hasMore}
            onClick={() => setOffset(offset + PAGE_SIZE)}
          >
            Next
          </Button>
        </div>
      </AdminGate>
    </Screen>
  )
}

/** Adding a radio nobody has heard yet — an apparatus an Operator knows about
 *  before it keys. */
function NewUnitForm({
  systems,
}: {
  systems: { id: number; ref: number; label?: string | null }[]
}) {
  const [create, creating] = useCreateUnitMutation()
  const [systemId, setSystemId] = useState('')
  const [ref, setRef] = useState('')
  const [label, setLabel] = useState('')
  const chosen = systemId === '' ? systems[0]?.id : Number(systemId)

  if (systems.length === 0) return null

  return (
    <form
      className="mt-3 grid grid-cols-[1fr_5rem_1fr_auto] items-end gap-2"
      onSubmit={async (event) => {
        event.preventDefault()
        if (chosen === undefined) return
        try {
          await create({
            systemId: chosen,
            ref: Number(ref),
            label: label.trim() === '' ? undefined : label.trim(),
          }).unwrap()
          setRef('')
          setLabel('')
        } catch {
          /* rendered below */
        }
      }}
    >
      <Field label="System" htmlFor="new-unit-system">
        <select
          id="new-unit-system"
          className={controlClass}
          value={systemId}
          onChange={(event) => setSystemId(event.target.value)}
        >
          {systems.map((row) => (
            <option key={row.id} value={row.id}>
              {row.label ?? `System ${row.ref}`}
            </option>
          ))}
        </select>
      </Field>
      <Field label="Radio id" htmlFor="new-unit-ref">
        <input
          id="new-unit-ref"
          type="number"
          required
          className={controlClass}
          value={ref}
          onChange={(event) => setRef(event.target.value)}
        />
      </Field>
      <Field label="Name" htmlFor="new-unit-label">
        <input
          id="new-unit-label"
          className={controlClass}
          placeholder="Engine 1"
          value={label}
          onChange={(event) => setLabel(event.target.value)}
        />
      </Field>
      <Button type="submit" size="sm" disabled={creating.isLoading}>
        Add
      </Button>
      {creating.error != null && (
        <div className="col-span-4">
          <FailureNote error={creating.error} />
        </div>
      )}
    </form>
  )
}

/** One radio's editable fields. */
function UnitForm({ row, onSaved }: { row: AdminUnit; onSaved: () => void }) {
  const [update, updating] = useUpdateUnitMutation()
  const [label, setLabel] = useState(row.label ?? '')

  return (
    <form
      className="flex items-end gap-2"
      onSubmit={async (event) => {
        event.preventDefault()
        try {
          await update({
            id: row.id,
            // `null` un-names the apparatus; an absent field could only leave
            // the name it already has.
            patch: { label: label.trim() === '' ? null : label.trim() },
          }).unwrap()
          onSaved()
        } catch {
          /* rendered below */
        }
      }}
    >
      <input
        aria-label={`Name radio ${row.ref}`}
        className={controlClass}
        value={label}
        onChange={(event) => setLabel(event.target.value)}
      />
      <Button type="submit" size="sm">
        Save
      </Button>
      {updating.error != null && <FailureNote error={updating.error} />}
    </form>
  )
}
