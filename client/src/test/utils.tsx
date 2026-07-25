import { render } from '@testing-library/react'
import type { ReactElement } from 'react'
import { Provider } from 'react-redux'
import { MemoryRouter } from 'react-router-dom'

import App from '@/App'
import { makeStore, type AppStore } from '@/store/store'

/** Render `ui` inside a fresh store + router, so RTK Query's cache and the
 *  playback queue never leak between tests. */
export function renderWithProviders(
  ui: ReactElement,
  { route = '/', store = makeStore() }: { route?: string; store?: AppStore } = {},
) {
  return {
    store,
    ...render(
      <Provider store={store}>
        <MemoryRouter initialEntries={[route]}>{ui}</MemoryRouter>
      </Provider>,
    ),
  }
}

/** Render the whole app at `route` — the shell, the router, and the shared
 *  audio element, which is where the archive queue actually reaches a speaker. */
export function renderApp(route: string) {
  return renderWithProviders(<App />, { route })
}
