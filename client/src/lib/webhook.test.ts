import { describe, expect, it } from 'vitest'

import {
  formatName,
  looksPostable,
  markName,
  webhookHealthLine,
} from './webhook'
import { MARKS, WEBHOOK_FORMATS, type AdminWebhook } from '@/types'

function hook(row: Partial<AdminWebhook> = {}): AdminWebhook {
  return {
    id: 1,
    label: 'dispatch',
    host: 'discord.com',
    format: 'radio-scout',
    marks: ['emergency'],
    scope: { all: true, sel: {} },
    disabled: false,
    queued: 0,
    lastSuccessMs: null,
    lastFailureMs: null,
    lastFailure: null,
    consecutiveFailures: 0,
    createdAtMs: 1_700_000_000_000,
    ...row,
  }
}

describe('what an address has to look like', () => {
  /** **Checked in the browser as well as on the server**, and the reason is not
   *  latency: submitting is the last moment the Operator can see what they
   *  pasted, because the address is a credential and the listing can never show
   *  it back. Being told before it disappears is the difference between fixing
   *  a typo and going back to Discord for the whole URL. */
  it.each([
    ['https://discord.com/api/webhooks/1/t0ken', true],
    ['http://hooks.internal:9000/x', true],
    ['  https://hooks.internal/x  ', true],
    ['', false],
    ['/api/webhooks/1/t', false],
    ['discord.com/api/webhooks', false],
    ['file:///etc/passwd', false],
    ['https:///nohost', false],
    // A line-wrapped paste out of a document: a good-looking host, and not a
    // URL anybody can fetch. The server refuses it too.
    ['https://scan example/x', false],
  ])('reads %s as postable: %s', (url, expected) => {
    expect(looksPostable(url)).toBe(expected)
  })
})

describe('what an operator is told about a webhook', () => {
  /** The line leads with the **host**, because the URL never comes back from
   *  the server and a screen of rows an Operator cannot tell apart is one they
   *  will misconfigure. */
  it('leads with the host and says when it has never delivered', () => {
    expect(webhookHealthLine(hook())).toBe(
      'discord.com · Radio-Scout JSON · Emergency · never delivered',
    )
  })

  /** **A webhook watching no marks is called out by name.** It is configured,
   *  enabled, correctly scoped, and will never fire — the one misconfiguration
   *  on this screen that produces no error anywhere, so nothing else would
   *  reveal it. */
  it('says when a webhook is watching nothing', () => {
    expect(webhookHealthLine(hook({ marks: [] }))).toContain('watching nothing')
  })

  /** A stored address that is not a URL at all is a webhook that will never
   *  deliver, and the row says so rather than rendering a blank where a host
   *  should be. */
  it('says when there is no usable address', () => {
    expect(webhookHealthLine(hook({ host: null }))).toContain('no usable URL')
  })

  /** The queue depth is the number that says whether anything is wrong right
   *  now — and it is the **durable** one, so an endpoint down for an hour reads
   *  as an hour of Calls waiting rather than an hour of Calls lost. */
  it('carries the depth, the failures and the last reason', () => {
    const line = webhookHealthLine(
      hook({
        disabled: true,
        queued: 4,
        consecutiveFailures: 2,
        lastFailure: 'sink-refused (429)',
        lastSuccessMs: 1_700_000_000_000,
      }),
    )

    expect(line).toContain('disabled')
    expect(line).toContain('4 queued')
    expect(line).toContain('2 failed · sink-refused (429)')
    expect(line).toContain('last delivered')
  })

  /** A failure with no reason stored still counts, rather than rendering a
   *  dangling separator. */
  it('counts a failure that has no reason beside it', () => {
    expect(webhookHealthLine(hook({ consecutiveFailures: 1 }))).toContain(
      '1 failed ·',
    )
    expect(webhookHealthLine(hook({ consecutiveFailures: 1 }))).not.toContain(
      '1 failed · ·',
    )
  })
})

describe('the vocabularies', () => {
  /** Every mark and every format has a **word**, not a slug, because these are
   *  read by a person. Written as a total `Record`, so #55's tone-out is a
   *  compile error here until it is given one. */
  it('names every mark and every format there is', () => {
    for (const mark of MARKS) {
      expect(markName(mark)).not.toBe(mark)
      expect(markName(mark)).not.toBe('')
    }
    for (const format of WEBHOOK_FORMATS) {
      expect(formatName(format)).not.toBe('')
    }
    expect(markName('emergency')).toBe('Emergency')
    expect(formatName('discord')).toBe('Discord message')
  })
})
