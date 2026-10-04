import { screen, waitFor, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { http, HttpResponse } from 'msw'
import { beforeEach, describe, expect, it } from 'vitest'

import { FakeInstance, curationHandlers, refusal } from '@/test/curation'
import { ORIGIN } from '@/test/handlers'
import { server } from '@/test/setup'
import { renderWithProviders } from '@/test/utils'

import { DirwatchesScreen } from './DirwatchesScreen'

let instance: FakeInstance

beforeEach(() => {
  instance = new FakeInstance()
})

function signedIn() {
  server.use(...curationHandlers(instance))
  return renderWithProviders(<DirwatchesScreen />)
}

describe('the Dirwatch screen', () => {
  it('asks for the password', async () => {
    renderWithProviders(<DirwatchesScreen />)

    expect(await screen.findByLabelText('Admin password')).toBeInTheDocument()
    expect(screen.queryByRole('list')).not.toBeInTheDocument()
  })

  /** **A control that would be refused is not offered.** With no roots in the
   *  TOML every watch would be refused, so there is no form — there is the one
   *  sentence that says how to turn Dirwatch on. */
  it('says how to turn dirwatch on when the instance allows no folder', async () => {
    instance.dirwatchRoots = []
    signedIn()

    expect(await screen.findByText(/Dirwatch is off/)).toHaveTextContent(
      '[dirwatch] roots',
    )
    expect(
      screen.queryByRole('form', { name: 'Watch a folder' }),
    ).not.toBeInTheDocument()
  })

  /** Watching Trunk Recorder's captureDir: the folder starts at the first
   *  root, and the fields Trunk Recorder's own files answer are not asked. */
  it('watches a trunk recorder folder', async () => {
    signedIn()
    const form = await screen.findByRole('form', { name: 'Watch a folder' })
    expect(screen.getByText(/A watch may be in \/srv\/recorders/)).toBeInTheDocument()
    expect(within(form).getByLabelText('Folder')).toHaveValue('/srv/recorders')
    expect(within(form).queryByLabelText('Mask')).not.toBeInTheDocument()
    expect(within(form).queryByLabelText('Talkgroup ref')).not.toBeInTheDocument()

    await userEvent.type(within(form).getByLabelText('Folder'), '/fulton')
    await userEvent.type(within(form).getByLabelText('What is it'), 'Fulton TR')
    await userEvent.click(within(form).getByLabelText(/Delete each file/))
    await userEvent.click(within(form).getByRole('button', { name: 'Watch' }))

    await waitFor(() => expect(instance.wrote).toHaveLength(1))
    expect(instance.wrote[0]).toEqual({
      method: 'POST',
      path: '/api/admin/dirwatches',
      body: {
        label: 'Fulton TR',
        directory: '/srv/recorders/fulton',
        format: 'trunk-recorder',
        extension: null,
        mask: null,
        systemRef: null,
        talkgroupRef: null,
        frequency: null,
        delayMs: 2000,
        deleteAfter: true,
        poll: false,
      },
    })
    // The form is ready for the next one, and the new watch is listed.
    expect(within(form).getByLabelText('What is it')).toHaveValue('')
    expect(
      await screen.findByRole('list', { name: 'Watches' }),
    ).toHaveTextContent('Fulton TR')
  })

  /** A mask watch asks for its mask and a Talkgroup; every number it sends
   *  is a number. */
  it('watches a folder through a filename mask', async () => {
    signedIn()
    const form = await screen.findByRole('form', { name: 'Watch a folder' })

    await userEvent.selectOptions(within(form).getByLabelText('Recorder'), 'mask')
    expect(within(form).getByText(/#TGAFS/)).toBeInTheDocument()
    await userEvent.type(within(form).getByLabelText('Mask'), 'cymx_#TG')
    await userEvent.type(within(form).getByLabelText('System ref'), '11')
    await userEvent.type(within(form).getByLabelText('Talkgroup ref'), '5')
    await userEvent.type(
      within(form).getByLabelText('Frequency (Hz) when a file has none'),
      '155000000',
    )
    await userEvent.type(within(form).getByLabelText('Audio extension'), 'mp3')
    await userEvent.clear(within(form).getByLabelText('Wait after the last write (ms)'))
    await userEvent.type(within(form).getByLabelText('Wait after the last write (ms)'), '500')
    await userEvent.click(within(form).getByLabelText(/Look every few seconds/))
    await userEvent.click(within(form).getByRole('button', { name: 'Watch' }))

    await waitFor(() => expect(instance.wrote).toHaveLength(1))
    expect(instance.wrote[0].body).toMatchObject({
      format: 'mask',
      mask: 'cymx_#TG',
      systemRef: 11,
      talkgroupRef: 5,
      frequency: 155_000_000,
      extension: 'mp3',
      delayMs: 500,
      poll: true,
    })
  })

  /** A field that is not a number is refused before anything is asked. */
  it('refuses a ref that is not a number before asking', async () => {
    signedIn()
    const form = await screen.findByRole('form', { name: 'Watch a folder' })

    await userEvent.type(within(form).getByLabelText('System ref'), 'eleven')
    await userEvent.click(within(form).getByRole('button', { name: 'Watch' }))

    expect(await within(form).findByRole('alert')).toHaveTextContent(
      'systemRef has to be a whole number',
    )
    expect(instance.wrote).toHaveLength(0)
  })

  /** The server's own sentence, beside the form that caused it. */
  it('shows the refusal for a folder outside the roots', async () => {
    signedIn()
    const form = await screen.findByRole('form', { name: 'Watch a folder' })

    await userEvent.clear(within(form).getByLabelText('Folder'))
    await userEvent.type(within(form).getByLabelText('Folder'), '/etc')
    await userEvent.click(within(form).getByRole('button', { name: 'Watch' }))

    expect(await within(form).findByRole('alert')).toHaveTextContent(
      /not inside a folder this instance allows/,
    )
  })

  /** Each watch says what it is doing — and what is wrong, when it is not
   *  watching. */
  it('shows each watch with its health', async () => {
    instance.dirwatch({
      label: 'Fulton TR',
      health: {
        status: 'watching',
        ingested: 3,
        refused: 1,
        lastRefusal: 'no-audio 54241.json',
      },
    })
    instance.dirwatch({
      directory: '/srv/recorders/moved',
      format: 'sdrtrunk',
      health: { status: 'no-such-directory', ingested: 0, refused: 0 },
    })
    signedIn()

    const list = await screen.findByRole('list', { name: 'Watches' })
    expect(within(list).getByText(/3 ingested/)).toBeInTheDocument()
    expect(within(list).getByText(/no-audio 54241.json/)).toBeInTheDocument()
    expect(within(list).getByText('/srv/recorders/moved')).toBeInTheDocument()
    expect(within(list).getByText(/the folder is gone/)).toBeInTheDocument()
  })

  /** Scan now, disable and remove: the three things done to a row without
   *  opening it. */
  it('scans, disables and removes a watch', async () => {
    const row = instance.dirwatch({ label: 'Fulton TR' })
    signedIn()
    const list = await screen.findByRole('list', { name: 'Watches' })

    await userEvent.click(within(list).getByRole('button', { name: 'Scan now' }))
    await userEvent.click(within(list).getByRole('button', { name: 'Disable' }))
    await waitFor(() => expect(instance.wrote).toHaveLength(2))
    expect(
      await within(list).findByRole('button', { name: 'Enable' }),
    ).toBeInTheDocument()
    expect(within(list).getByRole('button', { name: 'Scan now' })).toBeDisabled()
    await userEvent.click(within(list).getByRole('button', { name: 'Remove' }))

    await waitFor(() => expect(instance.wrote).toHaveLength(3))
    expect(instance.wrote.map((write) => [write.method, write.path])).toEqual([
      ['POST', `/api/admin/dirwatches/${row.id}/scan`],
      ['PATCH', `/api/admin/dirwatches/${row.id}`],
      ['DELETE', `/api/admin/dirwatches/${row.id}`],
    ])
    await waitFor(() =>
      expect(screen.queryByRole('list', { name: 'Watches' })).not.toBeInTheDocument(),
    )
  })

  /** Editing opens the row's own settings, and saving sends them whole — a
   *  cleared field as `null`, which is how the server is told to clear it. */
  it('edits a watch', async () => {
    const row = instance.dirwatch({
      label: 'Fulton TR',
      format: 'dsdplus',
      systemRef: 11,
      talkgroupRef: 5,
      frequency: 155_000_000,
    })
    signedIn()
    const list = await screen.findByRole('list', { name: 'Watches' })

    await userEvent.click(within(list).getByRole('button', { name: 'Edit' }))
    const form = await screen.findByRole('form', { name: 'Edit Fulton TR' })
    expect(within(form).getByLabelText('Talkgroup ref')).toHaveValue('5')
    await userEvent.clear(within(form).getByLabelText('Talkgroup ref'))
    await userEvent.click(within(form).getByRole('button', { name: 'Save' }))

    await waitFor(() => expect(instance.wrote).toHaveLength(1))
    expect(instance.wrote[0]).toMatchObject({
      method: 'PATCH',
      path: `/api/admin/dirwatches/${row.id}`,
      body: { format: 'dsdplus', systemRef: 11, talkgroupRef: null },
    })
    await waitFor(() =>
      expect(screen.queryByRole('form', { name: 'Edit Fulton TR' })).not.toBeInTheDocument(),
    )

    // Opened and closed again, nothing is written.
    await userEvent.click(within(list).getByRole('button', { name: 'Edit' }))
    await userEvent.click(within(list).getByRole('button', { name: 'Close' }))
    expect(instance.wrote).toHaveLength(1)
  })

  it('says so when the listing cannot be read', async () => {
    server.use(...curationHandlers(instance))
    server.use(
      http.get(`${ORIGIN}/api/admin/dirwatches`, () =>
        HttpResponse.json({ error: 'nope' }, { status: 500 }),
      ),
    )
    renderWithProviders(<DirwatchesScreen />)

    expect(await screen.findByRole('alert')).toHaveTextContent(/could not be read/)
  })

  /** A row action the server refuses says why, under the list. */
  it('shows why a scan was refused', async () => {
    instance.dirwatch({ label: 'Fulton TR' })
    server.use(...curationHandlers(instance))
    server.use(
      http.post(`${ORIGIN}/api/admin/dirwatches/:id/scan`, () =>
        refusal(404, 'dirwatch-not-found', 'no such dirwatch'),
      ),
    )
    renderWithProviders(<DirwatchesScreen />)
    const list = await screen.findByRole('list', { name: 'Watches' })

    await userEvent.click(within(list).getByRole('button', { name: 'Scan now' }))

    expect(await screen.findByRole('alert')).toHaveTextContent('no such dirwatch')
  })
})
