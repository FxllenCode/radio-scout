/**
 * Building an **Event** out of a multi-selection (#67, spec US 38) — the
 * ticket's first criterion, driven through the Search screen it lives on.
 *
 * It is here rather than beside the Events screen because the *create* half is
 * a search-screen gesture: an Operator picks the transmissions out of the
 * results they are already looking at. What happens to the incident afterwards
 * — renaming it, sharing it, letting it go — is `admin/events.test.tsx`.
 */
import { screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { http, HttpResponse } from 'msw'
import { beforeEach, describe, expect, it } from 'vitest'

import { ARCHIVE, ORIGIN, archivePage } from '@/test/handlers'
import { FakeInstance, curationHandlers } from '@/test/curation'
import { server } from '@/test/setup'
import { renderApp } from '@/test/utils'

let instance: FakeInstance

beforeEach(() => {
  instance = new FakeInstance()
  server.use(
    http.get(`${ORIGIN}/api/calls`, ({ request }) =>
      HttpResponse.json(archivePage(new URL(request.url))),
    ),
  )
})

/** The Search screen, with an Operator signed in — which is the only state in
 *  which any of this is visible. */
function asOperator() {
  server.use(...curationHandlers(instance))
  return renderApp('/search')
}

/** Choose an existing Event from the picker — waiting for the roster to arrive,
 *  because until it does the only option is "New event…". */
async function choose(id: number) {
  const picker = (await screen.findByLabelText('Event')) as HTMLSelectElement
  await waitFor(() =>
    expect(picker.querySelector(`option[value="${id}"]`)).not.toBeNull(),
  )
  await userEvent.selectOptions(picker, String(id))
}

/** Tick the checkbox on the row for `call`. */
async function pick(index: number) {
  const boxes = await screen.findAllByRole('checkbox', {
    name: /Select .* for an event/,
  })
  await userEvent.click(boxes[index])
}

describe('keeping search results in an event', () => {
  /**
   * **A Listener never sees the control at all.**
   *
   * Freezing copies audio that **Retention** can never reclaim, so it takes the
   * admin session — unlike a **Star** or a **Share link**, each of which is
   * bounded by something. Absent rather than disabled, so a signed-out Listener
   * gets the screen exactly as it was before this shipped.
   */
  it('offers no selection to a signed-out listener', async () => {
    renderApp('/search')

    await screen.findByRole('list', { name: 'Search results' })
    expect(screen.queryAllByRole('checkbox')).toHaveLength(0)
  })

  it('offers one to a signed-in operator', async () => {
    asOperator()

    expect(
      await screen.findAllByRole('checkbox', {
        name: /Select .* for an event/,
      }),
    ).toHaveLength(ARCHIVE.length)
  })

  /** Nothing is offered until something is picked, so the bar is not furniture
   *  above every search an Operator runs. */
  it('offers nothing until a call is picked', async () => {
    asOperator()
    await screen.findByRole('list', { name: 'Search results' })

    expect(
      screen.queryByRole('button', { name: 'Keep in event' }),
    ).not.toBeInTheDocument()

    await pick(0)

    expect(
      await screen.findByRole('button', { name: 'Keep in event' }),
    ).toBeInTheDocument()
    expect(screen.getByText('1 selected')).toBeInTheDocument()
  })

  /** Ticking twice unticks, which is the half of a checkbox that is easy to get
   *  wrong when the state is a list rather than a set. */
  it('unpicks a call that is picked again', async () => {
    asOperator()
    await pick(0)
    await pick(1)
    expect(screen.getByText('2 selected')).toBeInTheDocument()

    await pick(0)

    expect(screen.getByText('1 selected')).toBeInTheDocument()
  })

  /** **The headline.** A name and a selection become an incident, and what is
   *  sent is the ids that were ticked. */
  it('creates a named event out of the selection', async () => {
    asOperator()
    await pick(0)
    await pick(1)

    await userEvent.type(
      screen.getByLabelText('Event name'),
      'Mill Street fire',
    )
    await userEvent.click(screen.getByRole('button', { name: 'Keep in event' }))

    await waitFor(() =>
      expect(instance.wrote.at(-1)).toMatchObject({
        method: 'POST',
        path: '/api/admin/events',
        body: { name: 'Mill Street fire', callIds: [ARCHIVE[0].id, ARCHIVE[1].id] },
      }),
    )
  })

  /** ...and the selection goes once it has been kept, so the bar does not stand
   *  over a search claiming calls are still picked out. */
  it('drops the selection once the calls are frozen', async () => {
    asOperator()
    await pick(0)
    await userEvent.type(screen.getByLabelText('Event name'), 'Kept')
    await userEvent.click(screen.getByRole('button', { name: 'Keep in event' }))

    await waitFor(() =>
      expect(
        screen.queryByRole('button', { name: 'Keep in event' }),
      ).not.toBeInTheDocument(),
    )
  })

  /** An Event already on the Instance is the other path, and it is the same
   *  gesture with a different target. */
  it('adds the selection to an existing event', async () => {
    const event = instance.event({ name: 'The tornado' })
    asOperator()
    await pick(0)

    await choose(event.id)
    await userEvent.click(screen.getByRole('button', { name: 'Keep in event' }))

    await waitFor(() =>
      expect(instance.wrote.at(-1)).toMatchObject({
        method: 'POST',
        path: `/api/admin/events/${event.id}/calls`,
        body: { callIds: [ARCHIVE[0].id] },
      }),
    )
  })

  /** A new Event with no name cannot be submitted, because a nameless incident
   *  is one nobody will find again — and the server refuses it anyway, so the
   *  form says so before the round trip (`is_postable_url`'s rule). */
  it('will not make an event with no name', async () => {
    asOperator()
    await pick(0)

    expect(screen.getByRole('button', { name: 'Keep in event' })).toBeDisabled()
  })

  /** ...but an *existing* Event needs no name, so the button is live. */
  it('needs no name to add to an existing event', async () => {
    const event = instance.event({ name: 'The tornado' })
    asOperator()
    await pick(0)

    await choose(event.id)

    expect(
      screen.getByRole('button', { name: 'Keep in event' }),
    ).toBeEnabled()
  })

  /**
   * **What it says afterwards is the report, not "done".**
   *
   * A Call that aged out while an Operator was reading the page is normal and
   * worth saying; `freezeNotice` is silent when everything worked, so a notice
   * appearing at all means something is worth reading. The fake instance treats
   * a Call id of 900 or more as gone, which is how this drives that arm.
   */
  it('says what happened to calls that could not be frozen', async () => {
    const event = instance.event({ name: 'Partial' })
    asOperator()
    // Added *after* the curation handlers, so this one is reached first.
    server.use(
      http.post(`${ORIGIN}/api/admin/events/${event.id}/calls`, () =>
        HttpResponse.json({
          ...event,
          members: [],
          added: { frozen: 1, alreadyHeld: 0, missing: 2, unreadable: 0 },
        }),
      ),
    )
    await pick(0)

    await choose(event.id)
    await userEvent.click(screen.getByRole('button', { name: 'Keep in event' }))

    expect(await screen.findByRole('status')).toHaveTextContent(
      '1 call added. 2 calls are no longer in the archive.',
    )
  })

  /** A refused write says so and **keeps the selection**, because the selection
   *  is the only record of what an Operator picked and clearing it would make a
   *  retry mean building it again by hand. */
  it('keeps the selection when the write is refused', async () => {
    asOperator()
    server.use(
      http.post(`${ORIGIN}/api/admin/events`, () =>
        HttpResponse.json(
          { error: 'field-required', detail: 'name is required' },
          { status: 400 },
        ),
      ),
    )
    await pick(0)
    await userEvent.type(screen.getByLabelText('Event name'), 'Doomed')

    await userEvent.click(screen.getByRole('button', { name: 'Keep in event' }))

    expect(await screen.findByRole('status')).toHaveTextContent(
      /Nothing was changed/,
    )
    expect(screen.getByText('1 selected')).toBeInTheDocument()
  })

  /**
   * **Changing a filter drops the selection**, which is #49's rule one screen
   * along: a bulk action over rows that scrolled out of the answer is the one
   * thing multi-select must never do.
   */
  it('drops the selection when the search changes', async () => {
    asOperator()
    await pick(0)
    expect(screen.getByText('1 selected')).toBeInTheDocument()

    await userEvent.selectOptions(screen.getByLabelText('System'), '100')

    await waitFor(() =>
      expect(
        screen.queryByRole('button', { name: 'Keep in event' }),
      ).not.toBeInTheDocument(),
    )
  })

  /** ...and turning a page does **not**, because the search has not moved: the
   *  window is the screen's and the search is the Run's (#89's separation). */
  it('keeps the selection across a page turn', async () => {
    asOperator()
    await pick(0)

    await userEvent.click(
      await screen.findByRole('button', { name: 'Next page' }),
    )

    await waitFor(() =>
      expect(screen.getByText('1 selected')).toBeInTheDocument(),
    )
  })
})
