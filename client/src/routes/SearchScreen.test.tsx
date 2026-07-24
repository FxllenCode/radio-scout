import { render, screen } from '@testing-library/react'
import { axe } from 'vitest-axe'
import { describe, expect, it } from 'vitest'

import { SearchScreen } from './SearchScreen'

describe('SearchScreen', () => {
  it('renders the archive-search placeholder', () => {
    render(<SearchScreen />)
    expect(
      screen.getByRole('heading', { name: 'Search' }),
    ).toBeInTheDocument()
    expect(screen.getByText('Archive search')).toBeInTheDocument()
  })

  it('has no accessibility violations', async () => {
    const { container } = render(<SearchScreen />)
    expect(await axe(container)).toHaveNoViolations()
  })
})
