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
import { dateTimeLocalToMs, formatCallTime, msToDateTimeLocal } from '@/lib/archive'
import {
  useCreateAccessCodeMutation,
  useDeleteAccessCodeMutation,
  useGetAccessCodesQuery,
  useUpdateAccessCodeMutation,
} from '@/store/api'
import type { AdminAccessCode } from '@/types'

/**
 * Settings → Admin → Access codes (#68, spec US 52, ADR-0008).
 *
 * **This screen says who may hear a gated channel. It does not say which
 * channels are gated** — that is the `Restricted` toggle on the System and
 * Talkgroup forms, and the separation is the feature: a channel can be marked
 * sensitive before any code exists for it, which is the safe state to be in
 * halfway through setting this up.
 *
 * Two secrets and the listing carries neither. The **code** is Argon2id at rest
 * and cannot be read back by anybody, including this page; the **grant** — what
 * a browser actually carries — is shown exactly once, when a code is issued or
 * rotated. rdio-scanner stores its equivalent in plaintext, returns it from its
 * admin API, and exports it in the configuration document.
 */
export function AccessCodesScreen() {
  const signedIn = useAdminSession()
  const listing = useGetAccessCodesQuery(undefined, { skip: !signedIn })
  const [create, creating] = useCreateAccessCodeMutation()
  const [update] = useUpdateAccessCodeMutation()
  const [remove, removing] = useDeleteAccessCodeMutation()
  const [editing, setEditing] = useState<number | undefined>(undefined)
  const [label, setLabel] = useState('')
  const [code, setCode] = useState('')

  const rows = listing.data?.results ?? []
  const issued = creating.data

  return (
    <Screen title="Access codes" status={<SignOutButton />}>
      <AdminGate>
        <p className="mt-3 px-1 font-mono text-[11px] text-muted-foreground">
          A listener types one of these to hear a restricted channel. Which
          channels are restricted is a toggle on the system and talkgroup forms —
          with none marked, this instance is open to anybody who can reach it.
        </p>

        <form
          className="mt-3 grid grid-cols-[1fr_1fr_auto] items-end gap-2"
          onSubmit={async (event) => {
            event.preventDefault()
            try {
              await create({
                label: label.trim() === '' ? undefined : label.trim(),
                code: code.trim(),
              }).unwrap()
              setLabel('')
              setCode('')
            } catch {
              /* rendered below */
            }
          }}
        >
          <Field label="What is it for" htmlFor="new-code-label">
            <input
              id="new-code-label"
              className={controlClass}
              placeholder="fire ops"
              value={label}
              onChange={(event) => setLabel(event.target.value)}
            />
          </Field>
          <Field label="Code" htmlFor="new-code-secret">
            <input
              id="new-code-secret"
              className={controlClass}
              placeholder="at least 8 characters"
              value={code}
              onChange={(event) => setCode(event.target.value)}
            />
          </Field>
          <Button type="submit" size="sm" disabled={creating.isLoading}>
            Issue
          </Button>
        </form>
        {creating.error != null && <FailureNote error={creating.error} />}

        {issued && (
          // The grant, once. An Operator who wants a link that unlocks itself
          // pastes this; everyone else just says the code out loud and lets the
          // unlock form mint it again — which is why this is a convenience and
          // not, unlike an API key, the only way to use what was just made.
          <div
            role="status"
            className="mt-3 flex flex-col gap-1 rounded-xl border border-border bg-card px-4 py-3"
          >
            <p className="font-mono text-[10px] uppercase tracking-wider text-muted-foreground">
              A link that unlocks itself — shown once
            </p>
            <code className="break-all font-mono text-sm">
              ?grant={issued.grant}
            </code>
          </div>
        )}

        {listing.isError ? (
          <Placeholder role="alert">
            The access codes could not be read. Check that the server is
            reachable.
          </Placeholder>
        ) : rows.length === 0 ? (
          <Placeholder>
            {listing.isLoading ? 'Reading…' : 'No access codes yet.'}
          </Placeholder>
        ) : (
          <RowList label="Access codes">
            {rows.map((row) => (
              <RowCard
                key={row.id}
                title={row.label ?? `Code ${row.id}`}
                subtitle={subtitleOf(row)}
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
                        update({ id: row.id, patch: { disabled: !row.disabled } })
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
                  <CodeForm row={row} onSaved={() => setEditing(undefined)} />
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

/** What an Operator is actually asking of this listing: is this code being used,
 *  and until when. */
function subtitleOf(row: AdminAccessCode): string {
  const parts = [
    row.connections === 1 ? '1 listening' : `${row.connections} listening`,
    row.maxConnections == null
      ? 'no connection limit'
      : `limit ${row.maxConnections}`,
    row.expiresAtMs == null
      ? 'never expires'
      : `until ${formatCallTime(row.expiresAtMs)}`,
  ]
  if (row.disabled) parts.push('disabled')
  return parts.join(' · ')
}

/**
 * Everything about a code that can be changed, and the one thing that cannot.
 *
 * **Typing a new code rotates it, and every grant already out there dies with
 * it** — which is exactly what a rotation is for, and why the field is empty
 * rather than pre-filled: there is nothing to pre-fill it with (the code is
 * hashed), and a blank that means "leave it alone" is the only honest reading.
 */
function CodeForm({
  row,
  onSaved,
}: {
  row: AdminAccessCode
  onSaved: () => void
}) {
  const [update, updating] = useUpdateAccessCodeMutation()
  const [label, setLabel] = useState(row.label ?? '')
  const [code, setCode] = useState('')
  const [expires, setExpires] = useState(
    row.expiresAtMs == null ? '' : msToDateTimeLocal(row.expiresAtMs),
  )
  const [limit, setLimit] = useState(
    row.maxConnections == null ? '' : String(row.maxConnections),
  )
  const [scope, setScope] = useState(row.scope)

  return (
    <form
      aria-label={`Edit ${row.label ?? `code ${row.id}`}`}
      className="flex flex-col gap-3"
      onSubmit={async (event) => {
        event.preventDefault()
        try {
          await update({
            id: row.id,
            patch: {
              // `null` clears; an absent field could only ever leave what is
              // there, so an unlabelled code would be unreachable from here.
              label: label.trim() === '' ? null : label.trim(),
              // ...and here absence is exactly right: a code nobody retyped is
              // a code nobody meant to rotate.
              ...(code.trim() === '' ? {} : { code: code.trim() }),
              expiresAtMs:
                expires.trim() === '' ? null : (dateTimeLocalToMs(expires) ?? null),
              maxConnections: limit.trim() === '' ? null : Number(limit),
              scope,
            },
          }).unwrap()
          onSaved()
        } catch {
          /* rendered below */
        }
      }}
    >
      <div className="grid grid-cols-2 gap-2">
        <Field label="What is it for" htmlFor={`code-label-${row.id}`}>
          <input
            id={`code-label-${row.id}`}
            className={controlClass}
            value={label}
            onChange={(event) => setLabel(event.target.value)}
          />
        </Field>
        <Field label="New code (rotates it)" htmlFor={`code-secret-${row.id}`}>
          <input
            id={`code-secret-${row.id}`}
            className={controlClass}
            placeholder="leave blank to keep"
            value={code}
            onChange={(event) => setCode(event.target.value)}
          />
        </Field>
        <Field label="Expires" htmlFor={`code-expires-${row.id}`}>
          <input
            id={`code-expires-${row.id}`}
            type="datetime-local"
            className={controlClass}
            value={expires}
            onChange={(event) => setExpires(event.target.value)}
          />
        </Field>
        <Field label="Connection limit" htmlFor={`code-limit-${row.id}`}>
          <input
            id={`code-limit-${row.id}`}
            type="number"
            min={1}
            placeholder="unlimited"
            className={controlClass}
            value={limit}
            onChange={(event) => setLimit(event.target.value)}
          />
        </Field>
      </div>
      <div className="flex items-center gap-2">
        <Button type="submit" size="sm" disabled={updating.isLoading}>
          Save
        </Button>
        {updating.error != null && <FailureNote error={updating.error} />}
      </div>
      {/* The same editor a **Downstream** and a **Webhook** are scoped with,
          because a code's scope is the same thing: the live feed's own
          **Selection** matrix, so a **Patch** onto an opened channel is reached
          by one rule everywhere. */}
      <ScopeEditor
        id={`code-${row.id}`}
        scope={scope}
        onChange={setScope}
      />
    </form>
  )
}
