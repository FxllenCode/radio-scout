import { screen } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { http, HttpResponse } from 'msw'
import { describe, expect, it } from 'vitest'

import { server } from '@/test/setup'
import { renderWithProviders } from '@/test/utils'
import type { Call } from '@/types'

import { StarButton } from './StarButton'

const call: Call = {
  id: 1,
  systemRef: 11,
  talkgroupRef: 54241,
  talkgroupLabel: 'Fire Dispatch',
  audioUrl: '/api/call/1/audio',
}

const show = (overrides: Partial<Call> = {}) =>
  renderWithProviders(<StarButton call={{ ...call, ...overrides }} describedAs="Fire Dispatch" />)

describe('StarButton (#66, spec US 37)', () => {
  it('says whether the Call is starred, and toggles it', async () => {
    const user = userEvent.setup()
    show()

    const button = screen.getByRole('button', { name: 'Star Fire Dispatch' })
    expect(button).toHaveAttribute('aria-pressed', 'false')

    await user.click(button)

    // Instant, before any refetch: the archive page this row came from cannot
    // be re-read for every other place the same Call lives (`store/stars`).
    const starred = await screen.findByRole('button', { name: 'Unstar Fire Dispatch' })
    expect(starred).toHaveAttribute('aria-pressed', 'true')

    await user.click(starred)
    expect(
      await screen.findByRole('button', { name: 'Star Fire Dispatch' }),
    ).toHaveAttribute('aria-pressed', 'false')
  })

  /** A Call that arrived already starred — which is every starred Call, on
   *  every page fetched after somebody else starred it. */
  it('starts from what the server said', () => {
    show({ starred: true })

    expect(
      screen.getByRole('button', { name: 'Unstar Fire Dispatch' }),
    ).toHaveAttribute('aria-pressed', 'true')
  })

  /** A refused star takes its own mark back rather than leaving the row
   *  claiming something the Instance never recorded — the Call was pruned
   *  between the page and the tap, which Retention is entitled to do. */
  it('takes the mark back when the server refuses', async () => {
    const user = userEvent.setup()
    server.use(
      http.post('http://localhost/api/call/:id/star', () =>
        new HttpResponse('call not found\n', { status: 404 }),
      ),
    )
    show()

    await user.click(screen.getByRole('button', { name: 'Star Fire Dispatch' }))

    expect(
      await screen.findByRole('button', { name: 'Star Fire Dispatch' }),
    ).toHaveAttribute('aria-pressed', 'false')
  })
})
