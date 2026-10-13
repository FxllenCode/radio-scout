import { ChevronRight, ExternalLink, Share } from 'lucide-react'
import { Link } from 'react-router-dom'

import { Screen } from '@/components/layout/Screen'
import { useShell } from '@/components/layout/shell'
import { Button } from '@/components/ui/button'
import { StatusLamp } from '@/components/Led'
import { useInstance } from '@/hooks/useInstance'
import { useOnline } from '@/hooks/useOnline'
import { serverReadout } from '@/lib/feed'
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
  // Unreachable from a device with no network says nothing about the server
  // (#78) — the Live screen's own distinction, on the row that checks it.
  const deviceOffline = !useOnline()
  // What a waved-off banner gave up, offered again (#78): the shell's handles,
  // so this row and the banner can never disagree about what is on offer.
  const { update, install } = useShell()
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
            <StatusLamp color={online ? 'green' : 'red'} size={8} pulse={online} />
            {serverReadout({
              ok: online,
              checking: isLoading,
              failed: isError,
              offline: deviceOffline,
            })}
          </span>
        </li>
        {/* Named rows, so a screen reader can find them by what they are. */}
        {update.ready && (
          <li aria-label="Update" className="flex items-center justify-between gap-3 px-4 py-3.5">
            <span className="text-sm">A new version is ready</span>
            <Button className="h-11" variant="secondary" onClick={update.apply}>
              Reload to update
            </Button>
          </li>
        )}
        {install.entry !== 'none' && (
          <li aria-label="Install" className="flex items-center justify-between gap-3 px-4 py-3.5">
            <span className="min-w-0">
              <span className="block text-sm">Install the app</span>
              <span className="block text-xs text-muted-foreground">
                {install.entry === 'prompt' ? (
                  'Full screen, faster start, background audio'
                ) : (
                  // iOS has no install dialog to fire: the Share sheet is the
                  // only way, so the row says how.
                  <>
                    Tap <Share className="inline size-3.5 align-text-bottom" aria-label="Share" />{' '}
                    Share, then Add to Home Screen
                  </>
                )}
              </span>
            </span>
            {install.entry === 'prompt' && (
              <Button className="h-11" onClick={install.install}>
                Install app
              </Button>
            )}
          </li>
        )}
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
            <span className="font-mono text-xs text-muted-foreground">soon</span>
          </li>
        ))}
      </ul>
    </Screen>
  )
}
