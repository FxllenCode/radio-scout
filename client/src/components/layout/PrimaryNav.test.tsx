import { render, screen } from '@testing-library/react'
import { MemoryRouter } from 'react-router-dom'
import { describe, expect, it } from 'vitest'

import { PrimaryNav } from './PrimaryNav'

describe('PrimaryNav', () => {
  it('renders the four primary destinations as links', () => {
    render(
      <MemoryRouter>
        <PrimaryNav />
      </MemoryRouter>,
    )
    for (const label of ['Live', 'Talkgroups', 'Search', 'Settings']) {
      expect(screen.getByRole('link', { name: label })).toBeInTheDocument()
    }
  })

  it('marks the current tab active', () => {
    render(
      <MemoryRouter initialEntries={['/search']}>
        <PrimaryNav />
      </MemoryRouter>,
    )
    expect(screen.getByRole('link', { name: 'Search' })).toHaveClass(
      'text-foreground',
    )
    expect(screen.getByRole('link', { name: 'Live' })).not.toHaveClass(
      'text-foreground',
    )
  })
})
