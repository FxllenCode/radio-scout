import { screen } from '@testing-library/react'
import { describe, expect, it } from 'vitest'

import { renderApp } from '@/test/utils'

describe('App', () => {
  it('renders the Live screen and primary nav at the root route', () => {
    renderApp('/')

    expect(
      screen.getByRole('navigation', { name: 'Primary' }),
    ).toBeInTheDocument()
    expect(screen.getByRole('heading', { name: 'LIVE' })).toBeInTheDocument()
    expect(screen.getByText(/waiting for the first call/i)).toBeInTheDocument()
  })
})
