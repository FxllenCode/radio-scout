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
 *  otherwise reserved for LEDs. */
export function BottomTabBar() {
  return (
    <nav
      aria-label="Primary"
      className="fixed inset-x-0 bottom-0 z-20 mx-auto max-w-2xl border-t border-border bg-background/90 pb-[env(safe-area-inset-bottom)] backdrop-blur"
    >
      <ul className="grid grid-cols-4">
        {tabs.map(({ to, label, icon: Icon, end }) => (
          <li key={to}>
            <NavLink
              to={to}
              end={end}
              className={({ isActive }) =>
                cn(
                  'flex flex-col items-center gap-1 py-2.5 text-[11px] font-medium transition-colors',
                  isActive
                    ? 'text-foreground'
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
