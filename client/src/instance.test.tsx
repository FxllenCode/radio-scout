/**
 * The **Instance identity** across the app (#77, spec US 4, 57) — the name an
 * Operator gave their Instance, wherever the app says what it is.
 */
import { act, screen, waitFor } from '@testing-library/react'
import { afterEach, describe, expect, it } from 'vitest'

import { instanceIs } from '@/test/handlers'
import { offerInstall } from '@/test/install'
import { server } from '@/test/setup'
import { renderApp } from '@/test/utils'

afterEach(() => {
  document.title = 'Radio-Scout'
})

describe('the Instance name (#77)', () => {
  it('names the tab and heads every screen', async () => {
    server.use(instanceIs({ name: 'Fulton Scanner' }))
    renderApp('/')

    expect(await screen.findByText('Fulton Scanner')).toBeInTheDocument()
    await waitFor(() => expect(document.title).toBe('Fulton Scanner'))
  })

  it('is the product, with nothing added to the header, when unnamed', async () => {
    document.title = 'something else'
    renderApp('/search')

    await waitFor(() => expect(document.title).toBe('Radio-Scout'))
    expect(screen.queryByTestId('instance-name')).not.toBeInTheDocument()
  })

  it('offers to install under its own name', async () => {
    server.use(instanceIs({ name: 'Fulton Scanner' }))
    renderApp('/')
    await screen.findByText('Fulton Scanner')

    act(() => void offerInstall())

    expect(screen.getByText('Install Fulton Scanner')).toBeInTheDocument()
  })
})

describe("the Operator's clock (#77)", () => {
  it('writes every time on twelve hours when the Operator chose them', async () => {
    server.use(instanceIs({ timeFormat: '12h' }))
    renderApp('/search')

    // `ARCHIVE`'s newest Call, at 14:32:05 local.
    expect(await screen.findAllByText('2026-07-25 02:32:05 PM')).not.toHaveLength(0)
    expect(screen.queryByText('2026-07-25 14:32:05')).not.toBeInTheDocument()
  })

  it('writes them on twenty-four, as it always has, unless told otherwise', async () => {
    renderApp('/search')

    expect(await screen.findAllByText('2026-07-25 14:32:05')).not.toHaveLength(0)
  })
})
