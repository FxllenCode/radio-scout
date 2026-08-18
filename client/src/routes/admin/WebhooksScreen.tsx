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
import { NOTHING } from '@/lib/downstream'
import type { Selection } from '@/lib/selection'
import { formatName, looksPostable, markName, webhookHealthLine } from '@/lib/webhook'
import {
  useCreateWebhookMutation,
  useDeleteWebhookMutation,
  useGetWebhooksQuery,
  useUpdateWebhookMutation,
} from '@/store/api'
import {
  MARKS,
  WEBHOOK_FORMATS,
  type AdminWebhook,
  type Mark,
  type WebhookFormat,
} from '@/types'

/**
 * Settings → Admin → Webhooks (#54, spec US 21, 45).
 *
 * Where an Operator wires their own inbox: a URL that receives the Calls they
 * asked to hear about — one carrying an **Emergency** today — as JSON, or as a
 * message Discord renders.
 *
 * **This is not a notification surface** ([ADR-0014]). Nothing here asks a
 * Listener for permission, subscribes a device, or knows anything about anyone
 * but the Operator who is signed in. What a Recorder proves about a
 * transmission is a mark on a Call; this screen is about forwarding marked Calls
 * to an address its Operator chose.
 *
 * **The URL is a credential and the screen cannot show it back.** A Discord
 * webhook URL ends in a token, so it goes up once and the listing carries only
 * the host. Two consequences an Operator meets: the form validates the URL
 * *before* submitting, because that is the last moment they can see what they
 * typed; and editing a webhook leaves the URL field blank, where blank means
 * keep.
 */
export function WebhooksScreen() {
  const signedIn = useAdminSession()
  const listing = useGetWebhooksQuery(undefined, { skip: !signedIn })
  const [remove, removing] = useDeleteWebhookMutation()
  const [update] = useUpdateWebhookMutation()
  const [editing, setEditing] = useState<number | undefined>(undefined)

  const rows = listing.data?.results ?? []

  return (
    <Screen title="Webhooks" status={<SignOutButton />}>
      <AdminGate>
        <p className="mt-3 px-1 font-mono text-[11px] text-muted-foreground">
          Addresses that receive the calls you flag — an emergency, for now — as
          JSON or as a Discord message. Nothing is lost when one goes down:
          matching calls are queued and delivered in order once it comes back.
          The address is treated as a password: it is never shown again, never
          logged, and never exported.
        </p>

        <NewWebhookForm />

        {listing.isError ? (
          <Placeholder role="alert">
            The webhooks could not be read. Check that the server is reachable.
          </Placeholder>
        ) : rows.length === 0 ? (
          <Placeholder>
            {listing.isLoading ? 'Reading…' : 'No webhooks yet.'}
          </Placeholder>
        ) : (
          <RowList label="Webhooks">
            {rows.map((row) => (
              <RowCard
                key={row.id}
                title={row.label ?? row.host ?? `Webhook ${row.id}`}
                subtitle={webhookHealthLine(row)}
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
                  <WebhookForm row={row} onSaved={() => setEditing(undefined)} />
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

/** Which marks this webhook wants — checkboxes over the whole vocabulary.
 *
 *  Rendered from `MARKS` rather than hand-written, so #55's tone-out appears
 *  here by existing. */
function MarkPicker({
  id,
  marks,
  onChange,
}: {
  id: string
  marks: Mark[]
  onChange: (marks: Mark[]) => void
}) {
  return (
    <fieldset className="flex flex-col gap-1">
      <legend className="font-mono text-[11px] text-muted-foreground">
        Send a call when it carries
      </legend>
      {MARKS.map((mark) => (
        <label
          key={mark}
          className="flex items-center gap-2 font-mono text-[11px]"
          htmlFor={`${id}-mark-${mark}`}
        >
          <input
            id={`${id}-mark-${mark}`}
            type="checkbox"
            checked={marks.includes(mark)}
            onChange={(event) =>
              onChange(
                event.target.checked
                  ? [...marks, mark]
                  : marks.filter((held) => held !== mark),
              )
            }
          />
          {markName(mark)}
        </label>
      ))}
    </fieldset>
  )
}

/** Why an address was refused before it was ever sent.
 *
 *  One component rather than the same paragraph in both forms: the sentence is
 *  the whole of what an Operator gets — they cannot see what they pasted once it
 *  is submitted — so the two forms must not be able to drift into saying
 *  different things about the same rule. */
function BadAddressNote() {
  return (
    <p role="alert" className="font-mono text-[11px] text-destructive">
      That does not look like an address we can post to. It has to start with
      https:// (or http:// on your own network).
    </p>
  )
}

/** Which shape the body takes. */
function FormatPicker({
  id,
  format,
  onChange,
}: {
  id: string
  format: WebhookFormat
  onChange: (format: WebhookFormat) => void
}) {
  return (
    <Field label="Send it as" htmlFor={`${id}-format`}>
      <select
        id={`${id}-format`}
        className={controlClass}
        value={format}
        onChange={(event) => onChange(event.target.value as WebhookFormat)}
      >
        {WEBHOOK_FORMATS.map((choice) => (
          <option key={choice} value={choice}>
            {formatName(choice)}
          </option>
        ))}
      </select>
    </Field>
  )
}

/** Adding one: where it posts, in what shape, on what, and about which Calls. */
function NewWebhookForm() {
  const [create, creating] = useCreateWebhookMutation()
  const [label, setLabel] = useState('')
  const [url, setUrl] = useState('')
  const [format, setFormat] = useState<WebhookFormat>('radio-scout')
  // Ticked by default, because it is the only mark there is and a webhook
  // watching nothing is silently inert — the one misconfiguration on this
  // screen that produces no error anywhere.
  const [marks, setMarks] = useState<Mark[]>(['emergency'])
  const [scope, setScope] = useState<Selection>(NOTHING)
  // **Checked before submitting**, not after: this is the last moment the
  // Operator can see what they pasted.
  const [badUrl, setBadUrl] = useState(false)

  return (
    <form
      className="mt-3 flex flex-col gap-2 rounded-xl border border-border bg-card px-4 py-3"
      aria-label="Add a webhook"
      onSubmit={async (event) => {
        event.preventDefault()
        if (!looksPostable(url)) {
          setBadUrl(true)
          return
        }
        setBadUrl(false)
        try {
          await create({
            label: label.trim() === '' ? undefined : label.trim(),
            url: url.trim(),
            format,
            marks,
            scope,
          }).unwrap()
          setLabel('')
          setUrl('')
          setMarks(['emergency'])
          setScope(NOTHING)
        } catch {
          /* rendered below */
        }
      }}
    >
      <div className="grid grid-cols-[1fr_1fr] gap-2">
        <Field label="What is it" htmlFor="new-webhook-label">
          <input
            id="new-webhook-label"
            className={controlClass}
            placeholder="dispatch channel"
            value={label}
            onChange={(event) => setLabel(event.target.value)}
          />
        </Field>
        <FormatPicker id="new-webhook" format={format} onChange={setFormat} />
      </div>
      <Field
        label="Address (you will not be shown this again)"
        htmlFor="new-webhook-url"
      >
        <input
          id="new-webhook-url"
          // A credential, so it is masked while it is typed — and this is the
          // only moment it is ever on screen.
          type="password"
          className={controlClass}
          value={url}
          onChange={(event) => {
            setUrl(event.target.value)
            setBadUrl(false)
          }}
        />
      </Field>
      {badUrl && <BadAddressNote />}
      <MarkPicker id="new-webhook" marks={marks} onChange={setMarks} />
      <ScopeEditor id="new-webhook" scope={scope} onChange={setScope} />
      <div className="flex justify-end">
        <Button type="submit" size="sm" disabled={creating.isLoading}>
          Add
        </Button>
      </div>
      {creating.error != null && <FailureNote error={creating.error} />}
    </form>
  )
}

/** Editing one. The address field is blank and **absence leaves it alone**, so
 *  re-scoping does not mean going back to Discord for the URL. */
function WebhookForm({
  row,
  onSaved,
}: {
  row: AdminWebhook
  onSaved: () => void
}) {
  const [update, updating] = useUpdateWebhookMutation()
  const [label, setLabel] = useState(row.label ?? '')
  const [url, setUrl] = useState('')
  const [format, setFormat] = useState<WebhookFormat>(row.format)
  const [marks, setMarks] = useState<Mark[]>(row.marks)
  const [scope, setScope] = useState<Selection>(row.scope)
  const [badUrl, setBadUrl] = useState(false)

  return (
    <form
      aria-label={`Edit ${row.label ?? row.host ?? `webhook ${row.id}`}`}
      className="flex flex-col gap-2"
      onSubmit={async (event) => {
        event.preventDefault()
        // Only when one was typed: blank means keep, which is the whole reason
        // the field exists.
        if (url.trim() !== '' && !looksPostable(url)) {
          setBadUrl(true)
          return
        }
        setBadUrl(false)
        try {
          await update({
            id: row.id,
            patch: {
              label: label.trim() === '' ? null : label.trim(),
              ...(url.trim() === '' ? {} : { url: url.trim() }),
              format,
              marks,
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
        <Field label="What is it" htmlFor={`webhook-label-${row.id}`}>
          <input
            id={`webhook-label-${row.id}`}
            className={controlClass}
            value={label}
            onChange={(event) => setLabel(event.target.value)}
          />
        </Field>
        <FormatPicker
          id={`webhook-${row.id}`}
          format={format}
          onChange={setFormat}
        />
      </div>
      <Field
        label="Replace the address (leave blank to keep it)"
        htmlFor={`webhook-url-${row.id}`}
      >
        <input
          id={`webhook-url-${row.id}`}
          type="password"
          className={controlClass}
          value={url}
          onChange={(event) => {
            setUrl(event.target.value)
            setBadUrl(false)
          }}
        />
      </Field>
      {badUrl && <BadAddressNote />}
      <MarkPicker id={`webhook-${row.id}`} marks={marks} onChange={setMarks} />
      <ScopeEditor id={`webhook-${row.id}`} scope={scope} onChange={setScope} />
      <div className="flex justify-end">
        <Button type="submit" size="sm">
          Save
        </Button>
      </div>
      {updating.error != null && <FailureNote error={updating.error} />}
    </form>
  )
}
