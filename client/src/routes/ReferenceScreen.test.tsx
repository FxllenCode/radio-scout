import { screen } from '@testing-library/react'
import { http, HttpResponse } from 'msw'
import { describe, expect, it } from 'vitest'

import { ORIGIN } from '@/test/handlers'
import { server } from '@/test/setup'
import { renderWithProviders } from '@/test/utils'

import { ReferenceScreen } from './ReferenceScreen'

/** The **Reference page** (#77, spec US 58), as a Listener reads it. */
describe('ReferenceScreen', () => {
  it('shows the page the Operator published', async () => {
    server.use(
      http.get(`${ORIGIN}/api/reference`, () =>
        HttpResponse.json({
          html: '<h1>Codes</h1>\n<table><tbody><tr><td>10-4</td><td>Acknowledged</td></tr></tbody></table>\n',
          publishedAtMs: 1_700_000_000_000,
        }),
      ),
    )
    renderWithProviders(<ReferenceScreen />)

    expect(await screen.findByRole('heading', { name: 'Codes' })).toBeInTheDocument()
    expect(screen.getByRole('cell', { name: 'Acknowledged' })).toBeInTheDocument()
  })

  /** A bookmark from before the Operator took it down. */
  it('says there is none when there is none', async () => {
    renderWithProviders(<ReferenceScreen />)

    expect(await screen.findByText('This scanner has no reference page.')).toBeInTheDocument()
  })
})
