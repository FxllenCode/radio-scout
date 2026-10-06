import { screen, waitFor, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { http, HttpResponse } from 'msw'
import { beforeEach, describe, expect, it } from 'vitest'

import { FakeInstance, curationHandlers, refusal } from '@/test/curation'
import { ORIGIN } from '@/test/handlers'
import { server } from '@/test/setup'
import { renderWithProviders } from '@/test/utils'

import { EmbedsScreen } from './EmbedsScreen'

let instance: FakeInstance

beforeEach(() => {
  instance = new FakeInstance()
})

function signedIn() {
  server.use(...curationHandlers(instance))
  return renderWithProviders(<EmbedsScreen />)
}

/** The row for the embed called `name`. */
async function rowOf(name: string) {
  const list = await screen.findByRole('list', { name: 'Embeds' })
  const row = within(list)
    .getAllByRole('listitem')
    .find((item) => within(item).queryByText(name) !== null)
  if (!row) throw new Error(`no row for ${name}`)
  return row
}

describe('the Embeds screen', () => {
  it('asks for the password', async () => {
    renderWithProviders(<EmbedsScreen />)

    expect(await screen.findByLabelText('Admin password')).toBeInTheDocument()
    expect(screen.queryByRole('list', { name: 'Embeds' })).toBeNull()
  })

  it('says when there are none', async () => {
    signedIn()

    expect(await screen.findByText('No embeds yet.')).toBeInTheDocument()
  })

  /** The tracer: name it, choose what it plays, and it is listed with the
   *  snippet a host pastes. */
  it('makes an embed of the selection an operator chooses', async () => {
    signedIn()
    const form = await screen.findByRole('form', { name: 'Make an embed' })

    await userEvent.type(within(form).getByLabelText('Name'), 'Fire dispatch')
    await userEvent.click(within(form).getByRole('button', { name: 'Add a system' }))
    await userEvent.type(within(form).getByLabelText('System ref'), '11')
    await userEvent.type(
      within(form).getByLabelText('Talkgroup refs (blank = all)'),
      '100, 200',
    )
    await userEvent.click(within(form).getByRole('button', { name: 'Make embed' }))

    await waitFor(() => expect(instance.wrote).toHaveLength(1))
    expect(instance.wrote[0]).toEqual({
      method: 'POST',
      path: '/api/admin/embeds',
      body: {
        name: 'Fire dispatch',
        selection: { all: false, sel: { 11: { 100: true, 200: true } } },
      },
    })
    const row = await rowOf('Fire dispatch')
    expect(within(row).getByLabelText('Snippet')).toHaveValue(
      '<iframe src="http://localhost/embed?t=token1" title="Fire dispatch" ' +
        'width="100%" height="420" style="border:0;max-width:480px" ' +
        'loading="lazy" allow="autoplay"></iframe>',
    )
    // The form is ready for the next one.
    expect(within(form).getByLabelText('Name')).toHaveValue('')
  })

  /** An embed is a player, not a forwarder: the editor says so. */
  it('asks what the embed plays, in its own words', async () => {
    signedIn()
    const form = await screen.findByRole('form', { name: 'Make an embed' })

    expect(within(form).getByText('What it plays')).toBeInTheDocument()
    expect(within(form).getByLabelText('Play everything')).not.toBeChecked()
  })

  it('shows a refusal beside the form', async () => {
    signedIn()
    const form = await screen.findByRole('form', { name: 'Make an embed' })

    await userEvent.click(within(form).getByRole('button', { name: 'Make embed' }))

    expect(await within(form).findByRole('alert')).toHaveTextContent(
      'name is required',
    )
  })

  /** With `[server] public_url` set, the snippet carries the address the
   *  public reaches — and the screen does not warn about a guess it did not
   *  make. */
  it('points the snippet at the public address when the instance has one', async () => {
    instance.publicUrl = 'https://scanner.example.org'
    instance.embed({ name: 'Fire dispatch' })
    signedIn()

    const row = await rowOf('Fire dispatch')

    expect(
      within(row).getByLabelText<HTMLTextAreaElement>('Snippet').value,
    ).toContain('src="https://scanner.example.org/embed?t=token1"')
    expect(within(row).queryByText(/public_url/)).toBeNull()
  })

  /** ...and without one, it says the address is this screen's — which may be a
   *  LAN address no reader of the host's page can reach. */
  it('says the snippet is built from this address when the instance has no public one', async () => {
    instance.embed({ name: 'Fire dispatch' })
    signedIn()

    const row = await rowOf('Fire dispatch')

    expect(within(row).getByText(/public_url/)).toHaveTextContent(
      'http://localhost',
    )
  })

  /** Focusing the snippet selects all of it — the fallback when copying is
   *  refused, and the gesture an Operator reaches for anyway. */
  it('selects the whole snippet when it is focused', async () => {
    instance.embed({ name: 'Fire dispatch' })
    signedIn()
    const snippet = within(await rowOf('Fire dispatch')).getByLabelText<HTMLTextAreaElement>(
      'Snippet',
    )

    snippet.focus()

    expect(snippet.selectionStart).toBe(0)
    expect(snippet.selectionEnd).toBe(snippet.value.length)
  })

  it('says when the embeds could not be read', async () => {
    signedIn()
    server.use(
      http.get(`${ORIGIN}/api/admin/embeds`, () => new HttpResponse(null, { status: 500 })),
    )

    expect(await screen.findByRole('alert')).toHaveTextContent(
      'The embeds could not be read.',
    )
  })

  /** An https site — nearly every site — cannot frame a plain-http address;
   *  the frame would simply be blank, so the screen says so where the address
   *  is chosen. */
  it('warns that an https site cannot frame a plain http address', async () => {
    instance.publicUrl = 'http://scanner.example.org'
    instance.embed({ name: 'Plain' })
    signedIn()

    expect(
      within(await rowOf('Plain')).getByText(/cannot frame it/),
    ).toBeInTheDocument()
  })

  it('says nothing about framing once the address is https', async () => {
    instance.publicUrl = 'https://scanner.example.org'
    instance.embed({ name: 'Secure' })
    signedIn()

    expect(within(await rowOf('Secure')).queryByText(/cannot frame it/)).toBeNull()
  })

  it('copies the snippet', async () => {
    const user = userEvent.setup()
    instance.embed({ name: 'Fire dispatch' })
    signedIn()
    const row = await rowOf('Fire dispatch')

    await user.click(within(row).getByRole('button', { name: 'Copy snippet' }))

    expect(await navigator.clipboard.readText()).toBe(
      within(row).getByLabelText<HTMLTextAreaElement>('Snippet').value,
    )
    expect(await within(row).findByText('Copied')).toBeInTheDocument()
  })

  it('says so when the snippet could not be copied', async () => {
    const user = userEvent.setup()
    instance.embed({ name: 'Fire dispatch' })
    signedIn()
    const row = await rowOf('Fire dispatch')
    Object.defineProperty(navigator, 'clipboard', {
      configurable: true,
      value: { writeText: () => Promise.reject(new Error('denied')) },
    })

    await user.click(within(row).getByRole('button', { name: 'Copy snippet' }))

    expect(
      await within(row).findByText('Could not copy — select the snippet instead'),
    ).toBeInTheDocument()
  })

  it('previews the embed in a new tab', async () => {
    instance.embed({ name: 'Fire dispatch' })
    signedIn()
    const row = await rowOf('Fire dispatch')

    const preview = within(row).getByRole('link', { name: 'Preview' })

    expect(preview).toHaveAttribute('href', '/embed?t=token1')
    expect(preview).toHaveAttribute('target', '_blank')
  })

  /** An embed plays open listening only, and the screen says how much of what
   *  it names will never play — rather than an Operator learning it from the
   *  host. */
  it('says how many restricted channels will not play', async () => {
    instance.embed({ name: 'Everything', restricted: 2 })
    instance.embed({ name: 'Fire dispatch', restricted: 0 })
    signedIn()

    expect(
      within(await rowOf('Everything')).getByText(
        "2 restricted channels in this selection won't play here",
      ),
    ).toBeInTheDocument()
    expect(within(await rowOf('Fire dispatch')).queryByText(/restricted/)).toBeNull()
  })

  it('says one restricted channel in the singular', async () => {
    instance.embed({ name: 'Everything', restricted: 1 })
    signedIn()

    expect(
      within(await rowOf('Everything')).getByText(
        "1 restricted channel in this selection won't play here",
      ),
    ).toBeInTheDocument()
  })

  it('summarises what each embed plays', async () => {
    instance.embed({ name: 'Everything', selection: { all: true, sel: {} } })
    instance.embed({ name: 'Fire dispatch' })
    signedIn()

    expect(within(await rowOf('Everything')).getByText('plays everything')).toBeInTheDocument()
    expect(
      within(await rowOf('Fire dispatch')).getByText('plays 1 system'),
    ).toBeInTheDocument()
  })

  /** Re-scoping keeps the address — the host's snippet plays whatever this says
   *  next. */
  it('renames and re-scopes an embed', async () => {
    instance.embed({ name: 'Fire dispatch' })
    signedIn()
    const row = await rowOf('Fire dispatch')

    await userEvent.click(within(row).getByRole('button', { name: 'Edit' }))
    const form = within(row).getByRole('form', { name: 'Edit Fire dispatch' })
    await userEvent.clear(within(form).getByLabelText('Name'))
    await userEvent.type(within(form).getByLabelText('Name'), 'County fire')
    await userEvent.click(within(form).getByLabelText('Play everything'))
    await userEvent.click(within(form).getByRole('button', { name: 'Save' }))

    await waitFor(() => expect(instance.wrote).toHaveLength(1))
    expect(instance.wrote[0]).toEqual({
      method: 'PATCH',
      path: '/api/admin/embeds/1',
      body: { name: 'County fire', selection: { all: true, sel: {} } },
    })
    expect(await screen.findByText('County fire')).toBeInTheDocument()
    expect(screen.queryByRole('form', { name: /Edit/ })).toBeNull()
  })

  it('shows an edit refusal beside the edit', async () => {
    instance.embed({ name: 'Fire dispatch' })
    signedIn()
    const row = await rowOf('Fire dispatch')

    await userEvent.click(within(row).getByRole('button', { name: 'Edit' }))
    const form = within(row).getByRole('form', { name: 'Edit Fire dispatch' })
    await userEvent.clear(within(form).getByLabelText('Name'))
    await userEvent.click(within(form).getByRole('button', { name: 'Save' }))

    expect(await within(form).findByRole('alert')).toHaveTextContent(
      'name is required',
    )
  })

  it('closes an editor it opened', async () => {
    instance.embed({ name: 'Fire dispatch' })
    signedIn()
    const row = await rowOf('Fire dispatch')

    await userEvent.click(within(row).getByRole('button', { name: 'Edit' }))
    await userEvent.click(within(row).getByRole('button', { name: 'Close' }))

    expect(within(row).queryByRole('form')).toBeNull()
  })

  /** A delete that is refused — another tab got there first — says so rather
   *  than leaving a row that looks deleted and is not. */
  it('says so when a delete is refused', async () => {
    instance.embed({ name: 'Fire dispatch' })
    signedIn()
    server.use(
      http.delete(`${ORIGIN}/api/admin/embeds/:id`, () =>
        refusal(404, 'embed-not-found', 'no such embed'),
      ),
    )
    const row = await rowOf('Fire dispatch')

    await userEvent.click(within(row).getByRole('button', { name: 'Delete' }))

    expect(await screen.findByRole('alert')).toHaveTextContent('no such embed')
  })

  /** Deleting is the revoke: a host's frame says the feed is gone. */
  it('takes an embed down', async () => {
    instance.embed({ name: 'Fire dispatch' })
    signedIn()
    const row = await rowOf('Fire dispatch')

    await userEvent.click(within(row).getByRole('button', { name: 'Delete' }))

    await waitFor(() =>
      expect(instance.wrote).toEqual([
        { method: 'DELETE', path: '/api/admin/embeds/1', body: undefined },
      ]),
    )
    expect(await screen.findByText('No embeds yet.')).toBeInTheDocument()
  })
})
