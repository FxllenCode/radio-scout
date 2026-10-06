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
import { useShareLink } from '@/hooks/useShareLink'
import { NOTHING, forwardsEverything, scopeRows } from '@/lib/downstream'
import { embedAddress, embedSnippet } from '@/lib/embed'
import type { Selection } from '@/lib/selection'
import {
  useCreateEmbedMutation,
  useDeleteEmbedMutation,
  useGetEmbedsQuery,
  useUpdateEmbedMutation,
} from '@/store/api'
import type { AdminEmbed } from '@/types'

/**
 * Settings → Admin → Embeds (#75, spec US 59) — the scanner on somebody else's
 * homepage.
 *
 * An Operator names a Selection, and is handed an `<iframe>` a fire department
 * or a newsroom pastes into their page. **The snippet names the row, not the
 * Selection**, so re-scoping one here changes what every host framing it plays,
 * and deleting it puts "no longer available" in their frame — without anybody
 * editing somebody else's HTML.
 *
 * Two things this screen says because nothing else would: that an embed plays
 * **open channels only** (its address is public, so it cannot carry a code), and
 * — when `[server] public_url` is unset — that the snippet's address is the one
 * this browser is on, which may be a LAN address no reader of the host's page
 * can reach.
 */
export function EmbedsScreen() {
  const signedIn = useAdminSession()
  const listing = useGetEmbedsQuery(undefined, { skip: !signedIn })
  const [remove, removing] = useDeleteEmbedMutation()
  const [editing, setEditing] = useState<number | undefined>(undefined)

  const rows = listing.data?.results ?? []

  return (
    <Screen title="Embeds" status={<SignOutButton />}>
      <AdminGate>
        <p className="mt-3 px-1 font-mono text-[11px] text-muted-foreground">
          A player another site can frame — a fire department's homepage, a local
          newsroom. It plays the selection you choose, open channels only. Change
          it here and every site framing it changes too; delete it and their
          frame says it is gone.
        </p>

        <NewEmbedForm />

        {listing.isError ? (
          <Placeholder role="alert">
            The embeds could not be read. Check that the server is reachable.
          </Placeholder>
        ) : rows.length === 0 ? (
          <Placeholder>
            {listing.isLoading ? 'Reading…' : 'No embeds yet.'}
          </Placeholder>
        ) : (
          <RowList label="Embeds">
            {rows.map((row) => (
              <RowCard
                key={row.id}
                title={row.name}
                subtitle={summaryOf(row.selection)}
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
                    <Button variant="outline" size="sm" asChild>
                      <a href={row.path} target="_blank" rel="noopener">
                        Preview
                      </a>
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
                {editing === row.id ? (
                  <EmbedForm row={row} onSaved={() => setEditing(undefined)} />
                ) : (
                  <Snippet row={row} />
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

/** What an embed plays, in a phrase. */
function summaryOf(selection: Selection): string {
  if (forwardsEverything(selection)) return 'plays everything'
  const systems = scopeRows(selection).length
  if (systems === 0) return 'plays nothing'
  return `plays ${systems} system${systems === 1 ? '' : 's'}`
}

/** The snippet, the button that copies it, and what an Operator should know
 *  before pasting it anywhere. */
function Snippet({ row }: { row: AdminEmbed }) {
  const origin = window.location.origin
  const snippet = embedSnippet(row, origin)
  // The link controls' notice and its timer, rather than that shape a second
  // time: "Copied" is a thing that just happened, and comes back down.
  const { notice, say } = useShareLink()

  return (
    <div className="flex flex-col gap-2">
      <Field label="Snippet" htmlFor={`embed-snippet-${row.id}`}>
        <textarea
          id={`embed-snippet-${row.id}`}
          readOnly
          rows={3}
          className={`${controlClass} h-auto font-mono text-[11px]`}
          value={snippet}
          onFocus={(event) => event.currentTarget.select()}
        />
      </Field>
      <div className="flex items-center gap-2">
        <Button
          type="button"
          variant="outline"
          size="sm"
          onClick={async () => {
            try {
              await navigator.clipboard.writeText(snippet)
              say('Copied')
            } catch {
              say('Could not copy — select the snippet instead')
            }
          }}
        >
          Copy snippet
        </Button>
        {notice && (
          <span role="status" className="font-mono text-[11px] text-muted-foreground">
            {notice}
          </span>
        )}
      </div>
      {row.restricted > 0 && (
        <p className="font-mono text-[11px] text-amber-400">
          {row.restricted} restricted channel{row.restricted === 1 ? '' : 's'} in
          this selection won't play here
        </p>
      )}
      {row.url === null && (
        <p className="font-mono text-[11px] text-muted-foreground">
          This address is the one you are on now ({origin}).
          Set [server] public_url so the snippet points where the public reaches
          this instance.
        </p>
      )}
      {embedAddress(row, origin).startsWith('http:') && (
        // Mixed content: a page served over https — nearly every site there is
        // — is not allowed to frame a plain-http address, and the frame is
        // simply blank. Said here, where the address is chosen.
        <p className="font-mono text-[11px] text-amber-400">
          This address is plain http, and a site served over https — nearly all
          of them — cannot frame it. Serve this instance over https first.
        </p>
      )}
    </div>
  )
}

/** The two things an embed is: what it is called, and what it plays. One
 *  component for both forms, so they cannot drift apart in what they ask. */
function EmbedFields({
  id,
  name,
  onName,
  selection,
  onSelection,
}: {
  id: string
  name: string
  onName: (name: string) => void
  selection: Selection
  onSelection: (selection: Selection) => void
}) {
  return (
    <>
      <Field label="Name" htmlFor={`${id}-name`}>
        <input
          id={`${id}-name`}
          className={controlClass}
          placeholder="Fire dispatch"
          value={name}
          onChange={(event) => onName(event.target.value)}
        />
      </Field>
      <ScopeEditor
        id={id}
        scope={selection}
        onChange={onSelection}
        legend="What it plays"
        everythingLabel="Play everything"
      />
    </>
  )
}

/** Naming one and choosing what it plays. */
function NewEmbedForm() {
  const [create, creating] = useCreateEmbedMutation()
  const [name, setName] = useState('')
  const [selection, setSelection] = useState<Selection>(NOTHING)
  // Remounting the editor is how a saved form starts again from nothing: the
  // editor holds the rows being typed, and is seeded once (`ScopeEditor`).
  const [generation, setGeneration] = useState(0)

  return (
    <form
      className="mt-3 flex flex-col gap-2 rounded-xl border border-border bg-card px-4 py-3"
      aria-label="Make an embed"
      onSubmit={async (event) => {
        event.preventDefault()
        try {
          await create({ name: name.trim(), selection }).unwrap()
          setName('')
          setSelection(NOTHING)
          setGeneration(generation + 1)
        } catch {
          /* rendered below */
        }
      }}
    >
      <EmbedFields
        key={generation}
        id="new-embed"
        name={name}
        onName={setName}
        selection={selection}
        onSelection={setSelection}
      />
      <div className="flex justify-end">
        <Button type="submit" size="sm" disabled={creating.isLoading}>
          Make embed
        </Button>
      </div>
      {creating.error != null && <FailureNote error={creating.error} />}
    </form>
  )
}

/** Renaming or re-scoping one. The address stays. */
function EmbedForm({ row, onSaved }: { row: AdminEmbed; onSaved: () => void }) {
  const [update, updating] = useUpdateEmbedMutation()
  const [name, setName] = useState(row.name)
  const [selection, setSelection] = useState<Selection>(row.selection)

  return (
    <form
      aria-label={`Edit ${row.name}`}
      className="flex flex-col gap-2"
      onSubmit={async (event) => {
        event.preventDefault()
        try {
          await update({
            id: row.id,
            patch: { name: name.trim(), selection },
          }).unwrap()
          onSaved()
        } catch {
          /* rendered below */
        }
      }}
    >
      <EmbedFields
        id={`embed-${row.id}`}
        name={name}
        onName={setName}
        selection={selection}
        onSelection={setSelection}
      />
      <div className="flex items-center gap-2">
        <Button type="submit" size="sm" disabled={updating.isLoading}>
          Save
        </Button>
      </div>
      {updating.error != null && <FailureNote error={updating.error} />}
    </form>
  )
}
