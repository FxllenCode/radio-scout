import { useState } from 'react'

import { Button } from '@/components/ui/button'
import { controlClass } from '@/components/Field'
import { freezeNotice } from '@/lib/events'
import {
  useAddEventCallsMutation,
  useCreateEventMutation,
  useGetEventsQuery,
} from '@/store/api'

/** How many Events the picker offers. An Operator adding to one is almost
 *  always adding to something they made minutes ago, and the list is newest
 *  first — so this is a convenience, not a browser. The Events screen is where
 *  an older one is found. */
const RECENT_EVENTS = 50

/** The sentinel for "make a new one", which is the common case: an incident is
 *  usually curated once, from the search that found it. */
const NEW = ''

/**
 * Freezing a multi-selection into an **Event** (#67, spec US 38).
 *
 * The ticket's first criterion is "create/edit/delete Events from multi-selected
 * Calls", and this is the *create* half — it lives on the **Search screen**
 * because that is where an Operator is standing when they decide something
 * mattered, rather than on the Events screen, which is where they go afterwards
 * to rename it or let it go.
 *
 * **Two paths, one control.** A new Event and an existing one are the same
 * gesture with a different target, so they are one component: what changes is
 * which mutation runs, and both answer with the same document (the Event, plus
 * a report of what happened to the Calls named).
 *
 * **What it says afterwards is the report, not "done".** Freezing has four
 * endings and an Operator does different things about them — a Call that aged
 * out while they were reading the page is normal, and one whose audio would not
 * read means the object store is unwell and the incident they think they kept is
 * short a transmission. `freezeNotice` is silent when everything worked, so the
 * notice appearing at all means something is worth reading.
 */
export interface FreezeOutcome {
  /** What to say about it, or `null` when everything worked — see
   *  [`freezeNotice`]. */
  notice: string | null
  /** Whether anything really was frozen, which is what decides whether the
   *  selection goes. **A failure keeps it**: the selection is the only record of
   *  what an Operator picked, and clearing it would make a retry mean building
   *  it again by hand. */
  frozen: boolean
}

export function AddToEvent({
  callIds,
  onResult,
}: {
  callIds: number[]
  /**
   * What the freeze came to.
   *
   * **The screen renders the notice, not this component**, and that is not
   * tidiness: the selection going is what unmounts this, so a report rendered
   * here would vanish at the exact moment it was worth reading — which it did,
   * until a test noticed.
   */
  onResult: (outcome: FreezeOutcome) => void
}) {
  const events = useGetEventsQuery({ limit: RECENT_EVENTS, offset: 0 })
  const [create, creating] = useCreateEventMutation()
  const [add, adding] = useAddEventCallsMutation()
  const [target, setTarget] = useState<string>(NEW)
  const [name, setName] = useState('')

  const existing = events.data?.results ?? []
  const making = target === NEW
  const busy = creating.isLoading || adding.isLoading
  const ready = callIds.length > 0 && (!making || name.trim() !== '')

  async function submit() {
    try {
      const event = making
        ? await create({ name: name.trim(), callIds }).unwrap()
        : await add({ id: Number(target), callIds }).unwrap()
      setName('')
      onResult({ notice: freezeNotice(event.added), frozen: true })
    } catch {
      // The server's own sentence is not reachable here without a second
      // rendering of `curateError`, and the useful thing to say is the same
      // either way: nothing was kept, and the selection is still there.
      onResult({
        notice: 'Could not add those calls. Nothing was changed.',
        frozen: false,
      })
    }
  }

  return (
    <div className="mt-3 flex flex-col gap-2 rounded-xl border border-border bg-card px-3 py-2.5">
      <div className="flex flex-wrap items-center gap-2">
        <span className="font-mono text-xs text-muted-foreground">
          {callIds.length} selected
        </span>
        <label className="sr-only" htmlFor="add-to-event">
          Event
        </label>
        <select
          id="add-to-event"
          className={controlClass}
          value={target}
          onChange={(e) => setTarget(e.target.value)}
        >
          <option value={NEW}>New event…</option>
          {existing.map((event) => (
            <option key={event.id} value={String(event.id)}>
              {event.name}
            </option>
          ))}
        </select>
        {making && (
          <>
            <label className="sr-only" htmlFor="new-event-name">
              Event name
            </label>
            <input
              id="new-event-name"
              className={controlClass}
              value={name}
              placeholder="Name this incident"
              onChange={(e) => setName(e.target.value)}
            />
          </>
        )}
        <Button type="button" size="sm" disabled={!ready || busy} onClick={submit}>
          {busy ? 'Keeping…' : 'Keep in event'}
        </Button>
      </div>
      <p className="font-mono text-[10px] text-muted-foreground">
        Keeping copies each call&rsquo;s audio, so retention never takes it.
        Deleting the event is the only thing that gives those bytes back.
      </p>
    </div>
  )
}
