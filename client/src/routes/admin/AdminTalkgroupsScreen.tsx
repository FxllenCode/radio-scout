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
import {
  MemberRefsEditor,
  MergeConfirmation,
} from '@/components/admin/MergeEditor'
import { Screen } from '@/components/layout/Screen'
import { useAdminSession } from '@/hooks/useAdminSession'
import { useMerge } from '@/hooks/useMerge'
import { Button } from '@/components/ui/button'
import { pageSummary } from '@/lib/archive'
import { splitList } from '@/lib/curate'
import { LED_ORDER } from '@/lib/led'
import {
  useAssignTalkgroupsMutation,
  useCreateTalkgroupMutation,
  useDeleteTalkgroupMutation,
  useGetAdminTalkgroupsQuery,
  useGetGroupsQuery,
  useGetSystemsQuery,
  useGetTagsQuery,
  useUpdateTalkgroupMutation,
} from '@/store/api'
import type { AdminTalkgroup } from '@/types'

const PAGE_SIZE = 50

/**
 * Settings → Admin → Talkgroups (#49, spec US 45–46).
 *
 * The screen a county's Operator lives in, and the reason this is the only
 * curation listing that pages and filters server-side: hundreds of channels,
 * most auto-populated from traffic (#8) and named after a number.
 *
 * **Bulk assignment is the point** (US 46). Select rows, give them a Group or a
 * Tag, and it is one request in one transaction — where rdio needs a `PUT` of
 * the entire configuration document with those rows changed inside it, so a
 * dropped connection loses whatever else was in the document and a second open
 * tab silently undoes it.
 *
 * `blacklisted` is a per-row toggle rather than a comma-separated field on the
 * parent System, because an Operator is thinking "stop ingesting this channel"
 * and not "edit a string on something else". The System form still has the raw
 * list, for a Ref whose channel does not exist yet — the two write the same
 * column through the same parser.
 */
export function AdminTalkgroupsScreen() {
  const signedIn = useAdminSession()
  const systems = useGetSystemsQuery(undefined, { skip: !signedIn })
  const groups = useGetGroupsQuery(undefined, { skip: !signedIn })
  const tags = useGetTagsQuery(undefined, { skip: !signedIn })

  const [system, setSystem] = useState('')
  const [text, setText] = useState('')
  const [group, setGroup] = useState('')
  const [tag, setTag] = useState('')
  const [offset, setOffset] = useState(0)
  const [selected, setSelected] = useState<number[]>([])
  const [editing, setEditing] = useState<number | undefined>(undefined)
  // Merges open on their own control rather than inside the edit form: the
  // fields above are this channel's own, and a fold is about its relationship
  // to other channels — different work, and the destructive one.
  const [merging, setMerging] = useState<number | undefined>(undefined)

  const page = useGetAdminTalkgroupsQuery(
    {
      system: system === '' ? undefined : Number(system),
      q: text === '' ? undefined : text,
      group: group === '' ? undefined : group,
      tag: tag === '' ? undefined : tag,
      limit: PAGE_SIZE,
      offset,
    },
    { skip: !signedIn },
  )
  const [remove, removing] = useDeleteTalkgroupMutation()

  const rows = page.data?.results ?? []
  function refilter(apply: () => void) {
    apply()
    setOffset(0)
    // A selection is a set of rows the Operator can see; changing the question
    // means they can no longer see them, and a bulk action over rows that
    // scrolled out of the answer is the one thing multi-select must never do.
    setSelected([])
  }

  return (
    <Screen title="Talkgroups" status={<SignOutButton />}>
      <AdminGate>
        <form
          role="search"
          aria-label="Talkgroup filters"
          className="mt-3 grid grid-cols-2 gap-2"
          onSubmit={(event) => event.preventDefault()}
        >
          <Field label="System" htmlFor="tg-filter-system">
            <select
              id="tg-filter-system"
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
          <Field label="Search" htmlFor="tg-filter-q">
            <input
              id="tg-filter-q"
              className={controlClass}
              placeholder="label, name or ref"
              value={text}
              onChange={(event) => refilter(() => setText(event.target.value))}
            />
          </Field>
          <Field label="Group" htmlFor="tg-filter-group">
            <select
              id="tg-filter-group"
              className={controlClass}
              value={group}
              onChange={(event) => refilter(() => setGroup(event.target.value))}
            >
              <option value="">Every group</option>
              {(groups.data?.results ?? []).map((row) => (
                <option key={row.id} value={row.name}>
                  {row.name}
                </option>
              ))}
            </select>
          </Field>
          <Field label="Tag" htmlFor="tg-filter-tag">
            <select
              id="tg-filter-tag"
              className={controlClass}
              value={tag}
              onChange={(event) => refilter(() => setTag(event.target.value))}
            >
              <option value="">Every tag</option>
              {(tags.data?.results ?? []).map((row) => (
                <option key={row.id} value={row.name}>
                  {row.name}
                </option>
              ))}
            </select>
          </Field>
        </form>

        <NewTalkgroupForm systems={systems.data?.results ?? []} />

        {selected.length > 0 && (
          <BulkBar
            rows={rows.filter((row) => selected.includes(row.id))}
            onDone={() => setSelected([])}
            onClear={() => setSelected([])}
          />
        )}

        {rows.length > 0 && (
          <label className="mt-3 flex items-center gap-2 font-mono text-xs">
            <input
              type="checkbox"
              checked={rows.every((row) => selected.includes(row.id))}
              onChange={(event) =>
                setSelected(
                  event.target.checked ? rows.map((row) => row.id) : [],
                )
              }
            />
            {/* The other half of "foldable in bulk": a system that mints a TGID
                per patch event leaves dozens of near-identical rows, and forty
                individual clicks is the afternoon US 46 exists to save. Bounded
                to the page on purpose — a selection is rows an Operator can
                see, which is the same rule that clears it when a filter
                changes. */}
            Select all {rows.length} on this page
          </label>
        )}

        {page.isError ? (
          <Placeholder role="alert">
            The talkgroups could not be read. Check that the server is
            reachable.
          </Placeholder>
        ) : rows.length === 0 ? (
          <Placeholder>
            {page.isFetching ? 'Reading…' : 'No talkgroups match.'}
          </Placeholder>
        ) : (
          <RowList label="Talkgroups">
            {rows.map((row) => (
              <RowCard
                key={row.id}
                title={
                  <span className="flex items-center gap-2">
                    <input
                      type="checkbox"
                      aria-label={`Select ${row.label ?? row.ref}`}
                      checked={selected.includes(row.id)}
                      onChange={(event) =>
                        setSelected(
                          event.target.checked
                            ? [...selected, row.id]
                            : selected.filter((id) => id !== row.id),
                        )
                      }
                    />
                    {row.label ?? String(row.ref)}
                    {row.blacklisted && (
                      <span className="font-mono text-[10px] uppercase tracking-wider text-red-400">
                        blacklisted
                      </span>
                    )}
                  </span>
                }
                subtitle={`${row.systemLabel ?? `system ${row.systemRef}`} · ref ${
                  row.ref
                } · ${row.tag ?? 'untagged'} · ${
                  row.groups.length > 0 ? row.groups.join(', ') : 'no groups'
                } · ${row.calls} calls`}
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
                      onClick={() =>
                        setMerging(merging === row.id ? undefined : row.id)
                      }
                    >
                      Merges
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
                  <TalkgroupForm
                    row={row}
                    onSaved={() => setEditing(undefined)}
                  />
                )}
                {merging === row.id && <MemberRefsEditor row={row} />}
              </RowCard>
            ))}
          </RowList>
        )}
        {removing.error != null && (
          <FailureNote
            error={removing.error}
            onForce={() =>
              removing.originalArgs &&
              remove({ id: removing.originalArgs.id, force: true })
            }
          />
        )}

        <div className="mt-4 flex items-center justify-between gap-2">
          <Button
            variant="outline"
            size="sm"
            disabled={offset === 0}
            onClick={() => {
              setOffset(Math.max(0, offset - PAGE_SIZE))
              setSelected([])
            }}
          >
            Previous
          </Button>
          <p
            aria-live="polite"
            className="font-mono text-xs text-muted-foreground"
          >
            {pageSummary(
              offset,
              rows.length,
              page.data?.count ?? 0,
              'talkgroups',
            )}
          </p>
          <Button
            variant="outline"
            size="sm"
            disabled={!page.data?.hasMore}
            onClick={() => {
              setOffset(offset + PAGE_SIZE)
              setSelected([])
            }}
          >
            Next
          </Button>
        </div>
      </AdminGate>
    </Screen>
  )
}

/** Adding a channel by hand — for the one an Operator knows about before a
 *  recorder has heard it, which auto-populate (#8) can never create. */
function NewTalkgroupForm({
  systems,
}: {
  systems: { id: number; ref: number; label?: string | null }[]
}) {
  const [create, creating] = useCreateTalkgroupMutation()
  const [systemId, setSystemId] = useState('')
  const [ref, setRef] = useState('')
  const [label, setLabel] = useState('')
  const chosen = systemId === '' ? systems[0]?.id : Number(systemId)

  // Nothing a channel could belong to, so nothing to offer — rather than a form
  // that posts a Talkgroup under no System.
  if (systems.length === 0) return null

  return (
    <form
      aria-label="New talkgroup"
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
      <Field label="System" htmlFor="new-tg-system">
        <select
          id="new-tg-system"
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
      <Field label="Ref" htmlFor="new-tg-ref">
        <input
          id="new-tg-ref"
          type="number"
          required
          className={controlClass}
          value={ref}
          onChange={(event) => setRef(event.target.value)}
        />
      </Field>
      <Field label="Label" htmlFor="new-tg-label">
        <input
          id="new-tg-label"
          className={controlClass}
          placeholder="Fire Dispatch"
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

/** One action over every selected row (spec US 46). */
function BulkBar({
  rows,
  onDone,
  onClear,
}: {
  rows: AdminTalkgroup[]
  onDone: () => void
  onClear: () => void
}) {
  const [assign, assigning] = useAssignTalkgroupsMutation()
  const [addGroups, setAddGroups] = useState('')
  const [removeGroups, setRemoveGroups] = useState('')
  const [tag, setTag] = useState('')
  const ids = rows.map((row) => row.id)

  return (
    <form
      aria-label="Bulk assignment"
      className="mt-3 flex flex-col gap-2 rounded-xl border border-border bg-card px-3 py-3"
      onSubmit={async (event) => {
        event.preventDefault()
        try {
          await assign({
            ids,
            addGroups: splitList(addGroups),
            removeGroups: splitList(removeGroups),
            // Omitted leaves the Tag alone; `null` clears it — which is what
            // the explicit "clear" option is for, since a blank box has to
            // keep meaning "don't touch it".
            ...(tag === '' ? {} : { tag: tag === CLEAR ? null : tag }),
          }).unwrap()
          setAddGroups('')
          setRemoveGroups('')
          setTag('')
          onDone()
        } catch {
          /* rendered below */
        }
      }}
    >
      <p className="font-mono text-xs text-muted-foreground">
        {ids.length} selected
      </p>
      <div className="grid grid-cols-2 gap-2">
        <Field label="Add groups" htmlFor="bulk-add-groups">
          <input
            id="bulk-add-groups"
            className={controlClass}
            placeholder="Fire, Dispatch"
            value={addGroups}
            onChange={(event) => setAddGroups(event.target.value)}
          />
        </Field>
        <Field label="Remove groups" htmlFor="bulk-remove-groups">
          <input
            id="bulk-remove-groups"
            className={controlClass}
            value={removeGroups}
            onChange={(event) => setRemoveGroups(event.target.value)}
          />
        </Field>
      </div>
      <Field label="Set tag" htmlFor="bulk-tag">
        <input
          id="bulk-tag"
          className={controlClass}
          placeholder="leave blank to keep each row's tag"
          value={tag === CLEAR ? '' : tag}
          onChange={(event) => setTag(event.target.value)}
        />
      </Field>
      {assigning.error != null && <FailureNote error={assigning.error} />}
      <div className="flex gap-2">
        <Button type="submit" size="sm" disabled={assigning.isLoading}>
          Apply to {ids.length}
        </Button>
        <Button
          type="button"
          variant="outline"
          size="sm"
          onClick={() => setTag(CLEAR)}
        >
          Clear their tags
        </Button>
        <Button type="button" variant="outline" size="sm" onClick={onClear}>
          Deselect
        </Button>
      </div>
      {rows.length > 1 && <BulkFold rows={rows} onDone={onDone} />}
    </form>
  )
}

/** **Folding a whole selection into one channel** (#50, spec US 17).
 *
 *  The case the feature exists for: a system mints a fresh TGID per patch
 *  event, and a month later the panel is forty rows of churn around one real
 *  channel. Selecting them and saying which survives is the whole gesture —
 *  the others' Refs become that one request's fold list, so it is one
 *  transaction rather than forty, and it still goes through the same preview
 *  every other fold does.
 *
 *  **A Ref is unique only within its System**, so a selection spanning two
 *  cannot be folded: the other System's number would resolve to no channel here
 *  and record as a bare member Ref — a row that looks perfectly ordinary and did
 *  nothing the Operator wanted. Refused in the control rather than explained in
 *  the preview, because the preview is a request that would have to be sent
 *  first. */
function BulkFold({
  rows,
  onDone,
}: {
  rows: AdminTalkgroup[]
  onDone: () => void
}) {
  const [survivor, setSurvivor] = useState('')
  const oneSystem = new Set(rows.map((row) => row.systemId)).size === 1
  // **Derived, not stored.** The selection changes under this control — a row
  // ticked or unticked, a filter cleared — so a remembered id can name a row
  // that is no longer selected. Falling back to the first keeps the box and the
  // channel that would actually survive the same thing, which matters because
  // the box is the only place it is named.
  const target = rows.find((row) => String(row.id) === survivor) ?? rows[0]
  const merge = useMerge(target.id, onDone)

  return (
    <div className="flex flex-col gap-2 border-t border-border pt-2">
      <div className="flex items-end gap-2">
        <Field label="Fold into" htmlFor="bulk-fold-into">
          <select
            id="bulk-fold-into"
            className={controlClass}
            disabled={!oneSystem}
            value={String(target.id)}
            onChange={(event) => setSurvivor(event.target.value)}
          >
            {rows.map((row) => (
              <option key={row.id} value={row.id}>
                {row.label ?? `Talkgroup ${row.ref}`}
              </option>
            ))}
          </select>
        </Field>
        <Button
          type="button"
          size="sm"
          disabled={!oneSystem}
          onClick={() =>
            merge.preview({
              fold: rows
                .filter((row) => row.id !== target.id)
                .map((row) => row.ref),
              unfold: [],
            })
          }
        >
          Preview fold
        </Button>
      </div>
      {!oneSystem && (
        <p className="font-mono text-xs text-muted-foreground">
          A ref only means something inside its own system, so a fold takes one
          system at a time.
        </p>
      )}
      <MergeConfirmation merge={merge} into={target} />
    </div>
  )
}

/** The sentinel the tag box holds when the Operator has asked to clear rather
 *  than to set — a blank box has to keep meaning "leave it alone". */
const CLEAR = ' clear'

/** One Talkgroup's editable fields. */
function TalkgroupForm({
  row,
  onSaved,
}: {
  row: AdminTalkgroup
  onSaved: () => void
}) {
  const [update, updating] = useUpdateTalkgroupMutation()
  const [label, setLabel] = useState(row.label ?? '')
  const [name, setName] = useState(row.name ?? '')
  const [tag, setTag] = useState(row.tag ?? '')
  const [groups, setGroups] = useState(row.groups.join(', '))
  const [led, setLed] = useState(row.led ?? '')
  const [blacklisted, setBlacklisted] = useState(row.blacklisted)

  return (
    <form
      aria-label={`Edit ${row.label ?? row.ref}`}
      className="flex flex-col gap-2"
      onSubmit={async (event) => {
        event.preventDefault()
        try {
          await update({
            id: row.id,
            patch: {
              label: label.trim() === '' ? null : label.trim(),
              name: name.trim() === '' ? null : name.trim(),
              tag: tag.trim() === '' ? null : tag.trim(),
              groups: splitList(groups),
              led: led === '' ? null : led,
              blacklisted,
            },
          }).unwrap()
          onSaved()
        } catch {
          /* rendered below */
        }
      }}
    >
      <div className="grid grid-cols-2 gap-2">
        <Field label="Label" htmlFor={`tg-label-${row.id}`}>
          <input
            id={`tg-label-${row.id}`}
            className={controlClass}
            value={label}
            onChange={(event) => setLabel(event.target.value)}
          />
        </Field>
        <Field label="Name" htmlFor={`tg-name-${row.id}`}>
          <input
            id={`tg-name-${row.id}`}
            className={controlClass}
            value={name}
            onChange={(event) => setName(event.target.value)}
          />
        </Field>
        <Field label="Tag" htmlFor={`tg-tag-${row.id}`}>
          <input
            id={`tg-tag-${row.id}`}
            className={controlClass}
            value={tag}
            onChange={(event) => setTag(event.target.value)}
          />
        </Field>
        <Field label="Groups" htmlFor={`tg-groups-${row.id}`}>
          <input
            id={`tg-groups-${row.id}`}
            className={controlClass}
            placeholder="Fire, Dispatch"
            value={groups}
            onChange={(event) => setGroups(event.target.value)}
          />
        </Field>
      </div>
      <Field label="LED" htmlFor={`tg-led-${row.id}`}>
        <select
          id={`tg-led-${row.id}`}
          className={controlClass}
          value={led}
          onChange={(event) => setLed(event.target.value)}
        >
          {/* The palette itself, so a colour the server would refuse is not
              offerable — the client half of `curate::checked_led`. */}
          <option value="">Colour it by talkgroup</option>
          {LED_ORDER.map((colour) => (
            <option key={colour} value={colour}>
              {colour}
            </option>
          ))}
        </select>
      </Field>
      <label className="flex items-center gap-2 font-mono text-xs">
        <input
          type="checkbox"
          checked={blacklisted}
          onChange={(event) => setBlacklisted(event.target.checked)}
        />
        Never ingest calls on this talkgroup
      </label>
      {updating.error != null && <FailureNote error={updating.error} />}
      <Button type="submit" size="sm">
        Save
      </Button>
    </form>
  )
}
