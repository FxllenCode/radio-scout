import { describe, expect, it, onTestFinished, vi } from 'vitest'

import {
  FakeWorker,
  installServiceWorker,
  registered,
} from '@/test/serviceWorker'

import { registerServiceWorker, SKIP_WAITING } from './serviceWorker'

describe('service worker', () => {
  // At the root, so its scope is the whole app rather than a subdirectory of
  // it — a worker registered under /assets/ would control nothing that matters.
  it('registers at the app root', async () => {
    const container = installServiceWorker()

    const worker = registerServiceWorker({ reload: vi.fn() })

    await vi.waitFor(() => expect(container.registered).toEqual(['/sw.js']))
    worker.destroy()
  })

  it('is a no-op where the browser has no service workers', () => {
    expect(() => registerServiceWorker({ reload: vi.fn() })).not.toThrow()
  })

  // The rule this module exists for: a deploy must never interrupt someone who
  // is listening. The new version waits until it is asked for.
  it('announces a waiting version and reloads nothing', async () => {
    const container = installServiceWorker()
    const reload = vi.fn()
    const handle = registerServiceWorker({ reload })
    const changed = vi.fn()
    handle.subscribe(changed)
    await registered(container)

    const next = container.registration.findUpdate()
    container.registration.finishInstalling(next)

    expect(handle.updateReady).toBe(true)
    expect(changed).toHaveBeenCalled()
    expect(reload).not.toHaveBeenCalled()
    expect(next.posted).toEqual([])
    handle.destroy()
  })

  // The very first visit installs a worker too. That is not an update, and
  // offering to reload for it would be a banner nobody can explain.
  it('says nothing about the first worker a page ever installs', async () => {
    const container = installServiceWorker({ controlled: false })
    const handle = registerServiceWorker({ reload: vi.fn() })
    await registered(container)

    const first = container.registration.findUpdate()
    container.registration.finishInstalling(first)

    expect(handle.updateReady).toBe(false)
    handle.destroy()
  })

  // A tab left open across a deploy and then reloaded: the new version is
  // already waiting before we ever attach a listener.
  it('finds a version that was already waiting when the page loaded', async () => {
    const container = installServiceWorker()
    container.registration.waiting = new FakeWorker('installed')

    const handle = registerServiceWorker({ reload: vi.fn() })
    await registered(container)

    expect(handle.updateReady).toBe(true)
    handle.destroy()
  })

  // A worker can finish installing and be discarded — a second tab won the
  // race, or the browser threw it away. Offering to reload for one would send
  // the listener to a version that is already gone.
  it('says nothing about a worker that never becomes the waiting one', async () => {
    const container = installServiceWorker()
    const handle = registerServiceWorker({ reload: vi.fn() })
    await registered(container)

    const discarded = container.registration.findUpdate()
    discarded.become('redundant')

    expect(handle.updateReady).toBe(false)
    handle.destroy()
  })

  it('shrugs at an update the browser announces and then withdraws', async () => {
    const container = installServiceWorker()
    const handle = registerServiceWorker({ reload: vi.fn() })
    await registered(container)

    container.registration.dispatchEvent(new Event('updatefound'))

    expect(handle.updateReady).toBe(false)
    handle.destroy()
  })

  it('applies a waiting version only when asked, then reloads once it takes over', async () => {
    const container = installServiceWorker()
    const reload = vi.fn()
    const handle = registerServiceWorker({ reload })
    await registered(container)
    const next = container.registration.findUpdate()
    container.registration.finishInstalling(next)

    handle.applyUpdate()

    // Telling the worker to step forward is not the same as reloading: the
    // page waits for the new one to actually be in charge.
    expect(next.posted).toEqual([SKIP_WAITING])
    expect(reload).not.toHaveBeenCalled()

    container.takeControl(next)
    expect(reload).toHaveBeenCalledTimes(1)
    handle.destroy()
  })

  // A second tab can take the update, which changes *this* tab's controller
  // without anyone here asking. Reloading on that would throw a listener out
  // of the app mid-Call — the exact thing this module exists to prevent.
  it('does not reload for a handover it never asked for', async () => {
    const container = installServiceWorker()
    const reload = vi.fn()
    const handle = registerServiceWorker({ reload })
    await registered(container)
    const next = container.registration.findUpdate()
    container.registration.finishInstalling(next)

    container.takeControl(next)

    expect(reload).not.toHaveBeenCalled()
    handle.destroy()
  })

  // Nothing injects `reload` in the app — the default is the one that actually
  // runs on a listener's phone.
  it('reloads the page itself when the caller offers no other way', async () => {
    const reload = vi.fn()
    vi.stubGlobal('location', { reload })
    onTestFinished(() => {
      vi.unstubAllGlobals()
    })
    const container = installServiceWorker()
    const handle = registerServiceWorker()
    await registered(container)
    const next = container.registration.findUpdate()
    container.registration.finishInstalling(next)

    handle.applyUpdate()
    container.takeControl(next)

    expect(reload).toHaveBeenCalledTimes(1)
    handle.destroy()
  })

  it('does nothing when asked to apply an update that isn’t there', async () => {
    const container = installServiceWorker()
    const reload = vi.fn()
    const handle = registerServiceWorker({ reload })
    await registered(container)

    handle.applyUpdate()

    expect(reload).not.toHaveBeenCalled()
    handle.destroy()
  })

  // A worker that won't register costs caching and offline, nothing else. The
  // app must still run — and an unhandled rejection must not reach the console.
  it('survives a registration the browser refuses', async () => {
    const container = installServiceWorker()
    container.failure = new Error('insecure origin')

    const handle = registerServiceWorker({ reload: vi.fn() })
    await registered(container)

    expect(handle.updateReady).toBe(false)
    handle.destroy()
  })
})
