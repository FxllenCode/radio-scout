import '@testing-library/jest-dom'
import { beforeEach } from 'vitest'

/**
 * Setup for the real-browser layer (#34).
 *
 * Deliberately almost empty, and that is the point. The jsdom setup
 * (`setup.ts`) has to define `play`/`pause`/`load` onto
 * `HTMLMediaElement.prototype` and stand up MSW over Node's fetch; here the
 * browser brings its own media stack, its own `MediaSession` and its own
 * decoder, and shimming any of them would be shimming the very things this layer
 * exists to exercise.
 *
 * There is no MSW either. Every test here drives audio from a `data:` URL, so a
 * real decoder is handed real bytes with no network in the way — simpler than a
 * service-worker-based mock, and closer to what is being asserted.
 */

declare global {
  // eslint-disable-next-line no-var
  var IS_REACT_ACT_ENVIRONMENT: boolean
}

/**
 * Not an `act` environment, deliberately.
 *
 * `act` exists to make asynchronous React *synchronous* for a test — the right
 * bargain in jsdom, where nothing is real and a test drives every step. It is
 * the wrong one here: what this layer asserts is genuinely asynchronous and
 * belongs to the browser. A decoder reaching `HAVE_CURRENT_DATA`, an autoplay
 * policy refusing a `play()` a tick later, `timeupdate` arriving on the media
 * clock — none of that is a React update waiting to be flushed, and none of it
 * can be made into one.
 *
 * So tests here poll for the observable outcome (`expect.poll`) rather than
 * pretending to control the schedule, and React is told as much instead of
 * warning about updates it was never going to be handed inside `act`.
 *
 * In `beforeEach` rather than once at import, because Testing Library turns the
 * flag *on* when its own module initialises — which happens after this file has
 * already run.
 */
beforeEach(() => {
  globalThis.IS_REACT_ACT_ENVIRONMENT = false
})
