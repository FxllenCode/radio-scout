import { fireEvent, screen, waitFor, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { http, HttpResponse } from 'msw'
import { beforeEach, describe, expect, it } from 'vitest'

import { LOG_EVENTS, ORIGIN, logPage } from '@/test/handlers'
import { server } from '@/test/setup'
import { renderWithProviders } from '@/test/utils'

import { LogsScreen } from './LogsScreen'

/** Whether the server thinks this browser is signed in, for the handlers
 *  below — flipped by a successful sign-in, the way a cookie would be. */
let signedIn = false

/** Every `/api/admin/logs` URL the screen has asked for. */
let asked: URL[] = []

beforeEach(() => {
  signedIn = false
  asked = []
  server.use(
    http.get(`${ORIGIN}/api/admin/session`, () =>
      signedIn
        ? HttpResponse.json({ csrf_token: 'a-csrf-token', expires_in_secs: 3600 })
        : new HttpResponse('admin session required\n', { status: 401 }),
    ),
    http.post(`${ORIGIN}/api/admin/login`, async ({ request }) => {
      const body = (await request.json()) as { password: string }
      if (body.password !== 'correct-horse') {
        return new HttpResponse('invalid password\n', { status: 401 })
      }
      signedIn = true
      return HttpResponse.json({
        csrf_token: 'a-csrf-token',
        expires_in_secs: 3600,
      })
    }),
    http.post(`${ORIGIN}/api/admin/logout`, () => {
      signedIn = false
      return new HttpResponse(null, { status: 204 })
    }),
    http.get(`${ORIGIN}/api/admin/logs`, ({ request }) => {
      const url = new URL(request.url)
      asked.push(url)
      if (!signedIn) {
        return new HttpResponse('admin session required\n', { status: 401 })
      }
      return HttpResponse.json(logPage(url))
    }),
  )
})

const renderScreen = () => renderWithProviders(<LogsScreen />)

/** Sign in the way an operator does. */
async function signIn(password = 'correct-horse') {
  await userEvent.type(
    await screen.findByLabelText(/admin password/i),
    password,
  )
  await userEvent.click(screen.getByRole('button', { name: /sign in/i }))
}

/** The messages the log list is showing, in order. */
async function shownMessages() {
  const list = await screen.findByRole('list', { name: /log events/i })
  return within(list)
    .getAllByRole('listitem')
    .map((item) => within(item).getByTestId('log-message').textContent)
}

/** The most recent query the screen made. */
const lastQuery = () => asked[asked.length - 1]

describe('the admin gate (#19, #30)', () => {
  it('asks for the password when there is no session', async () => {
    renderScreen()

    expect(await screen.findByLabelText(/admin password/i)).toBeInTheDocument()
    expect(screen.queryByRole('list', { name: /log events/i })).toBeNull()
  })

  it('shows the log once the password is accepted', async () => {
    renderScreen()

    await signIn()

    expect(await shownMessages()).toEqual(LOG_EVENTS.map((e) => e.message))
  })

  // rdio-scanner answers a locked-out address with the same 401 as a wrong
  // password, so an operator cannot tell "I mistyped it" from "I am locked
  // out" (#19 fixed the server half; this is the half they read).
  it('says so when the password is refused', async () => {
    renderScreen()

    await signIn('wrong')

    expect(await screen.findByRole('alert')).toHaveTextContent(
      /password was not accepted/i,
    )
    expect(screen.getByLabelText(/admin password/i)).toBeInTheDocument()
  })

  it('says how long to wait when the address is locked out', async () => {
    server.use(
      http.post(
        `${ORIGIN}/api/admin/login`,
        () =>
          new HttpResponse('too many attempts\n', {
            status: 429,
            headers: { 'retry-after': '900' },
          }),
      ),
    )
    renderScreen()

    await signIn('wrong')

    expect(await screen.findByRole('alert')).toHaveTextContent(/15 minutes/i)
  })

  it('goes straight to the log when the session is still live', async () => {
    signedIn = true
    renderScreen()

    expect(await shownMessages()).toEqual(LOG_EVENTS.map((e) => e.message))
    expect(screen.queryByLabelText(/admin password/i)).toBeNull()
  })

  it('signs out again', async () => {
    signedIn = true
    renderScreen()
    await shownMessages()

    await userEvent.click(screen.getByRole('button', { name: /sign out/i }))

    expect(await screen.findByLabelText(/admin password/i)).toBeInTheDocument()
  })
})

describe('the log itself (#30)', () => {
  beforeEach(() => {
    signedIn = true
  })

  it('shows each event with its level, time and message', async () => {
    renderScreen()

    const list = await screen.findByRole('list', { name: /log events/i })
    const first = within(list).getAllByRole('listitem')[0]

    expect(within(first).getByTestId('log-message')).toHaveTextContent(
      'ingest rejected',
    )
    expect(within(first).getByText('WARN')).toBeInTheDocument()
    expect(within(first).getByText(/2026-07-25 14:32:05/)).toBeInTheDocument()
  })

  // ADR-0011 rule 6: the variable half of a line is fields, not a sentence —
  // which is what makes it readable *and* searchable. rdio stores the sentence.
  it("shows an event's structured fields and its correlation ref", async () => {
    renderScreen()

    const list = await screen.findByRole('list', { name: /log events/i })
    const first = within(list).getAllByRole('listitem')[0]

    expect(within(first).getByText(/reason=duplicate/)).toBeInTheDocument()
    expect(within(first).getByText(/0123456789abcdef/)).toBeInTheDocument()
  })

  // Not every event has both halves: a line logged inside a request with no
  // fields of its own has a ref and nothing else, and a level this client does
  // not recognise still has to render as itself rather than vanish.
  it('renders an event with a ref but no fields, whatever its level', async () => {
    server.use(
      http.get(`${ORIGIN}/api/admin/logs`, () =>
        HttpResponse.json({
          results: [
            {
              id: 9,
              atMs: Date.parse('2026-07-25T14:32:05'),
              level: 'NOTICE',
              target: 'radio_scout::live',
              message: 'listener disconnected',
              requestId: 'abcdef0123456789',
            },
          ],
          count: 1,
          limit: 100,
          offset: 0,
          hasMore: false,
        }),
      ),
    )
    renderScreen()

    const list = await screen.findByRole('list', { name: /log events/i })
    const only = within(list).getAllByRole('listitem')[0]

    expect(within(only).getByText('NOTICE')).toBeInTheDocument()
    expect(within(only).getByText(/ref abcdef0123456789/)).toBeInTheDocument()
  })

  it('filters by level', async () => {
    renderScreen()
    await shownMessages()

    await userEvent.selectOptions(
      screen.getByLabelText(/level/i),
      'warn',
    )

    const louder = LOG_EVENTS.filter((event) => event.level !== 'INFO')
    await waitFor(() => expect(lastQuery().searchParams.get('level')).toBe('warn'))
    // ...and the narrowed page is what is actually on screen, not just what was
    // asked for.
    await waitFor(async () =>
      expect(await shownMessages()).toEqual(louder.map((e) => e.message)),
    )
  })

  it('filters by date range', async () => {
    renderScreen()
    await shownMessages()

    await userEvent.type(screen.getByLabelText(/^from$/i), '2026-07-25T14:31')
    await userEvent.type(screen.getByLabelText(/^to$/i), '2026-07-25T14:33')

    await waitFor(() => {
      expect(lastQuery().searchParams.get('after')).toBe(
        String(Date.parse('2026-07-25T14:31')),
      )
      expect(lastQuery().searchParams.get('before')).toBe(
        String(Date.parse('2026-07-25T14:33')),
      )
    })
    // The window holds exactly the one event inside it.
    await waitFor(async () =>
      expect(await shownMessages()).toEqual(['ingest rejected']),
    )
  })

  // The filters apply as they change, so submitting the form has nothing left
  // to do — and letting the browser do its default would reload the page an
  // operator is in the middle of reading.
  it('does not reload the page when the filter form is submitted', async () => {
    renderScreen()
    await shownMessages()

    const notCancelled = fireEvent.submit(
      screen.getByRole('search', { name: /log filters/i }),
    )

    expect(notCancelled).toBe(false)
    expect(await shownMessages()).toEqual(LOG_EVENTS.map((e) => e.message))
  })

  it('pages through a long log', async () => {
    // A log far longer than a page — which is the normal state of one, and the
    // only state where paging is reachable.
    server.use(
      http.get(`${ORIGIN}/api/admin/logs`, ({ request }) => {
        const url = new URL(request.url)
        asked.push(url)
        return HttpResponse.json({
          ...logPage(url),
          count: 500,
          hasMore: true,
        })
      }),
    )
    renderScreen()
    await shownMessages()
    const pageSize = lastQuery().searchParams.get('limit')

    await userEvent.click(screen.getByRole('button', { name: /next page/i }))

    await waitFor(() =>
      expect(lastQuery().searchParams.get('offset')).toBe(pageSize),
    )

    // ...and back again, which lands on the first page — where there is
    // nowhere further back to go, so the control says so. Asserted on the
    // screen rather than on a request: RTK Query already holds that page, and
    // serving it from cache instead of asking twice is the point of the cache.
    await userEvent.click(screen.getByRole('button', { name: /previous page/i }))

    await waitFor(() =>
      expect(
        screen.getByRole('button', { name: /previous page/i }),
      ).toBeDisabled(),
    )
  })

  it('says when nothing matches rather than showing an empty box', async () => {
    server.use(
      http.get(`${ORIGIN}/api/admin/logs`, () =>
        HttpResponse.json({
          results: [],
          count: 0,
          limit: 100,
          offset: 0,
          hasMore: false,
        }),
      ),
    )
    renderScreen()

    expect(await screen.findByText(/no log events/i)).toBeInTheDocument()
  })

  it('says so when the log cannot be read', async () => {
    server.use(
      http.get(`${ORIGIN}/api/admin/logs`, () =>
        new HttpResponse(null, { status: 500 }),
      ),
    )
    renderScreen()

    expect(await screen.findByRole('alert')).toHaveTextContent(
      /could not be read/i,
    )
  })
})
