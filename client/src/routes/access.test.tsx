/**
 * **Access codes**, end to end through the browser (#68, spec US 52).
 *
 * Three things are asserted here and nowhere else, because each of them is only
 * true of the whole app rather than of any one module:
 *
 * - an Instance that gates nothing is **exactly the app it was** — no control,
 *   no lock, and nothing on any request,
 * - a locked row is **drawn and cannot be switched on**, and its tap is the
 *   unlock, and
 * - the grant an unlock hands back **reaches the server on every kind of URL**:
 *   a `fetch`, an `<audio src>` and a download link, which are three different
 *   mechanisms and one query parameter.
 */
import { screen, waitFor, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { http, HttpResponse } from 'msw'
import { beforeEach, describe, expect, it } from 'vitest'

import { CATALOG, ORIGIN } from '@/test/handlers'
import { server } from '@/test/setup'
import { renderApp, renderWithProviders } from '@/test/utils'
import { makeStore, type AppStore } from '@/store/store'
import { selectGrant } from '@/store/access'
import type { Catalog } from '@/types'

import { TalkgroupsScreen } from './TalkgroupsScreen'

const GRANT = `rsg_${'a1b2c3d4'.repeat(4)}`

/** A store with storage of its own, so no test inherits another's grant. */
function browser(): AppStore {
  const map = new Map<string, string>()
  return makeStore({
    namespace: 'test',
    storage: {
      get length() {
        return map.size
      },
      clear: () => map.clear(),
      getItem: (key) => map.get(key) ?? null,
      key: (index) => [...map.keys()][index] ?? null,
      removeItem: (key) => void map.delete(key),
      setItem: (key, value) => void map.set(key, value),
    },
  })
}

/** The catalog an Instance that gates one channel serves: the row is still
 *  there, its activity is not, and `access.gating` is what draws the control. */
function gatedCatalog(over: Partial<Catalog['access']> = {}): Catalog {
  return {
    ...CATALOG,
    access: { gating: true, ...over },
    systems: CATALOG.systems.map((system) =>
      system.ref !== 100
        ? system
        : {
            ...system,
            talkgroups: system.talkgroups.map((talkgroup) =>
              talkgroup.ref === 2
                ? {
                    ref: 2,
                    label: 'Alpha Law',
                    groups: [],
                    locked: true,
                  }
                : talkgroup,
            ),
          },
    ),
  }
}

/** Every request path the app made, so "did the grant reach the server" is a
 *  question about what really went out rather than about a mock. */
function watchRequests(): string[] {
  const seen: string[] = []
  server.events.on('request:start', ({ request }) => {
    seen.push(new URL(request.url).pathname + new URL(request.url).search)
  })
  return seen
}

beforeEach(() => {
  server.events.removeAllListeners()
})

describe('an instance that gates nothing', () => {
  /** The acceptance criterion, from this side of the wire: no control, no lock,
   *  and — the half that matters — nothing added to a single request. */
  it('is exactly the app it was before this existed', async () => {
    const seen = watchRequests()
    renderWithProviders(<TalkgroupsScreen />, { store: browser() })

    await screen.findByRole('group', { name: 'Alpha' })

    expect(screen.queryByRole('button', { name: /locked/i })).toBeNull()
    expect(screen.queryByRole('button', { name: /unlocked/i })).toBeNull()
    expect(
      seen.filter((path) => path.includes('grant=')),
      'nothing to send, so nothing sent',
    ).toEqual([])
  })
})

describe('a locked channel', () => {
  beforeEach(() => {
    server.use(
      http.get(`${ORIGIN}/api/catalog`, () => HttpResponse.json(gatedCatalog())),
    )
  })

  /** The row stays — the Operator gated the channel, not the fact that it
   *  exists — and what changes is the control: there is no switch to turn on. */
  it('is drawn, and is not a switch', async () => {
    renderWithProviders(<TalkgroupsScreen />, { store: browser() })

    const alpha = within(await screen.findByRole('group', { name: 'Alpha' }))
    const locked = alpha.getByRole('button', {
      name: /Alpha Law is locked/i,
    })

    expect(locked).toBeTruthy()
    expect(
      alpha.queryByRole('switch', { name: /Alpha Law/i }),
      'a switch that turns on and then delivers nothing is the one thing a panel must not offer',
    ).toBeNull()
  })

  it('says how many are locked, above the list', async () => {
    renderWithProviders(<TalkgroupsScreen />, { store: browser() })

    expect(await screen.findByRole('button', { name: /1 locked/i })).toBeTruthy()
  })

  /** Tapping the row is the unlock, which is the only thing tapping it could
   *  usefully mean. */
  it('opens the unlock when its row is tapped', async () => {
    renderWithProviders(<TalkgroupsScreen />, { store: browser() })
    const alpha = within(await screen.findByRole('group', { name: 'Alpha' }))

    await userEvent.click(alpha.getByRole('button', { name: /Alpha Law is locked/i }))

    expect(await screen.findByRole('dialog', { name: /unlock channels/i })).toBeTruthy()
  })
})

describe('unlocking', () => {
  beforeEach(() => {
    server.use(
      http.get(`${ORIGIN}/api/catalog`, () => HttpResponse.json(gatedCatalog())),
    )
  })

  /** The code goes up in a **body** — never a query string, which is where the
   *  grant rides — and what comes back is what the browser then holds. */
  it('sends the code in a body and holds the grant it gets back', async () => {
    const posted: unknown[] = []
    server.use(
      http.post(`${ORIGIN}/api/unlock`, async ({ request }) => {
        posted.push(await request.json())
        return HttpResponse.json({ grant: GRANT, label: 'Fire Ops', scope: {} })
      }),
    )
    const store = browser()
    renderWithProviders(<TalkgroupsScreen />, { store })
    await userEvent.click(await screen.findByRole('button', { name: /1 locked/i }))

    await userEvent.type(screen.getByLabelText('Access code'), 'FIRE-2026-OPS')
    await userEvent.click(screen.getByRole('button', { name: /^unlock$/i }))

    await waitFor(() => expect(selectGrant(store.getState())).toBe(GRANT))
    expect(posted).toEqual([{ code: 'FIRE-2026-OPS' }])
    expect(
      screen.queryByRole('dialog', { name: /unlock channels/i }),
      'closed on success',
    ).toBeNull()
  })

  /** ...and from then on it is on **every** request, which is the property the
   *  base query wrapper exists for: an endpoint added later inherits it. */
  it('puts the grant on every request afterwards', async () => {
    server.use(
      http.post(`${ORIGIN}/api/unlock`, () =>
        HttpResponse.json({ grant: GRANT, scope: {} }),
      ),
    )
    const store = browser()
    renderWithProviders(<TalkgroupsScreen />, { store })
    await userEvent.click(await screen.findByRole('button', { name: /1 locked/i }))
    await userEvent.type(screen.getByLabelText('Access code'), 'FIRE-2026-OPS')

    const seen = watchRequests()
    await userEvent.click(screen.getByRole('button', { name: /^unlock$/i }))

    await waitFor(() =>
      expect(
        seen.some((path) => path.startsWith('/api/catalog') && path.includes(GRANT)),
      ).toBe(true),
    )
  })

  /** A refusal keeps the sheet open — the field still holds what was mistyped —
   *  and says which of the three endings it was. */
  it('reports a wrong code without closing the sheet', async () => {
    server.use(
      http.post(
        `${ORIGIN}/api/unlock`,
        () => new HttpResponse('invalid access code\n', { status: 401 }),
      ),
    )
    renderWithProviders(<TalkgroupsScreen />, { store: browser() })
    await userEvent.click(await screen.findByRole('button', { name: /1 locked/i }))

    await userEvent.type(screen.getByLabelText('Access code'), 'not-the-code')
    await userEvent.click(screen.getByRole('button', { name: /^unlock$/i }))

    expect(await screen.findByRole('alert')).toHaveTextContent(/doesn't work here/i)
    expect(screen.getByRole('dialog', { name: /unlock channels/i })).toBeTruthy()
  })

  /** **Nothing is sent when there is nothing to send**, and the disabled submit
   *  is the whole of what makes that true — it is the only submit affordance in
   *  the form, so Enter on an empty field implicitly submits nothing either. It
   *  matters because an empty unlock is not harmless: it would spend one of
   *  this address's attempts against the server's lockout. */
  it('does nothing when there is nothing to send', async () => {
    const seen = watchRequests()
    renderWithProviders(<TalkgroupsScreen />, { store: browser() })
    await userEvent.click(await screen.findByRole('button', { name: /1 locked/i }))

    await userEvent.type(screen.getByLabelText('Access code'), '{Enter}')

    expect(screen.queryByRole('alert')).toBeNull()
    expect(seen.filter((path) => path.startsWith('/api/unlock'))).toEqual([])
    expect(
      screen.getByRole('button', { name: /^unlock$/i }),
      'and the control stays refused until there is something to send',
    ).toBeDisabled()
  })

  it('tells an expired code apart from a wrong one', async () => {
    server.use(
      http.post(
        `${ORIGIN}/api/unlock`,
        () => new HttpResponse('access code expired\n', { status: 410 }),
      ),
    )
    renderWithProviders(<TalkgroupsScreen />, { store: browser() })
    await userEvent.click(await screen.findByRole('button', { name: /1 locked/i }))

    await userEvent.type(screen.getByLabelText('Access code'), 'FIRE-2026-OPS')
    await userEvent.click(screen.getByRole('button', { name: /^unlock$/i }))

    expect(await screen.findByRole('alert')).toHaveTextContent(/expired/i)
  })
})

describe('a grant that has stopped working', () => {
  /** The server **degrades** rather than refusing — every read answers with the
   *  open channels — so the app keeps working and this is the only place a
   *  Listener finds out why some channels went. */
  it('is let go of, and said once', async () => {
    server.use(
      http.get(`${ORIGIN}/api/catalog`, () =>
        HttpResponse.json(gatedCatalog({ stale: 'expired' })),
      ),
      http.post(`${ORIGIN}/api/unlock`, () =>
        HttpResponse.json({ grant: GRANT, scope: {} }),
      ),
    )
    const store = browser()
    renderApp('/talkgroups', store)
    await userEvent.click(await screen.findByRole('button', { name: /1 locked/i }))
    await userEvent.type(screen.getByLabelText('Access code'), 'FIRE-2026-OPS')
    await userEvent.click(screen.getByRole('button', { name: /^unlock$/i }))

    // Held for exactly as long as it takes the catalog to say otherwise.
    expect(await screen.findByText(/access code has expired/i)).toBeTruthy()
    await waitFor(() => expect(selectGrant(store.getState())).toBeUndefined())

    await userEvent.click(screen.getByRole('button', { name: /^ok$/i }))
    await waitFor(() =>
      expect(screen.queryByText(/access code has expired/i)).toBeNull(),
    )
  })
})
