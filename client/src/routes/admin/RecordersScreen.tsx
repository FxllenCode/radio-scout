import { AdminGate, Placeholder, SignOutButton } from '@/components/admin/AdminUi'
import { Screen } from '@/components/layout/Screen'
import { StatusLed } from '@/components/StatusLed'
import { formatCallTime } from '@/lib/archive'
import type { LedColor } from '@/lib/led'
import {
  available,
  bucketAt,
  channelKey,
  channelName,
  channelTraces,
  concerns,
  dashboardHealth,
  formatFrequency,
  isStale,
  isTrunked,
  plot,
  reasonLabel,
  recorderName,
  stateLabel,
  type Health,
} from '@/lib/recorders'
import { asOf, formatUptime } from '@/lib/status'
import { useAdminSession } from '@/hooks/useAdminSession'
import { useGetFrequencyHealthQuery, useGetRecordersQuery } from '@/store/api'
import type { ChannelHealth, Dashboard, HealthReport, RecorderView } from '@/types'

/** How often the live view asks again.
 *
 *  Trunk Recorder pushes a `rates` frame every three seconds, so anything faster
 *  than this is asking for the same answer twice. The whole document is held in
 *  the server's memory, so a poll is a serialize and nothing else. */
const REFRESH_MS = 2_000

/** What each verdict looks like, spelled once so a badge and a dot cannot come
 *  to disagree — `StatusScreen`'s table, deliberately identical. */
const LED: Record<Health, LedColor> = { ok: 'green', warn: 'yellow', bad: 'red' }
const VERDICT: Record<Health, string> = {
  ok: 'All receiving',
  warn: 'Worth a look',
  bad: 'Needs attention',
}

/**
 * Settings → Admin → Recorders (#71, spec US 50–51) — what the SDRs are doing
 * right now, and how each frequency has been receiving.
 *
 * Two questions on one screen, polled at two completely different rates: the
 * live view is a couple of seconds behind whatever Trunk Recorder last said, and
 * the charts are a quarter-hourly rollup measured from the Calls themselves. The
 * second is what "a dying dongle announces itself" means — the status socket
 * carries no error, spike, signal or drift figure at all, so the health history
 * comes from what each Call's own recorder metadata said.
 *
 * rdio-scanner has nothing like either: the only signal there that a receiver
 * has died is calls stopping, which is indistinguishable from a quiet night.
 */
export function RecordersScreen() {
  const signedIn = useAdminSession()
  const { data: dashboard, isFetching } = useGetRecordersQuery(undefined, {
    skip: !signedIn,
    pollingInterval: REFRESH_MS,
  })

  return (
    <Screen title="Recorders" status={<SignOutButton />}>
      <AdminGate>
        {dashboard === undefined ? (
          <Placeholder>
            {isFetching ? 'Asking the instance…' : 'Nothing to report yet.'}
          </Placeholder>
        ) : (
          <Dashboard dashboard={dashboard} signedIn={signedIn} />
        )}
      </AdminGate>
    </Screen>
  )
}

function Dashboard({ dashboard, signedIn }: { dashboard: Dashboard; signedIn: boolean }) {
  const verdict = dashboardHealth(dashboard)

  return (
    <div className="mt-3 space-y-4">
      <section className="rounded-xl border border-border bg-card px-4 py-4">
        <p className="flex items-center gap-2 text-sm">
          <StatusLed color={LED[verdict]} size={10} pulse={verdict === 'ok'} />
          <span className="font-semibold">
            {dashboard.recorders.length === 0 ? 'No recorder connected' : VERDICT[verdict]}
          </span>
        </p>
        {dashboard.recorders.length === 0 && (
          <p className="mt-2 text-xs text-muted-foreground">
            Trunk Recorder dials in to <code>/api/recorder-status</code> with the
            same API key it uploads with. See <em>docs/recorders.md</em> for the
            status plugin and the one line of config that points it here — until
            then this screen is empty and nothing else is affected.
          </p>
        )}
      </section>

      {dashboard.recorders.map((recorder) => (
        <RecorderCard key={recorder.id} recorder={recorder} atMs={dashboard.atMs} />
      ))}

      <HealthCharts signedIn={signedIn} />
    </div>
  )
}

function RecorderCard({ recorder, atMs }: { recorder: RecorderView; atMs: number }) {
  const found = concerns(recorder, atMs)
  const heard = recorder.connected
    ? isStale(recorder, atMs)
      ? `silent ${asOf(recorder.lastMessageMs, atMs)}`
      : `heard ${asOf(recorder.lastMessageMs, atMs)}`
    : `gone ${asOf(recorder.disconnectedAtMs ?? recorder.lastMessageMs, atMs)}`

  return (
    <Card
      title={recorderName(recorder)}
      note={`${heard} · up ${formatUptime(
        Math.max(0, atMs - recorder.connectedAtMs) / 1_000,
      )}`}
    >
      {found.length > 0 && (
        <div className="px-4 py-2">
          <ul className="space-y-1.5">
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
        </div>
      )}

      {recorder.systems.map((system) => (
        <Row
          key={system.sysNum}
          label={system.shortName ?? `System ${system.sysNum}`}
          value={`${
            isTrunked(system.systemType)
              ? `${system.decodeRate.toFixed(1)} msg/s`
              : 'conventional'
          }${system.sysid === undefined ? '' : ` · sysid ${system.sysid}`}`}
        />
      ))}

      {recorder.sdrs.map((sdr) => (
        <Row
          key={sdr.sourceNum}
          label={`SDR ${sdr.sourceNum}${sdr.device === undefined ? '' : ` · ${sdr.device}`}`}
          value={`${available(recorder.demodulators, sdr.sourceNum)}/${
            sdr.demodulators
          } free · ${formatFrequency(sdr.centerHz)} · gain ${sdr.gain}`}
        />
      ))}

      {recorder.calls.length > 0 && (
        <div className="px-4 py-2">
          <h3 className="font-mono text-[10px] uppercase tracking-wider text-muted-foreground">
            In hand
          </h3>
          <ul className="mt-1.5 space-y-1">
            {recorder.calls.map((call) => (
              <li
                key={call.id}
                className="flex items-baseline justify-between gap-3 text-xs"
              >
                <span className="truncate">
                  {call.talkgroupLabel ?? `Talkgroup ${call.talkgroup}`}
                  {call.emergency && (
                    <span className="ml-1.5 font-mono text-[10px] uppercase text-red-500">
                      emergency
                    </span>
                  )}
                </span>
                <span className="shrink-0 text-right font-mono text-[11px] text-muted-foreground">
                  {call.notRecorded === undefined
                    ? stateLabel(call.state)
                    : reasonLabel(call.notRecorded)}
                </span>
              </li>
            ))}
          </ul>
        </div>
      )}

      {recorder.notRecorded.length > 0 && (
        <div className="px-4 py-2">
          <h3 className="font-mono text-[10px] uppercase tracking-wider text-muted-foreground">
            Not recorded, since it connected
          </h3>
          <ul className="mt-1.5 space-y-1">
            {recorder.notRecorded.map((tally) => (
              <li
                key={tally.reason}
                className="flex items-baseline justify-between gap-3 text-xs"
              >
                <span>{reasonLabel(tally.reason)}</span>
                <span className="shrink-0 font-mono text-[11px] text-muted-foreground">
                  {tally.count}
                  {tally.lastTalkgroupLabel !== undefined &&
                    ` · last ${tally.lastTalkgroupLabel}`}
                </span>
              </li>
            ))}
          </ul>
        </div>
      )}

      {recorder.refused.length > 0 && (
        <div className="px-4 py-2">
          <h3
            id={`refused-${recorder.id}`}
            className="font-mono text-[10px] uppercase tracking-wider text-muted-foreground"
          >
            Recently turned down
          </h3>
          <ul aria-labelledby={`refused-${recorder.id}`} className="mt-1.5 space-y-1">
            {recorder.refused.map((call) => (
              <li
                key={call.id}
                className="flex items-baseline justify-between gap-3 text-xs"
              >
                <span className="truncate">
                  {call.talkgroupLabel ?? `Talkgroup ${call.talkgroup}`}
                </span>
                <span className="shrink-0 text-right font-mono text-[11px] text-muted-foreground">
                  {reasonLabel(call.notRecorded ?? call.state)} ·{' '}
                  {asOf(call.startedAtMs, atMs)}
                </span>
              </li>
            ))}
          </ul>
        </div>
      )}
    </Card>
  )
}

/** The per-frequency, per-SDR charts (spec US 51).
 *
 *  Fetched **once** rather than on the dashboard's two-second poll: this is a
 *  quarter-hourly rollup, and re-reading a chart of yesterday twenty times a
 *  minute would cost a Pi the one read this screen is careful about. */
function HealthCharts({ signedIn }: { signedIn: boolean }) {
  // **No range**, deliberately: the server discovers how far its own history
  // reaches and widens the grain to fit, reporting the width it really used.
  // A window built from `Date.now()` would be a fresh cache key on every render
  // and so a refetch on every render — and the rollup's own quarter-hour is the
  // finest this can ever be, so asking for it is asking for the truth.
  const { data: report } = useGetFrequencyHealthQuery(
    { bucketMs: 15 * 60 * 1_000 },
    { skip: !signedIn },
  )

  if (report === undefined) return null

  return (
    <Card
      title="Receive health"
      note={`${formatCallTime(report.fromMs)} → ${formatCallTime(report.toMs)}`}
    >
      {report.channels.length === 0 ? (
        <Row label="No calls measured yet" value="—" />
      ) : (
        report.channels.map((channel) => (
          <ChannelRow key={channelKey(channel)} channel={channel} report={report} />
        ))
      )}
      {report.omitted > 0 && (
        <Row
          label="Not shown"
          value={`${report.omitted} quieter ${report.omitted === 1 ? 'channel' : 'channels'}`}
        />
      )}
    </Card>
  )
}

function ChannelRow({
  channel,
  report,
}: {
  channel: ChannelHealth
  report: HealthReport
}) {
  return (
    <div className="px-4 py-2.5">
      <span className="text-sm">{channelName(channel)}</span>
      <div className="mt-1 grid grid-cols-1 gap-x-4 gap-y-2 sm:grid-cols-2">
        {channelTraces(channel).map((trace) => (
          <div key={trace.label}>
            <div className="flex items-baseline justify-between gap-2 font-mono text-[11px] text-muted-foreground">
              <span>{trace.label}</span>
              <span className="text-right">{trace.summary}</span>
            </div>
            <Sparkline trace={trace.trace} report={report} label={trace.label} />
          </div>
        ))}
      </div>
    </div>
  )
}

/** One trace, drawn as bars.
 *
 *  A bucket nothing was measured in is **drawn as a gap**, never as a
 *  zero-height bar: a chart that drew "we did not hear anything" as "a perfect
 *  minute" would say something false about a receiver that may have been dead.
 *  Every bar that *was* measured gets a floor of a pixel or two, the density
 *  ribbon's rule (#62) — traffic drawn as nothing is the same lie in the other
 *  direction.
 */
function Sparkline({
  trace,
  report,
  label,
}: {
  trace: (number | null)[]
  report: HealthReport
  label: string
}) {
  const heights = plot(trace)

  return (
    <div
      className="mt-1.5 flex h-6 items-end gap-px"
      role="img"
      aria-label={`${label}, ${trace.length} buckets from ${formatCallTime(
        bucketAt(report, 0),
      )}`}
    >
      {heights.map((height, index) => (
        <span
          key={bucketAt(report, index)}
          className={
            height === null
              ? 'flex-1 bg-transparent'
              : 'flex-1 rounded-sm bg-muted-foreground/60'
          }
          style={height === null ? undefined : { height: `${8 + height * 92}%` }}
        />
      ))}
    </div>
  )
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
      <div className="divide-y divide-border">{children}</div>
    </section>
  )
}

function Row({ label, value }: { label: string; value: string }) {
  return (
    <div className="flex items-baseline justify-between gap-3 px-4 py-2">
      <span className="text-sm">{label}</span>
      <span className="text-right font-mono text-xs text-muted-foreground">{value}</span>
    </div>
  )
}
