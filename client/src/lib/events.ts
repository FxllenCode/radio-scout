/**
 * **Events** — incidents frozen against Retention (#67, spec US 38).
 *
 * Pure: a report or a row in, a sentence or a URL out. The screens own *when*
 * an Event is assembled; this owns what the numbers mean — which is what keeps
 * the Search screen's "add to event" and the Events screen's own from telling a
 * different story about the same request.
 *
 * # Why a report needs a sentence at all
 *
 * Freezing has four endings and an Operator does different things about them.
 * "2 of 4 added" would hide the two that matter: a Call that **aged out while
 * they were reading the page** is normal and nothing to do about, where a Call
 * whose audio **would not read** means the object store is unwell and the
 * incident they think they kept is short two transmissions. The server counts
 * them apart for that reason; this is where the counting becomes something to
 * read. [`freezeNotice`] is **silent when everything worked**, so a notice
 * appearing at all means something is worth reading — and no row is written
 * for a Call whose audio would not read, so adding it again really does freeze
 * it.
 *
 * # Where the report is shown
 *
 * **The report is the parent's, which is the bug a test caught.** `AddToEvent`
 * reports through `onResult` rather than rendering its own notice, because a
 * successful freeze drops the selection and the selection going is what
 * *unmounts* that control — so a report rendered inside it vanished at the
 * exact moment it was worth reading.
 *
 * Multi-select lives on the **Search** screen and is *absent* rather than
 * disabled for a Listener, because curating an Event is admin-gated
 * ([ADR-0017](../../../docs/adr/0017-events-are-admin-gated.md)). Changing a
 * filter drops it, which is #49's rule one screen along — a bulk action over
 * rows that scrolled out of the answer is the one thing multi-select must never
 * do — and it lives in the effect that already tells the **Run** the search
 * changed, so the back button gets the same treatment as a dropdown.
 *
 * The server half — why an Event is a copy, and what freezing costs — is
 * `src/event/mod.rs`.
 */
import type { FreezeReport } from '@/types'

/** Where an Event's download comes from, in one of the two formats. */
export function eventExportUrl(id: number, format: 'zip' | 'wav'): string {
  return `/api/admin/events/${id}/export?format=${format}`
}

/**
 * What a freeze came to, as a sentence — or **`null` when there is nothing
 * worth saying**.
 *
 * Nothing worth saying is exactly one case: every Call named was frozen, which
 * is what happens essentially every time. A notice that appeared on success as
 * well as on trouble would be furniture, and an Operator would stop reading the
 * one that mattered (`useShareLink`'s rule, one screen along).
 */
export function freezeNotice(report: FreezeReport): string | null {
  const troubles = [
    plural(report.alreadyHeld, 'was', 'were', 'already in this event'),
    plural(report.missing, 'is', 'are', 'no longer in the archive'),
    plural(report.unreadable, 'could not', 'could not', 'be read from storage'),
  ].filter((part): part is string => part !== null)
  if (troubles.length === 0) return null

  const kept = report.frozen === 1 ? '1 call added' : `${report.frozen} calls added`
  return `${kept}. ${sentenceCase(troubles.join('; '))}.`
}

/** `2 calls were already in this event`, or nothing when the count is zero. */
function plural(count: number, one: string, many: string, tail: string): string | null {
  if (count === 0) return null
  const noun = count === 1 ? '1 call' : `${count} calls`
  return `${noun} ${count === 1 ? one : many} ${tail}`
}

function sentenceCase(text: string): string {
  return text.charAt(0).toUpperCase() + text.slice(1)
}

/**
 * How much disk an Event is holding, for a human.
 *
 * **Shown because it is spent for good.** Every other number on an admin screen
 * describes something **Retention** will eventually reclaim; frozen bytes are
 * the one thing on this Instance that no policy can take back, so the screen
 * that creates them is the screen that has to say how many.
 *
 * Decimal units (a gigabyte is 10⁹), because that is what a disk is sold in and
 * what `[retention] max_size_gb` means — the number an Operator would compare
 * this against.
 */
export function formatBytes(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes < 0) return '—'
  const units = ['B', 'kB', 'MB', 'GB', 'TB']
  let value = bytes
  let unit = 0
  while (value >= 1000 && unit < units.length - 1) {
    value /= 1000
    unit += 1
  }
  // Whole bytes are whole; everything else keeps one decimal, so a county's
  // incident reads as "1.4 GB" rather than "1.43829 GB" or a flat "1 GB".
  const shown = unit === 0 ? String(Math.round(value)) : value.toFixed(1)
  return `${shown} ${units[unit]}`
}

/** The line under an Event's name: how much of it there is, and what that costs. */
export function eventSummary(calls: number, bytes: number): string {
  const transmissions = calls === 1 ? '1 call' : `${calls} calls`
  return `${transmissions} · ${formatBytes(bytes)} kept`
}
