import { render } from '@testing-library/react'
import { Provider } from 'react-redux'
import { userEvent } from 'vitest/browser'
import { describe, expect, it, vi } from 'vitest'

import { CallPlayer } from '@/components/CallPlayer'
import { ARTWORK_SIZES } from '@/lib/artwork'
import { PushProvider } from '@/hooks/usePush'
import { advance, received, replay } from '@/store/live'
import { makeStore, type AppStore } from '@/store/store'
import { inertPush } from '@/test/push'
import { wavDataUrl } from '@/test/wav'
import type { Call } from '@/types'

/**
 * The audio player and its Media-Session wiring, in a browser that has a media
 * stack (#34, ADR-0010's middle layer).
 *
 * Everything here is something the jsdom suite structurally cannot say. There,
 * `play`/`pause`/`load` are functions `src/test/setup.ts` defines, `duration` is
 * a number a test assigns, and `MediaMetadata` stores whatever object it is
 * handed. That is the right place to test what the component *decides*, and most
 * of `CallPlayer` stays there. It is the wrong place — an impossible place — to
 * ask whether a real decoder accepts the WAV we write by hand, whether swapping
 * `src` on one element restarts playback, or whether a real `MediaSession` keeps
 * the artwork we encode.
 *
 * **Not** iOS. Chromium on a desktop is a real engine, not the platform whose
 * background behaviour this all exists for; that stays the real-device manual
 * gate (ADR-0005, #33).
 */

/** A Call whose audio is real, decodable bytes — no network, no mock. */
function call(id: number, seconds = 0.4): Call {
  return {
    id,
    systemRef: 11,
    systemLabel: 'Fulton County',
    talkgroupRef: 54241,
    talkgroupLabel: 'FD Dispatch',
    talkgroupTag: 'Fire Dispatch',
    talkgroupGroup: 'Fire',
    audioUrl: wavDataUrl(seconds, 300 + id * 100),
  }
}

function mount(): { store: AppStore; audio: HTMLAudioElement } {
  // No storage: this layer is about the element, and a persisted Selection
  // leaking between files would decide which Calls arrive.
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
  return { store, audio }
}

/**
 * Grant the page user activation, the way a listener's first tap does.
 *
 * Chromium's autoplay policy is left on (see `vitest.browser.config.ts`), so
 * this is what lets audio start — and it is faithful rather than a workaround:
 * on a device the gesture that unlocks the audio session is the listener
 * pressing play once, and it holds for the life of the page. A Call arriving
 * over the live feed has no gesture of its own, and never will.
 */
async function listenerIsHere() {
  const button = document.createElement('button')
  button.textContent = 'start listening'
  document.body.append(button)
  await userEvent.click(button)
  button.remove()
}

/** The element really is playing decoded audio — not merely un-paused. */
async function playing(audio: HTMLAudioElement) {
  await expect.poll(() => audio.paused).toBe(false)
  // HAVE_CURRENT_DATA or better: the decoder produced samples.
  await expect.poll(() => audio.readyState).toBeGreaterThanOrEqual(2)
}

describe('the one reused element', () => {
  it('plays a Call, reporting the duration the file really is', async () => {
    await listenerIsHere()
    const { store, audio } = mount()

    store.dispatch(received(call(1, 0.4), 1))

    await playing(audio)
    // Decoded, not declared: nothing in this test told it 0.4.
    await expect.poll(() => audio.duration).toBeGreaterThan(0.3)
    expect(audio.duration).toBeLessThan(0.6)
    // ...and the store learned it from the element — which is what draws the
    // waveform and sets the lock-screen scrubber.
    await expect
      .poll(() => store.getState().transport.duration)
      .toBeGreaterThan(0.3)
  })

  /**
   * ADR-0005's central decision, checked where it can be: **one element, reused**
   * — never WebAudio (iOS mutes it in the background, which is why rdio-scanner
   * has no working background audio) and never a fresh element per Call (that
   * tears down the audio session in the gaps). So a new Call has to restart
   * playback through `src` alone.
   */
  it('restarts playback on the same element when the Call changes', async () => {
    await listenerIsHere()
    const { store, audio } = mount()
    store.dispatch(received(call(1, 0.4), 1))
    await playing(audio)
    const first = audio.src

    store.dispatch(received(call(2, 0.4), 2))

    await expect.poll(() => audio.src).not.toBe(first)
    await playing(audio)
    // The same element — which is the whole assertion. (That a fresh `src`
    // starts at zero is the browser's doing, not ours; the rewind we *do* own is
    // the one below, where `src` never changes.)
    expect(document.querySelectorAll('audio')).toHaveLength(1)
  })

  /**
   * Replay hands back the Call already loaded, so `src` does not change — and an
   * element sitting at the end of it would "play" in silence. Only a real media
   * stack has an end to sit at.
   */
  it('rewinds a Call it is already holding, rather than playing its end', async () => {
    await listenerIsHere()
    const { store, audio } = mount()
    store.dispatch(received(call(1, 0.4), 1))
    await playing(audio)
    // Let it get somewhere before asking for it again.
    await expect.poll(() => audio.currentTime).toBeGreaterThan(0.1)
    const src = audio.src

    store.dispatch(replay(1))

    await expect.poll(() => audio.currentTime).toBeLessThan(0.1)
    expect(audio.src).toBe(src)
    await playing(audio)
  })
})

/**
 * The lock-screen buttons, bound in a real `MediaSession`.
 *
 * `setActionHandler` **throws** for an action the browser does not implement,
 * which is why `bindTransport` binds each one inside its own `try` — "a missing
 * control must cost us that control, never the audio". jsdom's fake accepts
 * every action name it is handed, so it can only say we *asked*; a real
 * implementation is the only thing that can say which asks were honoured, and
 * the failure mode when they are not is dead lock-screen buttons.
 */
describe('the lock-screen transport', () => {
  it('binds the four actions a queue needs, in a real implementation', async () => {
    await listenerIsHere()
    const bound = vi.spyOn(globalThis.MediaSession.prototype, 'setActionHandler')

    mount()

    // Every one of the four was accepted — nothing landed in `bindTransport`'s
    // catch, which is where an unimplemented action goes.
    const accepted = bound.mock.calls
      .filter(([, handler]) => handler !== null)
      .map(([action]) => action)
    expect(new Set(accepted)).toEqual(
      new Set(['play', 'pause', 'nexttrack', 'previoustrack']),
    )
    expect(bound.mock.results.map((one) => one.type)).not.toContain('throw')
  })
})

/**
 * The keep-alive loop (#15, spec US 31) is a WAV this project writes byte by
 * byte in `lib/silence.ts` — RIFF sizes, block align, a ±1-LSB square. jsdom
 * decodes nothing, so nothing had ever *played* it: a wrong chunk length would
 * have shipped as silence on the one platform the mechanism exists for, and the
 * suite would have stayed green.
 *
 * **What is deliberately not asserted: whether `loop` is gapless.** #34 asks for
 * it, and it is out of reach here — measuring the seam would need WebAudio to
 * sample the output, and the gap that matters is the one iOS's audio session
 * sees, not Chromium's. A gap long enough to drop the session is exactly the
 * failure the real-device gate (#33, research §14) exists to catch, so that is
 * where it stays rather than being approximated with a number that would pass on
 * a laptop either way.
 */
describe('the keep-alive loop', () => {
  it('is audio a real decoder accepts, and it loops', async () => {
    await listenerIsHere()
    const { store, audio } = mount()
    // The gap it bridges: a Call has played, and nothing is behind it.
    store.dispatch(received(call(1, 0.4), 1))
    await playing(audio)
    store.dispatch(advance())

    await expect.poll(() => audio.loop).toBe(true)
    await playing(audio)
    // One second, exactly as `silence.ts` builds it — which is only true if the
    // header it wrote describes the samples it wrote.
    await expect.poll(() => audio.duration).toBeGreaterThan(0.9)
    expect(audio.duration).toBeLessThan(1.1)
  })
})

describe('the Media Session', () => {
  /**
   * A real `MediaMetadata`, constructed with what we hand it. jsdom's is a fake
   * that keeps whatever object it is given; a browser's validates the shape and
   * normalises the artwork list.
   */
  it('keeps the metadata we give it', async () => {
    await listenerIsHere()
    const { store, audio } = mount()

    store.dispatch(received(call(1, 0.4), 1))
    await playing(audio)

    const metadata = navigator.mediaSession.metadata
    expect(metadata).not.toBeNull()
    expect(metadata?.title).toBe('FD Dispatch')
    expect(metadata?.artist).toBe('Fulton County')
    expect(metadata?.album).toBe('Fire Dispatch · Fire')
    expect(metadata?.artwork.length).toBe(ARTWORK_SIZES.length)
    await expect.poll(() => navigator.mediaSession.playbackState).toBe('playing')
  })

  /**
   * The artwork is an **image a real decoder paints** — at the size it claims.
   *
   * This is the assertion that needed a browser, and it is not the one you would
   * first reach for. Chromium takes the artwork list without looking at the
   * bytes: it stores URLs and decodes them later, when something actually draws
   * a lock screen. So `metadata.artwork` being non-empty says *nothing* about the
   * PNG being valid — I checked, by corrupting every chunk CRC in `lib/png.ts`
   * and watching this file stay green.
   *
   * `png.test.ts` parses the bytes we wrote and confirms they say what we meant,
   * which is a different question: it is our encoder agreeing with itself. Only
   * an image decoder can say the file is one a browser will paint, so this hands
   * each artwork URL to `createImageBitmap` and checks the dimensions come back
   * as the size the metadata advertises.
   */
  it('publishes artwork a real image decoder paints at the size it claims', async () => {
    await listenerIsHere()
    const { store, audio } = mount()

    store.dispatch(received(call(1, 0.4), 1))
    await playing(audio)

    const artwork = [...(navigator.mediaSession.metadata?.artwork ?? [])]
    expect(artwork).toHaveLength(ARTWORK_SIZES.length)
    for (const image of artwork) {
      expect(image.type).toBe('image/png')
      const response = await fetch(image.src)
      const bitmap = await createImageBitmap(await response.blob())
      // `sizes` is what the OS picks from; a PNG whose header disagreed with it
      // would be scaled or dropped on a lock screen.
      expect(`${bitmap.width}x${bitmap.height}`).toBe(image.sizes)
      bitmap.close()
    }
  })

  /**
   * `setPositionState` is "limited availability" and **rejects** a pair it
   * dislikes — a `NaN` duration, a position past the end. `lib/mediaSession.ts`
   * swallows that on purpose ("the scrubber is polish; losing it must not take
   * the player down"), which means a browser refusing it is *invisible* from
   * outside: nothing throws, nothing changes, and the scrubber is quietly gone.
   *
   * So this asserts on the call itself, through a spy on the real
   * implementation — the pair we pass has to be one a browser accepts, and the
   * duration in it has to be the element's, not a placeholder. Asserting "the
   * player is still mounted" would have proved nothing at all, since it would
   * still be mounted either way.
   */
  it('passes a real browser a position state it accepts', async () => {
    await listenerIsHere()
    const accepted = vi.spyOn(
      globalThis.MediaSession.prototype,
      'setPositionState',
    )
    const { store, audio } = mount()

    store.dispatch(received(call(1, 0.4), 1))
    await playing(audio)
    await expect.poll(() => audio.duration).toBeGreaterThan(0)

    // The state carrying a duration — `setNowPlaying` clears it first with no
    // argument, so the interesting call is the one with a pair in it.
    await expect
      .poll(() => accepted.mock.calls.filter(([state]) => state).length)
      .toBeGreaterThan(0)
    const [state] = accepted.mock.calls.findLast(([one]) => one) ?? []
    expect(state?.duration).toBeCloseTo(audio.duration, 1)
    expect(state?.position).toBe(0)
    expect(state?.playbackRate).toBe(1)
    // ...and the browser *accepted* it. A pair it dislikes throws, which the
    // wrapper catches, so the throw is invisible from outside — but the spy
    // wraps the real implementation, and its recorded result is not.
    expect(accepted.mock.results.map((one) => one.type)).not.toContain('throw')
  })
})
