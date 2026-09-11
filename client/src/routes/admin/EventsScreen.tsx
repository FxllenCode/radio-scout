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
import { formatCallTime, pageSummary } from '@/lib/archive'
import { callCategory, systemName, talkgroupName } from '@/lib/call'
import { eventExportUrl, eventSummary } from '@/lib/events'
import {
  useDeleteEventMutation,
  useGetCatalogQuery,
  useGetEventQuery,
  useGetEventsQuery,
  useLazyGetEventShareQuery,
  useRemoveEventCallMutation,
  useUpdateEventMutation,
} from '@/store/api'
import type { AdminEvent } from '@/types'

/** How many Events a screenful holds. */
const PAGE_SIZE = 50

/**
 * Settings → Admin → Events (#67, spec US 38) — incidents that outlive
 * **Retention**.
 *
 * **The one screen on this Instance that spends disk for good.** Every other
 * number an Operator sees here describes something Retention will eventually
 * reclaim; an Event's members are copies, deliberately outside every policy, so
 * each row says how many bytes it is holding and the delete says what it gives
 * back.
 *
 * **Assembling one happens somewhere else.** An Event is made *from* a
 * multi-selection on the Search screen, because that is where an Operator is
 * when they decide something mattered — this screen is where they rename it,
 * drop a Call that should not be in it, share it, download it, or let the whole
 * thing go.
 *
 * **Sharing is a toggle, and turning it off is a revoke.** It is the only revoke
 * there is here, so the link really dies and sharing again mints a different
 * one — the `SharesScreen` rule reached by a different road. The link is fetched
 * only when an Operator asks to copy it, which is what keeps a credential out of
 * a listing that renders every incident on the Instance.
 */
export function EventsScreen() {
  const signedIn = useAdminSession()
  const [offset, setOffset] = useState(0)
  const listing = useGetEventsQuery(
    { limit: PAGE_SIZE, offset },
    { skip: !signedIn },
  )
  const [opened, setOpened] = useState<number | undefined>(undefined)
  const [remove, removing] = useDeleteEventMutation()

  const rows = listing.data?.results ?? []

  return (
    <Screen title="Events" status={<SignOutButton />}>
      <AdminGate>
        <p className="mt-3 px-1 font-mono text-[11px] text-muted-foreground">
          A named incident, kept for good. Adding a call copies its audio, so
          retention never takes it — and nothing else ever gives those bytes
          back. Build one from the search screen; delete it here to release it.
        </p>

        {rows.length === 0 ? (
          <Placeholder>
            {listing.isLoading
              ? 'Loading…'
              : 'No events yet. Select calls on the search screen to make one.'}
          </Placeholder>
        ) : (
          <RowList label="Events">
            {rows.map((row) => (
              <EventRow
                key={row.id}
                event={row}
                open={opened === row.id}
                onToggle={() =>
                  setOpened(opened === row.id ? undefined : row.id)
                }
                onDelete={() => remove(row.id)}
              />
            ))}
          </RowList>
        )}
        {removing.error != null && <FailureNote error={removing.error} />}

        <div className="mt-4 flex items-center justify-between gap-2">
          <Button
            variant="outline"
            size="sm"
            disabled={offset === 0}
            onClick={() => setOffset(Math.max(0, offset - PAGE_SIZE))}
          >
            Previous
          </Button>
          <p
            aria-live="polite"
            className="font-mono text-xs text-muted-foreground"
          >
            {pageSummary(
              offset,
              rows.length,
              listing.data?.count ?? 0,
              'events',
            )}
          </p>
          <Button
            variant="outline"
            size="sm"
            disabled={!listing.data?.hasMore}
            onClick={() => setOffset(offset + PAGE_SIZE)}
          >
            Next
          </Button>
        </div>
      </AdminGate>
    </Screen>
  )
}

/** One Event: its counts, and — when opened — its members and everything an
 *  Operator does to it. */
function EventRow({
  event,
  open,
  onToggle,
  onDelete,
}: {
  event: AdminEvent
  open: boolean
  onToggle: () => void
  onDelete: () => void
}) {
  return (
    <RowCard
      title={event.name}
      subtitle={[
        eventSummary(event.calls, event.bytes),
        event.shared && 'shared',
        `made ${formatCallTime(event.createdAtMs)}`,
      ]
        .filter(Boolean)
        .join(' · ')}
      actions={
        <Button type="button" variant="outline" size="sm" onClick={onToggle}>
          {open ? 'Close' : 'Open'}
        </Button>
      }
    >
      {open && <EventEditor event={event} onDelete={onDelete} />}
    </RowCard>
  )
}

/** Everything an Operator does to one incident. */
function EventEditor({
  event,
  onDelete,
}: {
  event: AdminEvent
  onDelete: () => void
}) {
  const detail = useGetEventQuery(event.id)
  const { data: catalog } = useGetCatalogQuery()
  const [update, updating] = useUpdateEventMutation()
  const [dropCall, dropping] = useRemoveEventCallMutation()
  const [fetchShare, share] = useLazyGetEventShareQuery()
  const [name, setName] = useState(event.name)
  const [notes, setNotes] = useState(event.notes ?? '')
  // **Confirmed in the component, not by a second round trip.** Deleting is the
  // one thing here that destroys audio, and the screen already knows how much —
  // so the confirmation is the number, in place, rather than a server refusal an
  // Operator has to interpret (`curate::events`' own note).
  const [confirming, setConfirming] = useState(false)

  const members = detail.data?.members ?? []
  // An export needs somewhere to export from and an instance willing to serve
  // it. `catalog.export.enabled` is the same bit the Listener's own control
  // reads (`lib/export`), so the two cannot come to disagree.
  const downloadable = (catalog?.export.enabled ?? false) && event.calls > 0

  return (
    <div className="flex flex-col gap-3 pt-1">
      <Field label="Name" htmlFor={`event-name-${event.id}`}>
        <input
          id={`event-name-${event.id}`}
          className={controlClass}
          value={name}
          onChange={(e) => setName(e.target.value)}
        />
      </Field>
      <Field label="Notes" htmlFor={`event-notes-${event.id}`}>
        <input
          id={`event-notes-${event.id}`}
          className={controlClass}
          value={notes}
          placeholder="what happened"
          onChange={(e) => setNotes(e.target.value)}
        />
      </Field>
      <div className="flex flex-wrap gap-2">
        <Button
          type="button"
          size="sm"
          onClick={() =>
            update({
              id: event.id,
              patch: { name, notes: notes.trim() === '' ? null : notes },
            })
          }
        >
          Save
        </Button>
        <Button
          type="button"
          variant="outline"
          size="sm"
          onClick={() =>
            update({ id: event.id, patch: { shared: !event.shared } })
          }
        >
          {event.shared ? 'Stop sharing' : 'Share'}
        </Button>
        {event.shared && (
          <Button
            type="button"
            variant="outline"
            size="sm"
            onClick={() => void fetchShare(event.id)}
          >
            Copy link
          </Button>
        )}
        {/* **Gated exactly as the share page's are.** An instance with
            `[export] enabled = false` refuses these, and an empty event is
            refused too — and a control that is offered and then refused is a
            control that lies (#64's rule, which the public half already keeps
            and this half did not). */}
        {downloadable && (
          <>
            <Button asChild variant="outline" size="sm">
              <a href={eventExportUrl(event.id, 'zip')}>Download zip</a>
            </Button>
            <Button asChild variant="outline" size="sm">
              <a href={eventExportUrl(event.id, 'wav')}>Download audio</a>
            </Button>
          </>
        )}
      </div>

      {share.data?.url && (
        // The link itself rather than a copy-to-clipboard: it is the one thing
        // on this screen an Operator needs to get *out* of the browser, and a
        // readable input works where the clipboard API is refused (an insecure
        // origin, which a Pi on a LAN very often is).
        <Field label="Share link" htmlFor={`event-link-${event.id}`}>
          <input
            id={`event-link-${event.id}`}
            className={controlClass}
            readOnly
            value={new URL(share.data.url, window.location.origin).toString()}
            onFocus={(e) => e.currentTarget.select()}
          />
        </Field>
      )}

      <p className="font-mono text-[10px] uppercase tracking-wider text-muted-foreground">
        Calls
      </p>
      {members.length === 0 ? (
        <p className="font-mono text-xs text-muted-foreground">
          {detail.isLoading ? 'Loading…' : 'Nothing in this event yet.'}
        </p>
      ) : (
        <ul aria-label={`Calls in ${event.name}`} className="flex flex-col gap-1">
          {members.map((member) => (
            <li
              key={member.memberId}
              className="flex items-center justify-between gap-2"
            >
              <span className="min-w-0 truncate font-mono text-xs">
                {talkgroupName(member)} ·{' '}
                <span className="text-muted-foreground">
                  {[
                    systemName(member),
                    callCategory(member),
                    formatCallTime(member.timestamp),
                  ]
                    .filter(Boolean)
                    .join(' · ')}
                </span>
              </span>
              <Button
                type="button"
                variant="outline"
                size="sm"
                onClick={() =>
                  dropCall({ id: event.id, member: member.memberId })
                }
              >
                Remove
              </Button>
            </li>
          ))}
        </ul>
      )}

      {confirming ? (
        <div className="flex flex-wrap items-center gap-2">
          <span className="font-mono text-xs text-red-400">
            Delete “{event.name}” and release its {event.calls}{' '}
            {event.calls === 1 ? 'call' : 'calls'}? This cannot be undone.
          </span>
          <Button type="button" size="sm" onClick={onDelete}>
            Delete it
          </Button>
          <Button
            type="button"
            variant="outline"
            size="sm"
            onClick={() => setConfirming(false)}
          >
            Keep it
          </Button>
        </div>
      ) : (
        <Button
          type="button"
          variant="outline"
          size="sm"
          className="self-start"
          onClick={() => setConfirming(true)}
        >
          Delete event
        </Button>
      )}

      {updating.error != null && <FailureNote error={updating.error} />}
      {dropping.error != null && <FailureNote error={dropping.error} />}
    </div>
  )
}
