import { fireEvent, screen, waitFor, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { http, HttpResponse } from 'msw'
import { beforeEach, describe, expect, it } from 'vitest'

import { FakeInstance, curationHandlers, refusal } from '@/test/curation'
import { ORIGIN } from '@/test/handlers'
import { server } from '@/test/setup'
import { renderWithProviders } from '@/test/utils'

import { AdminScreen } from './AdminScreen'
import { AdminTalkgroupsScreen } from './AdminTalkgroupsScreen'
import { ApiKeysScreen } from './ApiKeysScreen'
import { GroupsScreen, TagsScreen } from './LabelsScreen'
import { SystemsScreen } from './SystemsScreen'
import { UnitsScreen } from './UnitsScreen'

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
