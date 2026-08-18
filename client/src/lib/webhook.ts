/**
 * The **Webhook** screen's pure half (#54): what a row says about itself, and
 * what a browser can tell an Operator before the server has to.
 *
 * The scope half is [`./downstream`]'s and is shared verbatim — a Webhook is
 * scoped by the same [`Selection`] a Downstream is, so a form that edited it a
 * second way would be a second set of round-tripping rules to keep honest.
 */
import type { AdminWebhook, Mark, WebhookFormat } from '@/types'

import { formatCallTime } from './archive'

/** What an Operator calls a mark. */
export function markName(mark: Mark): string {
  // A `Record` rather than a `switch`, so #55's `tone` is a compile error here
  // until it is given a word rather than rendering as a slug.
  const names: Record<Mark, string> = { emergency: 'Emergency' }
  return names[mark]
}

/** What an Operator calls a body shape. */
export function formatName(format: WebhookFormat): string {
  const names: Record<WebhookFormat, string> = {
    'radio-scout': 'Radio-Scout JSON',
    discord: 'Discord message',
  }
  return names[format]
}

/**
 * Whether a URL is one this Instance could actually post to.
 *
 * The server refuses the same thing, and this is deliberately a **second**
 * check rather than a substitute for it: a webhook URL is a credential, so the
 * moment an Operator submits the form is the last moment it is ever on their
 * screen. Being told "that is not a URL" before it disappears is the difference
 * between fixing a typo and pasting the whole thing again from Discord.
 *
 * `http://` is allowed as well as `https://` because an Operator's automation
 * may well be a script on the same LAN — and the rule here is deliberately the
 * same one `webhook::is_postable_url` applies on the server, down to refusing
 * whitespace: a browser that accepted what the server refuses would send an
 * Operator's credential up only to have it bounced, at which point the form has
 * already cleared it.
 */
export function looksPostable(url: string): boolean {
  const trimmed = url.trim()
  const scheme = ['http://', 'https://'].find((prefix) =>
    trimmed.startsWith(prefix),
  )
  if (scheme == null) return false
  // Whitespace anywhere is not a URL anybody can fetch — `https://scan example`
  // is what a line-wrapped paste out of a document looks like, and it has a
  // perfectly good "host" by the rule below.
  if (/\s/.test(trimmed)) return false
  // Everything up to the first path, query or fragment has to be *something* —
  // `https:///x` has a scheme and no host, and would fail at delivery time
  // rather than here. Sliced past the scheme we just matched rather than split
  // on `://`, so there is no "what if there was no separator" arm to write that
  // the line above has already ruled out.
  return trimmed.slice(scheme.length).split(/[/?#]/)[0] !== ''
}

/**
 * What an Operator needs to know about a webhook at a glance, in one line.
 *
 * The [`healthLine`](./downstream) shape, with two differences that are this
 * feature's own:
 *
 * - It leads with the **host**, not the URL, because the URL is the credential
 *   and never comes back from the server. A row whose host is missing is one
 *   whose stored value is not a URL at all, and it says so — that webhook will
 *   never deliver, and nothing else on the screen would reveal it.
 * - **A webhook watching no marks is called out**, because it is silently inert:
 *   it is configured, enabled, correctly scoped, and will never fire. That is
 *   the one misconfiguration here that produces no error anywhere.
 */
export function webhookHealthLine(row: AdminWebhook): string {
  const parts = [row.host ?? 'no usable URL']
  parts.push(formatName(row.format))
  parts.push(
    row.marks.length === 0
      ? 'watching nothing'
      : row.marks.map(markName).join(', '),
  )
  if (row.disabled) parts.push('disabled')
  if (row.queued > 0) parts.push(`${row.queued} queued`)
  if (row.consecutiveFailures > 0) {
    parts.push(
      `${row.consecutiveFailures} failed${
        row.lastFailure == null ? '' : ` · ${row.lastFailure}`
      }`,
    )
  }
  parts.push(
    row.lastSuccessMs == null
      ? 'never delivered'
      : `last delivered ${formatCallTime(row.lastSuccessMs)}`,
  )
  return parts.join(' · ')
}
