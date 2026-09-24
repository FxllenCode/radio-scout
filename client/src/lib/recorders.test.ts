import { describe, expect, it } from 'vitest'

import {
  available,
  bucketAt,
  bucketCount,
  channelKey,
  channelName,
  concerns,
  channelTraces,
  dashboardHealth,
  formatFrequency,
  isStale,
  isTrunked,
  overall,
  plot,
  reasonLabel,
  recorderName,
  stateLabel,
  STALE_MS,
  traceMean,
  traceRange,
} from './recorders'
import type {
  ChannelHealth,
  Demodulator,
  Dashboard,
  HealthReport,
  RecorderView,
} from '@/types'

const AT = 1_700_000_000_000

function recorder(over: Partial<RecorderView> = {}): RecorderView {
  return {
    id: 1,
    name: 'butco-pi',
    connected: true,
    connectedAtMs: AT - 60_000,
    lastMessageMs: AT - 1_000,
    sdrs: [],
    systems: [],
    demodulators: [],
    calls: [],
    notRecorded: [],
    refused: [],
    ...over,
  }
}

function demodulator(over: Partial<Demodulator> = {}): Demodulator {
  return {
    id: '0_0',
    sourceNum: 0,
    recNum: 0,
    state: 'available',
    calls: 0,
    recordedSeconds: 0,
    ...over,
  }
}

function channel(over: Partial<ChannelHealth> = {}): ChannelHealth {
  return {
    systemRef: 11,
    freq: 774_031_250,
    samples: 2,
    airMs: 8_000,
    errorCount: 6,
    spikeCount: 1,
    errorRate: [45],
    spikeRate: [7.5],
    signalDbm: [-63],
    noiseDbm: [-94],
    driftHz: [-137],
    ...over,
  }
}

describe('concerns', () => {
  it('says nothing about a recorder that is working', () => {
    const working = recorder({
      sdrs: [sdr(0, 4)],
      systems: [system('p25', 39.3)],
      demodulators: [demodulator()],
    })

    expect(concerns(working, AT)).toEqual([])
    expect(overall(concerns(working, AT))).toBe('ok')
  })

  /** The whole reason a departed recorder stays on the list at all: a row that
   *  vanished would be indistinguishable from one that was never set up. */
  it('leads with a recorder that disconnected', () => {
    const gone = recorder({ connected: false, disconnectedAtMs: AT - 5_000 })

    const found = concerns(gone, AT)

    expect(found).toHaveLength(1)
    expect(found[0].health).toBe('bad')
    expect(found[0].message).toContain('disconnected')
  })

  /** And says nothing else about it: every other reading would be describing
   *  the last thing it said before it went. */
  it('says nothing else about a recorder that is not there', () => {
    const gone = recorder({
      connected: false,
      systems: [system('p25', 0)],
      sdrs: [sdr(0, 4)],
      demodulators: [demodulator({ state: 'recording' })],
    })

    expect(concerns(gone, AT)).toHaveLength(1)
  })

  /** Connected and silent is the failure no other signal on the instance would
   *  show — the socket being open proves only that nothing has closed it. */
  it('flags a recorder that is connected and has stopped talking', () => {
    const silent = recorder({ lastMessageMs: AT - STALE_MS })

    const found = concerns(silent, AT)

    expect(found[0].health).toBe('bad')
    expect(found[0].message).toContain('stopped reporting')
  })

  it('does not flag a recorder that spoke a moment ago', () => {
    expect(concerns(recorder({ lastMessageMs: AT - STALE_MS + 1 }), AT)).toEqual([])
  })

  /** Zero decode rate on a trunked System is a receiver that has lost its
   *  control channel — the clearest "this is broken" there is. */
  it('flags a trunked system decoding nothing', () => {
    const deaf = recorder({ systems: [system('p25', 0)] })

    const found = concerns(deaf, AT)

    expect(found[0].health).toBe('bad')
    expect(found[0].message).toContain('control channel')
  })

  /** And says nothing about a conventional one, which has no control channel to
   *  decode and reads zero forever. */
  it('says nothing about a conventional system decoding nothing', () => {
    const conventional = recorder({ systems: [system('conventionalP25', 0)] })

    expect(concerns(conventional, AT)).toEqual([])
  })

  /** A System the recorder never named still has to be nameable, or the one
   *  concern an Operator most needs would point at nothing. */
  it('names an unnamed system by its number', () => {
    const deaf = recorder({
      systems: [{ sysNum: 3, systemType: 'p25', decodeRate: 0, controlChannels: [] }],
    })

    expect(concerns(deaf, AT)[0].message).toContain('System 3')
  })

  /** A System whose type never arrived is not assumed to be either: guessing
   *  wrong here is a red light on a healthy receiver or silence on a dead one. */
  it('says nothing about a system whose type is unknown', () => {
    const unknown = recorder({
      systems: [{ sysNum: 0, decodeRate: 0, controlChannels: [] }],
    })

    expect(concerns(unknown, AT)).toEqual([])
  })

  /** Not a fault — a receiver at capacity, and the next transmission on that
   *  SDR is the one nobody gets to hear. */
  it('warns when every demodulator on an SDR is in use', () => {
    const busy = recorder({
      sdrs: [sdr(0, 2)],
      demodulators: [
        demodulator({ id: '0_0', state: 'recording' }),
        demodulator({ id: '0_1', recNum: 1, state: 'recording' }),
      ],
    })

    const found = concerns(busy, AT)

    expect(found[0].health).toBe('warn')
    expect(found[0].message).toContain('SDR 0')
  })

  it('says nothing while one demodulator is still free', () => {
    const roomy = recorder({
      sdrs: [sdr(0, 2)],
      demodulators: [
        demodulator({ id: '0_0', state: 'recording' }),
        demodulator({ id: '0_1', recNum: 1 }),
      ],
    })

    expect(concerns(roomy, AT)).toEqual([])
  })

  /** An SDR carrying no demodulators at all is a control-channel receiver, not
   *  one that has run out. */
  it('says nothing about an SDR that carries no demodulators', () => {
    expect(concerns(recorder({ sdrs: [sdr(0, 0)] }), AT)).toEqual([])
  })

  /** Ids are stable across refreshes so React keeps the row rather than
   *  rewriting it — and unique per SDR, or two busy dongles would collide. */
  it('gives each concern a stable, distinct id', () => {
    const busy = recorder({
      sdrs: [sdr(0, 1), sdr(1, 1)],
      demodulators: [
        demodulator({ id: '0_0', state: 'recording' }),
        demodulator({ id: '1_0', sourceNum: 1, state: 'recording' }),
      ],
    })

    const ids = concerns(busy, AT).map((concern) => concern.id)

    expect(new Set(ids).size).toBe(ids.length)
    expect(concerns(busy, AT).map((c) => c.id)).toEqual(ids)
  })
})

describe('overall and dashboardHealth', () => {
  it('reads the loudest concern there is', () => {
    expect(overall([])).toBe('ok')
    expect(overall([{ id: 'a', health: 'warn', message: '' }])).toBe('warn')
    expect(
      overall([
        { id: 'a', health: 'warn', message: '' },
        { id: 'b', health: 'bad', message: '' },
      ]),
    ).toBe('bad')
  })

  it('folds a whole dashboard into one answer', () => {
    const dashboard: Dashboard = {
      atMs: AT,
      recorders: [recorder(), recorder({ id: 2, connected: false })],
    }

    expect(dashboardHealth(dashboard)).toBe('bad')
  })

  /** An Instance nobody has dialled into is not unhealthy — it is one an
   *  Operator has not set this up on, which the screen says in words. */
  it('reads an empty dashboard as healthy', () => {
    expect(dashboardHealth({ atMs: AT, recorders: [] })).toBe('ok')
  })
})

describe('reading the dashboard', () => {
  it('measures staleness against the server clock it was handed', () => {
    expect(isStale(recorder({ lastMessageMs: AT - STALE_MS }), AT)).toBe(true)
    expect(isStale(recorder({ lastMessageMs: AT }), AT)).toBe(false)
  })

  /** A recorder that is gone is not "stale" — it is gone, and saying both would
   *  put two lines on the screen about one fact. */
  it('never calls a disconnected recorder stale', () => {
    expect(isStale(recorder({ connected: false, lastMessageMs: 0 }), AT)).toBe(false)
  })

  it('knows which system types have a control channel', () => {
    expect(isTrunked('p25')).toBe(true)
    expect(isTrunked('smartnet')).toBe(true)
    expect(isTrunked('conventionalDMR')).toBe(false)
    expect(isTrunked(undefined)).toBe(false)
  })

  it('counts only its own SDR’s free demodulators', () => {
    const slots = [
      demodulator({ id: '0_0' }),
      demodulator({ id: '0_1', recNum: 1, state: 'recording' }),
      demodulator({ id: '1_0', sourceNum: 1 }),
    ]

    expect(available(slots, 0)).toBe(1)
    expect(available(slots, 1)).toBe(1)
    expect(available(slots, 2)).toBe(0)
  })

  /** Nearly every install leaves `instanceId` empty, so the fallback is the
   *  common path rather than the exception. */
  it('names an unnamed recorder by its connection', () => {
    expect(recorderName(recorder())).toBe('butco-pi')
    expect(recorderName(recorder({ name: undefined, id: 7 }))).toBe('Recorder 7')
  })

  it('puts every state and reason into words', () => {
    expect(stateLabel('available')).toBe('Available')
    expect(reasonLabel('no-recorder')).toBe('No demodulator free')
  })

  /** A recorder one version ahead should read oddly rather than read as
   *  nothing — a blank cell is indistinguishable from a bug here. */
  it('shows a word it has never heard of rather than hiding it', () => {
    expect(stateLabel('hyperdrive')).toBe('hyperdrive')
    expect(reasonLabel('sunspots')).toBe('sunspots')
  })
})

describe('the health charts', () => {
  it('reads a frequency the way it is written on a radio', () => {
    expect(formatFrequency(774_031_250)).toBe('774.0313 MHz')
  })

  /** Two SDRs on one frequency are the whole point of the chart, so they must
   *  never render — or key — alike. */
  /** A channel with no SDR behind it still needs a key of its own, or two
   *  rdio-dialect frequencies would collide into one row. */
  it('keys a channel that names no SDR', () => {
    expect(channelKey(channel())).toBe('11-774031250-any')
    expect(channelKey(channel({ sdr: 0 }))).toBe('11-774031250-0')
  })

  it('tells two SDRs on one frequency apart', () => {
    const zero = channel({ sdr: 0 })
    const one = channel({ sdr: 1 })

    expect(channelName(zero)).not.toBe(channelName(one))
    expect(channelKey(zero)).not.toBe(channelKey(one))
    expect(channelName(channel())).toBe('774.0313 MHz')
  })

  it('averages a trace over the buckets that were measured', () => {
    expect(traceMean([2, null, 4])).toBe(3)
    expect(traceMean([null, null])).toBeUndefined()
  })

  it('spans a trace over the readings it really has', () => {
    expect(traceRange([2, null, 4])).toEqual({ min: 2, max: 4 })
    expect(traceRange([null])).toBeUndefined()
  })

  /** A gap has to stay a gap: a chart that plotted "not measured" as the bottom
   *  of its range would draw a dead receiver as a perfect one. */
  it('plots a gap as a gap', () => {
    expect(plot([0, null, 10])).toEqual([0, null, 1])
    expect(plot([null, null])).toEqual([null, null])
  })

  /** Every reading really was the same; drawing them along the bottom edge
   *  would say they were the worst seen. */
  it('plots a flat trace down the middle', () => {
    expect(plot([5, 5, 5])).toEqual([0.5, 0.5, 0.5])
  })

  it('reads a report’s axis', () => {
    const report: HealthReport = {
      fromMs: 1_000,
      toMs: 4_000,
      bucketMs: 1_000,
      finestBucketMs: 900_000,
      channels: [channel({ errorRate: [1, 2, 3] })],
      omitted: 0,
    }

    expect(bucketCount(report)).toBe(3)
    expect(bucketAt(report, 2)).toBe(3_000)
  })

  it('reads an empty report as no buckets at all', () => {
    expect(
      bucketCount({
        fromMs: 0,
        toMs: 0,
        bucketMs: 1,
        finestBucketMs: 900_000,
        channels: [],
        omitted: 0,
      }),
    ).toBe(0)
  })
})

function sdr(sourceNum: number, demodulators: number) {
  return {
    sourceNum,
    centerHz: 774_000_000,
    rateHz: 2_400_000,
    gain: 40,
    errorHz: 0,
    minHz: 773_000_000,
    maxHz: 775_000_000,
    demodulators,
  }
}

function system(systemType: string, decodeRate: number) {
  return { sysNum: 0, shortName: 'butco', systemType, decodeRate, controlChannels: [] }
}

describe('channelTraces', () => {
  /** The spec names four things to chart — errors, spikes, signal and drift —
   *  and every one of them gets a trace of its own, in that order. */
  it('charts all four, each with the number that summarises it', () => {
    const traces = channelTraces(channel())

    expect(traces.map((trace) => trace.label)).toEqual([
      'Decode errors',
      'Spikes',
      'Signal',
      'Drift',
    ])
    // 6 errors and 1 spike over 8 seconds of air.
    expect(traces[0]).toMatchObject({ trace: [45], summary: '45.0 err/min' })
    expect(traces[1]).toMatchObject({ trace: [7.5], summary: '7.5 spikes/min' })
    expect(traces[2]).toMatchObject({ trace: [-63], summary: '-63 dBm · noise -94 dBm' })
    expect(traces[3]).toMatchObject({ trace: [-137], summary: '-137 Hz' })
  })

  /** Drift is signed — a receiver pulling one way is the thing worth seeing —
   *  so a positive one says so rather than reading as a magnitude. */
  it('signs a positive drift', () => {
    expect(channelTraces(channel({ driftHz: [12] }))[3].summary).toBe('+12 Hz')
  })

  /** Nothing measured is never drawn as a perfect score. */
  it('says when nothing was measured rather than showing a zero', () => {
    const traces = channelTraces(
      channel({
        airMs: 0,
        signalDbm: [null],
        noiseDbm: [null],
        driftHz: [null],
      }),
    )

    expect(traces.map((trace) => trace.summary)).toEqual([
      'no air',
      'no air',
      'not measured',
      'not measured',
    ])
  })

  /** Noise without a signal beside it is still worth reading. */
  it('shows the noise floor alone when no signal was measured', () => {
    expect(channelTraces(channel({ signalDbm: [null] }))[2].summary).toBe(
      'not measured · noise -94 dBm',
    )
  })
})
