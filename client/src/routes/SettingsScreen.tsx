import { Screen } from '@/components/layout/Screen'
import { StatusLed } from '@/components/StatusLed'
import { useGetHealthQuery } from '@/store/api'

/** Settings — connection/server status, audio enhancement, notifications, theme,
 *  admin (#17/#19). The server-status row is live now (RTK Query → /healthz),
 *  proving the store + one-origin wiring end to end. */
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
        {['Audio enhancement', 'Notifications', 'Theme', 'Admin'].map((label) => (
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
