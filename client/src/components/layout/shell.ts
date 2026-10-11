import { createContext, useContext } from 'react'

import type { AppUpdate } from '@/hooks/useAppUpdate'
import type { InstallState } from '@/hooks/useInstall'

/**
 * What the shell holds that a screen may need to offer again (#78).
 *
 * The update and the install each have one handle per page — a service worker
 * registration, a single-use install event — and the shell owns both, because
 * their banners are docked there. Settings is where a Listener who waved a
 * banner off goes looking for it, so it reads the same handles rather than
 * making second ones that could disagree.
 */
export interface Shell {
  update: AppUpdate
  install: InstallState
}

/** Outside a shell — a screen rendered on its own — there is nothing to
 *  offer, and nothing is drawn. */
const NO_SHELL: Shell = {
  update: { ready: false, apply: () => {} },
  install: { offer: 'none', entry: 'none', install: () => {}, dismiss: () => {} },
}

export const ShellContext = createContext<Shell>(NO_SHELL)

export const useShell = (): Shell => useContext(ShellContext)
