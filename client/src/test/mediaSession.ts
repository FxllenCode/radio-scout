/**
 * A stand-in for the Media Session API, which jsdom does not implement.
 *
 * This is a *browser* fake, not a fake of our own code: tests assert on what
 * the adapter wrote to it, and `fire()` plays the part of the lock screen by
 * invoking a bound handler the way the OS would. Real browsers vary in what
 * they support, so the fake can be told to reject actions or to lack
 * `setPositionState` entirely.
 */
import { onTestFinished, vi } from 'vitest'

export class FakeMediaMetadata {
  title: string
  artist: string
  album: string
  artwork: MediaImage[]

  constructor(init: MediaMetadataInit = {}) {
    this.title = init.title ?? ''
    this.artist = init.artist ?? ''
    this.album = init.album ?? ''
    this.artwork = (init.artwork ?? []) as MediaImage[]
  }
}

export interface FakeMediaSession {
  metadata: FakeMediaMetadata | null
  playbackState: MediaSessionPlaybackState
  /** Every `setPositionState` call, in order — `undefined` means "cleared". */
  positions: (MediaPositionState | undefined)[]
  handlers: Map<MediaSessionAction, MediaSessionActionHandler | null>
  setActionHandler(
    action: MediaSessionAction,
    handler: MediaSessionActionHandler | null,
  ): void
  setPositionState?(state?: MediaPositionState): void
  /** Invoke a bound handler, as pressing the lock-screen button would. */
  fire(action: MediaSessionAction): void
}

export interface MediaSessionOptions {
  /** Actions whose `setActionHandler` throws, as an unsupported action does. */
  unsupported?: MediaSessionAction[]
  /** `true` for a browser with `setPositionState`, `false` for one without,
   *  `'throws'` for one that rejects the state we hand it (MDN flags the API
   *  "limited availability"). */
  positionState?: boolean | 'throws'
  /** Set false for a browser without `navigator.audioSession` (iOS < 16.4). */
  audioSession?: boolean
}

/** Install the fake on `navigator`, undone when the test finishes. */
export function installMediaSession({
  unsupported = [],
  positionState = true,
  audioSession = true,
}: MediaSessionOptions = {}): FakeMediaSession {
  const session: FakeMediaSession = {
    metadata: null,
    playbackState: 'none',
    positions: [],
    handlers: new Map(),
    setActionHandler(action, handler) {
      if (unsupported.includes(action)) {
        throw new TypeError(`unsupported action: ${action}`)
      }
      session.handlers.set(action, handler)
    },
    fire(action) {
      const handler = session.handlers.get(action)
      if (!handler) throw new Error(`no handler bound for ${action}`)
      handler({ action })
    },
  }
  if (positionState === 'throws') {
    session.setPositionState = () => {
      throw new TypeError('invalid position state')
    }
  } else if (positionState) {
    session.setPositionState = (state) => session.positions.push(state)
  }

  define(navigator, 'mediaSession', session)
  if (audioSession) define(navigator, 'audioSession', { type: 'auto' })
  vi.stubGlobal('MediaMetadata', FakeMediaMetadata)
  onTestFinished(() => {
    remove(navigator, 'mediaSession')
    remove(navigator, 'audioSession')
    vi.unstubAllGlobals()
  })
  return session
}

/** What `navigator.audioSession.type` currently is — `undefined` if the browser
 *  (or this test) has no AudioSession API. */
export function audioSessionType(): string | undefined {
  return (navigator as Navigator & { audioSession?: { type: string } })
    .audioSession?.type
}

function define(target: object, property: string, value: unknown) {
  Object.defineProperty(target, property, {
    value,
    configurable: true,
    writable: true,
  })
}

function remove(target: object, property: string) {
  Reflect.deleteProperty(target, property)
}
