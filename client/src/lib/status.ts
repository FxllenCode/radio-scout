/**
 * The status page's pure half (#70, spec US 48) — what the numbers *mean*.
 *
 * `GET /api/admin/status` is a page of facts; the screen's job is to answer one
 * question from them, which is whether an Operator has to do something. That
 * answer is [`concerns`], and it lives here rather than in the component so
 * every rule about it is a value a test constructs.
 *
 * # What is deliberately not a concern
 *
 * Two things this could flag and does not, because every threshold anyone could
 * pick for them is wrong for somebody:
 *
 * - **A System that has been silent.** A county is quiet at three in the
 *   morning and a rural system can be quiet for a day. The page shows the age
 *   and lets the Operator read it.
 * - **An archive sitting at its size cap.** Retention prunes down to the cap, so
 *   being at it is the policy working — flagging it would cry wolf on every
 *   instance that has one. What *is* flagged is the one storage state nothing
 *   can fix: frozen **Event** audio over the cap, which the sweeper reports once
 *   per sweep and can do nothing about.
 */
import type { ArchiveHealth, InstanceStatus, StorageHealth } from '@/types'

/** How a reading should be read. */
export type Health = 'ok' | 'warn' | 'bad'

/** One thing worth saying out loud, and how loudly. */
export interface Concern {
  /** Stable across refreshes, so React keeps the row rather than rewriting it. */
  id: string
  health: Health
  message: string
}

/** Below this fraction of the volume free, the disk is worth mentioning. */
const DISK_WARN = 0.15
/** ...and below this, it is worth acting on tonight. */
const DISK_BAD = 0.05

/**
 * Everything worth saying about this Instance, **worst first** — so one glance
 * is one answer.
 *
 * An empty list is the ordinary state and is what the page says "healthy" from:
 * a verdict derived from the absence of concerns cannot disagree with the list
 * beside it.
 */
export function concerns(status: InstanceStatus): Concern[] {
  const found: Concern[] = []

  for (const worker of status.workers) {
    if (!worker.running) {
      found.push({
        id: `worker-${worker.name}`,
        health: 'bad',
        message: `The ${worker.name} worker has stopped; restart the instance.`,
      })
    }
  }

  const volume = volumeOf(status.storage)
  if (volume !== undefined && volume.used > 1 - DISK_WARN) {
    found.push({
      id: 'disk',
      health: volume.used > 1 - DISK_BAD ? 'bad' : 'warn',
      message: `${Math.round((1 - volume.used) * 100)}% of the volume holding the audio is free.`,
    })
  }

  const cap = status.retention.maxSizeBytes
  if (cap !== undefined && status.archive.frozenAudioBytes > cap) {
    found.push({
      id: 'frozen',
      health: 'bad',
      message:
        'Frozen event audio is over the size cap on its own, so retention cannot get under it.',
    })
  }

  for (const sink of status.sinks) {
    if (sink.failing > 0) {
      found.push({
        id: `sink-${sink.sink}`,
        health: 'warn',
        message: `${sink.failing} of ${sink.total} ${sink.sink}s are failing.`,
      })
    }
  }

  // Worst first, and `sort` is stable in every engine this ships to, so equally
  // severe concerns keep the order they were found in — which is the order they
  // are written above rather than whatever a comparator happened to do.
  const rank: Record<Health, number> = { bad: 0, warn: 1, ok: 2 }
  return found.sort((a, b) => rank[a.health] - rank[b.health])
}

/** The one verdict, from the list rather than beside it. */
export function overall(found: Concern[]): Health {
  if (found.some((concern) => concern.health === 'bad')) return 'bad'
  if (found.some((concern) => concern.health === 'warn')) return 'warn'
  return 'ok'
}

/** A volume that really exists, with the fraction of it that is spent. */
export interface Volume {
  freeBytes: number
  totalBytes: number
  /** How much of it is used, between 0 and 1. */
  used: number
}

/**
 * The volume holding the audio, or nothing at all.
 *
 * Nothing on the S3 backend, where the server sends no reading because the
 * question is somebody else's machine's — and nothing for a volume of zero
 * bytes, which is not a real answer and would put `NaN` into a bar's width.
 *
 * **The two readings come back with the fraction** rather than the fraction
 * alone, because every caller that wants one wants all three — and a caller
 * given only the fraction has to re-check the two `undefined`s this function
 * has already ruled out, which is a pair of branches nothing can reach and
 * nothing can test.
 */
export function volumeOf(storage: StorageHealth): Volume | undefined {
  const { freeBytes, totalBytes } = storage
  if (freeBytes === undefined || !totalBytes) return undefined
  return { freeBytes, totalBytes, used: (totalBytes - freeBytes) / totalBytes }
}

/** Everything this Instance is storing — **including** the frozen bytes, because
 *  the size cap counts them and an Operator comparing the two would otherwise be
 *  comparing different numbers. */
export function totalStored(archive: ArchiveHealth): number {
  return archive.audioBytes + archive.frozenAudioBytes
}

/**
 * How long this process has been up, to two units.
 *
 * Two rather than one because the interesting cases are either side of a
 * boundary: "1h" hides whether an instance came up a minute ago or an hour ago,
 * and "3d 4h 12m 9s" is a number nobody reads.
 */
export function formatUptime(seconds: number): string {
  const whole = Math.max(0, Math.floor(seconds))
  const days = Math.floor(whole / 86_400)
  const hours = Math.floor((whole % 86_400) / 3_600)
  const minutes = Math.floor((whole % 3_600) / 60)
  if (days > 0) return `${days}d ${hours}h`
  if (hours > 0) return `${hours}h ${minutes}m`
  if (minutes > 0) return `${minutes}m`
  return `${whole}s`
}

/**
 * How old the database and disk readings are.
 *
 * They are cached server-side and the instant they were taken rides on the wire,
 * so the page says how old they are rather than implying they are live. A
 * browser clock a little ahead of the server's reads as "just now" rather than
 * as the future, the [`lastHeard`](./panel) rule.
 */
export function asOf(atMs: number, now: number): string {
  const seconds = Math.floor(Math.max(0, now - atMs) / 1_000)
  if (seconds < 2) return 'just now'
  if (seconds < 60) return `${seconds}s ago`
  return `${Math.floor(seconds / 60)}m ago`
}
