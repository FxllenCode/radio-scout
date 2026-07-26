import { useEffect, useRef, useState } from 'react'

import { DockedBanner } from '@/components/layout/DockedBanner'
import { Button } from '@/components/ui/button'
import { createInstall, type Install, type InstallOffer } from '@/lib/install'

import { StatusLed } from './StatusLed'

/**
 * The install-to-home-screen banner (#15, spec US 28; design brief 19A).
 *
 * Installing isn't a nicety on this app — it is the gate. iOS gives a
 * home-screen web app the standalone display mode background audio needs, and
 * Web Push (#16) is offered to nothing else. rdio-scanner's answer to that was
 * to ship separate native apps; ours is to ask, once, in the place a phone
 * expects to be asked: docked above the tab bar, dismissible, and remembered.
 */
export function InstallBanner() {
  const [offer, setOffer] = useState<InstallOffer>('none')
  const install = useRef<Install>(null)

  useEffect(() => {
    const created = createInstall()
    install.current = created
    const sync = () => setOffer(created.offer)
    const release = created.subscribe(sync)
    sync()
    return () => {
      release()
      created.destroy()
      install.current = null
    }
  }, [])

  if (offer === 'none') return null

  return (
    <DockedBanner>
      <span className="flex size-10 shrink-0 items-center justify-center rounded-[10px] border border-border bg-background">
        <StatusLed color="red" size={14} />
      </span>
      <div className="min-w-0 flex-1">
        <p className="text-sm font-semibold">Install Radio-Scout</p>
        <p className="text-xs text-muted-foreground">
          {offer === 'prompt'
            ? 'Full screen, faster start, background audio'
            : 'Tap Share, then Add to Home Screen'}
        </p>
      </div>
      {offer === 'prompt' && (
        <Button size="sm" onClick={() => void install.current?.install()}>
          Install
        </Button>
      )}
      <button
        type="button"
        aria-label="Dismiss install prompt"
        className="shrink-0 rounded-md px-1.5 py-1 text-muted-foreground hover:text-foreground"
        onClick={() => install.current?.dismiss()}
      >
        ✕
      </button>
    </DockedBanner>
  )
}
