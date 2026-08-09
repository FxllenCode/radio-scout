import { render, screen } from '@testing-library/react'
import { MemoryRouter } from 'react-router-dom'
import { describe, expect, it } from 'vitest'

import { UnitLink } from './UnitLink'
import type { Call } from '@/types'

const call: Call = {
  id: 1,
  systemRef: 11,
  talkgroupRef: 54241,
  audioUrl: '/api/call/1/audio',
}

function show(overrides: Partial<Call>) {
  return render(
    <MemoryRouter>
      <UnitLink call={{ ...call, ...overrides }} />
    </MemoryRouter>,
  )
}

describe('UnitLink (#47, spec US 42/44)', () => {
  it('names the radio and links to its history within its System', () => {
    show({ unitRef: 1200, unitLabel: 'Engine 1' })

    const link = screen.getByRole('link', { name: 'History for unit Engine 1' })
    expect(link).toHaveTextContent('Engine 1')
    expect(link).toHaveAttribute('href', '/unit/11/1200')
  })

  /** An uncurated archive is all bare numbers, and they still have to be
   *  tappable: "where else was 1610092" is the question a number can answer. */
  it('links a radio nobody has named, by its Ref', () => {
    show({ unitRef: 1610092 })

    expect(
      screen.getByRole('link', { name: 'History for unit 1610092' }),
    ).toHaveAttribute('href', '/unit/11/1610092')
  })

  /** Not a dash, not an empty span: a placeholder on every row of an archive
   *  fed by a recorder that sends no source is clutter bought for nothing. */
  it('renders nothing when no radio was heard', () => {
    const { container } = show({})

    expect(container).toBeEmptyDOMElement()
  })
})
