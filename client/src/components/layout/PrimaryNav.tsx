import { ListFilter, Radio, Search, Settings } from 'lucide-react'
import { NavLink } from 'react-router-dom'

import { cn } from '@/lib/utils'

const tabs = [
  { to: '/', label: 'Live', icon: Radio, end: true },
  { to: '/talkgroups', label: 'Talkgroups', icon: ListFilter, end: false },
  { to: '/search', label: 'Search', icon: Search, end: false },
  { to: '/settings', label: 'Settings', icon: Settings, end: false },
] as const

/** The primary navigation (docs/design/brief.md): Live · Talkgroups · Search ·
 *  Settings. Active tab uses the restrained near-white accent; color is
 *  otherwise reserved for LEDs.
 *
 *  `searchTo` is where the Search tab points: since #61 the search state lives
 *  in the URL, so a bare `/search` here would discard the filters the Listener
 *  set on the way past. The shell remembers it; this only follows, because a
 *  tab bar that unmounts and remounts with each screen could not.
 *
 *  `layout` is where it sits (#78): along the bottom on a phone, where the
 *  thumb is, and down the left on a desk, where there is no thumb and a bottom
 *  bar would spend a strip the width of the screen on four words. */
export function PrimaryNav({
  searchTo = '/search',
  layout = 'bottom',
}: {
  searchTo?: string
  layout?: 'bottom' | 'side'
}) {
  const side = layout === 'side'
  return (
    <nav
      aria-label="Primary"
      data-layout={layout}
      className={cn(
        'z-20 bg-background/90 backdrop-blur',
        side
          ? 'sticky top-0 h-[100dvh] w-52 shrink-0 border-r border-border pt-[calc(env(safe-area-inset-top)+1.25rem)]'
          : 'fixed inset-x-0 bottom-0 mx-auto max-w-2xl border-t border-border pb-[env(safe-area-inset-bottom)]',
      )}
    >
      <ul className={side ? 'flex flex-col gap-1 px-3' : 'grid grid-cols-4'}>
        {tabs.map(({ to, label, icon: Icon, end }) => (
          <li key={to}>
            <NavLink
              to={to === '/search' ? searchTo : to}
              end={end}
              className={({ isActive }) =>
                cn(
                  'font-medium transition-colors',
                  side
                    ? 'flex min-h-11 items-center gap-3 rounded-lg px-3 text-sm'
                    : 'flex flex-col items-center gap-1 py-2.5 text-[11px]',
                  isActive
                    ? side
                      ? 'bg-muted text-foreground'
                      : 'text-foreground'
                    : 'text-muted-foreground hover:text-foreground',
                )
              }
            >
              <Icon className="size-5" aria-hidden />
              <span>{label}</span>
            </NavLink>
          </li>
        ))}
      </ul>
    </nav>
  )
}
