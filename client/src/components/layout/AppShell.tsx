import { Outlet } from 'react-router-dom'

import { CallPlayer } from '@/components/CallPlayer'

import { BottomTabBar } from './BottomTabBar'

/** Mobile-first shell: a scrolling content area above a fixed bottom tab bar.
 *  On wider screens it centers to a comfortable column; the full desktop
 *  sidebar layout (brief item 29) is a later ticket.
 *
 *  The audio element lives here, outside the router outlet, so playback keeps
 *  going while the listener moves between tabs (ADR-0005: one reused element). */
export function AppShell() {
  return (
    <div className="mx-auto flex min-h-[100dvh] w-full max-w-2xl flex-col">
      <main className="flex-1 pb-24">
        <Outlet />
      </main>
      <BottomTabBar />
      <CallPlayer />
    </div>
  )
}
