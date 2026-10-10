import { useState } from 'react'

import {
  AdminGate,
  FailureNote,
  Field,
  Placeholder,
  SignOutButton,
  controlClass,
} from '@/components/admin/AdminUi'
import { Screen } from '@/components/layout/Screen'
import { Button } from '@/components/ui/button'
import { useAdminSession } from '@/hooks/useAdminSession'
import {
  useGetInstanceQuery,
  useGetReferenceEditorQuery,
  usePublishReferenceMutation,
  useUnpublishReferenceMutation,
  useUpdateInstanceMutation,
} from '@/store/api'
import type { DefaultPanelSort, Instance, ReferenceEditor, TimeFormat } from '@/types'

/**
 * Settings → Admin → Instance (#77, spec US 4, 57, 58) — what the Operator
 * decides about the Instance itself: its name, where Listeners go for help, the
 * defaults every app starts from, and the **Reference page**.
 *
 * Curation, and only here (2026-10-10): none of it is in `radio-scout.toml`,
 * so there is one place to change it and nothing to tell apart. What it is
 * changed *to* is what every Listener's app is drawn with from their next look
 * — the server holds it in memory and replaces it on the save.
 */
export function InstanceScreen() {
  return (
    <Screen title="Instance" status={<SignOutButton />}>
      <AdminGate>
        <p className="mt-3 px-1 font-mono text-[11px] text-muted-foreground">
          What your listeners see this scanner as, and how their app starts
          out. Each of them can still change the talkgroup order for themselves.
        </p>
        <Identity />
        <Reference />
      </AdminGate>
    </Screen>
  )
}

/** The settings, once they have been read — so the form opens on them. */
function Identity() {
  const signedIn = useAdminSession()
  const { data, isError } = useGetInstanceQuery(undefined, { skip: !signedIn })

  if (isError)
    return (
      <Placeholder role="alert">
        This instance's settings could not be read. Check that the server is
        reachable.
      </Placeholder>
    )
  if (!data) return <Placeholder>Reading…</Placeholder>
  return <IdentityForm saved={data} />
}

const TIME_FORMATS: { value: TimeFormat; label: string }[] = [
  { value: '24h', label: '24-hour (14:30)' },
  { value: '12h', label: '12-hour (2:30 PM)' },
]

const PANEL_SORTS: { value: DefaultPanelSort; label: string }[] = [
  { value: 'name', label: 'A–Z' },
  { value: 'ref', label: 'By talkgroup ID' },
  { value: 'active', label: 'Most active first' },
]

function IdentityForm({ saved }: { saved: Instance }) {
  const [name, setName] = useState(saved.name ?? '')
  const [helpUrl, setHelpUrl] = useState(saved.helpUrl ?? '')
  const [timeFormat, setTimeFormat] = useState(saved.timeFormat)
  const [panelSort, setPanelSort] = useState(saved.panelSort)
  const [playbackGoesLive, setPlaybackGoesLive] = useState(saved.playbackGoesLive)
  const [dimmerSecs, setDimmerSecs] = useState(String(saved.dimmerSecs))
  const [showListeners, setShowListeners] = useState(saved.showListeners)
  const [update, updating] = useUpdateInstanceMutation()

  return (
    <form
      aria-label="This instance"
      className="mt-4 flex flex-col gap-3 rounded-xl border border-border bg-card p-4"
      onSubmit={(event) => {
        event.preventDefault()
        void update({
          // Blank is a clear, which the server reads as `null`.
          name: name.trim() === '' ? null : name,
          helpUrl: helpUrl.trim() === '' ? null : helpUrl,
          timeFormat,
          panelSort,
          playbackGoesLive,
          dimmerSecs: Number(dimmerSecs) || 0,
          showListeners,
        })
      }}
    >
      <Field label="Name" htmlFor="instance-name">
        <input
          id="instance-name"
          className={controlClass}
          placeholder="Radio-Scout"
          maxLength={64}
          value={name}
          onChange={(event) => setName(event.target.value)}
        />
      </Field>
      <Field label="Help link" htmlFor="instance-help">
        <input
          id="instance-help"
          className={controlClass}
          placeholder="https://… or you@example.com"
          value={helpUrl}
          onChange={(event) => setHelpUrl(event.target.value)}
        />
      </Field>
      <div className="grid grid-cols-2 gap-3">
        <Field label="Time" htmlFor="instance-time">
          <select
            id="instance-time"
            className={controlClass}
            value={timeFormat}
            onChange={(event) => setTimeFormat(event.target.value as TimeFormat)}
          >
            {TIME_FORMATS.map((format) => (
              <option key={format.value} value={format.value}>
                {format.label}
              </option>
            ))}
          </select>
        </Field>
        <Field label="Talkgroup order for new listeners" htmlFor="instance-sort">
          <select
            id="instance-sort"
            className={controlClass}
            value={panelSort}
            onChange={(event) => setPanelSort(event.target.value as DefaultPanelSort)}
          >
            {PANEL_SORTS.map((sort) => (
              <option key={sort.value} value={sort.value}>
                {sort.label}
              </option>
            ))}
          </select>
        </Field>
      </div>
      <Field label="Dim the display after (seconds, 0 = never)" htmlFor="instance-dimmer">
        <input
          id="instance-dimmer"
          type="number"
          min={0}
          max={3600}
          className={controlClass}
          value={dimmerSecs}
          onChange={(event) => setDimmerSecs(event.target.value)}
        />
      </Field>
      <Check
        id="instance-goes-live"
        checked={playbackGoesLive}
        onChange={setPlaybackGoesLive}
        label="Go live when playback catches up"
        hint="Playing the archive forwards, reaching the newest call hands over to the live feed."
      />
      <Check
        id="instance-listeners"
        checked={showListeners}
        onChange={setShowListeners}
        label="Show how many are listening"
        hint="Your number to publish: listeners see it on the live screen."
      />
      <div className="flex items-center gap-3">
        <Button type="submit" size="sm" disabled={updating.isLoading}>
          Save
        </Button>
        {updating.isSuccess && (
          <span role="status" className="font-mono text-[11px] text-muted-foreground">
            Saved
          </span>
        )}
      </div>
      {updating.error != null && <FailureNote error={updating.error} />}
    </form>
  )
}

/** A yes-or-no setting, with the sentence that says what yes means. */
function Check({
  id,
  checked,
  onChange,
  label,
  hint,
}: {
  id: string
  checked: boolean
  onChange: (checked: boolean) => void
  label: string
  hint: string
}) {
  return (
    <div className="flex items-start gap-2">
      <input
        id={id}
        type="checkbox"
        className="mt-0.5"
        checked={checked}
        onChange={(event) => onChange(event.target.checked)}
      />
      <label htmlFor={id} className="flex flex-col text-sm">
        {label}
        <span className="font-mono text-[11px] text-muted-foreground">{hint}</span>
      </label>
    </div>
  )
}

/** The **Reference page**, once its editor has been read. */
function Reference() {
  const signedIn = useAdminSession()
  const { data, isError } = useGetReferenceEditorQuery(undefined, { skip: !signedIn })

  if (isError)
    return (
      <Placeholder role="alert">
        The reference page could not be read. Check that the server is reachable.
      </Placeholder>
    )
  if (!data) return <Placeholder>Reading…</Placeholder>
  return <ReferenceForm editor={data} />
}

/**
 * The page, in markdown. It opens on what is published, or on the seed — only
 * what is the same everywhere, and a table for the county's own codes — and
 * nothing reaches a Listener until it is published.
 */
function ReferenceForm({ editor }: { editor: ReferenceEditor }) {
  const [markdown, setMarkdown] = useState(editor.markdown ?? editor.seed)
  const [publish, publishing] = usePublishReferenceMutation()
  const [unpublish, unpublishing] = useUnpublishReferenceMutation()
  const published = editor.markdown !== undefined

  return (
    <form
      aria-label="Reference page"
      className="mt-4 flex flex-col gap-3 rounded-xl border border-border bg-card p-4"
      onSubmit={(event) => {
        event.preventDefault()
        void publish(markdown)
      }}
    >
      <div className="flex items-baseline justify-between gap-3">
        <h2 className="font-mono text-xs font-semibold uppercase tracking-wider">
          Reference page
        </h2>
        <span className="font-mono text-[11px] text-muted-foreground">
          {published ? 'Published' : 'Not published'}
        </span>
      </div>
      <p className="font-mono text-[11px] text-muted-foreground">
        Your county's codes, in markdown. Listeners find it under Settings once
        it is published. HTML is shown as text, not run.
      </p>
      <Field label="Markdown" htmlFor="reference-markdown">
        <textarea
          id="reference-markdown"
          rows={14}
          className={`${controlClass} h-auto`}
          value={markdown}
          onChange={(event) => setMarkdown(event.target.value)}
        />
      </Field>
      <div className="flex flex-wrap items-center gap-2">
        <Button type="submit" size="sm" disabled={publishing.isLoading}>
          Publish
        </Button>
        <Button
          type="button"
          variant="outline"
          size="sm"
          onClick={() => setMarkdown(editor.seed)}
        >
          Start from the template
        </Button>
        {published && (
          <Button
            type="button"
            variant="outline"
            size="sm"
            disabled={unpublishing.isLoading}
            onClick={() => void unpublish()}
          >
            Take down
          </Button>
        )}
      </div>
      {publishing.error != null && <FailureNote error={publishing.error} />}
      {unpublishing.error != null && <FailureNote error={unpublishing.error} />}
    </form>
  )
}
