import { useState } from 'react'

import {
  AdminGate,
  FailureNote,
  Placeholder,
  RowCard,
  RowList,
  SignOutButton,
  controlClass,
} from '@/components/admin/AdminUi'
import { Screen } from '@/components/layout/Screen'
import { useAdminSession } from '@/hooks/useAdminSession'
import { Button } from '@/components/ui/button'
import {
  useCreateGroupMutation,
  useCreateTagMutation,
  useDeleteGroupMutation,
  useDeleteTagMutation,
  useGetGroupsQuery,
  useGetTagsQuery,
  useUpdateGroupMutation,
  useUpdateTagMutation,
} from '@/store/api'
import type { AdminLabel } from '@/types'

/**
 * Settings → Admin → Groups / Tags (#49).
 *
 * One screen for both, because everything an Operator can do to a Group they can
 * do identically to a Tag — the split the server makes for the same reason. What
 * differs is only what deleting one *means*, and that is a sentence rather than
 * a control: a Group is a category a channel belongs to, so removing it removes
 * the links; a Tag is the single service label on a channel, so removing it
 * un-tags them. Either way the channels survive, which is what the count on each
 * row is there to make believable before the Operator presses the button.
 */
export function GroupsScreen() {
  const signedIn = useAdminSession()
  const listing = useGetGroupsQuery(undefined, { skip: !signedIn })
  const [create, creating] = useCreateGroupMutation()
  const [rename, renaming] = useUpdateGroupMutation()
  const [remove, removing] = useDeleteGroupMutation()

  return (
    <LabelsScreen
      title="Groups"
      noun="group"
      explains="A group clusters talkgroups across systems, for bulk selection. Deleting one leaves its talkgroups alone."
      listing={listing}
      create={create}
      creating={creating}
      rename={rename}
      renaming={renaming}
      remove={remove}
      removing={removing}
    />
  )
}

export function TagsScreen() {
  const signedIn = useAdminSession()
  const listing = useGetTagsQuery(undefined, { skip: !signedIn })
  const [create, creating] = useCreateTagMutation()
  const [rename, renaming] = useUpdateTagMutation()
  const [remove, removing] = useDeleteTagMutation()

  return (
    <LabelsScreen
      title="Tags"
      noun="tag"
      explains="A tag is the single service label on a talkgroup. Deleting one un-tags its talkgroups; it never deletes them."
      listing={listing}
      create={create}
      creating={creating}
      rename={rename}
      renaming={renaming}
      remove={remove}
      removing={removing}
    />
  )
}

/** What either screen is handed — the four operations, already bound to their
 *  own table by the two wrappers above. */
interface LabelsProps {
  title: string
  noun: string
  explains: string
  listing: {
    data?: { results: AdminLabel[] }
    isLoading: boolean
    isError: boolean
  }
  create: (name: string) => { unwrap: () => Promise<unknown> }
  creating: { error?: unknown; isLoading: boolean }
  rename: (edit: { id: number; name: string }) => {
    unwrap: () => Promise<unknown>
  }
  renaming: { error?: unknown }
  remove: (id: number) => { unwrap: () => Promise<unknown> }
  removing: { error?: unknown }
}

function LabelsScreen({
  title,
  noun,
  explains,
  listing,
  create,
  creating,
  rename,
  renaming,
  remove,
  removing,
}: LabelsProps) {
  const [name, setName] = useState('')
  const [editing, setEditing] = useState<number | undefined>(undefined)
  const rows = listing.data?.results ?? []

  return (
    <Screen title={title} status={<SignOutButton />}>
      <AdminGate>
        <p className="mt-3 px-1 font-mono text-[11px] text-muted-foreground">
          {explains}
        </p>

        <form
          className="mt-3 flex items-end gap-2"
          onSubmit={async (event) => {
            event.preventDefault()
            // Cleared only on success, so a refused name stays in the box for
            // the Operator to correct rather than being thrown away with the
            // error still on screen.
            try {
              await create(name).unwrap()
              setName('')
            } catch {
              /* the message is rendered from `creating.error` below */
            }
          }}
        >
          <div className="flex flex-1 flex-col gap-1">
            <label
              htmlFor="new-label"
              className="font-mono text-[10px] uppercase tracking-wider text-muted-foreground"
            >
              New {noun}
            </label>
            <input
              id="new-label"
              className={controlClass}
              value={name}
              onChange={(event) => setName(event.target.value)}
            />
          </div>
          <Button type="submit" size="sm" disabled={creating.isLoading}>
            Add
          </Button>
        </form>
        {creating.error != null && <FailureNote error={creating.error} />}

        {listing.isError ? (
          <Placeholder role="alert">
            That list could not be read. Check that the server is reachable.
          </Placeholder>
        ) : rows.length === 0 ? (
          <Placeholder>
            {listing.isLoading ? 'Reading…' : `No ${noun}s yet.`}
          </Placeholder>
        ) : (
          <RowList label={title}>
            {rows.map((row) => (
              <RowCard
                key={row.id}
                title={row.name}
                subtitle={`${row.talkgroups} talkgroup${row.talkgroups === 1 ? '' : 's'}`}
                actions={
                  <>
                    <Button
                      variant="outline"
                      size="sm"
                      onClick={() =>
                        setEditing(editing === row.id ? undefined : row.id)
                      }
                    >
                      {editing === row.id ? 'Cancel' : 'Rename'}
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
                  <RenameForm
                    row={row}
                    error={renaming.error}
                    onSave={async (next) => {
                      try {
                        await rename({ id: row.id, name: next }).unwrap()
                        setEditing(undefined)
                      } catch {
                        /* rendered by the form */
                      }
                    }}
                  />
                )}
              </RowCard>
            ))}
          </RowList>
        )}
        {removing.error != null && <FailureNote error={removing.error} />}
      </AdminGate>
    </Screen>
  )
}

/** The inline rename. Its own component so each row's draft is its own state —
 *  one shared draft would carry a half-typed name onto the next row opened. */
function RenameForm({
  row,
  error,
  onSave,
}: {
  row: AdminLabel
  error: unknown
  onSave: (name: string) => void
}) {
  const [draft, setDraft] = useState(row.name)

  return (
    <form
      className="flex flex-col gap-2"
      onSubmit={(event) => {
        event.preventDefault()
        onSave(draft)
      }}
    >
      <div className="flex items-end gap-2">
        <input
          aria-label={`Rename ${row.name}`}
          className={controlClass}
          value={draft}
          onChange={(event) => setDraft(event.target.value)}
        />
        <Button type="submit" size="sm">
          Save
        </Button>
      </div>
      {/* A refused rename has to say so *here*, beside the box holding the
          name that was refused — the editor stays open for exactly that. */}
      {error != null && <FailureNote error={error} />}
    </form>
  )
}
