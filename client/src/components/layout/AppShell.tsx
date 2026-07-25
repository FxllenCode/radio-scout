import { Outlet } from 'react-router-dom'

import { CallPlayer } from '@/components/CallPlayer'
import { LiveFeedLink } from '@/components/LiveFeedLink'

import { BottomTabBar } from './BottomTabBar'

/** Mobile-first shell: a scrolling content area above a fixed bottom tab bar.
 *  On wider screens it centers to a comfortable column; the full desktop
 *  sidebar layout (brief item 29) is a later ticket.
 *
 *  The audio element and the live-feed socket live here, outside the router
 *  outlet, so playback and the listening queue survive moving between tabs
 *  (ADR-0005: one reused element; ADR-0004: the queue is client state). */
export function AppShell() {
  return (
    <div className="mx-auto flex min-h-[100dvh] w-full max-w-2xl flex-col">
      <main className="flex-1 pb-24">
        <Outlet />
      </main>
      <BottomTabBar />
      <CallPlayer />
      <LiveFeedLink />
    </div>
  )
}
