import { describe, expect, it, vi } from 'vitest'

import { onPushOpen, PUSH_OPEN } from './pushOpen'

/** A stand-in for `navigator.serviceWorker`, which jsdom does not have. */
function source() {
  const target = new EventTarget()
  return {
    ...(target as unknown as {
      addEventListener: EventTarget['addEventListener']
      removeEventListener: EventTarget['removeEventListener']
    }),
    addEventListener: target.addEventListener.bind(target),
    removeEventListener: target.removeEventListener.bind(target),
    post: (data: unknown) =>
      target.dispatchEvent(Object.assign(new Event('message'), { data })),
  }
}

describe('a tapped notification handed to an open tab', () => {
  it('resumes the feed', () => {
    const worker = source()
    const resumed = vi.fn()
    onPushOpen(resumed, worker)

    worker.post({ type: PUSH_OPEN, url: '/?call=42' })

    expect(resumed).toHaveBeenCalledTimes(1)
  })

  // The worker also carries the update handover (`SKIP_WAITING`), and anything
  // else a later ticket adds: a message that isn't ours must not start audio.
  it('ignores every other message the worker sends', () => {
    const worker = source()
    const resumed = vi.fn()
    onPushOpen(resumed, worker)

    worker.post({ type: 'SKIP_WAITING' })
    worker.post(undefined)
    worker.post('a string')

    expect(resumed).not.toHaveBeenCalled()
  })

  it('stops listening when released', () => {
    const worker = source()
    const resumed = vi.fn()

    onPushOpen(resumed, worker)()
    worker.post({ type: PUSH_OPEN })

    expect(resumed).not.toHaveBeenCalled()
  })

  it('is inert in a browser with no service worker', () => {
    expect(() => onPushOpen(vi.fn(), undefined)()).not.toThrow()
  })
})
