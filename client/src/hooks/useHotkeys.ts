import { useEffect } from 'react'

import { useInstance } from '@/hooks/useInstance'
import { hotkeyFor, type HotkeyCommand } from '@/lib/hotkeys'
import { useAppDispatch, useAppStore } from '@/store/hooks'
import {
  avoid,
  selectLiveControls,
  toggleHoldSystem,
  toggleHoldTalkgroup,
} from '@/store/live'
import { nextCall, replayLive, selectNowPlaying, togglePause } from '@/store/transport'
import type { AppDispatch, RootState } from '@/store/store'

/**
 * What each key does, and whether it may — or `false` for a key that has
 * nothing to act on right now, which is then left to the browser (Space
 * scrolls a page with nothing playing, as it always did).
 *
 * Avoid, Hold and Replay are gated exactly as their buttons are
 * (`controlsFor`, through `selectLiveControls`). Pause and Skip are the
 * exception the mini-player already makes: they act on whichever source owns
 * the audio, live or archive, so they are gated on there being audio at all —
 * which is why they still work in **Playback mode**, where the Live screen's
 * own Pause and Skip are greyed out.
 */
function perform(
  command: HotkeyCommand,
  state: RootState,
  dispatch: AppDispatch,
  goesLive: boolean,
): boolean {
  const can = selectLiveControls(state)
  const playing = selectNowPlaying(state) !== null
  switch (command) {
    case 'pause':
      if (playing) dispatch(togglePause())
      return playing
    case 'skip':
      if (playing) dispatch(nextCall(goesLive))
      return playing
    case 'replay':
      // The button's own rule, and its own gate.
      if (can.replay) dispatch(replayLive())
      return can.replay
    case 'avoid':
      if (can.avoid) dispatch(avoid({ until: 0 }))
      return can.avoid
    case 'holdTalkgroup':
      if (can.holdTalkgroup) dispatch(toggleHoldTalkgroup())
      return can.holdTalkgroup
    case 'holdSystem':
      if (can.holdSystem) dispatch(toggleHoldSystem())
      return can.holdSystem
  }
}

/**
 * The keyboard, on every tab (#78, spec US 55) — mounted in the shell beside
 * the audio element, because that is what it controls and it does not stop
 * at the edge of the Live screen. The keymap is `lib/hotkeys`'.
 *
 * The state is read at the moment of the key press rather than subscribed to,
 * so a listener that is registered once answers for whatever is true when the
 * key goes down.
 */
export function useHotkeys(): void {
  const dispatch = useAppDispatch()
  const store = useAppStore()
  const goesLive = useInstance().playbackGoesLive

  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      const command = hotkeyFor(event)
      if (!command) return
      if (perform(command, store.getState(), dispatch, goesLive)) event.preventDefault()
    }
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  }, [dispatch, store, goesLive])
}
