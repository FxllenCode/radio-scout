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
import { formatCallTime } from '@/lib/archive'
import {
  useCreateApiKeyMutation,
  useDeleteApiKeyMutation,
  useGetApiKeysQuery,
  useUpdateApiKeyMutation,
} from '@/store/api'
import type { AdminApiKey } from '@/types'

/**
 * Settings → Admin → API keys (#49, ADR-0008).
 *
 * **This is where API keys live.** `RADIO_SCOUT_API_KEY` remains the bootstrap —
 * a first run still writes one into `.env` so a recorder can be pointed at a
 * fresh instance without opening a screen — but it seeds an *empty* roster and
 * then stands aside, so a key revoked here does not come back on the next boot.
 *
 * A key is stored hashed, so the moment it is issued is the only moment it can
 * be copied. rdio-scanner stores its keys in plaintext, which means its admin
 * page, its database and every backup of it hand out every recorder's secret.
 */
export function ApiKeysScreen() {
  const signedIn = useAdminSession()
  const listing = useGetApiKeysQuery(undefined, { skip: !signedIn })
  const [create, creating] = useCreateApiKeyMutation()
  const [update] = useUpdateApiKeyMutation()
  const [editing, setEditing] = useState<number | undefined>(undefined)
  const [remove, removing] = useDeleteApiKeyMutation()
  const [label, setLabel] = useState('')
  const [systemRef, setSystemRef] = useState('')

  const rows = listing.data?.results ?? []
  const issued = creating.data

  return (
    <Screen title="API keys" status={<SignOutButton />}>
      <AdminGate>
        <p className="mt-3 px-1 font-mono text-[11px] text-muted-foreground">
          A recorder authenticates with one of these. Scope a key to a system
          ref to limit what it may upload, or leave it blank for every system.
        </p>

        <form
          className="mt-3 grid grid-cols-[1fr_6rem_auto] items-end gap-2"
          onSubmit={async (event) => {
            event.preventDefault()
            try {
              await create({
                label: label.trim() === '' ? undefined : label.trim(),
                systemRef:
                  systemRef.trim() === '' ? undefined : Number(systemRef),
              }).unwrap()
              setLabel('')
              setSystemRef('')
            } catch {
              /* rendered below */
            }
          }}
        >
          <Field label="What is it for" htmlFor="new-key-label">
            <input
              id="new-key-label"
              className={controlClass}
              placeholder="the pi in the shed"
              value={label}
              onChange={(event) => setLabel(event.target.value)}
            />
          </Field>
          <Field label="System ref" htmlFor="new-key-system">
            <input
              id="new-key-system"
              type="number"
              placeholder="any"
              className={controlClass}
              value={systemRef}
              onChange={(event) => setSystemRef(event.target.value)}
            />
          </Field>
          <Button type="submit" size="sm" disabled={creating.isLoading}>
            Issue
          </Button>
        </form>
        {creating.error != null && <FailureNote error={creating.error} />}

        {issued && (
          // The one and only sight of it. Left on screen until the Operator
          // navigates away rather than dismissed on a timer: the whole reason
          // it is here is that nothing can ever show it again.
          <div
            role="status"
            className="mt-3 flex flex-col gap-1 rounded-xl border border-border bg-card px-4 py-3"
          >
            <p className="font-mono text-[10px] uppercase tracking-wider text-muted-foreground">
              Copy this now — it cannot be shown again
            </p>
            <code className="break-all font-mono text-sm">{issued.key}</code>
          </div>
        )}

        {listing.isError ? (
          <Placeholder role="alert">
            The keys could not be read. Check that the server is reachable.
          </Placeholder>
        ) : rows.length === 0 ? (
          <Placeholder>
            {listing.isLoading ? 'Reading…' : 'No keys yet.'}
          </Placeholder>
        ) : (
          <RowList label="API keys">
            {rows.map((row) => (
              <RowCard
                key={row.id}
                title={row.label ?? `Key ${row.id}`}
                subtitle={`${
                  row.systemRef == null
                    ? 'every system'
                    : `system ${row.systemRef}`
                } · added ${formatCallTime(row.createdAtMs)}${
                  row.disabled ? ' · disabled' : ''
                }`}
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
                        update({
                          id: row.id,
                          patch: { disabled: !row.disabled },
                        })
                      }
                    >
                      {row.disabled ? 'Enable' : 'Disable'}
                    </Button>
                    <Button
                      variant="outline"
                      size="sm"
                      onClick={() => remove(row.id)}
                    >
                      Revoke
                    </Button>
                  </>
                }
              >
                {editing === row.id && (
                  <KeyForm row={row} onSaved={() => setEditing(undefined)} />
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

/** What a key can be changed to without re-issuing it: what it is *for*, and
 *  which System it may ingest into.
 *
 *  The secret itself is not editable and never could be — it is stored hashed,
 *  so there is nothing to edit. Re-scoping is the useful half: a recorder moved
 *  to another System keeps its key rather than being handed a new one. */
function KeyForm({ row, onSaved }: { row: AdminApiKey; onSaved: () => void }) {
  const [update, updating] = useUpdateApiKeyMutation()
  const [label, setLabel] = useState(row.label ?? '')
  const [systemRef, setSystemRef] = useState(
    row.systemRef == null ? '' : String(row.systemRef),
  )

  return (
    <form
      aria-label={`Edit ${row.label ?? `key ${row.id}`}`}
      className="flex items-end gap-2"
      onSubmit={async (event) => {
        event.preventDefault()
        try {
          await update({
            id: row.id,
            patch: {
              // `null` clears; an absent field could only ever leave what is
              // there, so an unscoped key would be unreachable from here.
              label: label.trim() === '' ? null : label.trim(),
              systemRef: systemRef.trim() === '' ? null : Number(systemRef),
            },
          }).unwrap()
          onSaved()
        } catch {
          /* rendered below */
        }
      }}
    >
      <Field label="What is it for" htmlFor={`key-label-${row.id}`}>
        <input
          id={`key-label-${row.id}`}
          className={controlClass}
          value={label}
          onChange={(event) => setLabel(event.target.value)}
        />
      </Field>
      <Field label="System ref" htmlFor={`key-system-${row.id}`}>
        <input
          id={`key-system-${row.id}`}
          type="number"
          placeholder="any"
          className={controlClass}
          value={systemRef}
          onChange={(event) => setSystemRef(event.target.value)}
        />
      </Field>
      <Button type="submit" size="sm">
        Save
      </Button>
      {updating.error != null && <FailureNote error={updating.error} />}
    </form>
  )
}
