import { fireEvent, screen, waitFor, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { http, HttpResponse } from 'msw'
import { beforeEach, describe, expect, it, vi } from 'vitest'

import { FakeInstance, curationHandlers, refusal } from '@/test/curation'
import { ORIGIN } from '@/test/handlers'
import { server } from '@/test/setup'
import { renderWithProviders } from '@/test/utils'

import { AdminScreen } from './AdminScreen'
import { AdminTalkgroupsScreen } from './AdminTalkgroupsScreen'
import { ApiKeysScreen } from './ApiKeysScreen'
import { DownstreamsScreen } from './DownstreamsScreen'
import { GroupsScreen, TagsScreen } from './LabelsScreen'
import { ListenersScreen } from './ListenersScreen'
import { SystemsScreen } from './SystemsScreen'
import { UnitsScreen } from './UnitsScreen'
import { WebhooksScreen } from './WebhooksScreen'

/** The instance every test in this file drives. Rebuilt per test, so one test's
 *  edits are never another's starting state. */
let instance: FakeInstance

beforeEach(() => {
  instance = new FakeInstance()
})

/** Sign in and render, the ordinary case. */
function signedIn(ui: React.ReactElement) {
  server.use(...curationHandlers(instance))
  return renderWithProviders(ui)
}

/** What the server was asked to write, in order. */
function wrote() {
  return instance.wrote
}

// ---------------------------------------------------------------------------
// The gate
// ---------------------------------------------------------------------------

describe('the admin gate', () => {
  /** Every screen sits behind one session, so a browser with none is shown the
   *  password form rather than an empty list it would read as "nothing here".
   *
   *  Over every screen, because the gate being shared is the whole claim: one
   *  that forgot it would render its own list of nothing. */
  it.each([
    ['hub', () => <AdminScreen />],
    ['systems', () => <SystemsScreen />],
    ['talkgroups', () => <AdminTalkgroupsScreen />],
    ['groups', () => <GroupsScreen />],
    ['tags', () => <TagsScreen />],
    ['units', () => <UnitsScreen />],
    ['api keys', () => <ApiKeysScreen />],
    ['downstreams', () => <DownstreamsScreen />],
    ['webhooks', () => <WebhooksScreen />],
  ])('asks for the password on the %s screen', async (_name, ui) => {
    // The default handlers answer `/api/admin/session` with a 401.
    renderWithProviders(ui())

    expect(await screen.findByLabelText('Admin password')).toBeInTheDocument()
    expect(screen.queryByRole('list')).not.toBeInTheDocument()
  })

  /** ...and signing in reveals what was behind it, without a reload: the
   *  session query is invalidated by the login, which is what makes one form
   *  serve seven screens. */
  it('reveals the screen once the password is accepted', async () => {
    let open = false
    server.use(
      http.get(`${ORIGIN}/api/admin/session`, () =>
        open
          ? HttpResponse.json({ csrf_token: 'csrf-abc', expires_in_secs: 3600 })
          : new HttpResponse('admin session required\n', { status: 401 }),
      ),
      http.post(`${ORIGIN}/api/admin/login`, () => {
        open = true
        return HttpResponse.json({ csrf_token: 'csrf-abc', expires_in_secs: 3600 })
      }),
      ...curationHandlers(instance),
    )
    renderWithProviders(<AdminScreen />)
    await userEvent.type(
      await screen.findByLabelText('Admin password'),
      'correct-horse',
    )

    await userEvent.click(screen.getByRole('button', { name: 'Sign in' }))

    expect(await screen.findByRole('link', { name: /Talkgroups/ })).toBeInTheDocument()
  })

  /** The hub reaches each screen, so "run the instance from a browser" is one
   *  place an Operator can start from rather than seven URLs. */
  it('links the hub to every entity', async () => {
    signedIn(<AdminScreen />)

    const links = await screen.findAllByRole('link')

    expect(links.map((link) => link.getAttribute('href'))).toEqual([
      '/settings/admin/talkgroups',
      '/settings/admin/systems',
      '/settings/admin/units',
      '/settings/admin/groups',
      '/settings/admin/tags',
      '/settings/admin/api-keys',
      '/settings/admin/downstreams',
      '/settings/admin/webhooks',
      '/settings/admin/listeners',
      '/settings/logs',
    ])
  })

  /** The CSRF token is attached to every write without a screen mentioning it —
   *  the client half of the server's prefix layer. A mutation added later
   *  inherits it; before this it was a header each call site remembered.  */
  it('echoes the session csrf token on a write and never on a read', async () => {
    const seen: { method: string; csrf: string | null }[] = []
    server.use(
      http.all(`${ORIGIN}/api/admin/*`, ({ request }) => {
        seen.push({
          method: request.method,
          csrf: request.headers.get('x-csrf-token'),
        })
        return undefined
      }),
      ...curationHandlers(instance),
    )
    renderWithProviders(<GroupsScreen />)
    await screen.findByLabelText('New group')

    await userEvent.type(screen.getByLabelText('New group'), 'Fire')
    await userEvent.click(screen.getByRole('button', { name: 'Add' }))
    await screen.findByRole('list', { name: 'Groups' })

    expect(seen.some((r) => r.method === 'POST' && r.csrf === 'csrf-abc')).toBe(true)
    expect(seen.filter((r) => r.method === 'GET').every((r) => r.csrf === null)).toBe(
      true,
    )
  })
})

// ---------------------------------------------------------------------------
// Groups and Tags
// ---------------------------------------------------------------------------

describe('groups and tags', () => {
  it('creates, renames and deletes a group', async () => {
    signedIn(<GroupsScreen />)
    await screen.findByLabelText('New group')

    await userEvent.type(screen.getByLabelText('New group'), 'Fire')
    await userEvent.click(screen.getByRole('button', { name: 'Add' }))

    const list = await screen.findByRole('list', { name: 'Groups' })
    expect(within(list).getByText('Fire')).toBeInTheDocument()
    // The box is emptied only on success, so an Operator adding several in a
    // row is not deleting the last one by hand each time.
    expect(screen.getByLabelText('New group')).toHaveValue('')

    await userEvent.click(screen.getByRole('button', { name: 'Rename' }))
    const field = screen.getByLabelText('Rename Fire')
    await userEvent.clear(field)
    await userEvent.type(field, 'Fire/EMS')
    await userEvent.click(screen.getByRole('button', { name: 'Save' }))

    expect(await screen.findByText('Fire/EMS')).toBeInTheDocument()
    // ...and the editor closes, so the row reads as saved.
    expect(screen.queryByLabelText('Rename Fire')).not.toBeInTheDocument()

    await userEvent.click(screen.getByRole('button', { name: 'Delete' }))

    await waitFor(() =>
      expect(screen.queryByText('Fire/EMS')).not.toBeInTheDocument(),
    )
  })

  /** Tags are the same screen bound to the other table — asserted rather than
   *  assumed, because "they behave identically" is exactly the claim that
   *  stops being true silently. */
  it('creates a tag through the same screen', async () => {
    signedIn(<TagsScreen />)
    await screen.findByLabelText('New tag')

    await userEvent.type(screen.getByLabelText('New tag'), 'Fire Dispatch')
    await userEvent.click(screen.getByRole('button', { name: 'Add' }))

    expect(
      within(await screen.findByRole('list', { name: 'Tags' })).getByText(
        'Fire Dispatch',
      ),
    ).toBeInTheDocument()
    expect(wrote()[0]).toEqual({
      method: 'POST',
      path: '/api/admin/tags',
      body: { name: 'Fire Dispatch' },
    })
  })

  /** **The acceptance criterion.** A refused write says why, in the server's own
   *  words, beside the form that caused it — and the value stays in the box so
   *  it can be corrected. */
  it('shows a refusal inline and keeps what was typed', async () => {
    instance.label('groups', { name: 'Fire' })
    signedIn(<GroupsScreen />)
    await screen.findByLabelText('New group')

    await userEvent.type(screen.getByLabelText('New group'), 'Fire')
    await userEvent.click(screen.getByRole('button', { name: 'Add' }))

    expect(await screen.findByRole('alert')).toHaveTextContent(
      'the name "Fire" is already taken',
    )
    expect(screen.getByLabelText('New group')).toHaveValue('Fire')
  })

  /** A blank name is refused by the server naming the field, rather than posted
   *  and quietly turned into a group called nothing. */
  it('reports a blank name against its own field', async () => {
    signedIn(<GroupsScreen />)
    await screen.findByLabelText('New group')

    await userEvent.type(screen.getByLabelText('New group'), '   ')
    await userEvent.click(screen.getByRole('button', { name: 'Add' }))

    expect(await screen.findByRole('alert')).toHaveTextContent('name is required')
  })

  /** The count is what makes a delete decidable: a Group holding 40 channels is
   *  not one to remove without meaning to. */
  it('says how many talkgroups are behind each row', async () => {
    instance.label('groups', { name: 'Fire', talkgroups: 40 })
    instance.label('groups', { name: 'Law', talkgroups: 1 })
    signedIn(<GroupsScreen />)

    const list = await screen.findByRole('list', { name: 'Groups' })

    expect(within(list).getByText('40 talkgroups')).toBeInTheDocument()
    // Singular, because "1 talkgroups" is the kind of thing an Operator reads
    // as a bug in the number.
    expect(within(list).getByText('1 talkgroup')).toBeInTheDocument()
  })

  it('says so when the list cannot be read', async () => {
    server.use(
      http.get(
        `${ORIGIN}/api/admin/groups`,
        () => new HttpResponse(null, { status: 500 }),
      ),
      ...curationHandlers(instance),
    )
    renderWithProviders(<GroupsScreen />)

    expect(await screen.findByRole('alert')).toHaveTextContent(
      'That list could not be read',
    )
  })

  it('says so when there are none yet', async () => {
    signedIn(<TagsScreen />)

    expect(await screen.findByText('No tags yet.')).toBeInTheDocument()
  })

  /** A refused delete is reported too — the row list is not the only thing that
   *  can fail, and a delete that silently did nothing is worse than one that
   *  says why. */
  it('reports a refused delete', async () => {
    instance.label('tags', { name: 'Fire' })
    server.use(
      http.delete(`${ORIGIN}/api/admin/tags/:id`, () =>
        refusal(404, 'tag-not-found', 'no such tag'),
      ),
      ...curationHandlers(instance),
    )
    renderWithProviders(<TagsScreen />)
    await screen.findByRole('list', { name: 'Tags' })

    await userEvent.click(screen.getByRole('button', { name: 'Delete' }))

    expect(await screen.findByRole('alert')).toHaveTextContent('no such tag')
  })

  /** A refused rename leaves the editor open with the value still in it. */
  it('reports a refused rename without closing the editor', async () => {
    instance.label('groups', { name: 'Fire' })
    instance.label('groups', { name: 'Law' })
    signedIn(<GroupsScreen />)
    await screen.findByRole('list', { name: 'Groups' })

    await userEvent.click(screen.getAllByRole('button', { name: 'Rename' })[1])
    const field = screen.getByLabelText('Rename Law')
    await userEvent.clear(field)
    await userEvent.type(field, 'Fire')
    await userEvent.click(screen.getByRole('button', { name: 'Save' }))

    expect(await screen.findByRole('alert')).toHaveTextContent('already taken')
    expect(screen.getByLabelText('Rename Law')).toBeInTheDocument()
  })

  /** Cancel closes the editor without writing anything. */
  it('closes an editor without saving', async () => {
    instance.label('groups', { name: 'Fire' })
    signedIn(<GroupsScreen />)
    await screen.findByRole('list', { name: 'Groups' })

    await userEvent.click(screen.getByRole('button', { name: 'Rename' }))
    await userEvent.click(screen.getByRole('button', { name: 'Cancel' }))

    expect(screen.queryByLabelText('Rename Fire')).not.toBeInTheDocument()
    expect(wrote()).toEqual([])
  })
})

// ---------------------------------------------------------------------------
// Systems
// ---------------------------------------------------------------------------

describe('systems', () => {
  it('creates a system, and a blank ref asks the server to mint one', async () => {
    signedIn(<SystemsScreen />)
    await screen.findByLabelText('New system')

    await userEvent.type(screen.getByLabelText('New system'), 'Fulton')
    await userEvent.click(screen.getByRole('button', { name: 'Add' }))

    await screen.findByRole('list', { name: 'Systems' })
    expect(wrote()[0].body).toEqual({ ref: undefined, label: 'Fulton' })
  })

  /** The three fields nothing else in Radio-Scout can set. `enhancement` is the
   *  one that needs three states: a checkbox cannot say **inherit**, which is
   *  what a nullable column exists for. */
  it('edits the label, auto-populate and the enhancement scope', async () => {
    const system = instance.system({ label: 'Fulton' })
    signedIn(<SystemsScreen />)
    await screen.findByRole('list', { name: 'Systems' })

    await userEvent.click(screen.getByRole('button', { name: 'Edit' }))
    await userEvent.click(
      screen.getByLabelText(/Discover new talkgroups and units/),
    )
    await userEvent.selectOptions(screen.getByLabelText('Enhancement'), 'off')
    await userEvent.click(screen.getByRole('button', { name: 'Save' }))

    await waitFor(() => expect(wrote()).toHaveLength(1))
    expect(wrote()[0]).toEqual({
      method: 'PATCH',
      path: `/api/admin/systems/${system.id}`,
      body: {
        ref: 11,
        label: 'Fulton',
        autoPopulate: true,
        enhancement: false,
        blacklist: [],
      },
    })
  })

  /** `null` is how a nullable field goes back to inheriting — the whole reason
   *  the wire tells an omitted field from an explicit null. */
  it('clears a label and returns enhancement to inheriting', async () => {
    instance.system({ label: 'Fulton', enhancement: true })
    signedIn(<SystemsScreen />)
    await screen.findByRole('list', { name: 'Systems' })
    await userEvent.click(screen.getByRole('button', { name: 'Edit' }))

    await userEvent.clear(screen.getByLabelText('Label'))
    await userEvent.selectOptions(screen.getByLabelText('Enhancement'), '')
    await userEvent.click(screen.getByRole('button', { name: 'Save' }))

    await waitFor(() => expect(wrote()).toHaveLength(1))
    expect(wrote()[0].body).toMatchObject({ label: null, enhancement: null })
  })

  /** The blacklist an Operator types is checked **before** it is sent, because
   *  this is the one field rdio drops silently: it stores the list as free text
   *  and ignores what will not parse, so a mistyped ref refuses nothing. */
  it('refuses a blacklist entry that is not a ref, and will not send it', async () => {
    instance.system({})
    signedIn(<SystemsScreen />)
    await screen.findByRole('list', { name: 'Systems' })
    await userEvent.click(screen.getByRole('button', { name: 'Edit' }))

    await userEvent.type(
      screen.getByLabelText('Blacklisted talkgroup refs'),
      '100, fire',
    )

    expect(screen.getByRole('alert')).toHaveTextContent(
      'Blacklist entries must be talkgroup refs',
    )
    expect(screen.getByRole('button', { name: 'Save' })).toBeDisabled()
    expect(wrote()).toEqual([])
  })

  it('sends a corrected blacklist as refs', async () => {
    instance.system({})
    signedIn(<SystemsScreen />)
    await screen.findByRole('list', { name: 'Systems' })
    await userEvent.click(screen.getByRole('button', { name: 'Edit' }))

    await userEvent.type(
      screen.getByLabelText('Blacklisted talkgroup refs'),
      '100, 200',
    )
    await userEvent.click(screen.getByRole('button', { name: 'Save' }))

    await waitFor(() => expect(wrote()).toHaveLength(1))
    expect(wrote()[0].body).toMatchObject({ blacklist: [100, 200] })
  })

  /** **The delete that keeps a night of archive.** Refused with a count, and the
   *  refusal itself carries the way to go through with it. */
  it('refuses to delete a system with calls, then forces it on purpose', async () => {
    const system = instance.system({ calls: 12 })
    signedIn(<SystemsScreen />)
    await screen.findByRole('list', { name: 'Systems' })

    await userEvent.click(screen.getByRole('button', { name: 'Delete' }))

    const alert = await screen.findByRole('alert')
    expect(alert).toHaveTextContent('still has 12 calls')
    expect(screen.getByRole('list', { name: 'Systems' })).toBeInTheDocument()

    await userEvent.click(
      screen.getByRole('button', { name: 'Delete it and its 12 calls' }),
    )

    await waitFor(() =>
      expect(screen.queryByRole('list', { name: 'Systems' })).not.toBeInTheDocument(),
    )
    expect(wrote().map((write) => write.path)).toEqual([
      `/api/admin/systems/${system.id}`,
      `/api/admin/systems/${system.id}?force=true`,
    ])
  })

  /** A collision is a 409 too, and must **not** offer to force: forcing would
   *  do nothing about a taken ref, so the button would be a lie. */
  it('does not offer to force a refusal force cannot answer', async () => {
    instance.system({ ref: 11 })
    server.use(
      http.delete(`${ORIGIN}/api/admin/systems/:id`, () =>
        refusal(409, 'system-ref-taken', 'another system already answers to 11'),
      ),
      ...curationHandlers(instance),
    )
    renderWithProviders(<SystemsScreen />)
    await screen.findByRole('list', { name: 'Systems' })

    await userEvent.click(screen.getByRole('button', { name: 'Delete' }))

    expect(await screen.findByRole('alert')).toHaveTextContent('already answers to 11')
    expect(screen.queryByRole('button', { name: /Delete it and/ })).toBeNull()
  })

  it('says so when there are no systems yet', async () => {
    signedIn(<SystemsScreen />)

    expect(
      await screen.findByText(/No systems yet/),
    ).toBeInTheDocument()
  })

  it('says so when the systems cannot be read', async () => {
    server.use(
      http.get(
        `${ORIGIN}/api/admin/systems`,
        () => new HttpResponse(null, { status: 500 }),
      ),
      ...curationHandlers(instance),
    )
    renderWithProviders(<SystemsScreen />)

    expect(await screen.findByRole('alert')).toHaveTextContent(
      'The systems could not be read',
    )
  })

  it('reports a refused create', async () => {
    instance.system({ ref: 11 })
    signedIn(<SystemsScreen />)
    await screen.findByRole('list', { name: 'Systems' })

    await userEvent.type(screen.getByLabelText('Ref'), '11')
    await userEvent.click(screen.getByRole('button', { name: 'Add' }))

    expect(await screen.findByRole('alert')).toHaveTextContent(
      'another system already answers to 11',
    )
  })

  /** The editor closes on a save and stays open on a refusal, so "did that
   *  work?" is answered by the screen rather than by trying again. */
  it('leaves the editor open when a save is refused', async () => {
    instance.system({})
    server.use(
      http.patch(`${ORIGIN}/api/admin/systems/:id`, () =>
        refusal(409, 'system-ref-taken', 'another system already answers to 11'),
      ),
      ...curationHandlers(instance),
    )
    renderWithProviders(<SystemsScreen />)
    await screen.findByRole('list', { name: 'Systems' })
    await userEvent.click(screen.getByRole('button', { name: 'Edit' }))

    await userEvent.click(screen.getByRole('button', { name: 'Save' }))

    expect(await screen.findByRole('alert')).toHaveTextContent('already answers to 11')
    expect(screen.getByLabelText('Label')).toBeInTheDocument()
  })

  /** Closing an editor by hand leaves the row alone. */
  it('closes an editor without saving', async () => {
    instance.system({})
    signedIn(<SystemsScreen />)
    await screen.findByRole('list', { name: 'Systems' })

    await userEvent.click(screen.getByRole('button', { name: 'Edit' }))
    await userEvent.click(screen.getByRole('button', { name: 'Close' }))

    expect(screen.queryByLabelText('Label')).not.toBeInTheDocument()
    expect(wrote()).toEqual([])
  })
})

// ---------------------------------------------------------------------------
// Talkgroups
// ---------------------------------------------------------------------------

describe('talkgroups', () => {
  /** Set the screen up with a System and three channels. */
  function county() {
    instance.system({ ref: 11, label: 'Fulton' })
    instance.talkgroup({ ref: 100, label: 'Fire Dispatch', tag: 'Fire' })
    instance.talkgroup({ ref: 101, label: 'Fire Tac 1', tag: 'Fire' })
    instance.talkgroup({ ref: 200, label: 'Police Dispatch', tag: 'Law' })
  }

  /** **Spec US 46.** Select rows, one action, one request — where rdio needs a
   *  `PUT` of the whole configuration document with those rows changed inside
   *  it. */
  it('assigns a group and a tag across selected rows in one request', async () => {
    county()
    signedIn(<AdminTalkgroupsScreen />)
    await screen.findByRole('list', { name: 'Talkgroups' })

    await userEvent.click(screen.getByLabelText('Select Fire Dispatch'))
    await userEvent.click(screen.getByLabelText('Select Fire Tac 1'))

    const bar = await screen.findByRole('form', { name: 'Bulk assignment' })
    expect(within(bar).getByText('2 selected')).toBeInTheDocument()
    await userEvent.type(screen.getByLabelText('Add groups'), 'Fire, Dispatch')
    await userEvent.type(screen.getByLabelText('Set tag'), 'Fire Dispatch')
    await userEvent.click(screen.getByRole('button', { name: 'Apply to 2' }))

    await waitFor(() => expect(wrote()).toHaveLength(1))
    expect(wrote()[0]).toEqual({
      method: 'POST',
      path: '/api/admin/talkgroups/assign',
      body: {
        ids: [instance.talkgroups[0].id, instance.talkgroups[1].id],
        addGroups: ['Fire', 'Dispatch'],
        removeGroups: [],
        tag: 'Fire Dispatch',
      },
    })
    // ...and the selection is released, so the next action starts clean.
    await waitFor(() =>
      expect(screen.queryByRole('form', { name: 'Bulk assignment' })).toBeNull(),
    )
  })

  /** A blank tag box has to keep meaning "leave each row's tag alone", so
   *  clearing them is an explicit action rather than an empty field. */
  it('clears the tags of selected rows only when asked to', async () => {
    county()
    signedIn(<AdminTalkgroupsScreen />)
    await screen.findByRole('list', { name: 'Talkgroups' })
    await userEvent.click(screen.getByLabelText('Select Fire Dispatch'))

    // First: nothing typed, so the request must not mention the tag at all.
    await userEvent.type(screen.getByLabelText('Remove groups'), 'Old')
    await userEvent.click(screen.getByRole('button', { name: 'Apply to 1' }))
    await waitFor(() => expect(wrote()).toHaveLength(1))
    expect(wrote()[0].body).not.toHaveProperty('tag')

    // Then: asked for explicitly, which is `null` on the wire.
    await userEvent.click(screen.getByLabelText('Select Fire Dispatch'))
    await userEvent.click(
      await screen.findByRole('button', { name: 'Clear their tags' }),
    )
    await userEvent.click(screen.getByRole('button', { name: 'Apply to 1' }))

    await waitFor(() => expect(wrote()).toHaveLength(2))
    expect(wrote()[1].body).toMatchObject({ tag: null })
  })

  /** A selection is a set of rows the Operator can *see*. Changing the question
   *  means they can no longer see them, and a bulk action over rows that
   *  scrolled out of the answer is the one thing multi-select must never do. */
  it('drops the selection when the filters change', async () => {
    county()
    signedIn(<AdminTalkgroupsScreen />)
    await screen.findByRole('list', { name: 'Talkgroups' })
    await userEvent.click(screen.getByLabelText('Select Fire Dispatch'))
    expect(
      await screen.findByRole('form', { name: 'Bulk assignment' }),
    ).toBeInTheDocument()

    await userEvent.type(screen.getByLabelText('Search'), 'police')

    await waitFor(() =>
      expect(screen.queryByRole('form', { name: 'Bulk assignment' })).toBeNull(),
    )
  })

  it('deselects without applying anything', async () => {
    county()
    signedIn(<AdminTalkgroupsScreen />)
    await screen.findByRole('list', { name: 'Talkgroups' })
    await userEvent.click(screen.getByLabelText('Select Fire Dispatch'))

    await userEvent.click(screen.getByRole('button', { name: 'Deselect' }))

    expect(screen.queryByRole('form', { name: 'Bulk assignment' })).toBeNull()
    expect(wrote()).toEqual([])
  })

  /** Unchecking a row takes it back out of the selection — the other half of
   *  the checkbox, and the one a naive `[...selected, id]` would get wrong. */
  it('unselects one row without losing the others', async () => {
    county()
    signedIn(<AdminTalkgroupsScreen />)
    await screen.findByRole('list', { name: 'Talkgroups' })

    await userEvent.click(screen.getByLabelText('Select Fire Dispatch'))
    await userEvent.click(screen.getByLabelText('Select Fire Tac 1'))
    await userEvent.click(screen.getByLabelText('Select Fire Dispatch'))

    expect(
      within(screen.getByRole('form', { name: 'Bulk assignment' })).getByText(
        '1 selected',
      ),
    ).toBeInTheDocument()
  })

  it('reports a refused bulk action', async () => {
    county()
    server.use(
      http.post(`${ORIGIN}/api/admin/talkgroups/assign`, () =>
        refusal(400, 'field-required', 'name is required', { field: 'name' }),
      ),
      ...curationHandlers(instance),
    )
    renderWithProviders(<AdminTalkgroupsScreen />)
    await screen.findByRole('list', { name: 'Talkgroups' })
    await userEvent.click(screen.getByLabelText('Select Fire Dispatch'))

    await userEvent.click(screen.getByRole('button', { name: 'Apply to 1' }))

    expect(await screen.findByRole('alert')).toHaveTextContent('name is required')
  })

  /** The filters are the reason this listing pages server-side at all. */
  it('filters by system and by text', async () => {
    county()
    const asked: string[] = []
    server.use(
      http.get(`${ORIGIN}/api/admin/talkgroups`, ({ request }) => {
        asked.push(new URL(request.url).search)
        return undefined
      }),
      ...curationHandlers(instance),
    )
    renderWithProviders(<AdminTalkgroupsScreen />)
    await screen.findByRole('list', { name: 'Talkgroups' })

    await userEvent.selectOptions(
      screen.getByLabelText('System', { selector: '#tg-filter-system' }),
      '11',
    )
    await waitFor(() => expect(asked.at(-1)).toContain('system=11'))

    await userEvent.type(screen.getByLabelText('Search'), '200')

    await waitFor(() => expect(asked.at(-1)).toContain('q=200'))
    expect(await screen.findByText(/Police Dispatch/)).toBeInTheDocument()
  })

  /** **The blacklist toggle.** A per-row checkbox rather than a comma field on
   *  the parent, because an Operator is thinking "stop ingesting this channel". */
  it('blacklists a channel from its own row', async () => {
    county()
    signedIn(<AdminTalkgroupsScreen />)
    await screen.findByRole('list', { name: 'Talkgroups' })
    await userEvent.click(screen.getAllByRole('button', { name: 'Edit' })[0])

    await userEvent.click(screen.getByLabelText(/Never ingest calls/))
    await userEvent.click(screen.getByRole('button', { name: 'Save' }))

    await waitFor(() => expect(wrote()).toHaveLength(1))
    expect(wrote()[0].body).toMatchObject({ blacklisted: true })
    // ...and it lands on the System's own list, which is where ingest reads it.
    expect(instance.systems[0].blacklist).toEqual([100])
    expect(await screen.findByText('blacklisted')).toBeInTheDocument()
  })

  /** The LED select offers the palette and nothing else, so a colour the server
   *  would refuse is not reachable from the form. */
  it('offers exactly the LED palette', async () => {
    county()
    signedIn(<AdminTalkgroupsScreen />)
    await screen.findByRole('list', { name: 'Talkgroups' })
    await userEvent.click(screen.getAllByRole('button', { name: 'Edit' })[0])

    const options = within(screen.getByLabelText('LED'))
      .getAllByRole('option')
      .map((option) => (option as HTMLOptionElement).value)

    expect(options).toEqual([
      '',
      'blue',
      'cyan',
      'green',
      'magenta',
      'orange',
      'red',
      'white',
      'yellow',
    ])
  })

  it('edits a channel and clears what was emptied', async () => {
    county()
    signedIn(<AdminTalkgroupsScreen />)
    await screen.findByRole('list', { name: 'Talkgroups' })
    await userEvent.click(screen.getAllByRole('button', { name: 'Edit' })[0])

    const editor = within(
      await screen.findByRole('form', { name: 'Edit Fire Dispatch' }),
    )
    await userEvent.clear(editor.getByLabelText('Label'))
    await userEvent.type(editor.getByLabelText('Name'), 'Fire Dispatch (county)')
    await userEvent.clear(editor.getByLabelText('Tag'))
    await userEvent.type(editor.getByLabelText('Groups'), 'Fire, Dispatch')
    await userEvent.selectOptions(editor.getByLabelText('LED'), 'red')
    await userEvent.click(editor.getByRole('button', { name: 'Save' }))

    await waitFor(() => expect(wrote()).toHaveLength(1))
    expect(wrote()[0].body).toEqual({
      label: null,
      name: 'Fire Dispatch (county)',
      tag: null,
      groups: ['Fire', 'Dispatch'],
      led: 'red',
      blacklisted: false,
    })
  })

  it('reports a refused edit and keeps the editor open', async () => {
    county()
    server.use(
      http.patch(`${ORIGIN}/api/admin/talkgroups/:id`, () =>
        refusal(400, 'unknown-led', '"puce" is not an LED colour'),
      ),
      ...curationHandlers(instance),
    )
    renderWithProviders(<AdminTalkgroupsScreen />)
    await screen.findByRole('list', { name: 'Talkgroups' })
    await userEvent.click(screen.getAllByRole('button', { name: 'Edit' })[0])

    await userEvent.click(screen.getByRole('button', { name: 'Save' }))

    expect(await screen.findByRole('alert')).toHaveTextContent('not an LED colour')
    expect(
      screen.getByRole('form', { name: 'Edit Fire Dispatch' }),
    ).toBeInTheDocument()
  })

  it('closes a channel editor without saving', async () => {
    county()
    signedIn(<AdminTalkgroupsScreen />)
    await screen.findByRole('list', { name: 'Talkgroups' })

    await userEvent.click(screen.getAllByRole('button', { name: 'Edit' })[0])
    await userEvent.click(screen.getByRole('button', { name: 'Close' }))

    expect(screen.queryByRole('form', { name: 'Edit Fire Dispatch' })).toBeNull()
    expect(wrote()).toEqual([])
  })

  it('refuses to delete a channel with calls, then forces it', async () => {
    instance.system({ ref: 11, label: 'Fulton' })
    const channel = instance.talkgroup({ ref: 100, label: 'Fire', calls: 3 })
    signedIn(<AdminTalkgroupsScreen />)
    await screen.findByRole('list', { name: 'Talkgroups' })

    await userEvent.click(screen.getByRole('button', { name: 'Delete' }))
    expect(await screen.findByRole('alert')).toHaveTextContent('still has 3 calls')

    await userEvent.click(
      screen.getByRole('button', { name: 'Delete it and its 3 calls' }),
    )

    await waitFor(() =>
      expect(
        screen.queryByRole('list', { name: 'Talkgroups' }),
      ).not.toBeInTheDocument(),
    )
    expect(wrote().at(-1)?.path).toBe(`/api/admin/talkgroups/${channel.id}?force=true`)
  })

  it('says so when nothing matches, and when the list cannot be read', async () => {
    signedIn(<AdminTalkgroupsScreen />)
    expect(await screen.findByText('No talkgroups match.')).toBeInTheDocument()

    server.use(
      http.get(
        `${ORIGIN}/api/admin/talkgroups`,
        () => new HttpResponse(null, { status: 500 }),
      ),
    )
    renderWithProviders(<AdminTalkgroupsScreen />)

    expect(await screen.findAllByRole('alert')).not.toHaveLength(0)
  })

  /** A channel with nothing curated still reads as something: the Ref, which is
   *  what auto-populate leaves behind (#8) — in the list *and* in its editor,
   *  which is the only name that editor could be given. */
  it('shows an uncurated channel by its ref', async () => {
    instance.system({ ref: 11, label: 'Fulton' })
    instance.talkgroup({ ref: 54241 })
    signedIn(<AdminTalkgroupsScreen />)
    const list = await screen.findByRole('list', { name: 'Talkgroups' })

    expect(within(list).getByText('54241')).toBeInTheDocument()
    expect(within(list).getByText(/untagged/)).toBeInTheDocument()
    expect(within(list).getByText(/no groups/)).toBeInTheDocument()

    await userEvent.click(screen.getByRole('button', { name: 'Edit' }))

    const editor = within(screen.getByRole('form', { name: 'Edit 54241' }))
    // Every box opens empty rather than holding the word `null`.
    for (const field of ['Label', 'Name', 'Tag', 'Groups']) {
      expect(editor.getByLabelText(field)).toHaveValue('')
    }
    expect(editor.getByLabelText('LED')).toHaveValue('')
  })

  /** A tag typed into the bulk box is sent as a *set*, where the sentinel that
   *  means "clear" is not something an Operator can type. */
  it('sets a tag in bulk without the clear sentinel leaking into the box', async () => {
    county()
    signedIn(<AdminTalkgroupsScreen />)
    await screen.findByRole('list', { name: 'Talkgroups' })
    await userEvent.click(screen.getByLabelText('Select Fire Dispatch'))

    await userEvent.click(screen.getByRole('button', { name: 'Clear their tags' }))

    // The box shows nothing, because "clear" is an action rather than a value
    // an Operator typed — and typing over it takes control back.
    expect(screen.getByLabelText('Set tag')).toHaveValue('')
    await userEvent.type(screen.getByLabelText('Set tag'), 'Fire')
    await userEvent.click(screen.getByRole('button', { name: 'Apply to 1' }))

    await waitFor(() => expect(wrote()).toHaveLength(1))
    expect(wrote()[0].body).toMatchObject({ tag: 'Fire' })
  })
})

// ---------------------------------------------------------------------------
// Units
// ---------------------------------------------------------------------------

describe('units', () => {
  /** Naming an apparatus is the half a recorder can never supply. */
  it('names a radio', async () => {
    instance.system({ ref: 11, label: 'Fulton' })
    const unit = instance.unit({ ref: 1201 })
    signedIn(<UnitsScreen />)
    await screen.findByRole('list', { name: 'Units' })
    // Uncurated, so it reads as its radio id.
    expect(screen.getByText('Radio 1201')).toBeInTheDocument()

    await userEvent.click(screen.getByRole('button', { name: 'Edit' }))
    await userEvent.type(screen.getByLabelText('Name radio 1201'), 'Engine 1')
    await userEvent.click(screen.getByRole('button', { name: 'Save' }))

    await waitFor(() => expect(wrote()).toHaveLength(1))
    expect(wrote()[0]).toEqual({
      method: 'PATCH',
      path: `/api/admin/units/${unit.id}`,
      body: { label: 'Engine 1' },
    })
    expect(await screen.findByText('Engine 1')).toBeInTheDocument()
  })

  /** Un-naming is `null`, not an omitted field — the same reason every nullable
   *  field on this surface is a double option on the wire. */
  it('un-names a radio with an explicit null', async () => {
    instance.system({})
    instance.unit({ ref: 1201, label: 'Engine 1' })
    signedIn(<UnitsScreen />)
    await screen.findByRole('list', { name: 'Units' })

    await userEvent.click(screen.getByRole('button', { name: 'Edit' }))
    await userEvent.clear(screen.getByLabelText('Name radio 1201'))
    await userEvent.click(screen.getByRole('button', { name: 'Save' }))

    await waitFor(() => expect(wrote()).toHaveLength(1))
    expect(wrote()[0].body).toEqual({ label: null })
  })

  /** **The filter that matters**: an Operator sitting down to name a fleet wants
   *  the radios nobody has named, and no text search can ask for the absence of
   *  a name. */
  it('filters to the radios nobody has named', async () => {
    instance.system({})
    instance.unit({ ref: 1201, label: 'Engine 1' })
    instance.unit({ ref: 1202 })
    signedIn(<UnitsScreen />)
    await screen.findByRole('list', { name: 'Units' })

    await userEvent.click(screen.getByLabelText(/Only radios nobody has named/))

    await waitFor(() =>
      expect(screen.queryByText('Engine 1')).not.toBeInTheDocument(),
    )
    expect(screen.getByText('Radio 1202')).toBeInTheDocument()
  })

  it('adds a radio nobody has heard yet', async () => {
    instance.system({ ref: 11, label: 'Fulton' })
    signedIn(<UnitsScreen />)
    await screen.findByLabelText('Radio id')

    await userEvent.type(screen.getByLabelText('Radio id'), '4471')
    await userEvent.type(screen.getByLabelText('Name'), 'Spare')
    await userEvent.click(screen.getByRole('button', { name: 'Add' }))

    await waitFor(() => expect(wrote()).toHaveLength(1))
    expect(wrote()[0].body).toEqual({
      systemId: instance.systems[0].id,
      ref: 4471,
      label: 'Spare',
    })
  })

  /** With no System there is nothing a radio could belong to, so the form is
   *  absent rather than posting a Unit under nothing. */
  it('offers no add form until a system exists', async () => {
    signedIn(<UnitsScreen />)
    await screen.findByLabelText('Search')

    expect(screen.queryByLabelText('Radio id')).not.toBeInTheDocument()
  })

  it('reports a refused create', async () => {
    instance.system({})
    instance.unit({ ref: 1201 })
    signedIn(<UnitsScreen />)
    await screen.findByLabelText('Radio id')

    await userEvent.type(screen.getByLabelText('Radio id'), '1201')
    await userEvent.click(screen.getByRole('button', { name: 'Add' }))

    expect(await screen.findByRole('alert')).toHaveTextContent(
      'another unit already answers to 1201',
    )
  })

  it('deletes a radio, and says so when the list cannot be read', async () => {
    instance.system({})
    const unit = instance.unit({ ref: 1201, label: 'Engine 1' })
    signedIn(<UnitsScreen />)
    await screen.findByRole('list', { name: 'Units' })

    await userEvent.click(screen.getByRole('button', { name: 'Delete' }))

    await waitFor(() => expect(screen.queryByText('Engine 1')).toBeNull())
    expect(wrote().at(-1)?.path).toBe(`/api/admin/units/${unit.id}`)
  })

  it('says so when the units cannot be read', async () => {
    server.use(
      http.get(
        `${ORIGIN}/api/admin/units`,
        () => new HttpResponse(null, { status: 500 }),
      ),
      ...curationHandlers(instance),
    )
    renderWithProviders(<UnitsScreen />)

    expect(await screen.findByRole('alert')).toHaveTextContent(
      'The units could not be read',
    )
  })

  it('reports a refused rename', async () => {
    instance.system({})
    instance.unit({ ref: 1201 })
    server.use(
      http.patch(`${ORIGIN}/api/admin/units/:id`, () =>
        refusal(404, 'unit-not-found', 'no such unit'),
      ),
      ...curationHandlers(instance),
    )
    renderWithProviders(<UnitsScreen />)
    await screen.findByRole('list', { name: 'Units' })

    await userEvent.click(screen.getByRole('button', { name: 'Edit' }))
    await userEvent.click(screen.getByRole('button', { name: 'Save' }))

    expect(await screen.findByRole('alert')).toHaveTextContent('no such unit')
  })

  it('closes a radio editor without saving', async () => {
    instance.system({})
    instance.unit({ ref: 1201 })
    signedIn(<UnitsScreen />)
    await screen.findByRole('list', { name: 'Units' })

    await userEvent.click(screen.getByRole('button', { name: 'Edit' }))
    await userEvent.click(screen.getByRole('button', { name: 'Close' }))

    expect(screen.queryByLabelText('Name radio 1201')).not.toBeInTheDocument()
  })
})

// ---------------------------------------------------------------------------
// API keys
// ---------------------------------------------------------------------------

describe('api keys', () => {
  /** **Shown once.** The key is stored hashed, so the moment it is issued is the
   *  only moment it can be copied — and the screen says so. */
  it('issues a key, shows the secret once, and never lists it', async () => {
    signedIn(<ApiKeysScreen />)
    await screen.findByLabelText('What is it for')

    await userEvent.type(screen.getByLabelText('What is it for'), 'the pi')
    await userEvent.type(screen.getByLabelText('System ref'), '11')
    await userEvent.click(screen.getByRole('button', { name: 'Issue' }))

    const shown = await screen.findByRole('status')
    expect(shown).toHaveTextContent('issued-secret-0001')
    expect(shown).toHaveTextContent('cannot be shown again')
    expect(wrote()[0].body).toEqual({ label: 'the pi', systemRef: 11 })

    const list = await screen.findByRole('list', { name: 'API keys' })
    expect(within(list).queryByText(/issued-secret/)).toBeNull()
    expect(within(list).getByText(/system 11/)).toBeInTheDocument()
  })

  /** Disable is the durable off — the row stays, and nothing revives it. */
  it('disables and re-enables a key', async () => {
    const key = instance.key({ label: 'the pi' })
    signedIn(<ApiKeysScreen />)
    await screen.findByRole('list', { name: 'API keys' })

    await userEvent.click(screen.getByRole('button', { name: 'Disable' }))

    expect(await screen.findByText(/disabled/)).toBeInTheDocument()
    expect(wrote()[0]).toEqual({
      method: 'PATCH',
      path: `/api/admin/api-keys/${key.id}`,
      body: { disabled: true },
    })

    await userEvent.click(await screen.findByRole('button', { name: 'Enable' }))

    await waitFor(() => expect(wrote()).toHaveLength(2))
    expect(wrote()[1].body).toEqual({ disabled: false })
  })

  it('revokes a key', async () => {
    const key = instance.key({ label: 'the pi' })
    signedIn(<ApiKeysScreen />)
    await screen.findByRole('list', { name: 'API keys' })

    await userEvent.click(screen.getByRole('button', { name: 'Revoke' }))

    await waitFor(() => expect(screen.queryByText('the pi')).toBeNull())
    expect(wrote().at(-1)?.path).toBe(`/api/admin/api-keys/${key.id}`)
  })

  /** An unscoped key says so, because "every system" and "system 0" are very
   *  different permissions to hand a recorder. */
  it('says when a key opens every system', async () => {
    instance.key({ label: null, systemRef: null })
    signedIn(<ApiKeysScreen />)

    const list = await screen.findByRole('list', { name: 'API keys' })

    expect(within(list).getByText(/every system/)).toBeInTheDocument()
    expect(within(list).getByText(/^Key /)).toBeInTheDocument()
  })

  it('says so when there are none, and when they cannot be read', async () => {
    signedIn(<ApiKeysScreen />)
    expect(await screen.findByText('No keys yet.')).toBeInTheDocument()

    server.use(
      http.get(
        `${ORIGIN}/api/admin/api-keys`,
        () => new HttpResponse(null, { status: 500 }),
      ),
    )
    renderWithProviders(<ApiKeysScreen />)

    expect(await screen.findAllByRole('alert')).not.toHaveLength(0)
  })

  it('reports a refused issue and a refused revoke', async () => {
    instance.key({})
    server.use(
      http.post(`${ORIGIN}/api/admin/api-keys`, () =>
        refusal(400, 'field-required', 'label is required', { field: 'label' }),
      ),
      http.delete(`${ORIGIN}/api/admin/api-keys/:id`, () =>
        refusal(404, 'api-key-not-found', 'no such API key'),
      ),
      ...curationHandlers(instance),
    )
    renderWithProviders(<ApiKeysScreen />)
    await screen.findByRole('list', { name: 'API keys' })

    await userEvent.click(screen.getByRole('button', { name: 'Issue' }))
    expect(await screen.findByRole('alert')).toHaveTextContent('label is required')

    await userEvent.click(screen.getByRole('button', { name: 'Revoke' }))

    await waitFor(() =>
      expect(
        screen.getAllByRole('alert').some((alert) =>
          alert.textContent?.includes('no such API key'),
        ),
      ).toBe(true),
    )
  })
})

// ---------------------------------------------------------------------------
// The rest of the surface each screen owes
// ---------------------------------------------------------------------------

describe('creating and paging', () => {
  /** Auto-populate (#8) creates a channel the first time one is heard; this is
   *  the other way — an Operator who knows about it first. */
  it('adds a talkgroup by hand', async () => {
    instance.system({ ref: 11, label: 'Fulton' })
    signedIn(<AdminTalkgroupsScreen />)
    const form = within(await screen.findByRole('form', { name: 'New talkgroup' }))

    await userEvent.type(form.getByLabelText('Ref'), '100')
    await userEvent.type(form.getByLabelText('Label'), 'Fire Dispatch')
    await userEvent.click(form.getByRole('button', { name: 'Add' }))

    await waitFor(() => expect(wrote()).toHaveLength(1))
    expect(wrote()[0]).toEqual({
      method: 'POST',
      path: '/api/admin/talkgroups',
      body: {
        systemId: instance.systems[0].id,
        ref: 100,
        label: 'Fire Dispatch',
      },
    })
  })

  it('reports a refused talkgroup create', async () => {
    instance.system({ ref: 11 })
    instance.talkgroup({ ref: 100 })
    signedIn(<AdminTalkgroupsScreen />)
    const form = within(await screen.findByRole('form', { name: 'New talkgroup' }))

    await userEvent.type(form.getByLabelText('Ref'), '100')
    await userEvent.click(form.getByRole('button', { name: 'Add' }))

    expect(await screen.findByRole('alert')).toHaveTextContent(
      'another talkgroup already answers to 100',
    )
  })

  /** With no System there is nothing a channel could belong to, so the form is
   *  absent rather than posting one under nothing. */
  it('offers no talkgroup form until a system exists', async () => {
    signedIn(<AdminTalkgroupsScreen />)
    await screen.findByLabelText('Search')

    expect(screen.queryByRole('form', { name: 'New talkgroup' })).toBeNull()
  })

  /** A second System means the form has a choice to make, and the choice has to
   *  reach the request — a channel filed under the wrong System is a channel a
   *  recorder's uploads will never resolve to. */
  it('files a new talkgroup under the system that was chosen', async () => {
    instance.system({ ref: 11, label: 'Fulton' })
    const other = instance.system({ ref: 12, label: 'Neighbour' })
    signedIn(<AdminTalkgroupsScreen />)
    const form = within(await screen.findByRole('form', { name: 'New talkgroup' }))

    await userEvent.selectOptions(form.getByLabelText('System'), String(other.id))
    await userEvent.type(form.getByLabelText('Ref'), '300')
    await userEvent.click(form.getByRole('button', { name: 'Add' }))

    await waitFor(() => expect(wrote()).toHaveLength(1))
    expect(wrote()[0].body).toMatchObject({ systemId: other.id, ref: 300 })
  })

  it('files a new unit under the system that was chosen', async () => {
    instance.system({ ref: 11, label: 'Fulton' })
    const other = instance.system({ ref: 12, label: 'Neighbour' })
    signedIn(<UnitsScreen />)
    await screen.findByLabelText('Radio id')

    await userEvent.selectOptions(
      screen.getByLabelText('System', { selector: '#new-unit-system' }),
      String(other.id),
    )
    await userEvent.type(screen.getByLabelText('Radio id'), '4471')
    await userEvent.click(screen.getByRole('button', { name: 'Add' }))

    await waitFor(() => expect(wrote()).toHaveLength(1))
    expect(wrote()[0].body).toMatchObject({ systemId: other.id })
  })

  /** A tag is renamed through the same screen a group is — the claim that they
   *  behave identically, asserted on the *other* table too. */
  it('renames a tag', async () => {
    const tag = instance.label('tags', { name: 'Fire' })
    signedIn(<TagsScreen />)
    await screen.findByRole('list', { name: 'Tags' })

    await userEvent.click(screen.getByRole('button', { name: 'Rename' }))
    const field = screen.getByLabelText('Rename Fire')
    await userEvent.clear(field)
    await userEvent.type(field, 'EMS')
    await userEvent.click(screen.getByRole('button', { name: 'Save' }))

    await waitFor(() => expect(wrote()).toHaveLength(1))
    expect(wrote()[0]).toEqual({
      method: 'PATCH',
      path: `/api/admin/tags/${tag.id}`,
      body: { name: 'EMS' },
    })
  })

  /** The Group and Tag filters cascade off the same lists the forms fill, so an
   *  Operator narrows to what they just created rather than typing it again. */
  it('filters talkgroups by group and by tag', async () => {
    instance.system({ ref: 11, label: 'Fulton' })
    instance.talkgroup({ ref: 100, label: 'Fire Dispatch' })
    instance.label('groups', { name: 'Fire' })
    instance.label('tags', { name: 'Law' })
    const asked: string[] = []
    server.use(
      http.get(`${ORIGIN}/api/admin/talkgroups`, ({ request }) => {
        asked.push(new URL(request.url).search)
        return undefined
      }),
      ...curationHandlers(instance),
    )
    renderWithProviders(<AdminTalkgroupsScreen />)
    await screen.findByRole('list', { name: 'Talkgroups' })

    await userEvent.selectOptions(screen.getByLabelText('Group'), 'Fire')
    await waitFor(() => expect(asked.at(-1)).toContain('group=Fire'))

    await userEvent.selectOptions(screen.getByLabelText('Tag'), 'Law')

    await waitFor(() => expect(asked.at(-1)).toContain('tag=Law'))
  })

  /** Paging moves the window and **drops the selection**, for the reason a
   *  filter change does: a bulk action over rows that scrolled away is the one
   *  thing multi-select must never do. */
  it('pages talkgroups and releases the selection on the way', async () => {
    instance.system({ ref: 11, label: 'Fulton' })
    instance.talkgroup({ ref: 100, label: 'Fire Dispatch' })
    const asked: string[] = []
    server.use(
      http.get(`${ORIGIN}/api/admin/talkgroups`, ({ request }) => {
        asked.push(new URL(request.url).search)
        return HttpResponse.json({
          results: instance.talkgroups,
          count: 120,
          limit: 50,
          offset: 0,
          hasMore: true,
        })
      }),
      ...curationHandlers(instance),
    )
    renderWithProviders(<AdminTalkgroupsScreen />)
    await screen.findByRole('list', { name: 'Talkgroups' })
    await userEvent.click(screen.getByLabelText('Select Fire Dispatch'))
    expect(screen.getByText('1–1 of 120')).toBeInTheDocument()
    // The first page has nothing behind it.
    expect(screen.getByRole('button', { name: 'Previous' })).toBeDisabled()

    await userEvent.click(screen.getByRole('button', { name: 'Next' }))

    await waitFor(() => expect(asked.at(-1)).toContain('offset=50'))
    expect(screen.queryByRole('form', { name: 'Bulk assignment' })).toBeNull()

    await userEvent.click(screen.getByLabelText('Select Fire Dispatch'))
    await userEvent.click(screen.getByRole('button', { name: 'Previous' }))

    // Going back is served from the cache, so what proves it moved is the
    // window on screen rather than another request.
    await waitFor(() =>
      expect(screen.getByText('1–1 of 120')).toBeInTheDocument(),
    )
    expect(screen.getByRole('button', { name: 'Previous' })).toBeDisabled()
    expect(screen.queryByRole('form', { name: 'Bulk assignment' })).toBeNull()
  })

  it('pages units', async () => {
    instance.system({ ref: 11 })
    instance.unit({ ref: 1201, label: 'Engine 1' })
    const asked: string[] = []
    server.use(
      http.get(`${ORIGIN}/api/admin/units`, ({ request }) => {
        asked.push(new URL(request.url).search)
        return HttpResponse.json({
          results: instance.units,
          count: 90,
          limit: 50,
          offset: 0,
          hasMore: true,
        })
      }),
      ...curationHandlers(instance),
    )
    renderWithProviders(<UnitsScreen />)
    await screen.findByRole('list', { name: 'Units' })
    expect(screen.getByRole('button', { name: 'Previous' })).toBeDisabled()

    await userEvent.click(screen.getByRole('button', { name: 'Next' }))
    await waitFor(() => expect(asked.at(-1)).toContain('offset=50'))

    await userEvent.click(screen.getByRole('button', { name: 'Previous' }))

    await waitFor(() =>
      expect(screen.getByRole('button', { name: 'Previous' })).toBeDisabled(),
    )
  })

  /** The System filter on the Units screen narrows to one network's radios —
   *  a Ref is unique only within a System, so a fleet's numbering means nothing
   *  across one. */
  it('filters units by system', async () => {
    instance.system({ ref: 11, label: 'Fulton' })
    instance.unit({ ref: 1201 })
    const asked: string[] = []
    server.use(
      http.get(`${ORIGIN}/api/admin/units`, ({ request }) => {
        asked.push(new URL(request.url).search)
        return undefined
      }),
      ...curationHandlers(instance),
    )
    renderWithProviders(<UnitsScreen />)
    await screen.findByRole('list', { name: 'Units' })

    await userEvent.selectOptions(
      screen.getByLabelText('System', { selector: '#unit-filter-system' }),
      '11',
    )

    await waitFor(() => expect(asked.at(-1)).toContain('system=11'))
  })

  it('searches units by name', async () => {
    instance.system({ ref: 11 })
    instance.unit({ ref: 1201, label: 'Engine 1' })
    const asked: string[] = []
    server.use(
      http.get(`${ORIGIN}/api/admin/units`, ({ request }) => {
        asked.push(new URL(request.url).search)
        return undefined
      }),
      ...curationHandlers(instance),
    )
    renderWithProviders(<UnitsScreen />)
    await screen.findByRole('list', { name: 'Units' })

    await userEvent.type(screen.getByLabelText('Search'), 'engine')

    await waitFor(() => expect(asked.at(-1)).toContain('q=engine'))
  })

  /** A System created with an explicit Ref sends it; a blank one asks the
   *  server to mint the lowest free one (#8's own answer). */
  it('creates a system at a ref the operator chose', async () => {
    signedIn(<SystemsScreen />)
    await screen.findByLabelText('Ref')

    await userEvent.type(screen.getByLabelText('Ref'), '42')
    await userEvent.click(screen.getByRole('button', { name: 'Add' }))

    await waitFor(() => expect(wrote()).toHaveLength(1))
    expect(wrote()[0].body).toEqual({ ref: 42, label: undefined })
  })

  /** An unlabelled System still reads as something — its Ref, which is what a
   *  recorder that named nothing leaves behind. */
  it('shows an unlabelled system by its ref', async () => {
    instance.system({ ref: 11, label: null })
    signedIn(<SystemsScreen />)

    const list = await screen.findByRole('list', { name: 'Systems' })

    expect(within(list).getByText('System 11')).toBeInTheDocument()
  })

  /** A System's **Ref** is editable, because it is what the radio network calls
   *  it and a recorder that named a System without numbering it got one minted
   *  (#8). Renumbering keeps everything hanging off the **Id** — the Talkgroups,
   *  the Units and the Archive. */
  it('renumbers a system', async () => {
    const system = instance.system({ ref: 11, label: 'Fulton' })
    signedIn(<SystemsScreen />)
    await screen.findByRole('list', { name: 'Systems' })
    await userEvent.click(screen.getByRole('button', { name: 'Edit' }))

    const editor = within(screen.getByRole('form', { name: 'Edit Fulton' }))
    await userEvent.clear(editor.getByLabelText('Ref'))
    await userEvent.type(editor.getByLabelText('Ref'), '42')
    await userEvent.click(editor.getByRole('button', { name: 'Save' }))

    await waitFor(() => expect(wrote()).toHaveLength(1))
    expect(wrote()[0]).toEqual({
      method: 'PATCH',
      path: `/api/admin/systems/${system.id}`,
      body: { ref: 42, label: 'Fulton', autoPopulate: false, enhancement: null, blacklist: [] },
    })
  })

  /** **The spec names `label` on an API key.** It was editable through the API
   *  and unreachable from a browser until this form existed — and re-scoping is
   *  the useful half: a recorder moved to another System keeps its key. */
  it('relabels and re-scopes a key', async () => {
    const key = instance.key({ label: 'the pi', systemRef: 11 })
    signedIn(<ApiKeysScreen />)
    await screen.findByRole('list', { name: 'API keys' })

    await userEvent.click(screen.getByRole('button', { name: 'Edit' }))
    const editor = within(screen.getByRole('form', { name: 'Edit the pi' }))
    await userEvent.clear(editor.getByLabelText('What is it for'))
    await userEvent.type(editor.getByLabelText('What is it for'), 'the shed')
    await userEvent.clear(editor.getByLabelText('System ref'))
    await userEvent.click(editor.getByRole('button', { name: 'Save' }))

    await waitFor(() => expect(wrote()).toHaveLength(1))
    expect(wrote()[0]).toEqual({
      method: 'PATCH',
      path: `/api/admin/api-keys/${key.id}`,
      // `null` is how a key goes back to opening every System — an absent
      // field could only ever leave the scope it has.
      body: { label: 'the shed', systemRef: null },
    })
    // ...and the row now says so, where it said "system 11" before.
    const list = await screen.findByRole('list', { name: 'API keys' })
    expect(within(list).getByText(/every system/)).toBeInTheDocument()
    expect(within(list).queryByText(/system 11/)).toBeNull()
  })

  it('reports a refused key edit and keeps the editor open', async () => {
    instance.key({ label: 'the pi' })
    server.use(
      http.patch(`${ORIGIN}/api/admin/api-keys/:id`, () =>
        refusal(404, 'api-key-not-found', 'no such API key'),
      ),
      ...curationHandlers(instance),
    )
    renderWithProviders(<ApiKeysScreen />)
    await screen.findByRole('list', { name: 'API keys' })

    await userEvent.click(screen.getByRole('button', { name: 'Edit' }))
    await userEvent.click(screen.getByRole('button', { name: 'Save' }))

    expect(await screen.findByRole('alert')).toHaveTextContent('no such API key')
    expect(screen.getByRole('form', { name: 'Edit the pi' })).toBeInTheDocument()
  })

  it('closes a key editor without saving', async () => {
    instance.key({ label: 'the pi' })
    signedIn(<ApiKeysScreen />)
    await screen.findByRole('list', { name: 'API keys' })

    await userEvent.click(screen.getByRole('button', { name: 'Edit' }))
    await userEvent.click(screen.getByRole('button', { name: 'Close' }))

    expect(screen.queryByRole('form', { name: 'Edit the pi' })).toBeNull()
    expect(wrote()).toEqual([])
  })

  /** **A signed-out visit asks for nothing.** A `useQuery` fires as soon as its
   *  component mounts, and a component rendering the gate has still mounted —
   *  so without the skip every admin screen would fire its reads as 401s,
   *  filling the operator log and flashing "could not be read" before the
   *  password form appeared. */
  it.each([
    ['systems', () => <SystemsScreen />],
    ['talkgroups', () => <AdminTalkgroupsScreen />],
    ['units', () => <UnitsScreen />],
    ['groups', () => <GroupsScreen />],
    ['api keys', () => <ApiKeysScreen />],
  ])('issues no admin read while signed out on %s', async (_name, ui) => {
    const asked: string[] = []
    server.use(
      http.get(`${ORIGIN}/api/admin/*`, ({ request }) => {
        asked.push(new URL(request.url).pathname)
        return undefined
      }),
    )
    renderWithProviders(ui())

    await screen.findByLabelText('Admin password')

    expect(asked).toEqual(['/api/admin/session'])
  })

  /** Signing out is server-side, because ADR-0008 chose session state over a
   *  JWT precisely so that revocation is real. */
  it('signs out', async () => {
    let open = true
    server.use(
      http.get(`${ORIGIN}/api/admin/session`, () =>
        open
          ? HttpResponse.json({ csrf_token: 'csrf-abc', expires_in_secs: 3600 })
          : new HttpResponse('admin session required\n', { status: 401 }),
      ),
      http.post(`${ORIGIN}/api/admin/logout`, () => {
        open = false
        return new HttpResponse(null, { status: 204 })
      }),
      ...curationHandlers(instance),
    )
    renderWithProviders(<AdminScreen />)
    await screen.findByRole('link', { name: /Talkgroups/ })

    await userEvent.click(screen.getByRole('button', { name: 'Sign out' }))

    expect(await screen.findByLabelText('Admin password')).toBeInTheDocument()
  })
})

describe('the details the forms owe', () => {
  /** The three-way enhancement select round-trips every state, which is the
   *  whole reason the column is nullable: **inherit** is a real answer and a
   *  checkbox has nowhere to put it. */
  it.each([
    [null, '', 'on', true],
    [true, 'on', 'off', false],
    [false, 'off', '', null],
  ])(
    'shows enhancement %s as %o and saves the next choice',
    async (stored, shown, choose, sent) => {
      instance.system({ label: 'Fulton', enhancement: stored as boolean | null })
      signedIn(<SystemsScreen />)
      await screen.findByRole('list', { name: 'Systems' })
      await userEvent.click(screen.getByRole('button', { name: 'Edit' }))

      expect(screen.getByLabelText('Enhancement')).toHaveValue(shown)
      await userEvent.selectOptions(screen.getByLabelText('Enhancement'), choose)
      await userEvent.click(screen.getByRole('button', { name: 'Save' }))

      await waitFor(() => expect(wrote()).toHaveLength(1))
      expect(wrote()[0].body).toMatchObject({ enhancement: sent })
    },
  )

  /** An unlabelled System opens with an empty box rather than the word `null`,
   *  and its editor is still findable by the Ref it is known by. */
  it('edits a system nobody has named', async () => {
    instance.system({ ref: 11, label: null, blacklist: [100] })
    signedIn(<SystemsScreen />)
    await screen.findByRole('list', { name: 'Systems' })

    await userEvent.click(screen.getByRole('button', { name: 'Edit' }))

    const editor = within(screen.getByRole('form', { name: 'Edit system 11' }))
    expect(editor.getByLabelText('Label')).toHaveValue('')
    // The stored list is shown back canonically, so a save is a no-op.
    expect(editor.getByLabelText('Blacklisted talkgroup refs')).toHaveValue('100')
  })

  /** The filter bars are `role="search"` forms, so pressing Enter in one is a
   *  submit — which must do nothing, because filtering is already live. A form
   *  that reloaded the page here would throw away the whole screen's state. */
  it.each([
    ['talkgroups', () => <AdminTalkgroupsScreen />, 'Talkgroup filters'],
    ['units', () => <UnitsScreen />, 'Unit filters'],
  ])('does not submit the %s filter bar', async (_name, ui, label) => {
    instance.system({ ref: 11 })
    signedIn(ui())
    const filters = await screen.findByRole('search', { name: label })

    // Fired directly rather than typed: these bars have no submit button, so a
    // browser would not submit them on Enter either — and the arm still has to
    // hold, because a form that reloaded here would throw the screen away.
    fireEvent.submit(filters)

    expect(filters).toBeInTheDocument()
    expect(wrote()).toEqual([])
  })
})

// ---------------------------------------------------------------------------
// Merge curation (#50, spec US 17)
// ---------------------------------------------------------------------------

describe('folding channels together', () => {
  /** The member Refs a channel answers to are a read of their own — the
   *  listing deliberately does not carry them, so opening the editor is what
   *  fetches them. */
  it('lists the refs a channel already answers to', async () => {
    const owner = instance.talkgroup({ ref: 100, label: 'Fire Dispatch' })
    instance.members.set(owner.id, [{ ref: 8123, label: 'TAC 3' }])
    signedIn(<AdminTalkgroupsScreen />)
    await screen.findByRole('list', { name: 'Talkgroups' })

    await userEvent.click(screen.getByRole('button', { name: 'Merges' }))

    const members = within(await screen.findByRole('list', { name: 'Member refs' }))
    expect(members.getByRole('listitem')).toHaveTextContent('8123 · TAC 3')
  })

  /** **Nothing is folded without being shown first.** The preview is a
   *  `?dryRun` of the real transaction, so it names the channel that would go
   *  and the Calls that would move — and writes nothing until the Operator
   *  says so again. */
  it('previews a fold before anything is written', async () => {
    const owner = instance.talkgroup({ ref: 100, label: 'Fire Dispatch' })
    instance.talkgroup({ ref: 8123, label: 'TAC 3', calls: 412 })
    signedIn(<AdminTalkgroupsScreen />)
    await screen.findByRole('list', { name: 'Talkgroups' })
    await userEvent.click(screen.getAllByRole('button', { name: 'Merges' })[0])

    await userEvent.type(
      await screen.findByLabelText('Refs to fold in'),
      '8123',
    )
    await userEvent.click(screen.getByRole('button', { name: 'Preview' }))

    const preview = within(await screen.findByRole('group', { name: 'Fold preview' }))
    expect(preview.getByRole('listitem')).toHaveTextContent(
      '8123 · TAC 3 — folded in, bringing 412 calls',
    )
    // Previewed, not performed: the only request carried `?dryRun`.
    expect(wrote().map((it) => it.path)).toEqual([
      `/api/admin/talkgroups/${owner.id}/members?dryRun`,
    ])
    expect(instance.talkgroups).toHaveLength(2)
  })

  /** ...and confirming sends the same delta for real. */
  it('folds once the preview is confirmed', async () => {
    const owner = instance.talkgroup({ ref: 100, label: 'Fire Dispatch' })
    instance.talkgroup({ ref: 8123, label: 'TAC 3', calls: 412 })
    signedIn(<AdminTalkgroupsScreen />)
    await screen.findByRole('list', { name: 'Talkgroups' })
    await userEvent.click(screen.getAllByRole('button', { name: 'Merges' })[0])
    await userEvent.type(await screen.findByLabelText('Refs to fold in'), '8123')
    await userEvent.click(screen.getByRole('button', { name: 'Preview' }))
    await screen.findByRole('group', { name: 'Fold preview' })

    await userEvent.click(screen.getByRole('button', { name: /^Fold/ }))

    await waitFor(() => expect(instance.talkgroups).toHaveLength(1))
    expect(wrote().map((it) => [it.path, it.body])).toEqual([
      [`/api/admin/talkgroups/${owner.id}/members?dryRun`, { fold: [8123], unfold: [] }],
      [`/api/admin/talkgroups/${owner.id}/members`, { fold: [8123], unfold: [] }],
    ])
  })

  /** Backing out of a preview writes nothing — the confirmation is a real
   *  decision point, not a speed bump. */
  it('writes nothing when a preview is dismissed', async () => {
    instance.talkgroup({ ref: 100, label: 'Fire Dispatch' })
    instance.talkgroup({ ref: 8123, label: 'TAC 3', calls: 412 })
    signedIn(<AdminTalkgroupsScreen />)
    await screen.findByRole('list', { name: 'Talkgroups' })
    await userEvent.click(screen.getAllByRole('button', { name: 'Merges' })[0])
    await userEvent.type(await screen.findByLabelText('Refs to fold in'), '8123')
    await userEvent.click(screen.getByRole('button', { name: 'Preview' }))
    await screen.findByRole('group', { name: 'Fold preview' })

    await userEvent.click(screen.getByRole('button', { name: 'Cancel' }))

    expect(
      screen.queryByRole('group', { name: 'Fold preview' }),
    ).not.toBeInTheDocument()
    expect(wrote()).toHaveLength(1)
    expect(instance.talkgroups).toHaveLength(2)
  })

  /** **A Ref nothing answers to is shown as what it is.** The counts cannot
   *  tell it from a fold — and it is what a Ref belonging to another System
   *  looks like, which is the mistake this preview exists to catch. */
  it('says when a ref would be recorded rather than folded', async () => {
    instance.talkgroup({ ref: 100, label: 'Fire Dispatch' })
    signedIn(<AdminTalkgroupsScreen />)
    await screen.findByRole('list', { name: 'Talkgroups' })
    await userEvent.click(screen.getAllByRole('button', { name: 'Merges' })[0])

    await userEvent.type(await screen.findByLabelText('Refs to fold in'), '8123')
    await userEvent.click(screen.getByRole('button', { name: 'Preview' }))

    const preview = within(await screen.findByRole('group', { name: 'Fold preview' }))
    expect(preview.getByText(/no channel/i)).toBeInTheDocument()
  })

  /** Unfolding goes through the same preview, because it moves Calls too —
   *  back to the channel they arrived under. */
  it('previews an unfold before restoring the channel', async () => {
    const owner = instance.talkgroup({ ref: 100, label: 'Fire Dispatch' })
    instance.members.set(owner.id, [{ ref: 8123, label: 'TAC 3' }])
    signedIn(<AdminTalkgroupsScreen />)
    await screen.findByRole('list', { name: 'Talkgroups' })
    await userEvent.click(screen.getByRole('button', { name: 'Merges' }))

    await userEvent.click(await screen.findByRole('button', { name: 'Unfold 8123' }))

    await screen.findByRole('group', { name: 'Fold preview' })
    expect(wrote().map((it) => [it.path, it.body])).toEqual([
      [`/api/admin/talkgroups/${owner.id}/members?dryRun`, { fold: [], unfold: [8123] }],
    ])
  })

  /** **Bulk folding is one request** (spec US 46's argument, for merges): the
   *  Operator selects the churn rows plus the real channel, says which
   *  survives, and the rest become that one request's fold list. */
  it('folds a selection into the channel that survives', async () => {
    const owner = instance.talkgroup({ ref: 100, label: 'Fire Dispatch' })
    const churn = [8001, 8002, 8003].map((ref) =>
      instance.talkgroup({ ref, calls: 3 }),
    )
    signedIn(<AdminTalkgroupsScreen />)
    await screen.findByRole('list', { name: 'Talkgroups' })
    for (const row of [owner, ...churn]) {
      await userEvent.click(
        screen.getByRole('checkbox', { name: `Select ${row.label ?? row.ref}` }),
      )
    }

    await userEvent.selectOptions(
      screen.getByLabelText('Fold into'),
      String(owner.id),
    )
    await userEvent.click(screen.getByRole('button', { name: 'Preview fold' }))
    await screen.findByRole('group', { name: 'Fold preview' })
    await userEvent.click(screen.getByRole('button', { name: /^Fold/ }))

    await waitFor(() => expect(instance.talkgroups).toHaveLength(1))
    expect(wrote().at(-1)).toEqual({
      method: 'POST',
      path: `/api/admin/talkgroups/${owner.id}/members`,
      body: { fold: [8001, 8002, 8003], unfold: [] },
    })
  })

  /** **A Ref is unique only within its System**, so a selection spanning two
   *  cannot be folded: the other System's number would resolve to no channel
   *  here and record as a bare member Ref — a row that looks perfectly ordinary
   *  and did nothing the Operator wanted. */
  it('refuses to offer a fold across two systems', async () => {
    const first = instance.system({ ref: 11, label: 'Fulton' })
    const second = instance.system({ ref: 12, label: 'Coweta' })
    instance.talkgroup({ ref: 100, label: 'Fire Dispatch', systemId: first.id })
    instance.talkgroup({
      ref: 200,
      label: 'Coweta Fire',
      systemId: second.id,
      systemRef: 12,
    })
    signedIn(<AdminTalkgroupsScreen />)
    await screen.findByRole('list', { name: 'Talkgroups' })

    await userEvent.click(screen.getByRole('checkbox', { name: 'Select Fire Dispatch' }))
    await userEvent.click(screen.getByRole('checkbox', { name: 'Select Coweta Fire' }))

    expect(screen.getByLabelText('Fold into')).toBeDisabled()
    expect(screen.getByText(/one system at a time/i)).toBeInTheDocument()
  })
})

describe("a unit's ranges", () => {
  /** A fleet numbers its radios in blocks, and this is where the block is
   *  written down — the half of #45 that had only a CSV until now. */
  it('lists and adds the spans an apparatus answers to', async () => {
    const unit = instance.unit({ ref: 1200, label: 'Engine 1' })
    instance.ranges.set(unit.id, [{ from: 1201, to: 1249 }])
    signedIn(<UnitsScreen />)
    await screen.findByRole('list', { name: 'Units' })
    await userEvent.click(screen.getByRole('button', { name: 'Ranges' }))

    const ranges = within(await screen.findByRole('list', { name: 'Ranges' }))
    expect(ranges.getByText('1201–1249')).toBeInTheDocument()

    await userEvent.type(screen.getByLabelText('From'), '1250')
    await userEvent.type(screen.getByLabelText('To'), '1299')
    await userEvent.click(screen.getByRole('button', { name: 'Add range' }))

    await waitFor(() =>
      expect(instance.ranges.get(unit.id)).toEqual([
        { from: 1201, to: 1249 },
        { from: 1250, to: 1299 },
      ]),
    )
    expect(wrote().at(-1)?.body).toEqual({
      add: [{ from: 1250, to: 1299 }],
      remove: [],
    })
  })

  /** Removing one un-names the radios it covered — reversible, which is why
   *  this is the one merge edit with nothing to confirm. */
  it('removes a span', async () => {
    const unit = instance.unit({ ref: 1200, label: 'Engine 1' })
    instance.ranges.set(unit.id, [{ from: 1201, to: 1249 }])
    signedIn(<UnitsScreen />)
    await screen.findByRole('list', { name: 'Units' })
    await userEvent.click(screen.getByRole('button', { name: 'Ranges' }))

    await userEvent.click(await screen.findByRole('button', { name: 'Remove 1201–1249' }))

    await waitFor(() => expect(instance.ranges.get(unit.id)).toEqual([]))
    expect(wrote().at(-1)?.body).toEqual({
      add: [],
      remove: [{ from: 1201, to: 1249 }],
    })
  })

  /** **An overlap is refused whole**, and the sentence comes from the server —
   *  it is the side that knows which Range is in the way. */
  it("shows the server's sentence when a range overlaps", async () => {
    const engine = instance.unit({ ref: 1200, label: 'Engine 1' })
    instance.ranges.set(engine.id, [{ from: 4400, to: 4499 }])
    signedIn(<UnitsScreen />)
    await screen.findByRole('list', { name: 'Units' })
    await userEvent.click(screen.getByRole('button', { name: 'Ranges' }))
    await screen.findByRole('list', { name: 'Ranges' })

    await userEvent.type(screen.getByLabelText('From'), '4460')
    await userEvent.type(screen.getByLabelText('To'), '4470')
    await userEvent.click(screen.getByRole('button', { name: 'Add range' }))

    expect(await screen.findByRole('alert')).toHaveTextContent(/overlaps/i)
    expect(instance.ranges.get(engine.id)).toEqual([{ from: 4400, to: 4499 }])
  })
})

describe('when a merge is refused', () => {
  /** A preview that the server refuses shows the refusal instead of a
   *  confirmation — and offers nothing to confirm, which is the point: there is
   *  no promise to keep. */
  it("shows the server's sentence when a preview is refused", async () => {
    const owner = instance.talkgroup({ ref: 100, label: 'Fire Dispatch' })
    signedIn(<AdminTalkgroupsScreen />)
    await screen.findByRole('list', { name: 'Talkgroups' })
    await userEvent.click(screen.getByRole('button', { name: 'Merges' }))
    server.use(
      http.post(`${ORIGIN}/api/admin/talkgroups/${owner.id}/members`, () =>
        refusal(
          409,
          'talkgroup-ref-taken',
          'another talkgroup already answers to 8123',
        ),
      ),
    )

    await userEvent.type(await screen.findByLabelText('Refs to fold in'), '8123')
    await userEvent.click(screen.getByRole('button', { name: 'Preview' }))

    expect(await screen.findByRole('alert')).toHaveTextContent(
      'another talkgroup already answers to 8123',
    )
    expect(
      screen.queryByRole('group', { name: 'Fold preview' }),
    ).not.toBeInTheDocument()
  })

  /** **A refused commit takes the preview down with it.** Whatever the server
   *  refused, what was on screen has stopped being a promise about the run that
   *  follows — leaving the confirmation up would invite a second click on a
   *  sentence that is no longer true. */
  it('drops the preview when the fold itself is refused', async () => {
    const owner = instance.talkgroup({ ref: 100, label: 'Fire Dispatch' })
    instance.talkgroup({ ref: 8123, label: 'TAC 3', calls: 412 })
    signedIn(<AdminTalkgroupsScreen />)
    await screen.findByRole('list', { name: 'Talkgroups' })
    await userEvent.click(screen.getAllByRole('button', { name: 'Merges' })[0])
    await userEvent.type(await screen.findByLabelText('Refs to fold in'), '8123')
    await userEvent.click(screen.getByRole('button', { name: 'Preview' }))
    await screen.findByRole('group', { name: 'Fold preview' })
    // Only the real write is refused; the preview already happened.
    server.use(
      http.post(`${ORIGIN}/api/admin/talkgroups/${owner.id}/members`, ({ request }) =>
        new URL(request.url).searchParams.has('dryRun')
          ? HttpResponse.json({
              dryRun: true,
              folded: 1,
              unfolded: 0,
              callsRepointed: 412,
              moved: [],
            })
          : refusal(409, 'talkgroup-ref-taken', 'somebody folded it first'),
      ),
    )

    await userEvent.click(screen.getByRole('button', { name: /^Fold/ }))

    expect(await screen.findByRole('alert')).toHaveTextContent(
      'somebody folded it first',
    )
    expect(instance.talkgroups).toHaveLength(2)
  })

  /** An empty box is not a fold. Submitting one must not post a delta that
   *  names nothing — a request whose only possible answer is "nothing
   *  happened". */
  it('sends nothing when no refs were typed', async () => {
    instance.talkgroup({ ref: 100, label: null })
    signedIn(<AdminTalkgroupsScreen />)
    await screen.findByRole('list', { name: 'Talkgroups' })
    await userEvent.click(screen.getByRole('button', { name: 'Merges' }))

    fireEvent.submit(await screen.findByRole('form', { name: 'Fold refs into 100' }))

    expect(wrote()).toEqual([])
  })

  /** ...and the same for a blank span. Both boxes are `required`, so a browser
   *  refuses first — but a form is submittable from script, and `0-0` is a span
   *  nobody asked for. */
  it('sends nothing when a range is left blank', async () => {
    instance.unit({ ref: 1200, label: null })
    signedIn(<UnitsScreen />)
    await screen.findByRole('list', { name: 'Units' })
    await userEvent.click(screen.getByRole('button', { name: 'Ranges' }))

    fireEvent.submit(await screen.findByRole('form', { name: 'Add a range to 1200' }))

    expect(wrote()).toEqual([])
  })

  /** The preview counts in words an Operator reads, so a single Call is "1
   *  call" — and a chain fold says what is coming with the channel, because a
   *  Ref arriving unasked-for is the thing they would otherwise find later. */
  it('names one call singular and the refs a fold brings with it', async () => {
    const owner = instance.talkgroup({ ref: 100, label: 'Fire Dispatch' })
    signedIn(<AdminTalkgroupsScreen />)
    await screen.findByRole('list', { name: 'Talkgroups' })
    await userEvent.click(screen.getByRole('button', { name: 'Merges' }))
    server.use(
      http.post(`${ORIGIN}/api/admin/talkgroups/${owner.id}/members`, () =>
        HttpResponse.json({
          dryRun: true,
          folded: 1,
          unfolded: 0,
          callsRepointed: 1,
          moved: [
            {
              ref: 8123,
              movement: 'folded',
              label: 'TAC 3',
              calls: 1,
              carried: [9000, 9001],
            },
          ],
        }),
      ),
    )

    await userEvent.type(await screen.findByLabelText('Refs to fold in'), '8123')
    await userEvent.click(screen.getByRole('button', { name: 'Preview' }))

    const preview = within(await screen.findByRole('group', { name: 'Fold preview' }))
    expect(preview.getByRole('listitem')).toHaveTextContent(
      '8123 · TAC 3 — folded in, bringing 1 call — brings 9000, 9001 with it',
    )
    expect(preview.getByText('1 call would move.')).toBeInTheDocument()
  })
})

/** Most radios own no block at all — they are one apparatus with one id — so
 *  the empty state is the common one and has to read as a fact rather than as a
 *  list that failed to load. */
it('says so when an apparatus owns no ranges', async () => {
  instance.unit({ ref: 1200, label: 'Engine 1' })
  signedIn(<UnitsScreen />)
  await screen.findByRole('list', { name: 'Units' })

  await userEvent.click(screen.getByRole('button', { name: 'Ranges' }))

  expect(await screen.findByText('Only its own radio id.')).toBeInTheDocument()
  expect(screen.queryByRole('list', { name: 'Ranges' })).not.toBeInTheDocument()
})

describe('picking the channel that survives a bulk fold', () => {
  /** **The box always names the channel that would actually survive.**
   *
   *  It is the only place the survivor appears, and the selection changes
   *  underneath it — so a remembered id could name a row no longer selected
   *  while a different one silently absorbed the rest. Derived from the
   *  selection instead, and asserted without touching the control, because the
   *  untouched state is the one an Operator is most likely to fold from.
   */
  it('folds into the shown channel without the dropdown being touched', async () => {
    const owner = instance.talkgroup({ ref: 100, label: 'Fire Dispatch' })
    instance.talkgroup({ ref: 8001, calls: 3 })
    signedIn(<AdminTalkgroupsScreen />)
    await screen.findByRole('list', { name: 'Talkgroups' })
    await userEvent.click(screen.getByRole('checkbox', { name: /Select all/ }))

    expect(screen.getByLabelText('Fold into')).toHaveValue(String(owner.id))

    await userEvent.click(screen.getByRole('button', { name: 'Preview fold' }))

    // The confirmation names where the refs are going, not only what is going.
    const preview = within(await screen.findByRole('group', { name: 'Fold preview' }))
    expect(preview.getByText(/Into/)).toHaveTextContent('Into Fire Dispatch:')
    expect(wrote().at(-1)).toEqual({
      method: 'POST',
      path: `/api/admin/talkgroups/${owner.id}/members?dryRun`,
      body: { fold: [8001], unfold: [] },
    })
  })

  /** **Select-all is the other half of "foldable in bulk".** A system that mints
   *  a TGID per patch event leaves dozens of near-identical rows, and ticking
   *  forty boxes is the afternoon spec US 46 exists to save. */
  it('selects and deselects every row on the page at once', async () => {
    instance.talkgroup({ ref: 100, label: 'Fire Dispatch' })
    for (const ref of [8001, 8002, 8003]) instance.talkgroup({ ref })
    signedIn(<AdminTalkgroupsScreen />)
    await screen.findByRole('list', { name: 'Talkgroups' })

    await userEvent.click(screen.getByRole('checkbox', { name: /Select all 4/ }))

    expect(screen.getByText('4 selected')).toBeInTheDocument()

    await userEvent.click(screen.getByRole('checkbox', { name: /Select all 4/ }))

    expect(screen.queryByText('4 selected')).not.toBeInTheDocument()
  })

  /** One row selected is not a merge — there is nothing to fold into it — so the
   *  control stays away rather than offering a fold of nothing. */
  it('offers no fold for a single selected row', async () => {
    instance.talkgroup({ ref: 100, label: 'Fire Dispatch' })
    signedIn(<AdminTalkgroupsScreen />)
    await screen.findByRole('list', { name: 'Talkgroups' })

    await userEvent.click(screen.getByRole('checkbox', { name: 'Select Fire Dispatch' }))

    expect(screen.getByText('1 selected')).toBeInTheDocument()
    expect(screen.queryByLabelText('Fold into')).not.toBeInTheDocument()
  })
})

// ---------------------------------------------------------------------------
// The configuration document (#51, spec US 47)
// ---------------------------------------------------------------------------

describe('carrying the configuration', () => {
  /** **Backing up is one button**, and what comes down is the file an Operator
   *  keeps — so the click has to produce a real download rather than a page
   *  that renders the JSON. */
  it('downloads the configuration as a named file', async () => {
    instance.talkgroup({ ref: 100, label: 'Fire Dispatch' })
    const saved: { name: string; text: string }[] = []
    captureDownloads(saved)
    signedIn(<AdminScreen />)

    await userEvent.click(await screen.findByRole('button', { name: 'Export' }))

    await waitFor(() => expect(saved).toHaveLength(1))
    expect(saved[0].name).toMatch(/^radio-scout-config.*\.json$/)
    expect(JSON.parse(saved[0].text).version).toBe(1)
  })

  /** **A restore is previewed first.** It is the one admin action that touches
   *  every entity at once, so the counts an Operator sees before committing are
   *  a `?dryRun` of the same transaction — and nothing is written until they say
   *  so again. */
  it('previews an imported document before applying it', async () => {
    signedIn(<AdminScreen />)
    await screen.findByRole('button', { name: 'Export' })

    await upload(a_document())

    const preview = within(await screen.findByRole('group', { name: 'Import preview' }))
    expect(preview.getByText(/1 system/)).toBeInTheDocument()
    expect(preview.getByText(/2 talkgroups/)).toBeInTheDocument()
    // Previewed, not performed.
    expect(wrote().map((it) => it.path)).toEqual([
      '/api/admin/config/import?dryRun',
    ])
    expect(instance.talkgroups).toHaveLength(0)
  })

  /** ...and confirming sends the identical document for real. */
  it('applies the document once the preview is confirmed', async () => {
    signedIn(<AdminScreen />)
    await screen.findByRole('button', { name: 'Export' })
    await upload(a_document())
    await screen.findByRole('group', { name: 'Import preview' })

    await userEvent.click(screen.getByRole('button', { name: /^Import/ }))

    await waitFor(() => expect(instance.talkgroups).toHaveLength(2))
    expect(wrote().map((it) => it.path)).toEqual([
      '/api/admin/config/import?dryRun',
      '/api/admin/config/import',
    ])
  })

  /** Backing out writes nothing — the confirmation is a decision point. */
  it('writes nothing when a preview is dismissed', async () => {
    signedIn(<AdminScreen />)
    await screen.findByRole('button', { name: 'Export' })
    await upload(a_document())
    await screen.findByRole('group', { name: 'Import preview' })

    await userEvent.click(screen.getByRole('button', { name: 'Cancel' }))

    expect(
      screen.queryByRole('group', { name: 'Import preview' }),
    ).not.toBeInTheDocument()
    expect(wrote()).toHaveLength(1)
  })

  /** **A refused entry is shown with its path**, because a document is a file an
   *  Operator has open in an editor and "something was wrong" sends them
   *  hunting through a county's worth of JSON. */
  it('shows which entry the server would not take', async () => {
    signedIn(<AdminScreen />)
    await screen.findByRole('button', { name: 'Export' })
    server.use(
      http.post(`${ORIGIN}/api/admin/config/import`, () =>
        HttpResponse.json({
          dryRun: true,
          systems: { created: 1, updated: 0, unchanged: 0 },
          talkgroups: { created: 1, updated: 0, unchanged: 0 },
          units: { created: 0, updated: 0, unchanged: 0 },
          groupsCreated: 0,
          tagsCreated: 0,
          apiKeys: [],
          rejected: [
            {
              at: 'systems[0].talkgroups[1]',
              reason: 'unknown-led',
              detail: '"puce" is not an LED colour',
            },
          ],
        }),
      ),
    )

    await upload(a_document())

    const preview = within(await screen.findByRole('group', { name: 'Import preview' }))
    expect(preview.getByText('systems[0].talkgroups[1]')).toBeInTheDocument()
    expect(preview.getByText(/puce/)).toBeInTheDocument()
  })

  /** **A re-issued key is shown once**, beside the label that says which
   *  recorder it belongs to — the file could not carry the secret, so this is
   *  the only sight of it there will be. */
  it('shows each re-issued key exactly once, with its label', async () => {
    signedIn(<AdminScreen />)
    await screen.findByRole('button', { name: 'Export' })
    server.use(
      http.post(`${ORIGIN}/api/admin/config/import`, ({ request }) =>
        HttpResponse.json({
          dryRun: new URL(request.url).searchParams.has('dryRun'),
          systems: { created: 1, updated: 0, unchanged: 0 },
          talkgroups: { created: 0, updated: 0, unchanged: 0 },
          units: { created: 0, updated: 0, unchanged: 0 },
          groupsCreated: 0,
          tagsCreated: 0,
          apiKeys: new URL(request.url).searchParams.has('dryRun')
            ? []
            : [{ id: 1, key: 'issued-secret-0001', label: 'the pi', systemRef: 11, disabled: false, createdAtMs: 0 }],
          rejected: [],
        }),
      ),
    )
    await upload(a_document())
    await screen.findByRole('group', { name: 'Import preview' })

    await userEvent.click(screen.getByRole('button', { name: /^Import/ }))

    const issued = within(await screen.findByRole('group', { name: 'Re-issued keys' }))
    expect(issued.getByText(/the pi/)).toBeInTheDocument()
    expect(issued.getByText(/issued-secret-0001/)).toBeInTheDocument()
  })

  /** A file that is not JSON at all never reaches the server: the browser can
   *  tell, and a 422 an Operator has to interpret is worse than a sentence. */
  it('refuses a file that is not a document without asking the server', async () => {
    signedIn(<AdminScreen />)
    await screen.findByRole('button', { name: 'Export' })

    await upload('this is not json')

    expect(await screen.findByRole('alert')).toHaveTextContent(/not a configuration/i)
    expect(wrote()).toEqual([])
  })
})

/** The document every import test uploads. */
function a_document() {
  return JSON.stringify({
    version: 1,
    systems: [
      {
        ref: 11,
        label: 'Fulton',
        autoPopulate: true,
        talkgroups: [{ ref: 100, label: 'Fire Dispatch' }, { ref: 200 }],
      },
    ],
  })
}

/** Pick `text` as the import file. */
async function upload(text: string) {
  const input = screen.getByLabelText('Import a configuration document')
  await userEvent.upload(
    input,
    new File([text], 'radio-scout-config.json', { type: 'application/json' }),
  )
}

/** Record what an anchor-click download would have saved, since jsdom has no
 *  Downloads folder — the object URL is created and revoked either way.
 *
 *  Through `vi.stubGlobal` and `vi.spyOn` so the config's `restoreMocks` really
 *  puts them back: a hand-assigned `HTMLAnchorElement.prototype.click` would
 *  survive into every test after this one. */
function captureDownloads(saved: { name: string; text: string }[]) {
  const blobs = new Map<string, Blob>()
  let next = 0
  // jsdom implements neither, and `vi.spyOn` needs something to stand on — so
  // they are defined once (a no-op is enough for the shape) and then spied,
  // which is what `restoreMocks` can put back.
  for (const name of ['createObjectURL', 'revokeObjectURL'] as const) {
    if (!(name in URL)) {
      Object.defineProperty(URL, name, {
        value: () => '',
        configurable: true,
        writable: true,
      })
    }
  }
  vi.spyOn(URL, 'createObjectURL').mockImplementation((blob: Blob | MediaSource) => {
    const url = `blob:${next++}`
    blobs.set(url, blob as Blob)
    return url
  })
  vi.spyOn(URL, 'revokeObjectURL').mockImplementation(() => {})
  vi.spyOn(HTMLAnchorElement.prototype, 'click').mockImplementation(function (
    this: HTMLAnchorElement,
  ) {
    const blob = blobs.get(this.href)
    if (blob) void blob.text().then((text) => saved.push({ name: this.download, text }))
  })
}

describe('when a configuration document is refused', () => {
  /** **A failed export says so.** This is a `fetch` rather than an
   *  `<a download>` precisely so a session that lapsed while the screen was
   *  open can be reported — a navigation that 401s leaves an Operator staring
   *  at a button that did nothing. */
  it('says so when the export cannot be fetched', async () => {
    // First in the array wins within one `use` call, so the refusal has to
    // precede the handler it is standing in for.
    server.use(
      http.get(`${ORIGIN}/api/admin/config`, () => new HttpResponse(null, { status: 401 })),
      ...curationHandlers(instance),
    )
    renderWithProviders(<AdminScreen />)

    await userEvent.click(await screen.findByRole('button', { name: 'Export' }))

    expect(await screen.findByRole('alert')).toHaveTextContent(/session may have expired/i)
  })

  /** The visible control is the one an Operator uses; the input behind it is a
   *  hidden implementation detail, so the button has to really open it. */
  it('opens the file picker from the visible button', async () => {
    signedIn(<AdminScreen />)
    await screen.findByRole('button', { name: 'Export' })
    const input = screen.getByLabelText('Import a configuration document')
    const opened = vi.spyOn(input as HTMLInputElement, 'click')

    await userEvent.click(screen.getByRole('button', { name: 'Choose a file…' }))

    expect(opened).toHaveBeenCalled()
  })

  /** A document the server will not read at all — the wrong version, say —
   *  shows the server's own sentence and offers nothing to confirm, because
   *  there is nothing it promised to do. */
  it("shows the server's sentence when a preview is refused", async () => {
    signedIn(<AdminScreen />)
    await screen.findByRole('button', { name: 'Export' })
    server.use(
      http.post(`${ORIGIN}/api/admin/config/import`, () =>
        refusal(
          400,
          'unknown-document-version',
          'this is a version 99 configuration document, and this instance reads version 1',
        ),
      ),
    )

    await upload(a_document())

    expect(await screen.findByRole('alert')).toHaveTextContent(/version 99/)
    expect(
      screen.queryByRole('group', { name: 'Import preview' }),
    ).not.toBeInTheDocument()
  })

  /** **A refused commit takes the preview down with it**, for the reason a
   *  refused fold does: whatever the server refused, what is on screen has
   *  stopped being a promise about the run that follows. */
  it('drops the preview when the import itself is refused', async () => {
    signedIn(<AdminScreen />)
    await screen.findByRole('button', { name: 'Export' })
    await upload(a_document())
    await screen.findByRole('group', { name: 'Import preview' })
    // Only the real write is refused; the preview already happened.
    server.use(
      http.post(`${ORIGIN}/api/admin/config/import`, ({ request }) =>
        new URL(request.url).searchParams.has('dryRun')
          ? HttpResponse.json({ dryRun: true })
          : refusal(409, 'talkgroup-ref-taken', 'somebody curated it first'),
      ),
    )

    await userEvent.click(screen.getByRole('button', { name: 'Import' }))

    expect(await screen.findByRole('alert')).toHaveTextContent(
      'somebody curated it first',
    )
    expect(
      screen.queryByRole('group', { name: 'Import preview' }),
    ).not.toBeInTheDocument()
  })
})

// ---------------------------------------------------------------------------
// Downstream peers (#52, spec US 1-2)
// ---------------------------------------------------------------------------

describe('downstreams', () => {
  /** Adding a peer: where it is, the key *it* issued us, and what to send it.
   *
   *  The key is typed into a masked field and posted once. This is the only
   *  moment it is on screen — nothing reads it back, which is the whole
   *  difference from rdio-scanner, whose admin payload carries every peer's
   *  credential in plaintext. */
  it('adds a peer scoped to one system', async () => {
    signedIn(<DownstreamsScreen />)
    await screen.findByLabelText('Address')

    await userEvent.type(screen.getByLabelText('What is it'), 'county mirror')
    await userEvent.type(
      screen.getByLabelText('Address'),
      'https://peer.example',
    )
    await userEvent.type(
      screen.getByLabelText('The key that peer issued you'),
      'the-peers-key',
    )
    await userEvent.click(screen.getByRole('button', { name: 'Add a system' }))
    await userEvent.type(screen.getByLabelText('System ref'), '11')
    await userEvent.click(screen.getByRole('button', { name: 'Add' }))

    await waitFor(() => expect(wrote()).toHaveLength(1))
    expect(wrote()[0]).toEqual({
      method: 'POST',
      path: '/api/admin/downstreams',
      body: {
        label: 'county mirror',
        url: 'https://peer.example',
        apiKey: 'the-peers-key',
        scope: { all: false, sel: { 11: { '*': true } } },
      },
    })
  })

  /** Naming individual channels, then dropping the row again — the two edits
   *  the row list exists for. Refs are read as they are typed rather than
   *  refused mid-word, so what the form understood is on screen. */
  it('names channels within a system, and removes the row again', async () => {
    signedIn(<DownstreamsScreen />)
    await screen.findByLabelText('Address')
    await userEvent.type(screen.getByLabelText('Address'), 'https://peer')
    await userEvent.type(
      screen.getByLabelText('The key that peer issued you'),
      'k',
    )
    await userEvent.click(screen.getByRole('button', { name: 'Add a system' }))
    await userEvent.type(screen.getByLabelText('System ref'), '11')

    const channels = screen.getByLabelText('Talkgroup refs (blank = all)')
    await userEvent.type(channels, '100, 101')
    expect(channels).toHaveValue('100, 101')

    await userEvent.click(screen.getByRole('button', { name: 'Add' }))
    await waitFor(() => expect(wrote()).toHaveLength(1))
    expect((wrote()[0].body as { scope: unknown }).scope).toEqual({
      all: false,
      sel: { 11: { 100: true, 101: true } },
    })

    // ...and taking the row away takes the system with it.
    await userEvent.click(screen.getAllByRole('button', { name: 'Remove' })[0])
    expect(
      screen.queryByLabelText('Talkgroup refs (blank = all)'),
    ).not.toBeInTheDocument()
  })

  /** "Forward everything" is one checkbox, because it is what most operators
   *  want and the row list would be an empty gesture for it. */
  it('forwards everything when asked to', async () => {
    signedIn(<DownstreamsScreen />)
    await screen.findByLabelText('Address')

    await userEvent.type(screen.getByLabelText('Address'), 'https://peer')
    await userEvent.type(
      screen.getByLabelText('The key that peer issued you'),
      'k',
    )
    await userEvent.click(screen.getByLabelText('Forward everything'))
    await userEvent.click(screen.getByRole('button', { name: 'Add' }))

    await waitFor(() => expect(wrote()).toHaveLength(1))
    expect((wrote()[0].body as { scope: unknown }).scope).toEqual({
      all: true,
      sel: {},
    })
  })

  /** Turning "everything" back off leaves nothing selected rather than
   *  whatever was there before — the safe direction, and the only one that is
   *  honest: the rows it would restore were discarded when the box was ticked. */
  it('unticking everything forwards nothing until a system is named', async () => {
    signedIn(<DownstreamsScreen />)
    await screen.findByLabelText('Address')

    await userEvent.click(screen.getByLabelText('Forward everything'))
    await userEvent.click(screen.getByLabelText('Forward everything'))

    await userEvent.type(screen.getByLabelText('Address'), 'https://peer')
    await userEvent.type(
      screen.getByLabelText('The key that peer issued you'),
      'k',
    )
    await userEvent.click(screen.getByRole('button', { name: 'Add' }))

    await waitFor(() => expect(wrote()).toHaveLength(1))
    expect((wrote()[0].body as { scope: unknown }).scope).toEqual({
      all: false,
      sel: {},
    })
  })

  /** A listing that cannot be read says so, rather than rendering an empty list
   *  an Operator would read as "no peers configured". */
  it('says so when the listing cannot be read', async () => {
    server.use(...curationHandlers(instance))
    server.use(
      http.get(`${ORIGIN}/api/admin/downstreams`, () =>
        HttpResponse.json({ error: 'nope' }, { status: 500 }),
      ),
    )
    renderWithProviders(<DownstreamsScreen />)

    expect(await screen.findByRole('alert')).toHaveTextContent(
      /could not be read/,
    )
  })

  /** **The health an Operator acts on**, since #70's status page does not exist
   *  yet. The queue depth is the number that matters: a peer down for an hour
   *  reads as an hour of Calls *waiting*, which is the difference from rdio
   *  dropping them. */
  it('shows a struggling peer its queue depth and its last failure', async () => {
    instance.downstream({
      label: 'county mirror',
      queued: 12,
      consecutiveFailures: 4,
      lastFailure: 'sink-refused (503)',
      lastSuccessMs: null,
    })
    signedIn(<DownstreamsScreen />)

    const list = await screen.findByRole('list', { name: 'Downstreams' })
    expect(within(list).getByText(/12 queued/)).toBeInTheDocument()
    expect(within(list).getByText(/4 failed/)).toBeInTheDocument()
    expect(
      within(list).getByText(/sink-refused \(503\)/),
    ).toBeInTheDocument()
    expect(within(list).getByText(/never delivered/)).toBeInTheDocument()
  })

  /** A peer restored from a backup arrives with no credential, because a backup
   *  carries a peer's shape and never its key. Saying so is the only way to tell
   *  it from a peer whose key is simply wrong. */
  it('says when a peer still needs its key', async () => {
    instance.downstream({ hasKey: false, disabled: true })
    signedIn(<DownstreamsScreen />)

    const list = await screen.findByRole('list', { name: 'Downstreams' })
    expect(within(list).getByText(/needs its key/)).toBeInTheDocument()
  })

  /** **Re-scoping must not mean re-typing a credential the screen can never show
   *  again.** A blank key field is omitted from the PATCH entirely, which is
   *  what makes the server leave the stored one alone. */
  it('re-scopes a peer without touching its key', async () => {
    instance.downstream({ label: 'county mirror' })
    signedIn(<DownstreamsScreen />)
    await screen.findByRole('list', { name: 'Downstreams' })

    await userEvent.click(screen.getByRole('button', { name: 'Edit' }))
    const form = await screen.findByRole('form', {
      name: 'Edit county mirror',
    })
    await userEvent.clear(within(form).getByLabelText('What is it'))
    await userEvent.type(within(form).getByLabelText('What is it'), 'the mirror')
    await userEvent.clear(within(form).getByLabelText('Address'))
    await userEvent.type(
      within(form).getByLabelText('Address'),
      'https://elsewhere.example',
    )
    await userEvent.click(within(form).getByLabelText('Forward everything'))
    await userEvent.click(within(form).getByRole('button', { name: 'Save' }))

    await waitFor(() => expect(wrote()).toHaveLength(1))
    const body = wrote()[0].body as Record<string, unknown>
    expect(body).not.toHaveProperty('apiKey')
    expect(body.label).toBe('the mirror')
    expect(body.url).toBe('https://elsewhere.example')
    expect(body.scope).toEqual({ all: true, sel: {} })
  })

  /** Clearing the label clears it — `null` rather than an absent field, which
   *  could only ever leave what is there. */
  it('clears a peer label', async () => {
    instance.downstream({ label: 'county mirror' })
    signedIn(<DownstreamsScreen />)
    await screen.findByRole('list', { name: 'Downstreams' })

    await userEvent.click(screen.getByRole('button', { name: 'Edit' }))
    const form = await screen.findByRole('form', { name: 'Edit county mirror' })
    await userEvent.clear(within(form).getByLabelText('What is it'))
    await userEvent.click(within(form).getByRole('button', { name: 'Save' }))

    await waitFor(() => expect(wrote()).toHaveLength(1))
    expect((wrote()[0].body as { label: unknown }).label).toBeNull()
  })

  /** An unlabelled peer is titled by its address, which is its identity
   *  anyway — a row headed by nothing would be unreachable. */
  it('titles an unlabelled peer by its address', async () => {
    instance.downstream({ label: null, url: 'https://peer.example' })
    signedIn(<DownstreamsScreen />)

    const list = await screen.findByRole('list', { name: 'Downstreams' })
    await userEvent.click(within(list).getByRole('button', { name: 'Edit' }))

    expect(
      await screen.findByRole('form', { name: 'Edit https://peer.example' }),
    ).toBeInTheDocument()
  })

  /** ...and typing one in *does* send it, which is how a mistyped key is fixed
   *  without losing the backlog behind it. */
  it('replaces a key when one is typed', async () => {
    instance.downstream({ label: 'county mirror' })
    signedIn(<DownstreamsScreen />)
    await screen.findByRole('list', { name: 'Downstreams' })

    await userEvent.click(screen.getByRole('button', { name: 'Edit' }))
    const form = await screen.findByRole('form', { name: 'Edit county mirror' })
    await userEvent.type(
      within(form).getByLabelText(/Replace the key/),
      'the-right-key',
    )
    await userEvent.click(within(form).getByRole('button', { name: 'Save' }))

    await waitFor(() => expect(wrote()).toHaveLength(1))
    expect((wrote()[0].body as { apiKey: string }).apiKey).toBe('the-right-key')
  })

  /** Switching a peer off empties its queue, so switching it back on a week
   *  later does not replay the week. The screen shows that immediately. */
  it('disables a peer and its queue goes with it', async () => {
    const peer = instance.downstream({ queued: 7 })
    signedIn(<DownstreamsScreen />)
    const list = await screen.findByRole('list', { name: 'Downstreams' })
    expect(within(list).getByText(/7 queued/)).toBeInTheDocument()

    await userEvent.click(screen.getByRole('button', { name: 'Disable' }))

    await waitFor(() => expect(wrote()).toHaveLength(1))
    expect(wrote()[0]).toEqual({
      method: 'PATCH',
      path: `/api/admin/downstreams/${peer.id}`,
      body: { disabled: true },
    })
    await waitFor(() =>
      expect(screen.queryByText(/7 queued/)).not.toBeInTheDocument(),
    )
    expect(await screen.findByText(/disabled/)).toBeInTheDocument()
  })

  it('removes a peer', async () => {
    const peer = instance.downstream({})
    signedIn(<DownstreamsScreen />)
    await screen.findByRole('list', { name: 'Downstreams' })

    await userEvent.click(screen.getByRole('button', { name: 'Remove' }))

    await waitFor(() => expect(wrote()).toHaveLength(1))
    expect(wrote()[0].method).toBe('DELETE')
    expect(wrote()[0].path).toBe(`/api/admin/downstreams/${peer.id}`)
  })

  /** A refusal is rendered from the server's own sentence, beside the form —
   *  the rule every other curation screen follows (#49). */
  it('renders the server refusal for a peer with no address', async () => {
    signedIn(<DownstreamsScreen />)
    await screen.findByLabelText('Address')

    await userEvent.type(
      screen.getByLabelText('The key that peer issued you'),
      'k',
    )
    await userEvent.click(screen.getByRole('button', { name: 'Add' }))

    expect(await screen.findByText(/url is required/)).toBeInTheDocument()
  })

  /** **A scope this form cannot draw is not silently narrowed.** The matrix can
   *  say "everything except this channel"; the row list cannot, so the editor
   *  says so and offers the one replacement it can make honestly. */
  it('refuses to edit a scope carrying exceptions', async () => {
    instance.downstream({
      label: 'county mirror',
      scope: { all: false, sel: { 11: { '*': true, 100: false } } },
    })
    signedIn(<DownstreamsScreen />)
    await screen.findByRole('list', { name: 'Downstreams' })

    await userEvent.click(screen.getByRole('button', { name: 'Edit' }))
    const form = await screen.findByRole('form', { name: 'Edit county mirror' })

    expect(
      within(form).getByText(/carries exceptions/),
    ).toBeInTheDocument()
    expect(
      within(form).queryByLabelText('System ref'),
    ).not.toBeInTheDocument()
  })

  /** **The edit form never shows a stored key back**, because it cannot: the
   *  server has no field for it in the listing. So the input opens empty and is
   *  labelled as a *replacement*, which is the honest offer — the alternative,
   *  a field pre-filled with something, would be a screen inventing a
   *  credential. */
  it('opens the key field empty, however long the peer has had one', async () => {
    instance.downstream({ label: 'county mirror', hasKey: true })
    signedIn(<DownstreamsScreen />)
    await screen.findByRole('list', { name: 'Downstreams' })

    await userEvent.click(screen.getByRole('button', { name: 'Edit' }))
    const form = await screen.findByRole('form', { name: 'Edit county mirror' })

    const key = within(form).getByLabelText(/Replace the key/)
    expect(key).toHaveValue('')
    expect(key).toHaveAttribute('type', 'password')
  })
})

// ---------------------------------------------------------------------------
// Webhooks (#54)
// ---------------------------------------------------------------------------

describe('webhooks', () => {
  /** Adding one: where it posts, in what shape, on what mark, and about which
   *  Calls.
   *
   *  The address is typed into a masked field and posted once. This is the only
   *  moment it is on screen — a Discord webhook URL ends in a token, so the
   *  listing can never show it back, which is a notch stricter than the
   *  Downstream key beside it (there the URL is public and only the key is
   *  guarded). */
  it('adds a webhook watching for emergencies on one system', async () => {
    signedIn(<WebhooksScreen />)
    await screen.findByLabelText(/^Address/)

    await userEvent.type(screen.getByLabelText('What is it'), 'dispatch')
    await userEvent.type(
      screen.getByLabelText(/^Address/),
      'https://discord.com/api/webhooks/1/t0ken',
    )
    await userEvent.click(screen.getByRole('button', { name: 'Add a system' }))
    await userEvent.type(screen.getByLabelText('System ref'), '11')
    await userEvent.click(screen.getByRole('button', { name: 'Add' }))

    await waitFor(() => expect(wrote()).toHaveLength(1))
    expect(wrote()[0]).toEqual({
      method: 'POST',
      path: '/api/admin/webhooks',
      body: {
        label: 'dispatch',
        url: 'https://discord.com/api/webhooks/1/t0ken',
        format: 'radio-scout',
        // Ticked by default: it is the only mark there is, and a webhook
        // watching nothing is silently inert.
        marks: ['emergency'],
        scope: { all: false, sel: { 11: { '*': true } } },
      },
    })
  })

  /** The Discord shape is a choice on the same form, because "which shape" is
   *  the one thing an Operator has to decide that a Downstream never asks. */
  it('sends the discord shape when it is chosen', async () => {
    signedIn(<WebhooksScreen />)
    await screen.findByLabelText(/^Address/)

    await userEvent.type(screen.getByLabelText(/^Address/), 'https://hooks.test/x')
    await userEvent.selectOptions(
      screen.getByLabelText('Send it as'),
      'discord',
    )
    await userEvent.click(screen.getByLabelText('Forward everything'))
    await userEvent.click(screen.getByRole('button', { name: 'Add' }))

    await waitFor(() => expect(wrote()).toHaveLength(1))
    expect((wrote()[0].body as { format: unknown }).format).toBe('discord')
  })

  /** **An unusable address never leaves the browser**, and that is the point of
   *  checking it here as well as on the server: submitting is the last moment
   *  the Operator can see what they pasted, so being told before it disappears
   *  is the difference between fixing a typo and going back to Discord for the
   *  whole URL. */
  it('refuses an address it could never post to, without sending it', async () => {
    signedIn(<WebhooksScreen />)
    await screen.findByLabelText(/^Address/)

    await userEvent.type(screen.getByLabelText(/^Address/), 'discord.com/api/x')
    await userEvent.click(screen.getByRole('button', { name: 'Add' }))

    expect(await screen.findByRole('alert')).toHaveTextContent(/https:\/\//)
    expect(wrote()).toHaveLength(0)
  })

  /** ...and the server's own refusal is rendered from the server's sentence,
   *  because a mark this release does not know is something only it can name. */
  it('renders the server refusal for a mark it does not know', async () => {
    server.use(...curationHandlers(instance))
    server.use(
      http.post(`${ORIGIN}/api/admin/webhooks`, () =>
        refusal(400, 'unknown-mark', '"tone" is not a mark a Call can carry'),
      ),
    )
    renderWithProviders(<WebhooksScreen />)
    await screen.findByLabelText(/^Address/)

    await userEvent.type(screen.getByLabelText(/^Address/), 'https://hooks.test/x')
    await userEvent.click(screen.getByLabelText('Forward everything'))
    await userEvent.click(screen.getByRole('button', { name: 'Add' }))

    expect(await screen.findByText(/not a mark/)).toBeInTheDocument()
  })

  /** The listing carries the **host** and never the URL, plus the health an
   *  Operator acts on — and a webhook watching nothing is called out, because
   *  it is the one misconfiguration here that produces no error anywhere. */
  it('shows a host, the marks, and what is queued', async () => {
    instance.webhook({
      label: 'dispatch',
      host: 'discord.com',
      format: 'discord',
      queued: 4,
      consecutiveFailures: 2,
      lastFailure: 'sink-refused (429)',
    })
    instance.webhook({ label: 'inert', marks: [] })
    signedIn(<WebhooksScreen />)

    const rows = await screen.findAllByRole('listitem')

    expect(rows[0]).toHaveTextContent('discord.com')
    expect(rows[0]).toHaveTextContent('Discord message')
    expect(rows[0]).toHaveTextContent('4 queued')
    expect(rows[0]).toHaveTextContent('sink-refused (429)')
    expect(rows[1]).toHaveTextContent('watching nothing')
  })

  /** **Editing leaves the address alone**, which is the only thing that makes
   *  re-scoping possible at all: the screen can never show the credential
   *  again, so a blank field has to mean keep. */
  it('re-scopes a webhook without re-pasting its address', async () => {
    const hook = instance.webhook({ label: 'dispatch' })
    signedIn(<WebhooksScreen />)
    await userEvent.click(await screen.findByRole('button', { name: 'Edit' }))

    // Scoped to the edit form: the add form above it has a scope editor of its
    // own, and both spell the checkbox the same way.
    const form = screen.getByRole('form', { name: /^Edit / })
    await userEvent.click(within(form).getByLabelText('Forward everything'))
    await userEvent.click(within(form).getByRole('button', { name: 'Save' }))

    await waitFor(() => expect(wrote()).toHaveLength(1))
    expect(wrote()[0].path).toBe(`/api/admin/webhooks/${hook.id}`)
    expect(wrote()[0].body).not.toHaveProperty('url')
    expect((wrote()[0].body as { scope: unknown }).scope).toEqual({
      all: true,
      sel: {},
    })
  })

  /** Switching one off empties its queue, which is what "disabled" has to mean:
   *  a webhook off for a week and switched back on must not post a week of
   *  emergencies into somebody's chat room at once. */
  it('disables a webhook and drops what was queued for it', async () => {
    const hook = instance.webhook({ queued: 9 })
    signedIn(<WebhooksScreen />)

    await userEvent.click(await screen.findByRole('button', { name: 'Disable' }))

    await waitFor(() => expect(wrote()).toHaveLength(1))
    expect(wrote()[0].body).toEqual({ disabled: true })
    expect(await screen.findByText(/disabled/)).toBeInTheDocument()
    expect(instance.webhooks.find((it) => it.id === hook.id)?.queued).toBe(0)
  })

  /** Removing one, through the same road every other row takes. */
  it('removes a webhook', async () => {
    const hook = instance.webhook({})
    signedIn(<WebhooksScreen />)

    await userEvent.click(await screen.findByRole('button', { name: 'Remove' }))

    await waitFor(() => expect(wrote()).toHaveLength(1))
    expect(wrote()[0]).toEqual({
      method: 'DELETE',
      path: `/api/admin/webhooks/${hook.id}`,
      body: undefined,
    })
  })

  /** A refused delete is shown rather than swallowed — the row is still there,
   *  and an Operator who pressed Remove and saw nothing happen would press it
   *  again. */
  it('shows a refused delete', async () => {
    instance.webhook({ label: 'dispatch' })
    server.use(...curationHandlers(instance))
    server.use(
      http.delete(`${ORIGIN}/api/admin/webhooks/:id`, () =>
        refusal(404, 'webhook-not-found', 'no such webhook'),
      ),
    )
    renderWithProviders(<WebhooksScreen />)

    await userEvent.click(await screen.findByRole('button', { name: 'Remove' }))

    expect(await screen.findByText(/no such webhook/)).toBeInTheDocument()
  })

  /** A listing that cannot be read says so rather than reading as "none". */
  it('says so when the webhooks cannot be read', async () => {
    server.use(...curationHandlers(instance))
    server.use(
      http.get(`${ORIGIN}/api/admin/webhooks`, () =>
        HttpResponse.json({ error: 'nope' }, { status: 500 }),
      ),
    )
    renderWithProviders(<WebhooksScreen />)

    expect(await screen.findByRole('alert')).toHaveTextContent(
      /could not be read/,
    )
  })

  /** **Unticking the last mark leaves a webhook watching nothing**, which the
   *  server accepts and the listing calls out — so the form must be able to
   *  reach that state rather than silently keeping the last one. */
  it('lets a mark be unticked', async () => {
    signedIn(<WebhooksScreen />)
    await screen.findByLabelText(/^Address/)
    await userEvent.type(screen.getByLabelText(/^Address/), 'https://hooks.test/x')
    await userEvent.click(screen.getByLabelText('Forward everything'))

    const emergency = screen.getByLabelText('Emergency')
    await userEvent.click(emergency)
    expect(emergency).not.toBeChecked()

    // ...and ticked again, because a checkbox that only goes one way is worse
    // than no checkbox at all.
    await userEvent.click(emergency)
    expect(emergency).toBeChecked()

    await userEvent.click(emergency)
    await userEvent.click(screen.getByRole('button', { name: 'Add' }))

    await waitFor(() => expect(wrote()).toHaveLength(1))
    expect((wrote()[0].body as { marks: unknown }).marks).toEqual([])
  })

  /** Replacing the address is the other half of "blank means keep": when one
   *  *is* typed, it goes — and it is checked first, on the edit form as well as
   *  the add form. */
  it('replaces the address when a new one is typed, and refuses a bad one', async () => {
    const hook = instance.webhook({ label: 'dispatch' })
    signedIn(<WebhooksScreen />)
    await userEvent.click(await screen.findByRole('button', { name: 'Edit' }))
    const form = screen.getByRole('form', { name: /^Edit / })

    await userEvent.type(within(form).getByLabelText(/^Replace/), 'not-a-url')
    await userEvent.click(within(form).getByRole('button', { name: 'Save' }))

    expect(within(form).getByRole('alert')).toHaveTextContent(/https:\/\//)
    expect(wrote()).toHaveLength(0)

    await userEvent.clear(within(form).getByLabelText(/^Replace/))
    await userEvent.type(
      within(form).getByLabelText(/^Replace/),
      'https://hooks.test/new',
    )
    await userEvent.clear(within(form).getByLabelText('What is it'))
    await userEvent.click(within(form).getByRole('button', { name: 'Save' }))

    await waitFor(() => expect(wrote()).toHaveLength(1))
    expect(wrote()[0].body).toMatchObject({
      url: 'https://hooks.test/new',
      // Cleared on purpose: a blank label is an explicit `null`, which is how
      // one is removed at all.
      label: null,
    })
    expect(instance.webhooks.find((it) => it.id === hook.id)?.host).toBe(
      'hooks.test',
    )
  })

  /** The editor closes again, and a refused save is rendered under the form
   *  rather than swallowed. */
  it('closes the editor, and shows a refused save', async () => {
    instance.webhook({ label: 'dispatch' })
    server.use(...curationHandlers(instance))
    server.use(
      http.patch(`${ORIGIN}/api/admin/webhooks/:id`, () =>
        refusal(400, 'unknown-format', '"slack" is not a webhook format'),
      ),
    )
    renderWithProviders(<WebhooksScreen />)
    await userEvent.click(await screen.findByRole('button', { name: 'Edit' }))
    const form = screen.getByRole('form', { name: /^Edit / })

    await userEvent.click(within(form).getByRole('button', { name: 'Save' }))
    expect(await screen.findByText(/not a webhook format/)).toBeInTheDocument()

    await userEvent.click(screen.getByRole('button', { name: 'Close' }))
    expect(screen.queryByRole('form', { name: /^Edit / })).not.toBeInTheDocument()
  })

  /** A webhook with no label is titled by its **host**, and one whose stored
   *  address is not a URL at all falls back to its id — because a row with no
   *  title at all is one an Operator cannot act on, and this is exactly the
   *  webhook that most needs acting on. */
  it('titles a row by its host, then by its id', async () => {
    instance.webhook({ label: null, host: 'hooks.test' })
    instance.webhook({ label: null, host: null })
    signedIn(<WebhooksScreen />)

    const rows = await screen.findAllByRole('listitem')

    expect(rows[0]).toHaveTextContent('hooks.test')
    expect(rows[1]).toHaveTextContent(/Webhook \d+/)

    // ...and the editor names itself the same way, so an unlabelled row is
    // still something a screen reader can announce.
    await userEvent.click(
      within(rows[1]).getByRole('button', { name: 'Edit' }),
    )
    expect(
      screen.getByRole('form', { name: /^Edit webhook \d+$/ }),
    ).toBeInTheDocument()
  })
})

// ---------------------------------------------------------------------------
// Tone profiles (#55, spec US 20)
// ---------------------------------------------------------------------------

describe("a channel's tone profiles", () => {
  /** Open the Tones panel on the first Talkgroup row. */
  async function openTones() {
    signedIn(<AdminTalkgroupsScreen />)
    await screen.findByRole('list', { name: 'Talkgroups' })
    await userEvent.click(screen.getByRole('button', { name: 'Tones' }))
  }

  /** The headline: a station's paging sequence, written down where an Operator
   *  can see what is being listened for on a channel. */
  it('lists the stations paged on a channel, and what pages them', async () => {
    const channel = instance.talkgroup({ ref: 54241, label: 'Fire Dispatch' })
    instance.tones.set(channel.id, [
      {
        id: 900,
        talkgroupId: channel.id,
        label: 'Station 12',
        steps: [
          { hz: 1122.5, minMs: 800 },
          { hz: 1465.6, minMs: 2000 },
        ],
        tolerancePct: 2,
        gapMaxMs: 300,
        disabled: false,
        createdAtMs: 0,
      },
    ])

    await openTones()

    const listed = within(await screen.findByRole('list', { name: 'Profiles' }))
    expect(listed.getByRole('listitem')).toHaveTextContent(
      'Station 12 — 1122.5 Hz for 0.8s → 1465.6 Hz for 2.0s · ±2%',
    )
  })

  /** Adding one is a *sequence* an Operator builds up, which is what lets the
   *  same form spell a single long group tone and Quick Call II's two. */
  it('adds a profile as an ordered sequence of tones', async () => {
    const channel = instance.talkgroup({ ref: 54241 })
    await openTones()
    await screen.findByRole('form', { name: 'Add a tone profile' })

    await userEvent.type(screen.getByLabelText('Station'), 'Station 12')
    await userEvent.type(screen.getByLabelText('Tone (Hz)'), '1122.5')
    await userEvent.clear(screen.getByLabelText('Held for (s)'))
    await userEvent.type(screen.getByLabelText('Held for (s)'), '0.8')
    await userEvent.click(screen.getByRole('button', { name: 'Add tone' }))
    await userEvent.type(screen.getByLabelText('Tone (Hz)'), '1465.6')
    await userEvent.clear(screen.getByLabelText('Held for (s)'))
    await userEvent.type(screen.getByLabelText('Held for (s)'), '2')
    await userEvent.click(screen.getByRole('button', { name: 'Add tone' }))

    const sequence = within(screen.getByRole('list', { name: 'Sequence' }))
    expect(sequence.getByText('1. 1122.5 Hz for 0.8s')).toBeInTheDocument()
    expect(sequence.getByText('2. 1465.6 Hz for 2.0s')).toBeInTheDocument()

    await userEvent.click(screen.getByRole('button', { name: 'Add profile' }))

    await waitFor(() =>
      expect(instance.tones.get(channel.id)?.[0]?.label).toBe('Station 12'),
    )
    expect(wrote().at(-1)?.body).toEqual({
      label: 'Station 12',
      steps: [
        { hz: 1122.5, minMs: 800 },
        { hz: 1465.6, minMs: 2000 },
      ],
      tolerancePct: 2,
      gapMaxMs: 300,
    })
  })

  /** **The check that matters most on this screen.** A profile outside the band
   *  detection listens to would be stored, never fire, and produce no error
   *  anywhere — a pager that is not being watched looks exactly like a pager
   *  that has not gone off. So the browser says so before it is sent. */
  it('refuses a profile that could never fire, before it is sent', async () => {
    const channel = instance.talkgroup({ ref: 54241 })
    await openTones()
    await screen.findByRole('form', { name: 'Add a tone profile' })

    await userEvent.type(screen.getByLabelText('Station'), 'Station 12')
    await userEvent.type(screen.getByLabelText('Tone (Hz)'), '40')
    await userEvent.click(screen.getByRole('button', { name: 'Add tone' }))

    expect(await screen.findByRole('alert')).toHaveTextContent(
      /outside the 200-3300 Hz range/,
    )
    expect(screen.getByRole('button', { name: 'Add profile' })).toBeDisabled()
    expect(instance.tones.get(channel.id)).toBeUndefined()
  })

  /** The slack is editable too, and it is the setting most likely to need
   *  changing after the fact: an ageing console drifts, and widening the
   *  tolerance is what an Operator reaches for when a station stops being
   *  caught. */
  it('sends the tolerance and gap an Operator chose', async () => {
    const channel = instance.talkgroup({ ref: 54241 })
    await openTones()
    await screen.findByRole('form', { name: 'Add a tone profile' })

    await userEvent.type(screen.getByLabelText('Station'), 'Station 12')
    await userEvent.type(screen.getByLabelText('Tone (Hz)'), '1122.5')
    await userEvent.click(screen.getByRole('button', { name: 'Add tone' }))
    await userEvent.clear(screen.getByLabelText('Tolerance (%)'))
    await userEvent.type(screen.getByLabelText('Tolerance (%)'), '3.5')
    await userEvent.clear(screen.getByLabelText('Max gap (ms)'))
    await userEvent.type(screen.getByLabelText('Max gap (ms)'), '500')
    await userEvent.click(screen.getByRole('button', { name: 'Add profile' }))

    await waitFor(() =>
      expect(instance.tones.get(channel.id)?.[0]?.tolerancePct).toBe(3.5),
    )
    expect(wrote().at(-1)?.body).toMatchObject({
      tolerancePct: 3.5,
      gapMaxMs: 500,
    })
  })

  /** A tolerance wide enough to page the neighbouring station is refused here
   *  too — the same rule, applied to a field the tone list does not carry. */
  it('refuses a tolerance wide enough to page somebody else', async () => {
    instance.talkgroup({ ref: 54241 })
    await openTones()
    await screen.findByRole('form', { name: 'Add a tone profile' })

    await userEvent.type(screen.getByLabelText('Tone (Hz)'), '1122.5')
    await userEvent.click(screen.getByRole('button', { name: 'Add tone' }))
    await userEvent.clear(screen.getByLabelText('Tolerance (%)'))
    await userEvent.type(screen.getByLabelText('Tolerance (%)'), '90')

    expect(await screen.findByRole('alert')).toHaveTextContent(/tolerance/)
  })

  /** A tone added by mistake comes back off without starting over. */
  it('takes a tone back out of the sequence', async () => {
    instance.talkgroup({ ref: 54241 })
    await openTones()
    await screen.findByRole('form', { name: 'Add a tone profile' })

    await userEvent.type(screen.getByLabelText('Tone (Hz)'), '1122.5')
    await userEvent.click(screen.getByRole('button', { name: 'Add tone' }))
    await userEvent.click(screen.getByRole('button', { name: 'Remove tone 1' }))

    expect(
      screen.queryByRole('list', { name: 'Sequence' }),
    ).not.toBeInTheDocument()
  })

  /** **Disable before delete.** A profile paging on somebody else's tones is
   *  something an Operator wants stopped now and worked out later, and deleting
   *  it would lose the frequencies they measured off a recording. */
  it('switches a profile off without losing what was measured', async () => {
    const channel = instance.talkgroup({ ref: 54241 })
    instance.tones.set(channel.id, [
      {
        id: 900,
        talkgroupId: channel.id,
        label: 'Station 12',
        steps: [{ hz: 1122.5, minMs: 800 }],
        tolerancePct: 2,
        gapMaxMs: 300,
        disabled: false,
        createdAtMs: 0,
      },
    ])
    await openTones()

    await userEvent.click(await screen.findByRole('button', { name: 'Disable' }))

    await waitFor(() =>
      expect(instance.tones.get(channel.id)?.[0]?.disabled).toBe(true),
    )
    expect(wrote().at(-1)?.body).toEqual({ disabled: true })
    // Still there, and it says so — a disabled profile is otherwise
    // indistinguishable from one that simply has not been paged.
    expect(
      await screen.findByRole('button', { name: 'Enable' }),
    ).toBeInTheDocument()
    expect(
      within(screen.getByRole('list', { name: 'Profiles' })).getByRole(
        'listitem',
      ),
    ).toHaveTextContent(/disabled/)
  })

  it('deletes a profile', async () => {
    const channel = instance.talkgroup({ ref: 54241 })
    instance.tones.set(channel.id, [
      {
        id: 900,
        talkgroupId: channel.id,
        label: 'Station 12',
        steps: [{ hz: 1122.5, minMs: 800 }],
        tolerancePct: 2,
        gapMaxMs: 300,
        disabled: false,
        createdAtMs: 0,
      },
    ])
    await openTones()

    await userEvent.click(
      await screen.findByRole('button', { name: 'Delete Station 12' }),
    )

    await waitFor(() => expect(instance.tones.get(channel.id)).toEqual([]))
  })

  /** A channel with nothing listened for says so, rather than showing an empty
   *  box an Operator has to interpret. */
  it('says when nothing is being listened for', async () => {
    instance.talkgroup({ ref: 54241 })
    await openTones()

    expect(
      await screen.findByText(/Nothing is being listened for/),
    ).toBeInTheDocument()
  })

  /** The server's refusal is rendered, not re-derived — it is the side that
   *  knows what an older release can read and what a newer one wrote. */
  it('shows the server’s own refusal', async () => {
    instance.talkgroup({ ref: 54241 })
    signedIn(<AdminTalkgroupsScreen />)
    await screen.findByRole('list', { name: 'Talkgroups' })
    server.use(
      http.post(`${ORIGIN}/api/admin/talkgroups/:id/tones`, () =>
        refusal(400, 'unusable-tone-profile', 'a tone profile needs at least one tone'),
      ),
    )
    await userEvent.click(screen.getByRole('button', { name: 'Tones' }))
    await screen.findByRole('form', { name: 'Add a tone profile' })

    await userEvent.type(screen.getByLabelText('Station'), 'Station 12')
    await userEvent.type(screen.getByLabelText('Tone (Hz)'), '1122.5')
    await userEvent.click(screen.getByRole('button', { name: 'Add tone' }))
    await userEvent.click(screen.getByRole('button', { name: 'Add profile' }))

    expect(
      await screen.findByText(/a tone profile needs at least one tone/),
    ).toBeInTheDocument()
  })
})

// ---------------------------------------------------------------------------
// Listener counts (#62, spec US 41)
// ---------------------------------------------------------------------------

describe('listener history', () => {
  const HOUR = 3_600_000

  /** Every `/api/admin/listeners` query string the screen sent, in order. */
  let asked: string[] = []

  /** A peak-listener series, answered for whatever range was asked for. */
  function serving(values: number[]) {
    server.use(
      http.get(`${ORIGIN}/api/admin/listeners`, ({ request }) => {
        const url = new URL(request.url)
        asked.push(url.search)
        const bucketMs = Number(url.searchParams.get('bucketMs') ?? HOUR)
        const fromMs = Number(url.searchParams.get('after') ?? 0)
        return HttpResponse.json({
          fromMs,
          toMs: fromMs + values.length * bucketMs,
          bucketMs,
          values,
        })
      }),
    )
  }

  beforeEach(() => {
    asked = []
  })

  /** The headline spec US 41 asks for: peak listeners, with a timestamp — said
   *  in a sentence rather than left for an Operator to squint at bars for. */
  it('says how many were on at once, and when', async () => {
    serving([0, 3, 9, 2])
    signedIn(<ListenersScreen />)

    expect(await screen.findByText(/Peak 9 listeners/)).toBeInTheDocument()
  })

  /** Nobody-ever is a different fact from a peak of zero, and reads
   *  differently: an instance nobody has found has no peak to report. */
  it('says so when nobody has listened', async () => {
    serving([0, 0, 0])
    signedIn(<ListenersScreen />)

    expect(
      await screen.findByText('Nobody has listened in this window.'),
    ).toBeInTheDocument()
  })

  it('draws a bar per bucket', async () => {
    serving([1, 2, 3, 4, 5])
    signedIn(<ListenersScreen />)

    const chart = await screen.findByRole('img', { name: /Peak listeners/ })
    expect(chart.querySelectorAll('span')).toHaveLength(5)
  })

  /** Each range asks at its own grain, so the labels are never describing
   *  buckets the server widened underneath them. */
  it('asks at the grain of the range that was picked', async () => {
    const user = userEvent.setup()
    serving([1])
    signedIn(<ListenersScreen />)
    await screen.findByRole('img', { name: /Peak listeners/ })
    expect(new URLSearchParams(asked.at(-1)).get('bucketMs')).toBe(String(HOUR))

    await user.click(screen.getByRole('button', { name: '30 days' }))

    await waitFor(() =>
      expect(new URLSearchParams(asked.at(-1)).get('bucketMs')).toBe(
        String(24 * HOUR),
      ),
    )
  })

  /** The gate is the screen's, not the endpoint's alone: a browser with no
   *  session is shown the password form rather than an empty chart it would
   *  read as "nobody has ever listened". */
  it('is behind the admin session', async () => {
    server.use(
      http.get(
        `${ORIGIN}/api/admin/session`,
        () => new HttpResponse(null, { status: 401 }),
      ),
    )
    renderWithProviders(<ListenersScreen />)

    expect(
      await screen.findByRole('button', { name: 'Sign in' }),
    ).toBeInTheDocument()
    expect(asked).toEqual([])
  })

  /** The one thing this screen must never grow. */
  it('says what is and is not recorded', async () => {
    serving([2])
    signedIn(<ListenersScreen />)

    expect(
      await screen.findByText(/no addresses, no sessions/i),
    ).toBeInTheDocument()
  })
})
