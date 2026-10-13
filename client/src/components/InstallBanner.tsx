import { DismissButton, DockedBanner } from '@/components/layout/DockedBanner'
import { Button } from '@/components/ui/button'
import type { InstallState } from '@/hooks/useInstall'
import { useInstance } from '@/hooks/useInstance'
import { instanceName } from '@/lib/instance'

import { ChannelLed } from './Led'

/**
 * The install-to-home-screen banner (#15, spec US 28; design brief 19A).
 *
 * Installing isn't a nicety on this app — it is the gate. iOS gives a
 * home-screen web app the standalone display mode background audio needs, and
 * offers it to nothing else. rdio-scanner's answer to that was
 * to ship separate native apps; ours is to ask, once, in the place a phone
 * expects to be asked: docked above the tab bar, dismissible, and remembered.
 *
 * The handle is the shell's ([`useInstall`]), not this banner's: Settings
 * offers the same install again after "not now" (#78), and the browser's
 * install event is single-use.
 */
export function InstallBanner({ install }: { install: InstallState }) {
  // What it will be called on the home screen (#77): the manifest and the tab
  // both carry the Instance's name, so the offer says the same thing.
  const name = instanceName(useInstance())
  const { offer } = install

  if (offer === 'none') return null

  return (
    <DockedBanner>
      <span className="flex size-10 shrink-0 items-center justify-center rounded-[10px] border border-border bg-background">
        <ChannelLed color="red" size={14} />
      </span>
      <div className="min-w-0 flex-1">
        <p className="text-sm font-semibold">Install {name}</p>
        <p className="text-xs text-muted-foreground">
          {offer === 'prompt'
            ? 'Full screen, faster start, background audio'
            : 'Tap Share, then Add to Home Screen'}
        </p>
      </div>
      {offer === 'prompt' && (
        <Button className="h-11" onClick={install.install}>
          Install
        </Button>
      )}
      <DismissButton label="Dismiss install prompt" onClick={install.dismiss} />
    </DockedBanner>
  )
}
