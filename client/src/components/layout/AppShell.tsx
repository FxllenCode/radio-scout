import { useEffect } from 'react'
import { Outlet } from 'react-router-dom'

import { CallPlayer } from '@/components/CallPlayer'
import { InstallBanner } from '@/components/InstallBanner'
import { LiveFeedLink } from '@/components/LiveFeedLink'
import { UpdateBanner } from '@/components/UpdateBanner'
import { useAppUpdate } from '@/hooks/useAppUpdate'
import { onPushOpen } from '@/lib/pushOpen'
import { useAppDispatch } from '@/store/hooks'
import { resume } from '@/store/transport'

import { BottomTabBar } from './BottomTabBar'

/** Mobile-first shell: a scrolling content area above a fixed bottom tab bar.
 *  On wider screens it centers to a comfortable column; the full desktop
 *  sidebar layout (brief item 29) is a later ticket.
 *
 *  The audio element and the live-feed socket live here, outside the router
 *  outlet, so playback and the listening queue survive moving between tabs
 *  (ADR-0005: one reused element; ADR-0004: the queue is client state). */
export function AppShell() {
  const update = useAppUpdate()
  const dispatch = useAppDispatch()

  // A tapped notification (#16) focuses this tab rather than opening a second
  // one, and the tap is the gesture that resumes playback — the listener came
  // back, so they should not land on a paused player they never paused.
  useEffect(() => onPushOpen(() => dispatch(resume())), [dispatch])

  return (
    <div className="mx-auto flex min-h-[100dvh] w-full max-w-2xl flex-col">
      <main className="flex-1 pb-24">
        <Outlet />
      </main>
      {/* One docked slot above the tab bar. A waiting version takes it: it is
          the rarer and the more actionable of the two, and a listener who
          reloads is offered the install again on the way back. */}
      {update.ready ? <UpdateBanner apply={update.apply} /> : <InstallBanner />}
      <BottomTabBar />
      <CallPlayer />
      <LiveFeedLink />
    </div>
  )
}
