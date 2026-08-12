import { ChevronRight } from 'lucide-react'
import { Link } from 'react-router-dom'

import { Screen } from '@/components/layout/Screen'
import { StatusLed } from '@/components/StatusLed'
import { NotificationsRow } from '@/components/NotificationsRow'
import { useGetHealthQuery } from '@/store/api'

/** Settings — connection/server status, audio enhancement, notifications, theme,
 *  admin (#19). Server settings are a TOML file + flags, not a UI (ADR-0012);
 *  the server-status row is live now (RTK Query → /healthz), notifications are
 *  the Web Push switch (#16), Logs is the operator log surface (#30), and Admin
 *  is where the instance's entities are curated (#49) — both behind the admin
 *  session, which those screens ask for themselves. */
export function SettingsScreen() {
  const { data, isSuccess, isError, isLoading } = useGetHealthQuery()
  const online = isSuccess && data?.trim() === 'ok'

  return (
    <Screen title="Settings">
      <ul className="divide-y divide-border overflow-hidden rounded-xl border border-border bg-card">
        <li className="flex items-center justify-between px-4 py-3.5">
          <span className="text-sm">Server</span>
          <span className="inline-flex items-center gap-2 font-mono text-xs text-muted-foreground">
            <StatusLed color={online ? 'green' : 'red'} size={8} pulse={online} />
            {isLoading ? 'checking…' : online ? 'online' : isError ? 'unreachable' : 'unknown'}
          </span>
        </li>
        <NotificationsRow />
        <li>
          <Link
            to="/settings/logs"
            className="flex items-center justify-between px-4 py-3.5 transition-colors hover:bg-muted/40"
          >
            <span className="text-sm">Logs</span>
            <span className="inline-flex items-center gap-1 font-mono text-xs text-muted-foreground">
              what the server has been doing
              <ChevronRight className="size-4" aria-hidden />
            </span>
          </Link>
        </li>
        <li>
          <Link
            to="/settings/admin"
            className="flex items-center justify-between px-4 py-3.5 transition-colors hover:bg-muted/40"
          >
            <span className="text-sm">Admin</span>
            <span className="inline-flex items-center gap-1 font-mono text-xs text-muted-foreground">
              systems, talkgroups, units, keys
              <ChevronRight className="size-4" aria-hidden />
            </span>
          </Link>
        </li>
        {['Audio enhancement', 'Theme'].map((label) => (
          <li
            key={label}
            className="flex items-center justify-between px-4 py-3.5 text-muted-foreground"
          >
            <span className="text-sm">{label}</span>
            <span className="font-mono text-xs text-muted-foreground/60">soon</span>
          </li>
        ))}
      </ul>
    </Screen>
  )
}
