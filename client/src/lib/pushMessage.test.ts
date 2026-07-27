import { describe, expect, it } from 'vitest'

import { notificationFor, targetFor, type PushPayload } from './pushMessage'

const call: PushPayload = {
  id: 42,
  systemRef: 11,
  talkgroupRef: 54241,
  system: 'Fulton County',
  talkgroup: 'Fire Dispatch',
  count: 1,
}

describe('the notification a push becomes', () => {
  it('is titled with the Talkgroup and says which System', () => {
    const { title, options } = notificationFor(call)

    expect(title).toBe('Fire Dispatch')
    expect(options.body).toBe('New call · Fulton County')
  })

  it('says how many Calls it stands for when the window swallowed some', () => {
    const { options } = notificationFor({ ...call, count: 4 })

    expect(options.body).toBe('4 new calls · Fulton County')
  })

  // The server's coalescing window has already fired; this is the device's own
  // half of the same rule — a Talkgroup occupies one notification, replaced
  // rather than stacked, so a shift's traffic can't bury the lock screen.
  it('replaces the last notification for the same Talkgroup', () => {
    const { options } = notificationFor(call)

    expect(options.tag).toBe('rs-11-54241')
    expect(options.renotify).toBe(true)
  })

  it('carries the Call to open on tap', () => {
    const { options } = notificationFor(call)

    expect(options.data).toEqual({ url: '/?call=42' })
  })

  it('falls back to the Talkgroup Ref when nothing has named it', () => {
    const { title, options } = notificationFor({
      ...call,
      system: undefined,
      talkgroup: undefined,
    })

    expect(title).toBe('Talkgroup 54241')
    expect(options.body).toBe('New call')
  })

  // iOS revokes a push subscription that receives a push and shows nothing, so
  // a payload we can't read still has to become *something* — showing nothing
  // would cost the listener every future notification.
  it.each([
    ['nothing at all', undefined],
    ['a payload that is not ours', { hello: 'world' } as unknown as PushPayload],
    ['a Call with no Refs', { id: 1 } as unknown as PushPayload],
  ])('still shows a notification for %s', (_what, payload) => {
    const { title, options } = notificationFor(payload)

    expect(title).toBe('Radio-Scout')
    expect(options.body).toBe('New activity')
    expect(options.data).toEqual({ url: '/' })
  })
})

describe('where a tap goes', () => {
  const client = (url: string) => ({ url, focused: false })

  it('focuses the app if it is already open', () => {
    const open = [client('http://localhost/search'), client('http://localhost/')]

    expect(targetFor(open, 'http://localhost')).toBe(open[0])
  })

  it('opens a window when nothing is', () => {
    expect(targetFor([], 'http://localhost')).toBeUndefined()
  })

  it('ignores a window on another origin', () => {
    expect(
      targetFor([client('https://example.com/')], 'http://localhost'),
    ).toBeUndefined()
  })
})
