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
import { ScopeEditor } from '@/components/admin/ScopeEditor'
import { Screen } from '@/components/layout/Screen'
import { Button } from '@/components/ui/button'
import { useAdminSession } from '@/hooks/useAdminSession'
import { NOTHING, healthLine } from '@/lib/downstream'
import type { Selection } from '@/lib/selection'
import {
  useCreateDownstreamMutation,
  useDeleteDownstreamMutation,
  useGetDownstreamsQuery,
  useUpdateDownstreamMutation,
} from '@/store/api'
import type { AdminDownstream } from '@/types'

/**
 * Settings → Admin → Downstreams (#52, spec US 1–2, 45).
 *
 * Where an Operator points this Instance at the peers it forwards to. Receiving
 * needs nothing configured — a peer's downstream is just a recorder with an API
 * key — so this screen is the whole feature's surface.
 *
 * **The health is on the row**, because #70's status page does not exist yet and
 * "my peer stopped getting calls" has to be answerable from somewhere. The
 * number that matters is `queued`: it is the durable queue, so a peer that has
 * been down for an hour shows an hour of Calls waiting rather than an hour of
 * Calls lost. rdio-scanner POSTs inline and drops the Call when a peer is
 * unreachable, and its admin page shows nothing at all about whether forwarding
 * is working — including, notably, every peer's API key in plaintext, which this
 * one deliberately cannot show even once.
 */
export function DownstreamsScreen() {
  const signedIn = useAdminSession()
  const listing = useGetDownstreamsQuery(undefined, { skip: !signedIn })
  const [remove, removing] = useDeleteDownstreamMutation()
  const [update] = useUpdateDownstreamMutation()
  const [editing, setEditing] = useState<number | undefined>(undefined)

  const rows = listing.data?.results ?? []

  return (
    <Screen title="Downstreams" status={<SignOutButton />}>
      <AdminGate>
        <p className="mt-3 px-1 font-mono text-[11px] text-muted-foreground">
          Other rdio-compatible instances this one forwards matching calls to.
          Nothing is lost when a peer goes down: matching calls are queued and
          drained in order once it comes back.
        </p>

        <NewPeerForm />

        {listing.isError ? (
          <Placeholder role="alert">
            The downstreams could not be read. Check that the server is
            reachable.
          </Placeholder>
        ) : rows.length === 0 ? (
          <Placeholder>
            {listing.isLoading ? 'Reading…' : 'No downstreams yet.'}
          </Placeholder>
        ) : (
          <RowList label="Downstreams">
            {rows.map((row) => (
              <RowCard
                key={row.id}
                title={row.label ?? row.url}
                subtitle={healthLine(row)}
                actions={
                  <>
                    <Button
                      type="button"
                      variant="outline"
                      size="sm"
                      onClick={() =>
                        setEditing(editing === row.id ? undefined : row.id)
                      }
                    >
                      {editing === row.id ? 'Close' : 'Edit'}
                    </Button>
                    <Button
                      type="button"
                      variant="outline"
                      size="sm"
                      onClick={() =>
                        update({
                          id: row.id,
                          patch: { disabled: !row.disabled },
                        })
                      }
                    >
                      {row.disabled ? 'Enable' : 'Disable'}
                    </Button>
                    <Button
                      type="button"
                      variant="outline"
                      size="sm"
                      onClick={() => remove(row.id)}
                    >
                      Remove
                    </Button>
                  </>
                }
              >
                {editing === row.id && (
                  <PeerForm row={row} onSaved={() => setEditing(undefined)} />
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

/** Adding a peer: where it is, the key *it* issued us, and what to send it. */
function NewPeerForm() {
  const [create, creating] = useCreateDownstreamMutation()
  const [label, setLabel] = useState('')
  const [url, setUrl] = useState('')
  const [apiKey, setApiKey] = useState('')
  const [scope, setScope] = useState<Selection>(NOTHING)

  return (
    <form
      className="mt-3 flex flex-col gap-2 rounded-xl border border-border bg-card px-4 py-3"
      aria-label="Add a downstream"
      onSubmit={async (event) => {
        event.preventDefault()
        try {
          await create({
            label: label.trim() === '' ? undefined : label.trim(),
            url: url.trim(),
            apiKey: apiKey.trim(),
            scope,
          }).unwrap()
          setLabel('')
          setUrl('')
          setApiKey('')
          setScope(NOTHING)
        } catch {
          /* rendered below */
        }
      }}
    >
      <div className="grid grid-cols-[1fr_1fr] gap-2">
        <Field label="What is it" htmlFor="new-downstream-label">
          <input
            id="new-downstream-label"
            className={controlClass}
            placeholder="county mirror"
            value={label}
            onChange={(event) => setLabel(event.target.value)}
          />
        </Field>
        <Field label="Address" htmlFor="new-downstream-url">
          <input
            id="new-downstream-url"
            className={controlClass}
            placeholder="https://peer.example"
            value={url}
            onChange={(event) => setUrl(event.target.value)}
          />
        </Field>
      </div>
      <Field label="The key that peer issued you" htmlFor="new-downstream-key">
        <input
          id="new-downstream-key"
          // A credential, so it is masked while it is typed — and it is the
          // only moment it is ever on screen: nothing reads it back.
          type="password"
          className={controlClass}
          value={apiKey}
          onChange={(event) => setApiKey(event.target.value)}
        />
      </Field>
      <ScopeEditor id="new-downstream" scope={scope} onChange={setScope} />
      <div className="flex justify-end">
        <Button type="submit" size="sm" disabled={creating.isLoading}>
          Add
        </Button>
      </div>
      {creating.error != null && <FailureNote error={creating.error} />}
    </form>
  )
}

/** Editing one. The key field is blank and **absence leaves it alone**, so
 *  re-scoping a peer does not mean re-typing a credential nothing can show. */
function PeerForm({
  row,
  onSaved,
}: {
  row: AdminDownstream
  onSaved: () => void
}) {
  const [update, updating] = useUpdateDownstreamMutation()
  const [label, setLabel] = useState(row.label ?? '')
  const [url, setUrl] = useState(row.url)
  const [apiKey, setApiKey] = useState('')
  const [scope, setScope] = useState<Selection>(row.scope)

  return (
    <form
      aria-label={`Edit ${row.label ?? row.url}`}
      className="flex flex-col gap-2"
      onSubmit={async (event) => {
        event.preventDefault()
        try {
          await update({
            id: row.id,
            patch: {
              label: label.trim() === '' ? null : label.trim(),
              url: url.trim(),
              // Omitted when blank — the whole point of the field.
              ...(apiKey.trim() === '' ? {} : { apiKey: apiKey.trim() }),
              scope,
            },
          }).unwrap()
          onSaved()
        } catch {
          /* rendered below */
        }
      }}
    >
      <div className="grid grid-cols-[1fr_1fr] gap-2">
        <Field label="What is it" htmlFor={`downstream-label-${row.id}`}>
          <input
            id={`downstream-label-${row.id}`}
            className={controlClass}
            value={label}
            onChange={(event) => setLabel(event.target.value)}
          />
        </Field>
        <Field label="Address" htmlFor={`downstream-url-${row.id}`}>
          <input
            id={`downstream-url-${row.id}`}
            className={controlClass}
            value={url}
            onChange={(event) => setUrl(event.target.value)}
          />
        </Field>
      </div>
      <Field
        label={
          row.hasKey
            ? 'Replace the key (leave blank to keep it)'
            : 'The key that peer issued you'
        }
        htmlFor={`downstream-key-${row.id}`}
      >
        <input
          id={`downstream-key-${row.id}`}
          type="password"
          className={controlClass}
          value={apiKey}
          onChange={(event) => setApiKey(event.target.value)}
        />
      </Field>
      <ScopeEditor
        id={`downstream-${row.id}`}
        scope={scope}
        onChange={setScope}
      />
      <div className="flex justify-end">
        <Button type="submit" size="sm">
          Save
        </Button>
      </div>
      {updating.error != null && <FailureNote error={updating.error} />}
    </form>
  )
}
