import { AdminGate, Placeholder, SignOutButton } from '@/components/admin/AdminUi'
import { Screen } from '@/components/layout/Screen'
import { StatusLed } from '@/components/StatusLed'
import { formatCallTime } from '@/lib/archive'
import { formatBytes } from '@/lib/events'
import type { LedColor } from '@/lib/led'
import { windowLabel } from '@/lib/panel'
import {
  asOf,
  concerns,
  formatUptime,
  overall,
  totalStored,
  volumeOf,
  type Health,
} from '@/lib/status'
import { useAdminSession } from '@/hooks/useAdminSession'
import { useGetInstanceStatusQuery } from '@/store/api'
import type { InstanceStatus } from '@/types'

/** How often the page asks again. The process-side numbers are free to read, and
 *  the database-side ones are cached server-side on a longer window — so this
 *  cadence is about how live the *page* feels, not about what it costs. */
const REFRESH_MS = 5_000

/** What each verdict looks like, spelled once so a badge and a dot cannot come
 *  to disagree about what "warn" is. */
const LED: Record<Health, LedColor> = { ok: 'green', warn: 'yellow', bad: 'red' }
const VERDICT: Record<Health, string> = {
  ok: 'Healthy',
  warn: 'Worth a look',
  bad: 'Needs attention',
}

/**
 * Settings → Admin → Status (#70, spec US 48) — is this Instance healthy.
 *
 * **One glance is one answer**, which is the whole ticket: the verdict at the
 * top is derived from [`concerns`] rather than kept beside it, so the headline
 * and the list under it cannot disagree. Everything below the verdict is the
 * evidence, in the order an Operator asks for it — is traffic arriving, is
 * anything being refused, are the workers alive, is there room.
 *
 * rdio-scanner has no equivalent at all: the only health signal there is the
 * absence of calls, which is indistinguishable from a quiet night.
 */
export function StatusScreen() {
  const signedIn = useAdminSession()
  const { data: status, isFetching } = useGetInstanceStatusQuery(undefined, {
    skip: !signedIn,
    pollingInterval: REFRESH_MS,
  })

  return (
    <Screen title="Status" status={<SignOutButton />}>
      <AdminGate>
        {status === undefined ? (
          <Placeholder>
            {isFetching ? 'Reading the instance…' : 'Nothing to report yet.'}
          </Placeholder>
        ) : (
          <Report status={status} />
        )}
      </AdminGate>
    </Screen>
  )
}

function Report({ status }: { status: InstanceStatus }) {
  const found = concerns(status)
  const verdict = overall(found)
  // **Said on every card built from them, not one.** The three below come from
  // one server-side reading refreshed on its own window, where everything above
  // is live — so a card that did not say so would read as five seconds old when
  // it can be fifteen.
  const age = `as of ${asOf(status.gaugesAtMs, Date.now())}`

  return (
    <div className="mt-3 space-y-4">
      <section className="rounded-xl border border-border bg-card px-4 py-4">
        <p className="flex items-center gap-2 text-sm">
          <StatusLed color={LED[verdict]} size={10} pulse={verdict === 'ok'} />
          <span className="font-semibold">{VERDICT[verdict]}</span>
          <span className="font-mono text-xs text-muted-foreground">
            up {formatUptime(status.uptimeSeconds)} · v{status.version}
          </span>
        </p>
        {found.length > 0 && (
          <ul className="mt-3 space-y-1.5">
            {found.map((concern) => (
              <li
                key={concern.id}
                className="flex items-start gap-2 font-mono text-xs text-muted-foreground"
              >
                <StatusLed color={LED[concern.health]} size={7} className="mt-1 shrink-0" />
                {concern.message}
              </li>
            ))}
          </ul>
        )}
      </section>

      <Card title="Now">
        <Row label="Listeners" value={String(status.listeners)} />
        {Object.entries(status.ingest).map(([outcome, count]) => (
          <Row key={outcome} label={`Uploads ${outcome}`} value={String(count)} />
        ))}
      </Card>

      <Card title="Systems" note={`last ${windowLabel(status.rateWindowMs)} · ${age}`}>
        {status.systems.length === 0 ? (
          <Row label="No systems yet" value="—" />
        ) : (
          status.systems.map((system) => (
            <Row
              key={system.ref}
              label={system.label ?? `System ${system.ref}`}
              value={`${system.calls} · ${
                system.lastCallAtMs === undefined
                  ? 'never heard'
                  : `last ${formatCallTime(system.lastCallAtMs)}`
              }`}
            />
          ))
        )}
      </Card>

      <Card title="Workers">
        {status.workers.map((worker) => (
          <Row
            key={worker.name}
            label={worker.name}
            value={`${worker.running ? '' : 'stopped · '}${
              worker.depth === 0 ? 'idle' : `${worker.depth} queued`
            } · ${worker.done} done`}
          />
        ))}
      </Card>

      <Card title="Storage" note={age}>
        <Row label="Calls" value={String(status.archive.calls)} />
        <Row
          label="Audio stored"
          value={`${formatBytes(totalStored(status.archive))}${
            status.archive.frozenAudioBytes > 0
              ? ` · ${formatBytes(status.archive.frozenAudioBytes)} frozen`
              : ''
          }`}
        />
        <Row label="Volume" value={volume(status)} />
        <Row
          label="Retention"
          value={`${status.retention.days === 0 ? 'kept for good' : `${status.retention.days} days`}${
            status.retention.maxSizeBytes === undefined
              ? ' · no size cap'
              : ` · cap ${formatBytes(status.retention.maxSizeBytes)}`
          }`}
        />
        {status.archive.oldestCallAtMs !== undefined && (
          <Row
            label="Oldest call"
            value={formatCallTime(status.archive.oldestCallAtMs)}
          />
        )}
      </Card>

      <Card title="Delivery" note={age}>
        {status.sinks.map((sink) => (
          <Row
            key={sink.sink}
            label={`${sink.sink}s`}
            value={
              sink.total === 0
                ? 'none configured'
                : `${sink.total} · ${sink.queued} queued · ${sink.failing} failing · ${
                    sink.lastSuccessMs === undefined
                      ? 'nothing delivered yet'
                      : `last ${formatCallTime(sink.lastSuccessMs)}`
                  }`
            }
          />
        ))}
      </Card>

      <Card title="Refused" note="since this process started">
        {Object.keys(status.refused).length === 0 ? (
          <Row label="Nothing refused" value="—" />
        ) : (
          Object.entries(status.refused).map(([reason, count]) => (
            <Row key={reason} label={reason} value={String(count)} />
          ))
        )}
        {Object.entries(status.errors).map(([stage, count]) => (
          <Row key={stage} label={`error at ${stage}`} value={String(count)} />
        ))}
      </Card>
    </div>
  )
}

/** How full the volume is, in the words an Operator would use — and "not on this
 *  machine" when the audio lives in a bucket, which is a different fact from
 *  "no room". */
function volume(status: InstanceStatus): string {
  const room = volumeOf(status.storage)
  if (room === undefined) return 'not on this machine'
  return `${formatBytes(room.freeBytes)} free of ${formatBytes(
    room.totalBytes,
  )} · ${Math.round(room.used * 100)}% used`
}

function Card({
  title,
  note,
  children,
}: {
  title: string
  note?: string
  children: React.ReactNode
}) {
  return (
    <section className="overflow-hidden rounded-xl border border-border bg-card">
      <h2 className="flex items-baseline justify-between gap-2 border-b border-border px-4 py-2.5 font-mono text-[10px] uppercase tracking-wider text-muted-foreground">
        {title}
        {note && <span className="normal-case tracking-normal">{note}</span>}
      </h2>
      <dl className="divide-y divide-border">{children}</dl>
    </section>
  )
}

function Row({ label, value }: { label: string; value: string }) {
  return (
    <div className="flex items-baseline justify-between gap-3 px-4 py-2">
      <dt className="text-sm">{label}</dt>
      <dd className="text-right font-mono text-xs text-muted-foreground">{value}</dd>
    </div>
  )
}
