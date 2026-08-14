import { useRef, useState } from 'react'

import { FailureNote } from '@/components/admin/AdminUi'
import { Button } from '@/components/ui/button'
import {
  useImportConfigMutation,
  usePreviewConfigMutation,
} from '@/store/api'
import type { Applied, DocumentReport, IssuedApiKey } from '@/types'

/**
 * **Carrying the configuration** (#51, spec US 47) — export the curated setup as
 * one JSON file, and take one back.
 *
 * Two things shape this component, and both are the same idea from opposite
 * ends.
 *
 * **The export is a download, not a page.** What an Operator wants is a file
 * they can keep, diff and hand to another Instance — so the response is fetched
 * and saved under the name the server chose, rather than rendered into a `<pre>`
 * they would have to select and copy. The server's `Content-Disposition` is the
 * authority on that name, because the server is the side that knows the date.
 *
 * **The import is previewed.** It is the one admin action that touches every
 * entity at once, so it goes the way a fold does (#50): `?dryRun` the real
 * transaction, show what came back — the counts, the entries it would refuse,
 * each with its **path** into the file — and only then send the identical
 * document for real. A restore an Operator could not inspect first is a restore
 * they would not run.
 */
export function ConfigDocument() {
  const [preview, previewing] = usePreviewConfigMutation()
  const [apply, applying] = useImportConfigMutation()
  const [pending, setPending] = useState<unknown>(undefined)
  const [report, setReport] = useState<DocumentReport | undefined>(undefined)
  const [issued, setIssued] = useState<IssuedApiKey[]>([])
  const [unreadable, setUnreadable] = useState('')
  const picker = useRef<HTMLInputElement>(null)

  return (
    <section
      aria-label="Configuration document"
      className="mt-4 flex flex-col gap-2 rounded-xl border border-border bg-card px-4 py-3"
    >
      <p className="font-mono text-[10px] uppercase tracking-wider text-muted-foreground">
        Backup and restore
      </p>
      <p className="text-xs text-muted-foreground">
        Systems, talkgroups, merges, groups, tags, named units and the API-key
        roster — as one file. Keys come back re-issued: the file never carries a
        secret.
      </p>

      <div className="flex flex-wrap items-center gap-2">
        <Button
          type="button"
          size="sm"
          onClick={() =>
            void download().catch(() =>
              // The reason this is a `fetch` and not an `<a download>`: a
              // navigation that 401s leaves an Operator staring at a button
              // that did nothing.
              setUnreadable(
                'The configuration could not be downloaded. Your admin session may have expired.',
              ),
            )
          }
        >
          Export
        </Button>
        <Button
          type="button"
          variant="outline"
          size="sm"
          onClick={() => picker.current?.click()}
        >
          Choose a file…
        </Button>
        <input
          ref={picker}
          type="file"
          accept="application/json,.json"
          aria-label="Import a configuration document"
          className="sr-only"
          onChange={async (event) => {
            const file = event.target.files?.item(0)
            // Cleared so picking the same file twice fires again — a browser
            // will not re-emit `change` for an unchanged value, and re-trying a
            // restore after a rejection is exactly what an Operator does next.
            event.target.value = ''
            if (!file) return
            setUnreadable('')
            setIssued([])
            let document: unknown
            try {
              document = JSON.parse(await file.text())
            } catch {
              // The browser can tell, so it says so — a refusal an Operator has
              // to interpret is worse than a sentence about the file they
              // picked.
              setUnreadable(
                'That is not a configuration document — it is not even JSON. Pick the file an export produced.',
              )
              return
            }
            setPending(document)
            preview(document)
              .unwrap()
              .then(setReport)
              .catch(() => setReport(undefined))
          }}
        />
      </div>

      {unreadable !== '' && (
        <p role="alert" className="font-mono text-xs text-red-400">
          {unreadable}
        </p>
      )}
      {previewing.error != null && <FailureNote error={previewing.error} />}
      {applying.error != null && <FailureNote error={applying.error} />}

      {report && (
        <div
          role="group"
          aria-label="Import preview"
          className="flex flex-col gap-2 rounded-lg border border-border px-3 py-2"
        >
          <ul className="flex flex-col gap-0.5 font-mono text-xs">
            <li>{counted(report.systems, 'system', 'systems')}</li>
            <li>{counted(report.talkgroups, 'talkgroup', 'talkgroups')}</li>
            <li>{counted(report.units, 'unit', 'units')}</li>
            <li>
              {`${report.groupsCreated} new groups, ${report.tagsCreated} new tags`}
            </li>
            {report.apiKeysToIssue > 0 && (
              <li>
                {report.apiKeysToIssue === 1
                  ? '1 API key re-issued'
                  : `${report.apiKeysToIssue} API keys re-issued`}
              </li>
            )}
            {/* A document carries a peer's shape and never the key that peer
                issued us (#52), so a restored peer arrives switched off. Said
                here rather than discovered on the Downstreams screen: an
                Operator restoring a county wants to know how many credentials
                they are about to have to go and find. */}
            {report.downstreamsToKey > 0 && (
              <li>
                {report.downstreamsToKey === 1
                  ? '1 downstream restored, disabled until you give it its key'
                  : `${report.downstreamsToKey} downstreams restored, disabled until you give them their keys`}
              </li>
            )}
          </ul>
          {report.rejected.length > 0 && (
            <ul className="flex flex-col gap-0.5 font-mono text-xs text-red-400">
              {report.rejected.map((entry) => (
                <li key={`${entry.at}-${entry.reason}`}>
                  <span className="font-semibold">{entry.at}</span>
                  {` — ${entry.detail}`}
                </li>
              ))}
            </ul>
          )}
          <p className="font-mono text-[11px] text-muted-foreground">
            Nothing already here is deleted — a document only adds and updates
            what it names.
          </p>
          <div className="flex gap-2">
            <Button
              type="button"
              size="sm"
              disabled={applying.isLoading}
              onClick={() =>
                void apply(pending)
                  .unwrap()
                  .then((done: DocumentReport) => {
                    setIssued(done.apiKeys)
                    setReport(undefined)
                  })
                  .catch(() => setReport(undefined))
              }
            >
              Import
            </Button>
            <Button
              type="button"
              variant="outline"
              size="sm"
              onClick={() => setReport(undefined)}
            >
              Cancel
            </Button>
          </div>
        </div>
      )}

      {issued.length > 0 && (
        <div
          role="group"
          aria-label="Re-issued keys"
          className="flex flex-col gap-1 rounded-lg border border-border px-3 py-2"
        >
          <p className="font-mono text-[11px] text-muted-foreground">
            Copy these now — nothing can show them again. Each recorder needs its
            new key.
          </p>
          {issued.map((key) => (
            <p key={key.id} className="font-mono text-xs break-all">
              {`${key.label ?? 'unlabelled'} · ${key.key}`}
            </p>
          ))}
        </div>
      )}
    </section>
  )
}

/** `1 system · 2 talkgroups` — what an import would do to one kind of row. */
function counted(applied: Applied, one: string, many: string): string {
  const total = applied.created + applied.updated + applied.unchanged
  const noun = total === 1 ? one : many
  return `${total} ${noun}: ${applied.created} new, ${applied.updated} changed, ${applied.unchanged} already right`
}

/** Fetch the document and save it under the name the server chose.
 *
 *  Deliberately not an `<a download href="/api/admin/config">`: that would be a
 *  plain navigation with no session-expiry handling and no way to surface a
 *  failure, and the browser would name the file after the URL's last segment
 *  (`config`) rather than after the date the server put in the header. */
async function download(): Promise<void> {
  const response = await fetch('/api/admin/config')
  if (!response.ok) throw new Error(String(response.status))
  const blob = await response.blob()

  const url = URL.createObjectURL(blob)
  const anchor = document.createElement('a')
  anchor.href = url
  anchor.download = filenameFrom(
    response.headers.get('content-disposition'),
    'radio-scout-config.json',
  )
  document.body.appendChild(anchor)
  anchor.click()
  document.body.removeChild(anchor)
  URL.revokeObjectURL(url)
}

/** The filename out of a `Content-Disposition`, or the fallback. */
function filenameFrom(
  header: string | null,
  fallback: string,
): string {
  const quoted = /filename="([^"]+)"/.exec(header ?? '')
  return quoted?.[1] ?? fallback
}
