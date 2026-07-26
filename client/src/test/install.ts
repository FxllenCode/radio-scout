/**
 * Stand-ins for the install surfaces a browser exposes, none of which jsdom
 * has: Chromium's `beforeinstallprompt` event, iOS Safari's
 * `navigator.standalone`, and the `display-mode` media query that says whether
 * we are already running installed.
 *
 * These are *browser* fakes, not fakes of our own code — tests fire them the
 * way a browser would and assert on what our module did about it.
 */
import { onTestFinished, vi } from 'vitest'

/**
 * Chromium's `BeforeInstallPromptEvent`: the browser hands the page its own
 * install dialog to fire later, and reports what the listener chose.
 */
export class FakeInstallPromptEvent extends Event {
  /** What the listener will pick when the dialog is shown. */
  outcome: 'accepted' | 'dismissed'
  /** How many times the page opened the dialog — a browser allows one. */
  prompted = 0

  constructor(outcome: 'accepted' | 'dismissed' = 'accepted') {
    super('beforeinstallprompt', { cancelable: true })
    this.outcome = outcome
  }

  prompt(): Promise<void> {
    this.prompted += 1
    return Promise.resolve()
  }

  get userChoice(): Promise<{ outcome: 'accepted' | 'dismissed' }> {
    return Promise.resolve({ outcome: this.outcome })
  }
}

/** Fire a `beforeinstallprompt`, as Chromium does once a page qualifies. */
export function offerInstall(
  outcome: 'accepted' | 'dismissed' = 'accepted',
): FakeInstallPromptEvent {
  const event = new FakeInstallPromptEvent(outcome)
  window.dispatchEvent(event)
  return event
}

/** Fire `appinstalled`, as a browser does the moment the install finishes —
 *  while the tab that asked for it is still open. */
export function completeInstall(): void {
  window.dispatchEvent(new Event('appinstalled'))
}

/** Be iOS Safari: no `beforeinstallprompt`, but a `navigator.standalone` that
 *  says whether the listener has already added us to their home screen. */
export function beIosSafari({ installed = false } = {}): void {
  Object.defineProperty(navigator, 'standalone', {
    value: installed,
    configurable: true,
    writable: true,
  })
  onTestFinished(() => Reflect.deleteProperty(navigator, 'standalone'))
}

/** Make `matchMedia` answer as a browser running us installed (or not). jsdom
 *  has `matchMedia` but never matches anything, which is the "not installed"
 *  answer — so only the installed case needs stubbing. */
export function beStandalone(standalone = true): void {
  vi.stubGlobal(
    'matchMedia',
    (query: string) =>
      ({
        matches: standalone && query.includes('standalone'),
        media: query,
        addEventListener() {},
        removeEventListener() {},
      }) as unknown as MediaQueryList,
  )
  onTestFinished(() => vi.unstubAllGlobals())
}
