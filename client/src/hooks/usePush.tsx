import {
  createContext,
  useCallback,
  useContext,
  useState,
  useSyncExternalStore,
  type ReactNode,
} from 'react'

import { createPush, type Push, type PushState } from '@/lib/push'

/**
 * One Web Push handle for the whole app (#16).
 *
 * Shared rather than per-component because three places need the *same* one:
 * the Settings switch turns it on, the live-feed link presents its id so the
 * server knows this listener is listening, and the same link re-registers the
 * Selection when it changes. Two handles would mean two subscribes on load and
 * a switch that could disagree with the socket about whether notifications are
 * on.
 */
const PushContext = createContext<Push | null>(null)

export function PushProvider({
  push,
  children,
}: {
  /** A handle to use instead of the browser's — what a test injects. */
  push?: Push
  children: ReactNode
}) {
  // Created once, on first render: `createPush` asks the server what it can do,
  // and re-asking on every render would be a request per render.
  const [handle] = useState(() => push ?? createPush())
  return <PushContext.Provider value={handle}>{children}</PushContext.Provider>
}

/** The shared handle and its current state, re-rendering when either moves.
 *
 *  Outside a provider there is no handle and the state is `unsupported`, which
 *  is also what a browser that cannot do this reports — so a component never
 *  has to branch on "is there a provider". */
export function usePush(): { push: Push | null; state: PushState; token?: string } {
  const push = useContext(PushContext)
  const subscribe = useCallback(
    (listener: () => void) => push?.subscribe(listener) ?? (() => {}),
    [push],
  )
  const state = useSyncExternalStore(subscribe, () => push?.state ?? 'unsupported')
  const token = useSyncExternalStore(subscribe, () => push?.token)
  return { push, state, token }
}
