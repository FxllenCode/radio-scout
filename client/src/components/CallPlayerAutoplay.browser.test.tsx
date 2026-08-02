import { render } from '@testing-library/react'
import { Provider } from 'react-redux'
import { expect, it } from 'vitest'

import { CallPlayer } from '@/components/CallPlayer'
import { PushProvider } from '@/hooks/usePush'
import { received } from '@/store/live'
import { makeStore } from '@/store/store'
import { selectIsPaused } from '@/store/transport'
import { inertPush } from '@/test/push'
import { wavDataUrl } from '@/test/wav'

/**
 * The `play()` rejection arm, against a browser genuinely refusing (#34).
 *
 * **A file of its own, and that is the whole mechanism.** User activation is a
 * property of the *page*, and Vitest gives each test file its own — so a single
 * `userEvent.click` anywhere in a file unlocks audio for every test after it.
 * This file never clicks anything, so Chromium's autoplay policy refuses with
 * `NotAllowedError`: exactly the production case the code was written for, since
 * a Call arriving over the live feed brings no gesture with it.
 *
 * In jsdom this arm is reachable only by replacing `play` with something that
 * rejects, which tests that the `catch` is wired rather than that a browser ever
 * takes it.
 *
 * What must *not* happen is a pause button drawn over silence. Recording the
 * refusal gives the listener a play button — one tap. Insisting we are playing
 * gives them silence and no way out of it.
 */
it('records a refused play() as paused, rather than showing it as playing', async () => {
  const store = makeStore({ storage: undefined })
  const { container } = render(
    <Provider store={store}>
      <PushProvider push={inertPush()}>
        <CallPlayer />
      </PushProvider>
    </Provider>,
  )
  const audio = container.querySelector('audio')
  if (!audio) throw new Error('the player renders one <audio> element')

  store.dispatch(
    received(
      {
        id: 1,
        systemRef: 11,
        talkgroupRef: 54241,
        talkgroupLabel: 'FD Dispatch',
        audioUrl: wavDataUrl(0.4, 440),
      },
      1,
    ),
  )

  await expect.poll(() => selectIsPaused(store.getState())).toBe(true)
  expect(audio.paused).toBe(true)
  // The Call is still the one loaded, so the tap that follows plays *it* rather
  // than skipping what the listener was refused.
  expect(store.getState().live.current?.id).toBe(1)
})
