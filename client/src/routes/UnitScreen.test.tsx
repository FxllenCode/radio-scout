import { screen, waitFor, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { http, HttpResponse } from 'msw'
import { describe, expect, it } from 'vitest'

import { ARCHIVE, ORIGIN, UNIT_HISTORY, archivePage } from '@/test/handlers'
import { server } from '@/test/setup'
import { renderApp } from '@/test/utils'
import { enterPlaybackMode, next, selectCurrentCall } from '@/store/playback'
import { makeStore } from '@/store/store'

/** Every `/api/calls` URL this screen asked for — the one thing an assertion
 *  about "its Calls" has to look at, since the list is an ordinary search. */
function watchSearches(): URL[] {
  const asked: URL[] = []
  server.use(
    http.get(`${ORIGIN}/api/calls`, ({ request }) => {
      const url = new URL(request.url)
      asked.push(url)
      // The Calls this radio was heard on, which the real server filters and
      // this stands in for: `ARCHIVE`'s oldest is the one it keyed.
      return HttpResponse.json(
        archivePage(
          url,
          url.searchParams.has('unit')
            ? ARCHIVE.filter((call) => call.unitRef === Number(url.searchParams.get('unit')))
            : ARCHIVE,
        ),
      )
    }),
  )
  return asked
}

describe('one radio’s history (#47, spec US 44)', () => {
  it('says what the apparatus is, what it answers to, and how long it has been around', async () => {
    renderApp('/unit/100/1200')

    expect(
      await screen.findByRole('heading', { name: 'Engine 1' }),
    ).toBeInTheDocument()
    expect(screen.getByTestId('unit-calls')).toHaveTextContent('2')
    // The Ranges an operator wrote, written back the way they wrote them.
    expect(screen.getByTestId('unit-members')).toHaveTextContent(
      '1201-1299; 4471',
    )
    expect(screen.getByTestId('unit-first')).toHaveTextContent('14:00:00')
    expect(screen.getByTestId('unit-last')).toHaveTextContent('14:30:00')
  })

  /** Which channel a radio lives on is the question the view exists for, so the
   *  server orders this and the screen only draws it. */
  it('lists where it talks, busiest first', async () => {
    renderApp('/unit/100/1200')

    const talkgroups = await screen.findByRole('list', { name: 'Talkgroups used' })
    const rows = within(talkgroups).getAllByRole('listitem')
    expect(rows).toHaveLength(1)
    expect(rows[0]).toHaveTextContent('Alpha Fire')
    expect(rows[0]).toHaveTextContent('2')
  })

  /** The list is an ordinary archive search with `unit` set — the same request
   *  the Search screen makes — so it pages, plays and downloads identically and
   *  this screen invents nothing. */
  it('lists its Calls by asking the archive for them', async () => {
    const asked = watchSearches()
    renderApp('/unit/100/1200')

    const calls = await screen.findByRole('list', { name: 'Unit calls' })
    expect(within(calls).getAllByRole('listitem')).toHaveLength(1)
    expect(calls).toHaveTextContent('Alpha Fire')

    const search = asked.find((url) => url.searchParams.has('unit'))
    expect(search?.searchParams.get('unit')).toBe('1200')
    expect(search?.searchParams.get('system')).toBe('100')
  })

  it('plays one of them, and downloads it under the Call rather than its object key', async () => {
    const user = userEvent.setup()
    watchSearches()
    const { store } = renderApp('/unit/100/1200')

    const calls = await screen.findByRole('list', { name: 'Unit calls' })
    expect(within(calls).getByRole('link', { name: /^Download/ })).toHaveAttribute(
      'href',
      '/api/call/1/download',
    )
    await user.click(within(calls).getByRole('button', { name: /^Play/ }))

    await waitFor(() =>
      expect(selectCurrentCall(store.getState())?.id).toBe(1),
    )
  })

  /** "Star from any row" is a promise about *every* row (#66), and this is a
   *  list of Calls like any other — it is also where "which of this radio's
   *  calls was the one" gets answered. */
  it('stars one of them', async () => {
    const user = userEvent.setup()
    renderApp('/unit/100/1200')

    const calls = await screen.findByRole('list', { name: 'Unit calls' })
    const [first] = within(calls).getAllByRole('button', { name: /^Star / })
    await user.click(first)

    // Only the row that was tapped: a list control that marked its neighbours
    // would be a Listener keeping the wrong Call.
    const marked = await within(calls).findAllByRole('button', { name: /^Unstar / })
    expect(marked).toHaveLength(1)
    expect(marked[0]).toHaveAttribute('aria-pressed', 'true')
  })

  /** **A Run started here walks past the end of the page it started on.**
   *
   *  Every screen that starts a Run owes the page-ahead (#32) — the Run names
   *  the page it is about to need and somebody has to hand it over. Without it a
   *  Run plays to the fiftieth Call and stops, which looks exactly like the
   *  radio having gone quiet rather than like a bug. Asserting only that the
   *  first Call plays would pass either way, which is how this went unnoticed.
   *
   *  In **playback mode**, because that is the mode a walk happens in at all
   *  (docs/using.md: the live feed and playback are mutually exclusive, and a
   *  Call played over a live feed is a single interruption). The Unit screen
   *  starts a Run on exactly the terms a search row does.
   */
  it('keeps playing past the end of the first page of its Calls', async () => {
    const user = userEvent.setup()
    const store = makeStore()
    store.dispatch(enterPlaybackMode())
    const many = Array.from({ length: 51 }, (_, index) => ({
      ...ARCHIVE[2],
      id: 1000 + index,
      audioUrl: `/api/call/${1000 + index}/audio`,
    }))
    const asked: URL[] = []
    server.use(
      http.get(`${ORIGIN}/api/calls`, ({ request }) => {
        const url = new URL(request.url)
        asked.push(url)
        return HttpResponse.json(archivePage(url, many))
      }),
    )
    renderApp('/unit/100/1200', store)

    const calls = await screen.findByRole('list', { name: 'Unit calls' })
    const rows = within(calls).getAllByRole('listitem')
    // Two Calls left on the page — the point the page-ahead is armed.
    expect(asked.filter((url) => url.searchParams.get('offset') === '50')).toEqual([])
    await user.click(within(rows[48]).getByRole('button', { name: /^Play/ }))

    await waitFor(() =>
      expect(
        asked.filter((url) => url.searchParams.get('offset') === '50'),
      ).toHaveLength(1),
    )
    // ...and the page it asked for is for *this radio*, not the whole archive.
    const ahead = asked.find((url) => url.searchParams.get('offset') === '50')
    expect(ahead?.searchParams.get('unit')).toBe('1200')

    // Over the boundary: 49, 50, then off the end onto page two. Dispatched
    // rather than clicked because this screen carries no transport bar — the
    // event is the Call *ending*, which the audio element fires and jsdom does
    // not, and #54's mini-player is what will put a button on it.
    for (let step = 0; step < 2; step += 1) {
      store.dispatch(next())
    }
    await waitFor(() =>
      expect(selectCurrentCall(store.getState())?.id).toBe(many[50].id),
    )
  })

  /** A curated **Unit** that has never keyed is a history with nothing in it —
   *  a different fact from a mistyped Ref, and one an operator should be able to
   *  see rather than be 404'd about. */
  it('says a curated radio has been quiet rather than showing an empty page', async () => {
    server.use(
      http.get(`${ORIGIN}/api/unit/:system/:ref`, () =>
        HttpResponse.json({
          ...UNIT_HISTORY,
          callCount: 0,
          firstHeardMs: undefined,
          lastHeardMs: undefined,
          talkgroups: [],
        }),
      ),
      // ...and the archive agrees, because the two answers come from the same
      // rows on the real server.
      http.get(`${ORIGIN}/api/calls`, ({ request }) =>
        HttpResponse.json(archivePage(new URL(request.url), [])),
      ),
    )
    renderApp('/unit/100/1200')

    expect(
      await screen.findByText('This radio has not been heard yet.'),
    ).toBeInTheDocument()
    expect(screen.queryByRole('list', { name: 'Unit calls' })).toBeNull()
  })

  /** A radio nobody has curated *and* nobody has heard is a 404, and the screen
   *  says which radio it could not find — a blank page would look like a slow
   *  network. */
  it('says so when there is no such radio', async () => {
    renderApp('/unit/100/9999')

    expect(await screen.findByRole('alert')).toHaveTextContent(
      'No radio 9999 on system 100.',
    )
  })

  /** A radio with no Ranges shows a dash rather than an empty cell, because the
   *  stat line is a fixed set of questions and a missing answer reads as a
   *  broken row. */
  it('shows a dash for an apparatus that owns nothing else', async () => {
    server.use(
      http.get(`${ORIGIN}/api/unit/:system/:ref`, () =>
        HttpResponse.json({ ...UNIT_HISTORY, memberRefs: undefined }),
      ),
    )
    renderApp('/unit/100/1200')

    expect(await screen.findByTestId('unit-members')).toHaveTextContent('—')
  })

  /** Recorders may send no labels at all, so every row still has to be nameable
   *  from its Refs — the same rule the rest of the app follows (CONTEXT.md). */
  it('falls back to Refs when nobody has named the System or the Talkgroup', async () => {
    server.use(
      http.get(`${ORIGIN}/api/unit/:system/:ref`, () =>
        HttpResponse.json({
          ...UNIT_HISTORY,
          systemLabel: undefined,
          talkgroups: [{ ...UNIT_HISTORY.talkgroups[0], label: undefined }],
        }),
      ),
    )
    renderApp('/unit/100/1200')

    const summary = await screen.findByRole('region', { name: 'Unit summary' })
    expect(summary).toHaveTextContent('System 100 · unit 1200')
    const talkgroups = screen.getByRole('list', { name: 'Talkgroups used' })
    expect(talkgroups).toHaveTextContent('Talkgroup 1')
  })

  /** Nobody browses to a radio: it is always arrived at from a Call, so the way
   *  back has to be somewhere obvious. */
  it('offers a way back to the archive', async () => {
    renderApp('/unit/100/1200')

    expect(
      await screen.findByRole('link', { name: 'Back to search' }),
    ).toHaveAttribute('href', '/search')
  })
})
