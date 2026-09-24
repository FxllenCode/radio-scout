/**
 * The recorder dashboard's pure half (#71, spec US 50–51) — what the numbers
 * *mean*.
 *
 * `GET /api/admin/recorders` is a page of facts about somebody else's software;
 * this turns them into the two answers an Operator is really asking for — *is
 * each receiver still working*, and *is any of them getting worse*. Both are
 * values a test constructs rather than a screen it has to render, which is
 * [`lib/status`]'s shape one screen along.
 *
 * # Nothing here reads a clock
 *
 * Every age on this screen is a subtraction, and the two clocks involved are
 * different machines'. So the server stamps the document (`Dashboard.atMs`) and
 * every function here takes that moment as a parameter — `lib/panel`'s rule,
 * which exists so a list does not re-sort itself under a thumb and which here
 * also stops a browser two minutes fast from reporting a healthy recorder as
 * silent.
 */
import type {
  ChannelHealth,
  Demodulator,
  Dashboard,
  HealthReport,
  RecorderView,
} from '@/types'

/** How a reading should be read — [`lib/status`]'s vocabulary, deliberately the
 *  same one, so the two operator screens grade things alike. */
export type Health = 'ok' | 'warn' | 'bad'

/**
 * How long a connected **Recorder** may say nothing before it is worth
 * mentioning.
 *
 * Trunk Recorder sends a `rates` frame every three seconds whatever is
 * happening, so ten seconds of quiet is already unusual — but the server reaps
 * at thirty to forty-five, and a screen that cried before the server acted would
 * flag every recorder on a slow network. This sits between the two: long enough
 * to be real, short enough to be seen before the row goes.
 */
export const STALE_MS = 20_000

/** Below this many control-channel messages a second, a trunked System is not
 *  hearing its tower. A receiver that is working reads in the tens; one that has
 *  lost the control channel reads zero, and there is very little in between. */
const DECODE_RATE_FLOOR = 1

/** Which Systems have no control channel to decode, so a zero rate says nothing
 *  at all about them. Trunk Recorder's own `system_type` strings. */
const CONVENTIONAL = new Set([
  'conventional',
  'conventionalP25',
  'conventionalDMR',
  'conventionalSIGMF',
])

/** One thing worth saying about a Recorder, and how loudly. */
export interface Concern {
  /** Stable across refreshes, so React keeps the row rather than rewriting it. */
  id: string
  health: Health
  message: string
}

/**
 * Everything worth saying about one **Recorder**, worst first.
 *
 * An empty list is the ordinary state. What is deliberately *not* here, on
 * [`lib/status`]'s own terms — every threshold anyone could pick for them is
 * wrong for somebody:
 *
 * - **A channel nobody is talking on.** A county is quiet at three in the
 *   morning, and a demodulator sitting idle is a demodulator that is available.
 * - **A transmission that was not recorded.** `duplicate` and `superseded` are
 *   the recorder working correctly, and `encrypted` is the radio system's
 *   decision rather than the receiver's. The tally is shown and the Operator
 *   reads it.
 */
export function concerns(recorder: RecorderView, atMs: number): Concern[] {
  const found: Concern[] = []

  if (!recorder.connected) {
    found.push({
      id: `gone-${recorder.id}`,
      health: 'bad',
      message: 'This recorder disconnected and has not come back.',
    })
    // Nothing below is worth saying about a recorder that is not there: every
    // one of them would be describing the last thing it said before it went.
    return found
  }

  if (isStale(recorder, atMs)) {
    found.push({
      id: `quiet-${recorder.id}`,
      health: 'bad',
      message:
        'This recorder is connected but has stopped reporting. Check that it is still running.',
    })
  }

  for (const system of recorder.systems) {
    if (!isTrunked(system.systemType) || system.decodeRate >= DECODE_RATE_FLOOR) continue
    found.push({
      id: `control-${recorder.id}-${system.sysNum}`,
      health: 'bad',
      message: `${system.shortName ?? `System ${system.sysNum}`} is decoding nothing from its control channel.`,
    })
  }

  // Every demodulator busy is not a fault — it is a receiver at capacity, and
  // the next transmission on that SDR is the one nobody gets to hear.
  for (const sdr of recorder.sdrs) {
    if (sdr.demodulators === 0 || available(recorder.demodulators, sdr.sourceNum) > 0) {
      continue
    }
    found.push({
      id: `busy-${recorder.id}-${sdr.sourceNum}`,
      health: 'warn',
      message: `Every demodulator on SDR ${sdr.sourceNum} is in use; more traffic than it can record.`,
    })
  }

  return found
}

/** The loudest of a list of concerns — an empty list is healthy, which is what
 *  makes a headline derived from this unable to disagree with the list under
 *  it. */
export function overall(found: Concern[]): Health {
  if (found.some((concern) => concern.health === 'bad')) return 'bad'
  if (found.some((concern) => concern.health === 'warn')) return 'warn'
  return 'ok'
}

/** The whole dashboard's verdict: the worst any one recorder has to say, and `bad`
 *  where an Operator has a dashboard and nothing on it at all. */
export function dashboardHealth(dashboard: Dashboard): Health {
  return overall(
    dashboard.recorders.flatMap((recorder) => concerns(recorder, dashboard.atMs)),
  )
}

/** Is this Recorder connected and yet saying nothing? */
export function isStale(recorder: RecorderView, atMs: number): boolean {
  return recorder.connected && atMs - recorder.lastMessageMs >= STALE_MS
}

/** Does a zero decode rate mean anything on this System? */
export function isTrunked(systemType: string | undefined): boolean {
  return systemType !== undefined && !CONVENTIONAL.has(systemType)
}

/** How many of one SDR's demodulators are free. */
export function available(demodulators: Demodulator[], sourceNum: number): number {
  return demodulators.filter(
    (slot) => slot.sourceNum === sourceNum && slot.state === 'available',
  ).length
}

/** What a **Recorder** is called on screen — its own `instanceId` where it set
 *  one, else the connection it holds. Nearly every install leaves the name
 *  empty, so this is the common path rather than the fallback. */
export function recorderName(recorder: RecorderView): string {
  return recorder.name ?? `Recorder ${recorder.id}`
}

/** What a demodulator or a call is doing, in words.
 *
 *  The server sends a closed vocabulary of slugs precisely so this table can
 *  exist; anything outside it is shown as it came rather than hidden, because a
 *  recorder one version ahead should read oddly rather than read as nothing. */
export function stateLabel(state: string): string {
  return STATES[state] ?? state
}

const STATES: Record<string, string> = {
  monitoring: 'Monitoring',
  recording: 'Recording',
  inactive: 'Inactive',
  active: 'Active',
  idle: 'Idle',
  stopped: 'Stopped',
  available: 'Available',
  ignore: 'Ignored',
  unknown: 'Unknown',
}

/** Why a transmission was not recorded, in words — Trunk Recorder's
 *  `MonitoringState`, which the server has already turned into slugs. */
export function reasonLabel(reason: string): string {
  return REASONS[reason] ?? reason
}

const REASONS: Record<string, string> = {
  'unknown-talkgroup': 'Talkgroup not in the list',
  'ignored-talkgroup': 'Talkgroup set to ignore',
  'no-source': 'No SDR covers that frequency',
  'no-recorder': 'No demodulator free',
  encrypted: 'Encrypted',
  duplicate: 'Already recording it',
  superseded: 'Superseded by another call',
}

/** A frequency, as an Operator reads one off a radio. */
export function formatFrequency(hz: number): string {
  return `${(hz / 1_000_000).toFixed(4)} MHz`
}

/** How a channel is named on a chart: the frequency, and which SDR heard it
 *  where the Recorder said. Two SDRs on one frequency are the whole point, so
 *  the two must never render alike. */
export function channelName(channel: ChannelHealth): string {
  const where = channel.sdr === undefined ? '' : ` · SDR ${channel.sdr}`
  return `${formatFrequency(channel.freq)}${where}`
}

/** A channel's key, stable across refreshes so React keeps its row. */
export function channelKey(channel: ChannelHealth): string {
  return `${channel.systemRef}-${channel.freq}-${channel.sdr ?? 'any'}`
}

/** One trace on a channel's chart, with the number that summarises it. */
export interface Trace {
  label: string
  trace: (number | null)[]
  summary: string
}

/** Every trace a channel is charted by (spec US 51) — decode errors, spikes,
 *  signal and drift, in the order an Operator reads a failing receiver: it
 *  starts making errors, then clicking, and the level and tuning say why.
 *
 *  The rates' summaries are over the **whole window's air**, not a mean of the
 *  buckets — a busy quarter-hour and a quiet one are not worth the same — and
 *  anything unmeasured says so in words rather than as a zero. Noise rides
 *  beside the signal rather than as a fifth chart: it is the floor the signal
 *  is read against, and rarely moves on its own. */
export function channelTraces(channel: ChannelHealth): Trace[] {
  const perMinute = (count: number, unit: string) =>
    channel.airMs <= 0 ? 'no air' : `${((count * 60_000) / channel.airMs).toFixed(1)} ${unit}`
  const mean = (trace: (number | null)[], format: (value: number) => string) => {
    const value = traceMean(trace)
    return value === undefined ? 'not measured' : format(value)
  }
  const noise = traceMean(channel.noiseDbm)

  return [
    {
      label: 'Decode errors',
      trace: channel.errorRate,
      summary: perMinute(channel.errorCount, 'err/min'),
    },
    {
      label: 'Spikes',
      trace: channel.spikeRate,
      summary: perMinute(channel.spikeCount, 'spikes/min'),
    },
    {
      label: 'Signal',
      trace: channel.signalDbm,
      summary: `${mean(channel.signalDbm, (dbm) => `${dbm.toFixed(0)} dBm`)}${
        noise === undefined ? '' : ` · noise ${noise.toFixed(0)} dBm`
      }`,
    },
    {
      label: 'Drift',
      trace: channel.driftHz,
      summary: mean(channel.driftHz, (hz) => `${hz >= 0 ? '+' : ''}${hz.toFixed(0)} Hz`),
    },
  ]
}

/** The mean of a trace, ignoring the buckets nothing was measured in — what a
 *  chart's summary line shows beside it. */
export function traceMean(trace: (number | null)[]): number | undefined {
  const measured = trace.filter((value): value is number => value !== null)
  if (measured.length === 0) return undefined
  return measured.reduce((sum, value) => sum + value, 0) / measured.length
}

/** A trace's span, for drawing it: the smallest and largest measured values, or
 *  `undefined` where there is nothing to draw.
 *
 *  A single reading gives a flat range, which a chart has to widen rather than
 *  divide by — so this reports what was really measured and leaves that
 *  decision to whoever plots it. */
export function traceRange(
  trace: (number | null)[],
): { min: number; max: number } | undefined {
  const measured = trace.filter((value): value is number => value !== null)
  if (measured.length === 0) return undefined
  return { min: Math.min(...measured), max: Math.max(...measured) }
}

/** Where each measured point of a trace sits, as a fraction of the plot's
 *  height — `null` where nothing was measured, so a chart draws a gap rather
 *  than a line through zero.
 *
 *  A flat trace plots down the middle rather than along an edge: every reading
 *  really was the same, and drawing them at the bottom would say they were the
 *  worst seen.
 */
export function plot(trace: (number | null)[]): (number | null)[] {
  const range = traceRange(trace)
  if (range === undefined) return trace.map(() => null)
  const span = range.max - range.min
  return trace.map((value) =>
    value === null ? null : span === 0 ? 0.5 : (value - range.min) / span,
  )
}

/** How many buckets a report holds — the length every trace on it shares. */
export function bucketCount(report: HealthReport): number {
  return report.channels[0]?.errorRate.length ?? 0
}

/** When one bucket starts, for a chart's labels. */
export function bucketAt(report: HealthReport, index: number): number {
  return report.fromMs + index * report.bucketMs
}
