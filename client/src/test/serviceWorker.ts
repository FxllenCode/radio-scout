/**
 * A stand-in for `navigator.serviceWorker`, which jsdom does not implement at
 * all — the property simply isn't there, which is also how a browser without
 * service workers looks.
 *
 * This is a *browser* fake: tests drive it the way a browser would (a new
 * worker is found, it finishes installing, it takes control) and assert on what
 * our module did about it. `EventTarget` does the event plumbing so the fake
 * never has to imitate `addEventListener`.
 */
import { act } from '@testing-library/react'
import { expect, onTestFinished, vi } from 'vitest'

export class FakeWorker extends EventTarget {
  state: ServiceWorkerState
  /** Every `postMessage`, in order — how a test sees `SKIP_WAITING` sent. */
  posted: unknown[] = []

  constructor(state: ServiceWorkerState = 'installing') {
    super()
    this.state = state
  }

  postMessage(message: unknown): void {
    this.posted.push(message)
  }

  /** Move to a new lifecycle state, as the browser does. */
  become(state: ServiceWorkerState): void {
    this.state = state
    this.dispatchEvent(new Event('statechange'))
  }
}

export class FakeRegistration extends EventTarget {
  installing: FakeWorker | null = null
  waiting: FakeWorker | null = null
  active: FakeWorker | null = null

  /** The browser found a new version and started installing it. */
  findUpdate(): FakeWorker {
    const worker = new FakeWorker('installing')
    this.installing = worker
    this.dispatchEvent(new Event('updatefound'))
    return worker
  }

  /** …and it finished, and is now waiting for the old one to let go. */
  finishInstalling(worker: FakeWorker): void {
    this.installing = null
    this.waiting = worker
    worker.become('installed')
  }
}

export class FakeContainer extends EventTarget {
  /** The worker running this page, if one already is. */
  controller: FakeWorker | null
  /** Every URL registration was asked for. */
  registered: string[] = []
  readonly registration = new FakeRegistration()
  /** Set to make `register` reject, as a bad scope or an offline load does. */
  failure?: Error

  constructor(controlled: boolean) {
    super()
    this.controller = controlled ? new FakeWorker('activated') : null
  }

  register(url: string): Promise<FakeRegistration> {
    this.registered.push(url)
    return this.failure
      ? Promise.reject(this.failure)
      : Promise.resolve(this.registration)
  }

  /** The waiting worker took over — what a browser fires after skipWaiting. */
  takeControl(worker: FakeWorker): void {
    this.controller = worker
    this.registration.waiting = null
    this.registration.active = worker
    worker.become('activated')
    this.dispatchEvent(new Event('controllerchange'))
  }
}

export interface ServiceWorkerFakeOptions {
  /** True for a page a worker already runs — i.e. not the very first load.
   *  It is the difference between "a new version is ready" and "the first
   *  version just installed", which must not be announced as an update. */
  controlled?: boolean
}

/** Install the fake on `navigator`, undone when the test finishes. */
export function installServiceWorker({
  controlled = true,
}: ServiceWorkerFakeOptions = {}): FakeContainer {
  const container = new FakeContainer(controlled)
  Object.defineProperty(navigator, 'serviceWorker', {
    value: container,
    configurable: true,
    writable: true,
  })
  onTestFinished(() => Reflect.deleteProperty(navigator, 'serviceWorker'))
  return container
}

/** Wait for the registration promise to settle, so the browser's lifecycle
 *  events have something listening for them. */
export function registered(container: FakeContainer): Promise<void> {
  return vi.waitFor(() => expect(container.registered).toHaveLength(1))
}

/**
 * Play out a deploy: a new version is found, finishes installing, and waits.
 *
 * Wrapped in `act` and awaiting registration first, so a component driven by
 * this sees the whole sequence the way a browser delivers it.
 */
export async function deploy(container: FakeContainer): Promise<FakeWorker> {
  await registered(container)
  const next = container.registration.findUpdate()
  await act(async () => container.registration.finishInstalling(next))
  return next
}
