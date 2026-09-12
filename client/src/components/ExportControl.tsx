/**
import { useGrant } from '@/hooks/useGrant'
 * Taking these results away (#65, spec US 33).
 *
 * One control on two screens — the Search screen's toolbar and the DVR's —
 * because both are already a scope and a time range, which is exactly what US
 * 33 asks to be able to export. It is the same component rather than two, for
 * `UnitLink`'s reason one feature along: two hand-written versions would be two
 * chances for one of them to send a window along with the filters, or to offer
 * a format the server does not write.
 *
 * **The download is a link, not a fetch.** The server answers
 * `Content-Disposition: attachment`, so a plain navigation saves the file under
 * the name the export chose and leaves the Listener where they were — where a
 * `fetch` would pull a gigabyte of audio into the tab's memory in order to hand
 * it straight back to the disk. That is the opposite trade from the
 * configuration document (#51), whose export is a `fetch` precisely because it
 * is small and can 401.
 */
import { Download } from 'lucide-react'
import { useState } from 'react'

import { Sheet } from '@/components/Sheet'
import { Button } from '@/components/ui/button'
import { useGrant } from '@/hooks/useGrant'
import { EXPORT_FORMATS, exportUrl, offerExport } from '@/lib/export'
import type { Catalog, SearchQuery } from '@/types'

export function ExportControl({
  search,
  count,
  catalog,
}: {
  /** The filters on screen — the export's filters, by construction. */
  search: SearchQuery
  /** How many Calls those filters matched, which the screen already knows. */
  count: number
  /** What this instance offers, or `undefined` until it has said. */
  catalog: Catalog | undefined
}) {
  // The **grant** goes on the link, never through the base query: a download
  // is a navigation (#68).
  const grant = useGrant()
  const [open, setOpen] = useState(false)
  const offer = offerExport(catalog, count)

  // Never drawn where the instance does not export — as opposed to drawn and
  // then refusing, which is the thing a catalog bit exists to prevent. One
  // guard, because [`offerExport`] answers "no control here" as `null` rather
  // than as a fourth field the screen would have to remember to read.
  if (!offer) return null

  return (
    <>
      <Button
        type="button"
        variant="outline"
        size="sm"
        aria-label="Export these results"
        className="h-7 px-2"
        onClick={() => setOpen(true)}
      >
        <Download className="size-3.5" aria-hidden />
      </Button>
      {open && (
        <Sheet title="Export" onClose={() => setOpen(false)}>
          {offer.offered ? (
            <>
              <p className="px-4 pb-3 font-mono text-[11px] text-muted-foreground">
                {offer.count.toLocaleString()} calls, oldest first.
              </p>
              <ul className="space-y-2 px-4 pb-4">
                {EXPORT_FORMATS.map((format) => (
                  <li key={format.id}>
                    <a
                      href={exportUrl(search, format.id, grant)}
                      className="flex flex-col gap-0.5 rounded-md border border-border bg-card px-3 py-2 hover:bg-accent"
                      onClick={() => setOpen(false)}
                    >
                      <span className="font-mono text-xs uppercase tracking-wider">
                        {format.label}
                      </span>
                      <span className="text-[11px] text-muted-foreground">
                        {format.detail}
                      </span>
                    </a>
                  </li>
                ))}
              </ul>
            </>
          ) : (
            <p className="px-4 pb-4 font-mono text-[11px] text-muted-foreground">
              {offer.why}
            </p>
          )}
        </Sheet>
      )}
    </>
  )
}
