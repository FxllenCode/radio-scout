import { describe, expect, it } from 'vitest'

import type { Call } from '@/types'

import { callAnnouncement, feedAnnouncement } from './announce'

const CALL: Call = {
  id: 1,
  systemRef: 11,
  systemLabel: 'Fulton County',
  talkgroupRef: 54241,
  talkgroupLabel: 'FD Dispatch',
  unitRef: 1602381,
  audioUrl: '/api/call/1/audio',
}

/** What a screen reader is told when a Call starts (#78) — the readout a
 *  sighted Listener glances at, as one sentence. */
describe('callAnnouncement', () => {
  it.each([
    ['a live Call', CALL, false, 'FD Dispatch, Fulton County, unit 1602381'],
    ['one from the archive', CALL, true, 'From the archive: FD Dispatch, Fulton County, unit 1602381'],
    ['a named unit', { ...CALL, unitLabel: 'Engine 7' }, false, 'FD Dispatch, Fulton County, unit Engine 7'],
    ['no unit at all', { ...CALL, unitRef: undefined }, false, 'FD Dispatch, Fulton County'],
    ['an uncurated Talkgroup', { ...CALL, talkgroupLabel: undefined, unitRef: undefined }, false, 'Talkgroup 54241, Fulton County'],
    // An emergency is the first thing said, as it is the first thing drawn.
    ['an emergency', { ...CALL, emergency: true, unitRef: undefined }, false, 'Emergency. FD Dispatch, Fulton County'],
  ] as const)('reads %s', (_case, call, fromArchive, said) => {
    expect(callAnnouncement(call, fromArchive)).toBe(said)
  })
})

/**
 * What a screen reader is told when the feed's standing changes (#78).
 *
 * Keyed on the *cause*, never on how far a reconnect has got: a socket
 * retrying in a loop flips between connecting and gone every few seconds, and
 * announcing each flip would repeat "the server isn't answering" for as long
 * as it is not.
 */
describe('feedAnnouncement', () => {
  it.each([
    ['live', {}, 'Live feed connected.'],
    ['off', {}, 'Live feed switched off.'],
    ['playback', {}, 'Playing from the archive.'],
    ['down', {}, 'Lost the link to the server. Retrying.'],
    ['down', { offline: true }, 'This device is offline.'],
    ['down', { refused: 'instance-full' }, 'This scanner is full right now.'],
    ['down', { refused: 'something-new' }, 'The server turned this connection away.'],
    // A Listener's own silence is theirs whatever the network is doing.
    ['off', { offline: true }, 'Live feed switched off.'],
  ] as const)('reads %s %o as %s', (status, facts, said) => {
    expect(feedAnnouncement(status, facts)).toBe(said)
  })
})
