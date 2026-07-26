/**
 * Install-to-home-screen (#15, spec US 28).
 *
 * Installing is what unlocks the rest of the mobile story: iOS gives a
 * home-screen web app the standalone display mode background audio needs, and
 * Web Push (#16) is available to *nothing else* (research §6). So the app has
 * to ask — and ask well.
 */

import { notifier } from './notifier'
import { readStored, writeStored } from './persist'

/** Where "not now" is remembered. */
const DISMISSED_KEY = 'radio-scout:install-dismissed'

/** What the app can offer the listener right now. */
export type InstallOffer =
  /** Nothing to offer: already installed, or this browser can't. */
  | 'none'
  /** The browser handed us its own install dialog to fire. */
  | 'prompt'
  /** No dialog exists (iOS Safari) — we can only show the steps. */
  | 'manual'

/** Chromium's `BeforeInstallPromptEvent`: not in any TS lib, because no
 *  standard defines it. */
interface InstallPromptEvent extends Event {
  prompt(): Promise<void>
  userChoice: Promise<{ outcome: 'accepted' | 'dismissed' }>
}

declare global {
  interface Navigator {
    /** iOS Safari only, and the whole point: it exists nowhere else, so its
     *  *presence* identifies the one browser that installs through the Share
     *  sheet, and its value says whether it already did. A feature check, not
     *  a user-agent sniff. */
    standalone?: boolean
  }
}

/** Are we the installed app rather than a browser tab? Two signals, because
 *  iOS Safari answered this years before it understood `display-mode`. */
function isInstalled(): boolean {
  if (navigator.standalone) return true
  return (
    globalThis.matchMedia?.('(display-mode: standalone)').matches === true
  )
}

export interface Install {
  /** What to offer right now. */
  readonly offer: InstallOffer
  /** Be told when [`offer`] changes; returns the release. The browser decides
   *  when a page qualifies to be installed, and it is rarely the moment the
   *  page loaded. */
  subscribe(listener: () => void): () => void
  /** Show the browser's install dialog and report what the listener chose. */
  install(): Promise<'accepted' | 'dismissed'>
  /** "Not now" — remembered, so the app asks once rather than nagging. */
  dismiss(): void
  /** Stop listening to the browser. */
  destroy(): void
}

export interface InstallOptions {
  /** Where the dismissal is remembered. Defaults to the browser's — which a
   *  browser may genuinely not have (site data blocked, sandboxed context),
   *  and a banner that can't be remembered is better than one that throws. */
  storage?: Storage
}

export function createInstall({
  storage = globalThis.localStorage,
}: InstallOptions = {}): Install {
  let pending: InstallPromptEvent | null = null
  // Held in memory as well as in storage: a browser that refuses to remember
  // must still let the listener close the banner for this visit.
  let dismissed = readStored(storage, DISMISSED_KEY) === 'true'
  const changes = notifier()

  const captured = (event: Event) => {
    // Chromium shows its own install bar unless the page takes the event —
    // and ours is the one that belongs above the tab bar (design brief 19).
    event.preventDefault()
    pending = event as InstallPromptEvent
    changes.changed()
  }
  // The tab that asked stays open after the install, and its own display mode
  // never changes — so without this the banner would keep offering to install
  // an app that already exists.
  let installed = false
  const finished = () => {
    installed = true
    pending = null
    changes.changed()
  }

  const dismiss = () => {
    dismissed = true
    writeStored(storage, DISMISSED_KEY, 'true')
    changes.changed()
  }

  window.addEventListener('beforeinstallprompt', captured)
  window.addEventListener('appinstalled', finished)

  return {
    dismiss,

    subscribe: changes.subscribe,

    get offer(): InstallOffer {
      if (dismissed || installed || isInstalled()) return 'none'
      if (pending) return 'prompt'
      // No dialog, but a browser that can install: the Share sheet is the
      // only route, so the app explains it (design brief 19).
      return navigator.standalone === false ? 'manual' : 'none'
    },

    async install() {
      if (!pending) return 'dismissed'
      // Single-use: the browser refuses a second dialog from the same event,
      // so the offer is spent whichever way the listener answers.
      const event = pending
      pending = null
      await event.prompt()
      const { outcome } = await event.userChoice
      // "Not now" in the browser's own dialog is the same no as closing our
      // banner. An accept needs no remembering — `appinstalled` follows.
      if (outcome === 'dismissed') dismiss()
      else changes.changed()
      return outcome
    },

    destroy() {
      window.removeEventListener('beforeinstallprompt', captured)
      window.removeEventListener('appinstalled', finished)
      changes.clear()
    },
  }
}
