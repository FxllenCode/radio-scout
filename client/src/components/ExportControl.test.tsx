import { screen } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { describe, expect, it } from 'vitest'

import { renderWithProviders } from '@/test/utils'

import { ExportControl } from './ExportControl'
import type { Catalog } from '@/types'

const catalog = (over: Partial<Catalog['export']> = {}): Catalog => ({
  systems: [],
  activityWindowMs: 0,
  sharing: true,
  export: { enabled: true, maxCalls: 1000, ...over },
  starred: { kept: false, keptDays: 0 },
})

/** Rendered inside a store because the download link carries this browser's
 *  **Access code** grant (#68) — a navigation cannot go through the base query,
 *  so the component reads it rather than each of its two callers remembering
 *  to hand it over. */
function draw(props: Partial<Parameters<typeof ExportControl>[0]> = {}) {
  return renderWithProviders(
    <ExportControl
      search={{ talkgroup: 7 }}
      count={12}
      catalog={catalog()}
      {...props}
    />,
  )
}

it('opens onto the two things an export can be', async () => {
  draw()

  await userEvent.click(screen.getByRole('button', { name: /export/i }))

  expect(screen.getByRole('link', { name: /zip of calls/i })).toBeInTheDocument()
  expect(screen.getByRole('link', { name: /one stitched file/i })).toBeInTheDocument()
})

/** The links are what downloads: the server names the file in
 *  `Content-Disposition`, so a plain link keeps the Listener on the page and
 *  saves it under the name the export chose. */
it('links at the search the listener is looking at', async () => {
  draw({ search: { talkgroup: 7, after: 1000 } })

  await userEvent.click(screen.getByRole('button', { name: /export/i }))

  expect(screen.getByRole('link', { name: /zip of calls/i })).toHaveAttribute(
    'href',
    '/api/calls/export?after=1000&format=zip&talkgroup=7',
  )
})

it('says how many calls are going', async () => {
  draw({ count: 412 })

  await userEvent.click(screen.getByRole('button', { name: /export/i }))

  expect(screen.getByText(/412 calls/i)).toBeInTheDocument()
})

/** The refusal arrives *before* the wait, and names both numbers, because
 *  "narrow the range" is only actionable if you know by how much. */
it('refuses a range over the cap instead of offering a download that fails', async () => {
  draw({ count: 4312, catalog: catalog({ maxCalls: 1000 }) })

  await userEvent.click(screen.getByRole('button', { name: /export/i }))

  expect(screen.getByText(/4,312 calls/)).toBeInTheDocument()
  expect(screen.queryByRole('link', { name: /zip of calls/i })).not.toBeInTheDocument()
})

it('is not drawn at all where the instance does not export', () => {
  draw({ catalog: catalog({ enabled: false }) })

  expect(screen.queryByRole('button', { name: /export/i })).not.toBeInTheDocument()
})

it('closes again', async () => {
  draw()

  await userEvent.click(screen.getByRole('button', { name: /export/i }))
  await userEvent.click(screen.getByRole('button', { name: /close/i }))

  expect(screen.queryByRole('link', { name: /zip of calls/i })).not.toBeInTheDocument()
})

describe('an empty result', () => {
  it('still draws the control, and says there is nothing to take', async () => {
    draw({ count: 0 })

    await userEvent.click(screen.getByRole('button', { name: /export/i }))

    expect(screen.getByText(/nothing here to export/i)).toBeInTheDocument()
  })
})

/** Choosing a format is the end of the errand: the download is under way and
 *  the panel over the results has nothing left to say. */
it('closes once a format has been chosen', async () => {
  draw()

  await userEvent.click(screen.getByRole('button', { name: /export/i }))
  await userEvent.click(screen.getByRole('link', { name: /zip of calls/i }))

  expect(screen.queryByRole('link', { name: /zip of calls/i })).not.toBeInTheDocument()
})
