import { screen, waitFor, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { beforeEach, describe, expect, it } from 'vitest'

import { http, HttpResponse } from 'msw'

import { FakeInstance, curationHandlers } from '@/test/curation'
import { CATALOG, ORIGIN } from '@/test/handlers'
import { server } from '@/test/setup'
import { renderWithProviders } from '@/test/utils'

import { EventsScreen } from './EventsScreen'

let instance: FakeInstance

beforeEach(() => {
  instance = new FakeInstance()
})

function signedIn() {
  server.use(...curationHandlers(instance))
  return renderWithProviders(<EventsScreen />)
}

/** Open the one Event on screen and hand back its editor. */
async function opened() {
  await userEvent.click(await screen.findByRole('button', { name: 'Open' }))
  return screen.getByRole('listitem')
}

describe('the Events screen', () => {
  /** Every admin screen sits behind one session, so a browser with none gets the
   *  password form rather than an empty list it would read as "nothing here". */
  it('asks for the password', async () => {
    renderWithProviders(<EventsScreen />)

    expect(await screen.findByLabelText('Admin password')).toBeInTheDocument()
    expect(screen.queryByRole('list')).not.toBeInTheDocument()
  })

  /** **What an Event costs is on the row.** Every other number an Operator sees
   *  here describes something Retention will reclaim; these bytes are the one
   *  thing on the Instance no policy can take back, so the screen that makes
   *  them says how many. */
  it('says how many calls an event holds and what they cost', async () => {
    instance.event({ name: 'Mill Street fire', calls: 12, bytes: 4_200_000 })
    signedIn()

    expect(await screen.findByText('Mill Street fire')).toBeInTheDocument()
    expect(
      screen.getByText(/12 calls · 4\.2 MB kept/),
    ).toBeInTheDocument()
  })

  it('says so when there are none', async () => {
    signedIn()

    expect(
      await screen.findByText(/No events yet/),
    ).toBeInTheDocument()
  })

  /** Opening one lists the frozen calls — the incident, not a count of it. */
  it('lists an event’s frozen calls when it is opened', async () => {
    instance.event({ name: 'Mill Street fire' }, [
      { talkgroupLabel: 'Fire Dispatch' },
      { talkgroupLabel: 'Fireground 2' },
    ])
    signedIn()

    const row = await opened()

    const calls = await within(row).findByRole('list', {
      name: 'Calls in Mill Street fire',
    })
    expect(within(calls).getAllByRole('listitem')).toHaveLength(2)
    expect(within(calls).getByText(/Fireground 2/)).toBeInTheDocument()
  })

  it('renames one, and says what it sent', async () => {
    instance.event({ name: 'Untitled' })
    signedIn()
    const row = await opened()

    await userEvent.clear(within(row).getByLabelText('Name'))
    await userEvent.type(within(row).getByLabelText('Name'), 'The tornado')
    await userEvent.type(within(row).getByLabelText('Notes'), 'Second alarm')
    await userEvent.click(within(row).getByRole('button', { name: 'Save' }))

    await waitFor(() =>
      expect(instance.wrote.at(-1)).toMatchObject({
        method: 'PATCH',
        body: { name: 'The tornado', notes: 'Second alarm' },
      }),
    )
    expect(await screen.findByText('The tornado')).toBeInTheDocument()
  })

  /** A blank note is sent as `null` rather than `""`, because the server tells
   *  absent from null and only `null` clears a field (`curate::nullable`). */
  it('clears a note with null rather than an empty string', async () => {
    instance.event({ name: 'Noted', notes: 'was something' })
    signedIn()
    const row = await opened()

    await userEvent.clear(within(row).getByLabelText('Notes'))
    await userEvent.click(within(row).getByRole('button', { name: 'Save' }))

    await waitFor(() =>
      expect(instance.wrote.at(-1)).toMatchObject({ body: { notes: null } }),
    )
  })

  /** **Sharing is a toggle**, and the link is fetched only when an Operator asks
   *  to copy it — which is what keeps a credential out of a listing that renders
   *  every incident on the Instance. */
  it('shares an event and hands back the link on request', async () => {
    instance.event({ name: 'Shared' })
    signedIn()
    const row = await opened()

    // Nothing to copy until it is shared.
    expect(
      within(row).queryByRole('button', { name: 'Copy link' }),
    ).not.toBeInTheDocument()

    await userEvent.click(within(row).getByRole('button', { name: 'Share' }))
    await userEvent.click(
      await within(row).findByRole('button', { name: 'Copy link' }),
    )

    // **Absolute**, because what an Operator does with this is paste it
    // somewhere that is not this page — a relative path would be useless there.
    const link = await within(row).findByLabelText('Share link')
    expect((link as HTMLInputElement).value).toMatch(
      /^http:\/\/localhost\/e\?t=.+/,
    )
  })

  /** ...and turning it off is a **revoke**: the control goes back to offering to
   *  share, and the link this Instance would hand out next is a different one. */
  it('stops sharing', async () => {
    instance.event({ name: 'Revoked' })
    signedIn()
    const row = await opened()

    await userEvent.click(within(row).getByRole('button', { name: 'Share' }))
    await userEvent.click(
      await within(row).findByRole('button', { name: 'Stop sharing' }),
    )

    expect(
      await within(row).findByRole('button', { name: 'Share' }),
    ).toBeInTheDocument()
    await waitFor(() =>
      expect(instance.wrote.at(-1)).toMatchObject({ body: { shared: false } }),
    )
  })

  /** Both downloads are ordinary links, so a browser saves them rather than the
   *  screen holding a county's incident in memory to hand over. */
  it('offers both downloads', async () => {
    const event = instance.event({ name: 'Downloadable' }, [{}])
    signedIn()
    const row = await opened()

    expect(
      await within(row).findByRole('link', { name: 'Download zip' }),
    ).toHaveAttribute('href', `/api/admin/events/${event.id}/export?format=zip`)
    expect(
      within(row).getByRole('link', { name: 'Download audio' }),
    ).toHaveAttribute('href', `/api/admin/events/${event.id}/export?format=wav`)
  })

  /** **A control that is offered and then refused is a control that lies**
   *  (#64's rule). An empty event has nothing to export and the server says so
   *  with a 404, so the screen does not offer it — the same gate the public
   *  share page already kept, on the half that had not. */
  it('offers no download for an empty event', async () => {
    instance.event({ name: 'Empty' })
    signedIn()
    const row = await opened()

    expect(
      within(row).queryByRole('link', { name: 'Download zip' }),
    ).not.toBeInTheDocument()
  })

  /** ...and none at all on an instance that does not export. */
  it('offers no download when the instance does not export', async () => {
    instance.event({ name: 'Closed' }, [{}])
    server.use(...curationHandlers(instance))
    server.use(
      http.get(`${ORIGIN}/api/catalog`, () =>
        HttpResponse.json({
          ...CATALOG,
          export: { enabled: false, maxCalls: 1000 },
        }),
      ),
    )
    renderWithProviders(<EventsScreen />)
    const row = await opened()

    expect(
      within(row).queryByRole('link', { name: 'Download zip' }),
    ).not.toBeInTheDocument()
  })

  it('drops one call from an event', async () => {
    instance.event({ name: 'Trimmed' }, [
      { talkgroupLabel: 'Fire Dispatch' },
      { talkgroupLabel: 'Fireground 2' },
    ])
    signedIn()
    const row = await opened()
    const calls = await within(row).findByRole('list', {
      name: 'Calls in Trimmed',
    })

    await userEvent.click(
      within(calls).getAllByRole('button', { name: 'Remove' })[0],
    )

    await waitFor(() =>
      expect(
        within(
          screen.getByRole('list', { name: 'Calls in Trimmed' }),
        ).getAllByRole('listitem'),
      ).toHaveLength(1),
    )
  })

  /** **Deleting is the one thing here that destroys audio**, so it is confirmed
   *  in place — with the number, which the screen already knows. A second round
   *  trip to be told "this would take 12 calls" would be a refusal an Operator
   *  has to interpret. */
  it('confirms a delete with the count before it releases anything', async () => {
    instance.event({ name: 'Doomed', calls: 12 })
    signedIn()
    const row = await opened()

    await userEvent.click(
      within(row).getByRole('button', { name: 'Delete event' }),
    )

    expect(
      within(row).getByText(/Delete “Doomed” and release its 12 calls/),
    ).toBeInTheDocument()
    expect(instance.events).toHaveLength(1)

    await userEvent.click(within(row).getByRole('button', { name: 'Delete it' }))

    await waitFor(() => expect(instance.events).toHaveLength(0))
  })

  /** ...and backing out of it changes nothing, which is the half of a
   *  confirmation that has to work. */
  it('keeps an event when the confirmation is declined', async () => {
    instance.event({ name: 'Spared' })
    signedIn()
    const row = await opened()

    await userEvent.click(
      within(row).getByRole('button', { name: 'Delete event' }),
    )
    await userEvent.click(within(row).getByRole('button', { name: 'Keep it' }))

    expect(
      within(row).getByRole('button', { name: 'Delete event' }),
    ).toBeInTheDocument()
    expect(instance.events).toHaveLength(1)
    expect(instance.wrote).toHaveLength(0)
  })

  /** One call reads as one call, which is the sentence a delete confirmation
   *  shows most often — an Operator trimming an incident down to nothing. */
  it('does not pluralise a single call', async () => {
    instance.event({ name: 'Single', calls: 1 })
    signedIn()
    const row = await opened()

    await userEvent.click(
      within(row).getByRole('button', { name: 'Delete event' }),
    )

    expect(
      within(row).getByText(/release its 1 call\?/),
    ).toBeInTheDocument()
  })

  /** A share link is read-only and selects itself on focus, because what an
   *  Operator does with it is copy it — and the clipboard API is refused on an
   *  insecure origin, which a Pi on a LAN very often is. */
  it('offers the share link ready to copy', async () => {
    instance.event({ name: 'Copyable' })
    signedIn()
    const row = await opened()
    await userEvent.click(within(row).getByRole('button', { name: 'Share' }))
    await userEvent.click(
      await within(row).findByRole('button', { name: 'Copy link' }),
    )

    const link = (await within(row).findByLabelText(
      'Share link',
    )) as HTMLInputElement
    expect(link).toHaveAttribute('readonly')

    link.focus()
    expect(link.selectionStart).toBe(0)
    expect(link.selectionEnd).toBe(link.value.length)
  })

  /** Events accumulate — one per incident — so the listing pages, and the
   *  controls have to move the window they describe. */
  it('pages through the events', async () => {
    const asked: string[] = []
    server.use(...curationHandlers(instance))
    // Added after the curation handlers so this one is reached first, and
    // *before* the render so the first page already reports a `hasMore`.
    server.use(
      http.get(`${ORIGIN}/api/admin/events`, ({ request }) => {
        const url = new URL(request.url)
        const offset = Number(url.searchParams.get('offset') ?? 0)
        asked.push(String(offset))
        return HttpResponse.json({
          results: [instance.event({ name: `Page at ${offset}` })],
          count: 120,
          limit: 50,
          offset,
          hasMore: offset < 50,
        })
      }),
    )
    renderWithProviders(<EventsScreen />)

    // The control renders disabled while the first page is in flight, and a
    // click on a disabled button does nothing at all.
    const next = await screen.findByRole('button', { name: 'Next' })
    await waitFor(() => expect(next).toBeEnabled())
    await userEvent.click(next)

    expect(await screen.findByText('Page at 50')).toBeInTheDocument()
    expect(asked).toContain('50')
    // The window moved, so the control that could not go back now can.
    expect(screen.getByRole('button', { name: 'Previous' })).toBeEnabled()

    await userEvent.click(screen.getByRole('button', { name: 'Previous' }))

    // ...and back at the start it cannot again, which is the other handler.
    await waitFor(() =>
      expect(screen.getByRole('button', { name: 'Previous' })).toBeDisabled(),
    )
  })

  /** **A refusal is the server's own sentence, beside the control that caused
   *  it.** An Operator who cannot tell a refused write from one that silently
   *  did nothing is one who stops trusting the screen (#49's rule). */
  it('says why a rename was refused', async () => {
    instance.event({ name: 'Named' })
    signedIn()
    const row = await opened()

    await userEvent.clear(within(row).getByLabelText('Name'))
    await userEvent.click(within(row).getByRole('button', { name: 'Save' }))

    expect(await within(row).findByRole('alert')).toHaveTextContent(
      'name is required',
    )
  })

  it('says why a delete was refused', async () => {
    instance.event({ name: 'Vanished' })
    signedIn()
    const row = await opened()
    await userEvent.click(
      within(row).getByRole('button', { name: 'Delete event' }),
    )
    // Somebody else deleted it between the listing and the confirmation.
    instance.events = []

    await userEvent.click(within(row).getByRole('button', { name: 'Delete it' }))

    expect(await screen.findByRole('alert')).toHaveTextContent('no such event')
  })

  it('says why dropping a call was refused', async () => {
    instance.event({ name: 'Trimmed' }, [{ talkgroupLabel: 'Fire Dispatch' }])
    signedIn()
    const row = await opened()
    const calls = await within(row).findByRole('list', {
      name: 'Calls in Trimmed',
    })
    // ...and here it is the member that has gone.
    instance.eventCalls.set(instance.events[0].id, [])

    await userEvent.click(
      within(calls).getByRole('button', { name: 'Remove' }),
    )

    expect(await within(row).findByRole('alert')).toHaveTextContent(
      'no such call in this event',
    )
  })

  it('closes an opened event again', async () => {
    instance.event({ name: 'Closeable' })
    signedIn()
    const row = await opened()

    await userEvent.click(within(row).getByRole('button', { name: 'Close' }))

    expect(within(row).queryByLabelText('Name')).not.toBeInTheDocument()
  })
})
