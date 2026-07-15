import { render, screen } from '@testing-library/react'
import { Provider } from 'react-redux'
import { MemoryRouter } from 'react-router-dom'
import { describe, expect, it } from 'vitest'

import App from './App'
import { store } from './store/store'

describe('App', () => {
  it('renders the Live screen and primary nav at the root route', () => {
    render(
      <Provider store={store}>
        <MemoryRouter initialEntries={['/']}>
          <App />
        </MemoryRouter>
      </Provider>,
    )

    expect(
      screen.getByRole('navigation', { name: 'Primary' }),
    ).toBeInTheDocument()
    expect(screen.getByRole('heading', { name: 'LIVE' })).toBeInTheDocument()
    expect(
      screen.getByText(/waiting for the first call/i),
    ).toBeInTheDocument()
  })
})
