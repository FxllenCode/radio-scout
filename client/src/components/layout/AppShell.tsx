import { Outlet, useLocation } from 'react-router-dom'

import { CallPlayer } from '@/components/CallPlayer'
import { InstallBanner } from '@/components/InstallBanner'
import { LiveFeedLink } from '@/components/LiveFeedLink'
import { MiniPlayer } from '@/components/MiniPlayer'
import { UpdateBanner } from '@/components/UpdateBanner'
import { useAppUpdate } from '@/hooks/useAppUpdate'
import { cn } from '@/lib/utils'
import { useAppSelector } from '@/store/hooks'
import { selectStrip } from '@/store/transport'

import { BottomTabBar } from './BottomTabBar'

/**
 * Where the mini-player does not go.
 *
 * The Live screen is the full player: the same Call, larger, with every control
 * rather than two. A strip there would spend the scarcest space on a phone
 * repeating what is already on screen.
 *
 * The **Search** screen is deliberately not on this list, though it draws its
 * own now-playing bar for a **Run**. That bar is inline in a long scrolling
 * result list, so it is gone the moment a Listener scrolls — which is precisely
 * the problem #56 exists to fix. The Live screen's display is the screen, and
 * never scrolls away from what it is about.
 */
const PLAYS_FOR_ITSELF = ['/']

/** Mobile-first shell: a scrolling content area above a fixed bottom tab bar.
 *  On wider screens it centers to a comfortable column; the full desktop
 *  sidebar layout (brief item 29) is a later ticket.
 *
 *  The audio element and the live-feed socket live here, outside the router
 *  outlet, so playback and the listening queue survive moving between tabs
 *  (ADR-0005: one reused element; ADR-0004: the queue is client state). Since
 *  #56 the *readout* does too: what is playing, or why nothing is, follows the
 *  Listener between tabs instead of being a thing only the Live screen knew. */
export function AppShell() {
  const update = useAppUpdate()
  const { pathname } = useLocation()
  const strip = useAppSelector(selectStrip)
  const room = !PLAYS_FOR_ITSELF.includes(pathname)
  const docked = room ? strip : null

  return (
    <div className="mx-auto flex min-h-[100dvh] w-full max-w-2xl flex-col">
      {/* Padded past the tab bar, and past the strip on every screen that can
          grow one — by *route*, never by whether something is playing. Keyed on
          the strip itself, a busy channel would shove the list under the
          Listener's thumb up and down every few seconds. A docked thing that
          covers the last row is a small lie about what the archive holds; a
          list that jumps while being read is a larger one. */}
      <main className={cn('flex-1', room ? 'pb-36' : 'pb-24')}>
        <Outlet />
      </main>
      {/* One docked column above the tab bar. A waiting version takes the
          banner slot from the install offer: it is the rarer and the more
          actionable of the two, and a listener who reloads is offered the
          install again on the way back. The mini-player sits under whichever
          won, closest to the thumb. */}
      <div className="pointer-events-none fixed inset-x-0 bottom-16 z-40 mx-auto flex max-w-2xl flex-col gap-2 px-3">
        {update.ready ? <UpdateBanner apply={update.apply} /> : <InstallBanner />}
        {docked && <MiniPlayer strip={docked} />}
      </div>
      <BottomTabBar />
      <CallPlayer />
      <LiveFeedLink />
    </div>
  )
}
