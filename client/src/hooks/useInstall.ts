import { useEffect, useRef, useState } from 'react'

import { createInstall, type Install, type InstallOffer } from '@/lib/install'

/** The install offer as React state, and the two things to do about it. */
export interface InstallState {
  /** What the banner offers ([`Install.offer`]). */
  offer: InstallOffer
  /** What the Settings entry offers ([`Install.entry`], #78). */
  entry: InstallOffer
  /** Fire the browser's own install dialog. */
  install(): void
  /** "Not now" — for the banner. */
  dismiss(): void
}

/**
 * The one install handle a page holds (#15, #78).
 *
 * One, because Chromium fires `beforeinstallprompt` once and the event is
 * single-use: a banner and a Settings entry each holding their own handle
 * would leave one of them offering an install it could never perform. So the
 * shell holds this, and both read it.
 */
export function useInstall(): InstallState {
  const [offer, setOffer] = useState<InstallOffer>('none')
  const [entry, setEntry] = useState<InstallOffer>('none')
  const handle = useRef<Install>(null)

  useEffect(() => {
    const created = createInstall()
    handle.current = created
    const sync = () => {
      setOffer(created.offer)
      setEntry(created.entry)
    }
    const release = created.subscribe(sync)
    sync()
    return () => {
      release()
      created.destroy()
      handle.current = null
    }
  }, [])

  return {
    offer,
    entry,
    install: () => void handle.current?.install(),
    dismiss: () => handle.current?.dismiss(),
  }
}
