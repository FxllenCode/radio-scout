import { render } from '@testing-library/react'
import { useEffect, type ReactElement } from 'react'
import { Provider } from 'react-redux'
import { MemoryRouter, useLocation, useNavigate } from 'react-router-dom'

import App from '@/App'
import { makeStore, type AppStore } from '@/store/store'

/**
 * Where the router is, and a handle on its history.
 *
 * The two things a test about URL state needs (#61) and `MemoryRouter` does not
 * otherwise hand out. `location` is what the address bar would read; `go(-1)` is
 * the browser's back button, which is the only way to assert that a **Run**
 * survives one and that a search restored from history is the search that was
 * left.
 */
export const routerProbe = {
  location: '',
  go: (_delta: number) => {},
}

// Fast Refresh does not apply to a module only the test runner ever loads, so
// the component beside these helpers costs nothing the rule exists to protect.
// eslint-disable-next-line react-refresh/only-export-components
function RouterProbe() {
  const location = useLocation()
  const navigate = useNavigate()
  useEffect(() => {
    routerProbe.location = `${location.pathname}${location.search}`
    routerProbe.go = navigate
  })
  return null
}

/** Render `ui` inside a fresh store + router, so RTK Query's cache and the
 *  playback queue never leak between tests. */
export function renderWithProviders(
  ui: ReactElement,
  {
    route = '/',
    store = makeStore(),
  }: { route?: string; store?: AppStore } = {},
) {
  return {
    store,
    ...render(
      <Provider store={store}>
        <MemoryRouter initialEntries={[route]}>
          {ui}
          <RouterProbe />
        </MemoryRouter>
      </Provider>,
    ),
  }
}

/** Render the whole app at `route` — the shell, the router, the shared audio
 *  element (where a queue actually reaches a speaker), and the live-feed
 *  socket. Pass a `store` to drive the feed from a test. */
export function renderApp(route: string, store?: AppStore) {
  return renderWithProviders(<App />, { route, store })
}
