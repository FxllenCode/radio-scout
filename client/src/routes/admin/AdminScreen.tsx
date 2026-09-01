import { ChevronRight } from 'lucide-react'
import { Link } from 'react-router-dom'

import { AdminGate, SignOutButton } from '@/components/admin/AdminUi'
import { ConfigDocument } from '@/components/admin/ConfigDocument'
import { Screen } from '@/components/layout/Screen'

/** Where each entity is curated, and what an Operator would come here to do.
 *
 *  Ordered by how often it is reached rather than alphabetically: Talkgroups is
 *  the screen a county's operator lives in, and API keys is the one they visit
 *  when a recorder arrives. */
const SECTIONS = [
  {
    to: '/settings/admin/talkgroups',
    label: 'Talkgroups',
    hint: 'labels, LEDs, groups, tags, blacklists',
  },
  {
    to: '/settings/admin/systems',
    label: 'Systems',
    hint: 'the networks you receive',
  },
  { to: '/settings/admin/units', label: 'Units', hint: 'name the radios' },
  {
    to: '/settings/admin/groups',
    label: 'Groups',
    hint: 'cross-system categories',
  },
  { to: '/settings/admin/tags', label: 'Tags', hint: 'service labels' },
  {
    to: '/settings/admin/api-keys',
    label: 'API keys',
    hint: 'what recorders authenticate with',
  },
  {
    to: '/settings/admin/downstreams',
    label: 'Downstreams',
    hint: 'instances you forward calls to',
  },
  {
    to: '/settings/admin/webhooks',
    label: 'Webhooks',
    hint: 'addresses that get your flagged calls',
  },
  {
    to: '/settings/admin/listeners',
    label: 'Listeners',
    hint: 'how many people have been on',
  },
  {
    to: '/settings/logs',
    label: 'Logs',
    hint: 'what the server has been doing',
  },
]

/**
 * Settings → Admin (#49, spec US 45) — the hub for running the Instance from a
 * browser, so that doing so never requires SSH.
 *
 * A hub rather than one long page, because these are eight unrelated jobs and a
 * phone can show one of them. rdio-scanner puts all of its configuration behind
 * a single tabbed page backed by one `PUT` of the whole document; splitting them
 * is what lets each screen write only its own rows.
 */
export function AdminScreen() {
  return (
    <Screen title="Admin" status={<SignOutButton />}>
      <AdminGate>
        <ul className="mt-3 divide-y divide-border overflow-hidden rounded-xl border border-border bg-card">
          {SECTIONS.map((section) => (
            <li key={section.to}>
              <Link
                to={section.to}
                className="flex items-center justify-between px-4 py-3.5 transition-colors hover:bg-muted/40"
              >
                <span className="text-sm">{section.label}</span>
                <span className="inline-flex items-center gap-1 font-mono text-xs text-muted-foreground">
                  {section.hint}
                  <ChevronRight className="size-4" aria-hidden />
                </span>
              </Link>
            </li>
          ))}
        </ul>
        <ConfigDocument />
        <p className="mt-4 px-1 font-mono text-[11px] text-muted-foreground">
          Ports, storage and retention are configured in radio-scout.toml — this
          screen owns the entities calls are addressed to, not the machine.
        </p>
      </AdminGate>
    </Screen>
  )
}
