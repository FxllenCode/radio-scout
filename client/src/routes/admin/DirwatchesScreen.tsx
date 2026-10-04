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
import { Button } from '@/components/ui/button'
import { useAdminSession } from '@/hooks/useAdminSession'
import {
  FORMATS,
  MASK_TOKENS,
  fieldsFor,
  formatChoice,
  healthLine,
  wholeNumber,
} from '@/lib/dirwatch'
import {
  useCreateDirwatchMutation,
  useDeleteDirwatchMutation,
  useGetDirwatchesQuery,
  useScanDirwatchMutation,
  useUpdateDirwatchMutation,
} from '@/store/api'
import type { AdminDirwatch, DirwatchFormat, NewDirwatch } from '@/types'

/**
 * Settings → Admin → Dirwatch (#72, spec US 14).
 *
 * Folders a Recorder drops Calls into — Trunk Recorder's `captureDir`,
 * SDRTrunk's recordings, DSDPlus's record folders — ingested through the same
 * pipeline an upload takes, with no upload configured at all.
 *
 * **Where a watch may be is not this screen's to decide.** A watch reads every
 * file in its folder and can delete them, so the folders allowed are
 * `[dirwatch] roots` in the TOML (ADR-0021), which the listing reports and this
 * never writes. With none, the form is not drawn — a control that would be
 * refused is not offered — and the screen says how to turn Dirwatch on instead.
 *
 * The health is on the row, as it is for a Downstream: rdio's dirwatch is a
 * black box that either ingests or silently does not, and "is my folder being
 * read?" has to be answerable from somewhere.
 */
export function DirwatchesScreen() {
  const signedIn = useAdminSession()
  const listing = useGetDirwatchesQuery(undefined, { skip: !signedIn })
  const [remove, removing] = useDeleteDirwatchMutation()
  const [update, updating] = useUpdateDirwatchMutation()
  const [scan, scanning] = useScanDirwatchMutation()
  const [editing, setEditing] = useState<number | undefined>(undefined)

  const rows = listing.data?.results ?? []
  const roots = listing.data?.roots ?? []

  return (
    <Screen title="Dirwatch" status={<SignOutButton />}>
      <AdminGate>
        <p className="mt-3 px-1 font-mono text-[11px] text-muted-foreground">
          Folders a recorder drops calls into, ingested exactly as an upload is —
          deduplicated, merged and enriched. A file that cannot be a call is
          left where it is and said so in the log.
        </p>

        {listing.isError ? (
          <Placeholder role="alert">
            The watches could not be read. Check that the server is reachable.
          </Placeholder>
        ) : listing.isLoading ? (
          <Placeholder>Reading…</Placeholder>
        ) : roots.length === 0 ? (
          <Placeholder>
            Dirwatch is off on this instance. Name the folders a watch may be
            in under [dirwatch] roots in radio-scout.toml (or
            RADIO_SCOUT_DIRWATCH_ROOTS) and restart.
          </Placeholder>
        ) : (
          <>
            <p className="mt-3 px-1 font-mono text-[11px] text-muted-foreground">
              A watch may be in {roots.join(', ')}.
            </p>
            <NewWatchForm root={roots[0]} />
          </>
        )}

        {rows.length > 0 && (
          <RowList label="Watches">
            {rows.map((row) => (
              <RowCard
                key={row.id}
                title={row.label ?? row.directory}
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
                      disabled={row.disabled}
                      onClick={() => scan(row.id)}
                    >
                      Scan now
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
                  <WatchForm row={row} onSaved={() => setEditing(undefined)} />
                )}
              </RowCard>
            ))}
          </RowList>
        )}
        {[removing.error, updating.error, scanning.error].map(
          (error, index) =>
            error != null && <FailureNote key={index} error={error} />,
        )}
      </AdminGate>
    </Screen>
  )
}

/** What the form's inputs hold, as text — a number box re-rendered from its
 *  parsed value fights the typing. */
interface Draft {
  label: string
  directory: string
  format: DirwatchFormat
  extension: string
  mask: string
  systemRef: string
  talkgroupRef: string
  frequency: string
  delayMs: string
  deleteAfter: boolean
  poll: boolean
}

function draftOf(row: AdminDirwatch): Draft {
  const text = (value: number | null | undefined) =>
    value == null ? '' : String(value)
  return {
    label: row.label ?? '',
    directory: row.directory,
    format: row.format,
    extension: row.extension ?? '',
    mask: row.mask ?? '',
    systemRef: text(row.systemRef),
    talkgroupRef: text(row.talkgroupRef),
    frequency: text(row.frequency),
    delayMs: String(row.delayMs),
    deleteAfter: row.deleteAfter,
    poll: row.poll,
  }
}

/** The body a draft asks for — or the field it cannot, refused before asking.
 *  Blank optionals are `null`, which both clears them on an edit and leaves
 *  them unset on a create. */
function bodyOf(draft: Draft): NewDirwatch | string {
  const numbers = {
    systemRef: wholeNumber(draft.systemRef),
    talkgroupRef: wholeNumber(draft.talkgroupRef),
    frequency: wholeNumber(draft.frequency),
    delayMs: wholeNumber(draft.delayMs),
  }
  const bad = Object.entries(numbers).find(([, value]) => value === null)
  if (bad) return `${bad[0]} has to be a whole number`
  const fields = fieldsFor(draft.format)
  const optional = (text: string) => (text.trim() === '' ? null : text.trim())
  return {
    label: optional(draft.label),
    directory: draft.directory.trim(),
    format: draft.format,
    extension: optional(draft.extension),
    mask: fields.mask ? optional(draft.mask) : null,
    systemRef: numbers.systemRef ?? null,
    talkgroupRef: fields.talkgroup ? (numbers.talkgroupRef ?? null) : null,
    frequency: numbers.frequency ?? null,
    delayMs: numbers.delayMs ?? 2000,
    deleteAfter: draft.deleteAfter,
    poll: draft.poll,
  }
}

/** Watching a new folder. */
function NewWatchForm({ root }: { root: string }) {
  const [create, creating] = useCreateDirwatchMutation()
  const fresh: Draft = {
    label: '',
    directory: root,
    format: 'trunk-recorder',
    extension: '',
    mask: '',
    systemRef: '',
    talkgroupRef: '',
    frequency: '',
    delayMs: '2000',
    deleteAfter: false,
    poll: false,
  }
  const [draft, setDraft] = useState<Draft>(fresh)

  return (
    <DraftForm
      id="new-dirwatch"
      label="Watch a folder"
      draft={draft}
      onChange={setDraft}
      submit="Watch"
      busy={creating.isLoading}
      error={creating.error}
      onSubmit={async (body) => {
        await create(body).unwrap()
        setDraft(fresh)
      }}
    />
  )
}

/** Editing one. */
function WatchForm({
  row,
  onSaved,
}: {
  row: AdminDirwatch
  onSaved: () => void
}) {
  const [update, updating] = useUpdateDirwatchMutation()
  const [draft, setDraft] = useState<Draft>(draftOf(row))

  return (
    <DraftForm
      id={`dirwatch-${row.id}`}
      label={`Edit ${row.label ?? row.directory}`}
      draft={draft}
      onChange={setDraft}
      submit="Save"
      busy={updating.isLoading}
      error={updating.error}
      onSubmit={async (body) => {
        await update({ id: row.id, patch: body }).unwrap()
        onSaved()
      }}
    />
  )
}

/** The one form both a create and an edit draw. */
function DraftForm({
  id,
  label,
  draft,
  onChange,
  submit,
  busy,
  error,
  onSubmit,
}: {
  id: string
  label: string
  draft: Draft
  onChange: (draft: Draft) => void
  submit: string
  busy: boolean
  error: unknown
  onSubmit: (body: NewDirwatch) => Promise<void>
}) {
  const [problem, setProblem] = useState<string | undefined>(undefined)
  const set = (patch: Partial<Draft>) => onChange({ ...draft, ...patch })
  const format = formatChoice(draft.format)
  const fields = fieldsFor(draft.format)

  return (
    <form
      aria-label={label}
      className="mt-3 flex flex-col gap-2 rounded-xl border border-border bg-card px-4 py-3"
      onSubmit={async (event) => {
        event.preventDefault()
        const body = bodyOf(draft)
        if (typeof body === 'string') {
          setProblem(body)
          return
        }
        setProblem(undefined)
        try {
          await onSubmit(body)
        } catch {
          /* rendered below */
        }
      }}
    >
      <div className="grid grid-cols-[1fr_1fr] gap-2">
        <Field label="Recorder" htmlFor={`${id}-format`}>
          <select
            id={`${id}-format`}
            className={controlClass}
            value={draft.format}
            onChange={(event) =>
              set({ format: event.target.value as DirwatchFormat })
            }
          >
            {FORMATS.map((choice) => (
              <option key={choice.value} value={choice.value}>
                {choice.label}
              </option>
            ))}
          </select>
        </Field>
        <Field label="What is it" htmlFor={`${id}-label`}>
          <input
            id={`${id}-label`}
            className={controlClass}
            placeholder="optional"
            value={draft.label}
            onChange={(event) => set({ label: event.target.value })}
          />
        </Field>
      </div>
      <Field label="Folder" htmlFor={`${id}-directory`}>
        <input
          id={`${id}-directory`}
          className={controlClass}
          value={draft.directory}
          onChange={(event) => set({ directory: event.target.value })}
        />
      </Field>
      <p className="font-mono text-[11px] text-muted-foreground">
        Point it at {format.folder}.
      </p>
      {fields.mask && (
        <Field label="Mask" htmlFor={`${id}-mask`}>
          <input
            id={`${id}-mask`}
            className={controlClass}
            placeholder="cymx_#TG_#DATE_#TIME_#HZ"
            value={draft.mask}
            onChange={(event) => set({ mask: event.target.value })}
          />
        </Field>
      )}
      {fields.mask && (
        <p className="font-mono text-[11px] text-muted-foreground">
          Tokens: {MASK_TOKENS}. #TIME is this server's wall clock; #ZTIME is
          UTC.
        </p>
      )}
      <div className="grid grid-cols-[1fr_1fr_1fr] gap-2">
        <Field label="System ref" htmlFor={`${id}-system`}>
          <input
            id={`${id}-system`}
            className={controlClass}
            inputMode="numeric"
            placeholder="from the files"
            value={draft.systemRef}
            onChange={(event) => set({ systemRef: event.target.value })}
          />
        </Field>
        {fields.talkgroup && (
          <Field label="Talkgroup ref" htmlFor={`${id}-talkgroup`}>
            <input
              id={`${id}-talkgroup`}
              className={controlClass}
              inputMode="numeric"
              placeholder="from the files"
              value={draft.talkgroupRef}
              onChange={(event) => set({ talkgroupRef: event.target.value })}
            />
          </Field>
        )}
        <Field label="Audio extension" htmlFor={`${id}-extension`}>
          <input
            id={`${id}-extension`}
            className={controlClass}
            placeholder={format.extension}
            value={draft.extension}
            onChange={(event) => set({ extension: event.target.value })}
          />
        </Field>
      </div>
      <div className="grid grid-cols-[1fr_1fr] gap-2">
        <Field label="Frequency (Hz) when a file has none" htmlFor={`${id}-frequency`}>
          <input
            id={`${id}-frequency`}
            className={controlClass}
            inputMode="numeric"
            value={draft.frequency}
            onChange={(event) => set({ frequency: event.target.value })}
          />
        </Field>
        <Field label="Wait after the last write (ms)" htmlFor={`${id}-delay`}>
          <input
            id={`${id}-delay`}
            className={controlClass}
            inputMode="numeric"
            value={draft.delayMs}
            onChange={(event) => set({ delayMs: event.target.value })}
          />
        </Field>
      </div>
      <label className="flex items-start gap-2 font-mono text-xs">
        <input
          type="checkbox"
          checked={draft.deleteAfter}
          onChange={(event) => set({ deleteAfter: event.target.checked })}
        />
        <span>
          Delete each file once it is ingested. Everything already in the
          folder is ingested and deleted too; a file that cannot be a call, or
          that fails to store, is never deleted.
        </span>
      </label>
      <label className="flex items-start gap-2 font-mono text-xs">
        <input
          type="checkbox"
          checked={draft.poll}
          onChange={(event) => set({ poll: event.target.checked })}
        />
        <span>
          Look every few seconds instead of waiting to be told — for a network
          share, which sends no notice of new files.
        </span>
      </label>
      <div className="flex justify-end">
        <Button type="submit" size="sm" disabled={busy}>
          {submit}
        </Button>
      </div>
      {problem !== undefined && (
        <p role="alert" className="font-mono text-xs text-red-400">
          {problem}
        </p>
      )}
      {error != null && <FailureNote error={error} />}
    </form>
  )
}
