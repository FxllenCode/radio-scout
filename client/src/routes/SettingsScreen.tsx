import { ChevronRight, ExternalLink } from 'lucide-react'
import { Link } from 'react-router-dom'

import { Screen } from '@/components/layout/Screen'
import { StatusLed } from '@/components/StatusLed'
import { useInstance } from '@/hooks/useInstance'
import { useGetHealthQuery } from '@/store/api'

/** Settings — connection/server status, audio enhancement, theme, admin (#19).
 *  Server settings are a TOML file + flags, not a UI (ADR-0012); the
 *  server-status row is live now (RTK Query → /healthz), Logs is the operator
 *  log surface (#30), and Admin is where the instance's entities are curated
 *  (#49) — both behind the admin session, which those screens ask for
 *  themselves. The **Reference page** and the **Help** link (#77) are the
 *  Operator's offers to Listeners, drawn only when they exist (ADR-0023).
 *  There was a notifications switch here until #107 removed them (ADR-0014). */
export function SettingsScreen() {
  const { data, isSuccess, isError, isLoading } = useGetHealthQuery()
  const online = isSuccess && data?.trim() === 'ok'
  // What the Operator offers their Listeners (#77): a Reference page and a
  // place to ask for help — each drawn only when there is one, because an entry
  // that leads nowhere is a control that lies.
  const { reference, helpUrl } = useInstance()

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
        {reference && (
          <li>
            <Link
              to="/reference"
              className="flex items-center justify-between px-4 py-3.5 transition-colors hover:bg-muted/40"
            >
              <span className="text-sm">Reference</span>
              <span className="inline-flex items-center gap-1 font-mono text-xs text-muted-foreground">
                codes and alphabets
                <ChevronRight className="size-4" aria-hidden />
              </span>
            </Link>
          </li>
        )}
        {helpUrl && (
          <li>
            <a
              href={helpUrl}
              target="_blank"
              rel="noopener noreferrer"
              className="flex items-center justify-between px-4 py-3.5 transition-colors hover:bg-muted/40"
            >
              <span className="text-sm">Help</span>
              <span className="inline-flex items-center gap-1 font-mono text-xs text-muted-foreground">
                {helpUrl.startsWith('mailto:') ? 'write to the operator' : 'from the operator'}
                <ExternalLink className="size-3.5" aria-hidden />
              </span>
            </a>
          </li>
        )}
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
