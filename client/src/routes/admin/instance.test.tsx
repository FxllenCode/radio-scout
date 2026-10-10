import { screen, waitFor, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { beforeEach, describe, expect, it } from 'vitest'

import { http, HttpResponse } from 'msw'

import { FakeInstance, REFERENCE_SEED, curationHandlers } from '@/test/curation'
import { ORIGIN } from '@/test/handlers'
import { server } from '@/test/setup'
import { renderWithProviders } from '@/test/utils'

import { InstanceScreen } from './InstanceScreen'

let instance: FakeInstance

beforeEach(() => {
  instance = new FakeInstance()
})

function signedIn() {
  server.use(...curationHandlers(instance))
  return renderWithProviders(<InstanceScreen />)
}

const identityForm = () => screen.findByRole('form', { name: 'This instance' })
const referenceForm = () => screen.findByRole('form', { name: 'Reference page' })

/** Settings → Admin → Instance (#77, spec US 4, 57, 58). */
describe('the Instance screen', () => {
  it('asks for the password', async () => {
    renderWithProviders(<InstanceScreen />)

    expect(await screen.findByLabelText('Admin password')).toBeInTheDocument()
  })

  /** The tracer: every setting the form shows is sent, as the server reads
   *  them — and blanks are sent as clears, so an emptied name is gone. */
  it('saves the identity an Operator sets', async () => {
    signedIn()
    const form = await identityForm()

    await userEvent.type(within(form).getByLabelText('Name'), 'Fulton Scanner')
    await userEvent.type(within(form).getByLabelText('Help link'), 'ops@example.com')
    await userEvent.selectOptions(within(form).getByLabelText('Time'), '12h')
    await userEvent.selectOptions(
      within(form).getByLabelText('Talkgroup order for new listeners'),
      'ref',
    )
    await userEvent.click(within(form).getByLabelText(/Go live when playback catches up/))
    await userEvent.clear(within(form).getByLabelText(/Dim the display after/))
    await userEvent.type(within(form).getByLabelText(/Dim the display after/), '30')
    await userEvent.click(within(form).getByLabelText(/Show how many are listening/))
    await userEvent.click(within(form).getByRole('button', { name: 'Save' }))

    await waitFor(() => expect(instance.wrote).toHaveLength(1))
    expect(instance.wrote[0]).toEqual({
      method: 'PATCH',
      path: '/api/admin/instance',
      body: {
        name: 'Fulton Scanner',
        helpUrl: 'ops@example.com',
        timeFormat: '12h',
        panelSort: 'ref',
        playbackGoesLive: true,
        dimmerSecs: 30,
        showListeners: true,
      },
    })
    expect(await within(form).findByRole('status')).toHaveTextContent('Saved')
  })

  it('opens on what is saved, and clears a field emptied', async () => {
    instance.identity = { ...instance.identity, name: 'Fulton Scanner', dimmerSecs: 5 }
    signedIn()
    const form = await identityForm()
    await waitFor(() => expect(within(form).getByLabelText('Name')).toHaveValue('Fulton Scanner'))
    expect(within(form).getByLabelText(/Dim the display after/)).toHaveValue(5)

    await userEvent.clear(within(form).getByLabelText('Name'))
    await userEvent.click(within(form).getByRole('button', { name: 'Save' }))

    await waitFor(() => expect(instance.wrote).toHaveLength(1))
    expect(instance.wrote[0].body).toMatchObject({ name: null, helpUrl: null })
  })

  /** The server's own sentence, beside the form that caused it. */
  it('says what the server refused', async () => {
    signedIn()
    const form = await identityForm()

    await userEvent.type(within(form).getByLabelText('Help link'), 'javascript:alert(1)')
    await userEvent.click(within(form).getByRole('button', { name: 'Save' }))

    expect(await screen.findByRole('alert')).toHaveTextContent('a help link has to be')
  })
})

/** A form that could not be read says so, rather than opening empty — an
 *  empty form saved would clear what is really there. */
describe('the Instance screen, unable to read', () => {
  it('says the settings could not be read', async () => {
    // After the fake's own routes, so this one is asked first.
    server.use(...curationHandlers(instance))
    server.use(http.get(`${ORIGIN}/api/instance`, () => new HttpResponse(null, { status: 500 })))
    renderWithProviders(<InstanceScreen />)

    expect(await screen.findByText(/settings could not be read/)).toBeInTheDocument()
  })

  it('says the reference page could not be read', async () => {
    server.use(...curationHandlers(instance))
    server.use(
      http.get(`${ORIGIN}/api/admin/reference`, () => new HttpResponse(null, { status: 500 })),
    )
    renderWithProviders(<InstanceScreen />)

    expect(await screen.findByText(/reference page could not be read/)).toBeInTheDocument()
  })
})

describe('the Reference page editor', () => {
  /** Nothing is published until an Operator presses publish — the seed is
   *  offered, never shown to Listeners unasked. */
  it('starts from the seed, and publishes what is written', async () => {
    signedIn()
    const form = await referenceForm()
    const editor = within(form).getByLabelText('Markdown')
    await waitFor(() => expect(editor).toHaveValue(REFERENCE_SEED))
    expect(within(form).getByText('Not published')).toBeInTheDocument()
    expect(within(form).queryByRole('button', { name: 'Take down' })).toBeNull()

    await userEvent.click(within(form).getByRole('button', { name: 'Publish' }))

    await waitFor(() =>
      expect(instance.wrote).toContainEqual({
        method: 'PUT',
        path: '/api/admin/reference',
        body: { markdown: REFERENCE_SEED },
      }),
    )
    expect(await within(form).findByText('Published')).toBeInTheDocument()
  })

  it('opens on what is published, and takes it down', async () => {
    instance.reference = '# Our codes\n'
    signedIn()
    const form = await referenceForm()
    await waitFor(() =>
      expect(within(form).getByLabelText('Markdown')).toHaveValue('# Our codes\n'),
    )

    await userEvent.click(within(form).getByRole('button', { name: 'Take down' }))

    await waitFor(() => expect(instance.reference).toBeUndefined())
    expect(await within(form).findByText('Not published')).toBeInTheDocument()
  })

  it('puts the seed back on request', async () => {
    instance.reference = '# Our codes\n'
    signedIn()
    const form = await referenceForm()
    await waitFor(() =>
      expect(within(form).getByLabelText('Markdown')).toHaveValue('# Our codes\n'),
    )

    await userEvent.click(within(form).getByRole('button', { name: 'Start from the template' }))

    expect(within(form).getByLabelText('Markdown')).toHaveValue(REFERENCE_SEED)
  })

  it('says what the server refused', async () => {
    signedIn()
    const form = await referenceForm()
    const editor = within(form).getByLabelText('Markdown')
    await waitFor(() => expect(editor).toHaveValue(REFERENCE_SEED))

    await userEvent.clear(editor)
    await userEvent.type(editor, '   ')
    await userEvent.click(within(form).getByRole('button', { name: 'Publish' }))

    expect(await screen.findByRole('alert')).toHaveTextContent('markdown is required')
  })
})
