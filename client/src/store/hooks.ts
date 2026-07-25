import { useDispatch, useSelector, useStore } from 'react-redux'

import type { AppDispatch, AppStore, RootState } from './store'

/** Typed Redux hooks — use these instead of the untyped originals. */
export const useAppDispatch = useDispatch.withTypes<AppDispatch>()
export const useAppSelector = useSelector.withTypes<RootState>()
/** For state a callback needs to *read* without subscribing to it — the live
 *  feed's catch-up cursor changes with every Call and must not re-subscribe. */
export const useAppStore = useStore.withTypes<AppStore>()
