import { describe, expect, it } from 'vitest'

import {
  asOf,
  concerns,
  formatUptime,
  overall,
  totalStored,
  volumeOf,
} from './status'
import type { InstanceStatus } from '@/types'

/** A perfectly well Instance: everything running, room on the disk, no peer in
 *  trouble. Every test below spoils exactly one thing about it. */
function well(): InstanceStatus {
  return {
    version: '0.1.0',
    startedAtMs: 1_700_000_000_000,
    uptimeSeconds: 3_600,
    listeners: 2,
    ingest: { stored: 120, duplicate: 3 },
    refused: { duplicate: 3 },
    errors: {},
    workers: [
      { name: 'retention', depth: 0, done: 2, running: true },
      { name: 'quiet', depth: 1, done: 119, running: true },
    ],
    gaugesAtMs: 1_700_000_003_600,
    rateWindowMs: 3_600_000,
    archive: {
      calls: 120,
      audioBytes: 50_000_000,
      frozenAudioBytes: 1_000_000,
      oldestCallAtMs: 1_699_000_000_000,
    },
    storage: { freeBytes: 40_000_000_000, totalBytes: 64_000_000_000 },
    retention: { days: 30 },
    systems: [{ ref: 11, label: 'Fulton', calls: 40, lastCallAtMs: 1_700_000_003_000 }],
    sinks: [
      { sink: 'downstream', total: 1, disabled: 0, failing: 0, queued: 0 },
      { sink: 'webhook', total: 0, disabled: 0, failing: 0, queued: 0 },
    ],
  }
}

describe('concerns', () => {
  it('says nothing about an instance that is well', () => {
    expect(concerns(well())).toEqual([])
    expect(overall(concerns(well()))).toBe('ok')
  })

  /** The half a depth cannot give. A worker that died settles everything it was
   *  holding on the way out, so a queue-depth table shows it as perfectly idle
   *  — which is why the server sends `running` at all. */
  it('names a worker whose loop has stopped, and calls it bad', () => {
    const status = well()
    status.workers[1].running = false

    const [first] = concerns(status)

    expect(first.health).toBe('bad')
    expect(first.message).toContain('quiet')
    expect(overall(concerns(status))).toBe('bad')
  })

  it('warns when the disk is filling, and gives up on it when it is nearly gone', () => {
    const low = well()
    low.storage.freeBytes = 6_400_000_000 // a tenth

    expect(concerns(low)[0]?.health).toBe('warn')

    const nearly = well()
    nearly.storage.freeBytes = 1_280_000_000 // a fiftieth

    expect(concerns(nearly)[0]?.health).toBe('bad')
  })

  /** **Not** a concern, deliberately. Retention prunes the archive down to the
   *  cap, so an instance sitting *at* its cap is the policy working — warning
   *  about it would cry wolf on every instance that has one. */
  it('says nothing about an archive sitting at its size cap', () => {
    const status = well()
    status.retention.maxSizeBytes = 51_000_000

    expect(concerns(status)).toEqual([])
  })

  /** ...but frozen bytes over the cap is the one storage state nothing can fix:
   *  an **Event**'s audio is spent for good, so the sweeper eats the archive
   *  around it and then stops. The server says so once per sweep; this is where
   *  an Operator sees it without reading the log. */
  it('calls frozen audio over the cap bad, because nothing can prune it', () => {
    const status = well()
    status.retention.maxSizeBytes = 500_000
    status.archive.frozenAudioBytes = 1_000_000

    const [first] = concerns(status)

    expect(first.health).toBe('bad')
    expect(first.message).toContain('Frozen')
  })

  it('warns about sinks that are failing, and says how many', () => {
    const status = well()
    status.sinks[0].failing = 2
    status.sinks[0].total = 3

    const [first] = concerns(status)

    expect(first.health).toBe('warn')
    expect(first.message).toContain('2')
    expect(first.message).toContain('downstream')
  })

  /** A county can be quiet for an hour at three in the morning and a rural
   *  system for a day. Every threshold anyone could pick here is wrong for
   *  somebody, so the page shows the age and lets the Operator read it. */
  it('says nothing about a system that has been silent', () => {
    const status = well()
    status.systems[0] = { ref: 11, calls: 0 }

    expect(concerns(status)).toEqual([])
  })

  /** The verdict is read off the list rather than kept beside it, so the three
   *  readings are the three the list can produce — including the middle one,
   *  which is the only state where an Operator reads the page and does nothing
   *  tonight. */
  it('reads a list of warnings as worth a look', () => {
    const status = well()
    status.sinks[1].failing = 1
    status.sinks[1].total = 1

    expect(overall(concerns(status))).toBe('warn')
  })

  it('puts the worst first, so one glance is one answer', () => {
    const status = well()
    status.sinks[0].failing = 1
    status.workers[0].running = false

    expect(concerns(status).map((concern) => concern.health)).toEqual(['bad', 'warn'])
  })
})

describe('volumeOf', () => {
  it('reports what the volume holds, not what the archive does', () => {
    expect(volumeOf({ freeBytes: 25, totalBytes: 100 })).toEqual({
      freeBytes: 25,
      totalBytes: 100,
      used: 0.75,
    })
  })

  it('is nothing at all when the store is not on this machine', () => {
    expect(volumeOf({})).toBeUndefined()
    // A volume of zero bytes cannot be a fraction of anything, and dividing by
    // it would put `NaN` into a progress bar's width.
    expect(volumeOf({ freeBytes: 0, totalBytes: 0 })).toBeUndefined()
  })
})

describe('totalStored', () => {
  it('counts the frozen bytes too, because the size cap does', () => {
    expect(totalStored(well().archive)).toBe(51_000_000)
  })
})

describe('formatUptime', () => {
  it.each([
    [0, '0s'],
    [45, '45s'],
    [90, '1m'],
    [3_600, '1h 0m'],
    [4_000, '1h 6m'],
    [86_400, '1d 0h'],
    [270_000, '3d 3h'],
  ])('renders %i seconds as %s', (seconds, shown) => {
    expect(formatUptime(seconds)).toBe(shown)
  })
})

describe('asOf', () => {
  /** The readings are cached server-side, so the page says how old they are
   *  rather than implying they are live. */
  it.each([
    [0, 'just now'],
    [900, 'just now'],
    [4_000, '4s ago'],
    [65_000, '1m ago'],
    // A browser clock a little ahead of the server's must not read as the
    // future.
    [-5_000, 'just now'],
  ])('renders %i ms old as %s', (age, shown) => {
    expect(asOf(1_000_000 - age, 1_000_000)).toBe(shown)
  })
})
