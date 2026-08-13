import { useState } from 'react'

import { Button } from '@/components/ui/button'
import { controlClass, Field, FailureNote } from '@/components/admin/AdminUi'
import { splitList } from '@/lib/curate'
import { useMerge, type Merge } from '@/hooks/useMerge'
import { useGetMembersQuery } from '@/store/api'
import type { AdminTalkgroup, MergeReport, MovedRef } from '@/types'

/**
 * **Merge curation** — the Refs a channel answers to (#50, spec US 17).
 *
 * #45 built folding and unfolding and gave it one way in, a CSV cell. This is
 * the screen, and its whole shape is the confirmation step: a fold is the only
 * curation act that rewrites the *archive* rather than the configuration, so
 * nothing here is ever performed straight from a click.
 *
 * Every action — folding refs in, unfolding one out, and the bulk fold on the
 * selection bar — takes the same road: build a delta, `?dryRun` it, render what
 * came back, and only then send the identical delta for real. One road rather
 * than three, so a path that skipped the preview would have to be written on
 * purpose.
 *
 * The delta is why this is not a set editor. A form is submitted by a tab that
 * read the list some minutes ago, and inferring an unmerge from absence is the
 * rdio-scanner failure the whole curation surface exists to not repeat.
 */

/** The Refs one channel answers to, and the edits to them.
 *
 *  Mounted only when its row is open, which is what makes the extra read
 *  affordable: the Talkgroup listing deliberately does not carry member Refs,
 *  because that would be a query per row on a page of five hundred for a column
 *  the page cannot edit. */
export function MemberRefsEditor({ row }: { row: AdminTalkgroup }) {
  const members = useGetMembersQuery(row.id)
  const [typed, setTyped] = useState('')
  const merge = useMerge(row.id, () => setTyped(''))

  const held = members.data?.results ?? []

  return (
    <section
      aria-label="Member refs"
      className="mt-2 flex flex-col gap-2 border-t border-border pt-2"
    >
      <p className="font-mono text-[10px] uppercase tracking-wider text-muted-foreground">
        Also answers to
      </p>
      {held.length === 0 ? (
        <p className="font-mono text-xs text-muted-foreground">
          {members.isFetching ? 'Reading…' : 'Only its own ref.'}
        </p>
      ) : (
        <ul aria-label="Member refs" className="flex flex-col gap-1">
          {held.map((member) => (
            <li
              key={member.ref}
              className="flex items-center justify-between gap-2 font-mono text-xs"
            >
              <span>
                {member.ref}
                {member.label != null && ` · ${member.label}`}
              </span>
              <Button
                type="button"
                variant="outline"
                size="sm"
                onClick={() => merge.preview({ fold: [], unfold: [member.ref] })}
              >
                {`Unfold ${member.ref}`}
              </Button>
            </li>
          ))}
        </ul>
      )}

      <form
        aria-label={`Fold refs into ${row.label ?? row.ref}`}
        className="flex items-end gap-2"
        onSubmit={(event) => {
          event.preventDefault()
          const fold = splitList(typed)
            .map(Number)
            .filter((ref) => Number.isFinite(ref))
          if (fold.length > 0) merge.preview({ fold, unfold: [] })
        }}
      >
        <Field label="Refs to fold in" htmlFor={`tg-fold-${row.id}`}>
          <input
            id={`tg-fold-${row.id}`}
            className={controlClass}
            placeholder="8123, 8124"
            value={typed}
            onChange={(event) => setTyped(event.target.value)}
          />
        </Field>
        <Button type="submit" size="sm">
          Preview
        </Button>
      </form>

      <MergeConfirmation merge={merge} />
    </section>
  )
}

/** The preview, and the two buttons that answer it.
 *
 *  Rendered as a `group` rather than a dialog on purpose: a modal over a row
 *  that is already expanded hides the channel the Operator is deciding about,
 *  and on a phone it would cover the whole screen.
 *
 *  `into` names the surviving channel. The row editor leaves it off — the
 *  preview is already inside that row — but the bulk bar must pass it, because
 *  there the survivor was *chosen among the selection* and a confirmation that
 *  listed only what is arriving would never say where it is arriving. */
export function MergeConfirmation({
  merge,
  into,
}: {
  merge: Merge
  into?: AdminTalkgroup
}) {
  if (merge.error != null) return <FailureNote error={merge.error} />
  if (!merge.pending) return null
  const { report, apply } = merge.pending

  return (
    <div
      role="group"
      aria-label="Fold preview"
      className="flex flex-col gap-2 rounded-xl border border-border bg-card px-3 py-2"
    >
      {into && (
        <p className="font-mono text-xs">
          Into <strong>{into.label ?? `Talkgroup ${into.ref}`}</strong>:
        </p>
      )}
      <ul className="flex flex-col gap-1 font-mono text-xs">
        {report.moved.map((moved) => (
          <li key={`${moved.movement}-${moved.ref}`}>
            {describe(moved)}
            {moved.carried.length > 0 && (
              <span className="text-muted-foreground">
                {` — brings ${moved.carried.join(', ')} with it`}
              </span>
            )}
          </li>
        ))}
      </ul>
      <p aria-live="polite" className="font-mono text-xs text-muted-foreground">
        {summary(report)}
      </p>
      <div className="flex gap-2">
        <Button type="button" size="sm" onClick={apply}>
          {arriving(report) === 0
            ? `Unfold ${report.unfolded}`
            : `Fold ${arriving(report)}`}
        </Button>
        <Button type="button" variant="outline" size="sm" onClick={merge.cancel}>
          Cancel
        </Button>
      </div>
    </div>
  )
}

/** One Ref's line in the confirmation.
 *
 *  `recorded` gets its own sentence because the counts cannot express it: no
 *  channel was absorbed and no Call moved, which is exactly what a Ref selected
 *  from *another System* looks like from here. Saying "no channel" is what turns
 *  that from an ordinary-looking row into something an Operator notices. */
function describe({ ref, movement, label, calls }: MovedRef): string {
  const named = label != null ? `${ref} · ${label}` : String(ref)
  if (movement === 'recorded') return `${named} — no channel yet, just recorded`
  const moving = calls === 1 ? '1 call' : `${calls} calls`
  return movement === 'unfolded'
    ? `${named} — becomes its own channel again, taking ${moving}`
    : `${named} — folded in, bringing ${moving}`
}

/** Refs arriving — folded in or merely recorded. The button counts these rather
 *  than `folded`, because a delta of two Refs where only one names a channel is
 *  still two Refs the Operator asked for. */
function arriving(report: MergeReport): number {
  return report.moved.filter((moved) => moved.movement !== 'unfolded').length
}

/** The one sentence an Operator reads before committing. */
function summary(report: MergeReport): string {
  const calls =
    report.callsRepointed === 1 ? '1 call' : `${report.callsRepointed} calls`
  return `${calls} would move.`
}
