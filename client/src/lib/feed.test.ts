import { describe, expect, it } from 'vitest'

import {
  controlsFor,
  feedReadout,
  feedStatus,
  type ControlFacts,
  type Controls,
  type FeedBadge,
  type FeedStatus,
  type ReadoutFacts,
} from './feed'
import type { LiveStatus } from './liveFeed'

/** Every combination of the three facts, so precedence is pinned rather than
 *  sampled — a Listener can have switched the feed off *and* be playing the
 *  archive *and* have lost the socket, and exactly one answer is right. */
const CASES: {
  off: boolean
  playback: boolean
  link: LiveStatus
  expected: FeedStatus
}[] = [
  { off: false, playback: false, link: 'connected', expected: 'live' },
  { off: false, playback: false, link: 'connecting', expected: 'down' },
  { off: false, playback: false, link: 'offline', expected: 'down' },
  { off: false, playback: true, link: 'connected', expected: 'playback' },
  { off: false, playback: true, link: 'connecting', expected: 'playback' },
  { off: false, playback: true, link: 'offline', expected: 'playback' },
  { off: true, playback: false, link: 'connected', expected: 'off' },
  { off: true, playback: false, link: 'connecting', expected: 'off' },
  { off: true, playback: false, link: 'offline', expected: 'off' },
  { off: true, playback: true, link: 'connected', expected: 'off' },
  { off: true, playback: true, link: 'connecting', expected: 'off' },
  { off: true, playback: true, link: 'offline', expected: 'off' },
]

describe('feedStatus', () => {
  it.each(CASES)(
    'is $expected with off=$off playback=$playback link=$link',
    ({ off, playback, link, expected }) => {
      expect(feedStatus({ off, playback, link })).toBe(expected)
    },
  )

  /** Turned away (#77) is **Feed down** whatever the socket says: the server
   *  opens the socket before it says no, so `connected` alone would flash a
   *  green light at a Listener being refused on every retry. The Listener's
   *  own silences still outrank it. */
  it.each([
    [false, false, 'connected', 'down'],
    [false, false, 'connecting', 'down'],
    [false, true, 'connected', 'playback'],
    [true, false, 'connected', 'off'],
  ] as const)('with off=%s playback=%s link=%s and a refusal, is %s', (off, playback, link, expected) => {
    expect(feedStatus({ off, playback, link, refused: 'instance-full' })).toBe(expected)
  })
})

/** Nothing held, nothing on the air, nothing to replay. */
const NOTHING: ControlFacts = {
  onAir: false,
  systemHold: false,
  talkgroupHold: false,
  hasRecent: false,
}

/** A Call on the air, both kinds of hold, and a RECENT list — every
 *  precondition satisfied at once, so a case using this isolates the *status*. */
const EVERYTHING: ControlFacts = {
  onAir: true,
  systemHold: true,
  talkgroupHold: true,
  hasRecent: true,
}

const EVERY_CONTROL: (keyof Controls)[] = [
  'holdSystem',
  'holdTalkgroup',
  'skip',
  'replay',
  'pause',
  'avoid',
  'priority',
  'queue',
  'recent',
]

/** The expectation written as the controls a Listener can reach, which is how
 *  the screen reads — the rest are then asserted disabled by construction. */
const only = (...enabled: (keyof Controls)[]): Controls =>
  Object.fromEntries(
    EVERY_CONTROL.map((control) => [control, enabled.includes(control)]),
  ) as Record<keyof Controls, boolean>

const CONTROL_CASES: {
  what: string
  status: FeedStatus
  facts: ControlFacts
  expected: Controls
}[] = [
  {
    what: 'a Call on the air with the feed live',
    status: 'live',
    facts: { ...NOTHING, onAir: true },
    expected: only(
      'holdSystem',
      'holdTalkgroup',
      'skip',
      'replay',
      'pause',
      'avoid',
      'priority',
      'queue',
    ),
  },
  {
    // The queue counter is the one control here that needs no Call: US 24 asks
    // to see what is waiting, and "nothing" is an answer a Listener can act on.
    what: 'a lull with nothing behind it',
    status: 'live',
    facts: NOTHING,
    expected: only('queue'),
  },
  {
    // The hold outlives the Call it was placed on, and releasing it is the only
    // way back — so the control that can release it stays reachable, and the
    // one that cannot does not.
    what: 'a System hold outliving the Call it was placed on',
    status: 'live',
    facts: { ...NOTHING, systemHold: true },
    expected: only('holdSystem', 'queue'),
  },
  {
    what: 'a Talkgroup hold outliving the Call it was placed on',
    status: 'live',
    facts: { ...NOTHING, talkgroupHold: true },
    expected: only('holdTalkgroup', 'queue'),
  },
  {
    // US 13: with nothing playing, Replay reaches back to the last Call.
    //
    // …and so do **Hold, Avoid and Priority** (#56, #58). The Call the display
    // is showing
    // outlives the transmission — the card stays up, dimmed — and a Listener
    // reaching for "mute that" the second a chatty Talkgroup stops keying is
    // reaching for the thing on the screen in front of them. Skip and Pause do
    // not follow: there is genuinely nothing on the air to skip or hold.
    what: 'a lull with something in RECENT',
    status: 'live',
    facts: { ...NOTHING, hasRecent: true },
    expected: only(
      'holdSystem',
      'holdTalkgroup',
      'replay',
      'avoid',
      'priority',
      'queue',
      'recent',
    ),
  },
  {
    // The other half of that: nothing on the air and nothing ever heard, so
    // there is no displayed Call for Hold, Avoid or Priority to mean anything
    // about.
    // Pinned separately because it is what stops "reaches the last Call" from
    // quietly becoming "is always reachable".
    what: 'a lull on a feed that has never carried a Call',
    status: 'live',
    facts: NOTHING,
    expected: only('queue'),
  },
  {
    // A dropped socket stops Calls arriving; it does not stop the one playing,
    // and the Listener asked for none of it to stop.
    what: 'the socket gone while a Call plays on',
    status: 'down',
    facts: EVERYTHING,
    expected: only(...EVERY_CONTROL),
  },
  {
    // Switching off stops the Call, so these facts cannot co-occur today — the
    // answer must not depend on that staying true.
    what: 'the feed switched off, whatever else is true',
    status: 'off',
    facts: EVERYTHING,
    expected: only(),
  },
  {
    what: 'playback mode owning the audio, whatever else is true',
    status: 'playback',
    facts: EVERYTHING,
    expected: only(),
  },
]

describe('controlsFor', () => {
  it.each(CONTROL_CASES)('$what', ({ status, facts, expected }) => {
    expect(controlsFor(status, facts)).toEqual(expected)
  })
})

/**
 * Dot, word and prose decided together, so no state can end up amber but
 * pulsing, lit green under the words FEED OFF, or telling a Listener the server
 * is gone under a header that says it is connected.
 *
 * The distinctions are the point: a Listener has to be able to tell a silence
 * they chose from one they didn't, because only one of them is theirs to fix.
 *
 * The prose is asserted, not merely reached: a distinctive phrase per state, so
 * swapping two states' sentences fails here rather than shipping a Listener the
 * wrong instruction under the right label.
 */
const READOUT_CASES: {
  status: FeedStatus
  link: LiveStatus
  /** The rest of what the readout is told — the device's network, and whether
   *  the Instance has a System yet. */
  facts?: ReadoutFacts
  badge: FeedBadge
  headline: string
  /** A phrase only this state's `detail` says. */
  says: string
}[] = [
  {
    // The quiet a Listener hears most: a working Instance between Calls. It
    // used to tell every one of them to point a Trunk Recorder at the server
    // (#78) — setup instructions for somebody else, at the moment a Listener
    // is wondering whether anything is wrong.
    status: 'live',
    link: 'connected',
    badge: { label: 'connected', color: 'green', pulse: true },
    headline: 'Listening — nothing on the air.',
    says: 'play here the moment they happen',
  },
  {
    // The one quiet where the setup sentence is the right answer: no recorder
    // has ever sent anything, so the Instance has no Systems at all.
    status: 'live',
    link: 'connected',
    facts: { firstRun: true },
    badge: { label: 'connected', color: 'green', pulse: true },
    headline: 'Waiting for the first call…',
    says: 'Point a Trunk Recorder or SDRTrunk instance at this server',
  },
  {
    status: 'off',
    link: 'offline',
    badge: { label: 'FEED OFF', color: 'orange', pulse: false },
    headline: 'The live feed is switched off.',
    says: 'Turn the feed back on to listen',
  },
  {
    // Amber and still, like Feed off: both are silences the Listener asked for,
    // and neither is a fault. The words say which.
    status: 'playback',
    link: 'connected',
    badge: { label: 'PLAYBACK', color: 'orange', pulse: false },
    headline: 'Playing from the archive.',
    // #56 put the way back on this very screen, so the sentence stopped
    // sending a Listener to another tab to find it.
    says: 'Go back to the live feed',
  },
  {
    // The device is online and the server is not answering (#78). Not "calls
    // are still being recorded": if the server is what is down, that is the
    // one thing that is not true.
    status: 'down',
    link: 'offline',
    badge: { label: 'NO LINK', color: 'red', pulse: false },
    headline: 'The server isn\'t answering.',
    says: 'Your connection is fine',
  },
  {
    // Still **Feed down** as far as the cause goes — but a connection being
    // *made* is not one that has gone, and the first seconds of every visit
    // land here. Telling a Listener the server is unreachable while we are
    // reaching it would make the message worthless the rest of the time.
    status: 'down',
    link: 'connecting',
    badge: { label: 'linking…', color: 'red', pulse: true },
    headline: 'Connecting to the server…',
    says: 'as soon as the connection is up',
  },
  {
    // The device has no network (#78): the other half of **Feed down**, and
    // the half that is the Listener's to fix — so it must not read like the
    // server's fault. Still, because nothing can be attempted until it is back.
    status: 'down',
    link: 'offline',
    facts: { offline: true },
    badge: { label: 'OFFLINE', color: 'red', pulse: false },
    headline: 'This device is offline.',
    says: 'still being recorded',
  },
]

/** No network is no network, however far the socket had got: a reconnect in
 *  flight, or a refusal the server sent before the device dropped, are both
 *  stale the moment it goes. */
describe('feedReadout, offline', () => {
  it.each([
    ['connecting', undefined],
    ['offline', undefined],
    ['connected', 'instance-full'],
  ] as const)('reads OFFLINE over a %s link (refused: %s)', (link, refused) => {
    expect(feedReadout('down', link, { offline: true, refused }).badge.label).toBe('OFFLINE')
  })

  /** The Listener's own silences are theirs whatever the network is doing —
   *  a Listener who switched off on a train is still switched off. */
  it.each(['off', 'playback'] as const)('leaves %s its own words', (status) => {
    expect(feedReadout(status, 'offline', { offline: true }).badge.label).not.toBe('OFFLINE')
  })

  /** A live feed is, by definition, reaching us — a stale `navigator.onLine`
   *  does not get to say otherwise. */
  it('never reads a delivering feed as offline', () => {
    expect(feedReadout('live', 'connected', { offline: true }).badge.label).toBe('connected')
  })
})

/** What a Listener turned away is told (#77): *why*, in words that say what
 *  to do about it — which for a full Instance is nothing but wait. */
describe('feedReadout, turned away', () => {
  it.each([
    ['instance-full', 'FULL', 'This scanner is full right now.', 'as soon as'],
    ['access-connection-limit', 'CODE IN USE', 'Your access code is in use elsewhere.', 'another device'],
    ['something-new', 'REFUSED', 'The server turned this connection away.', 'keeps trying'],
  ])('reads %s as %s', (reason, label, headline, says) => {
    const readout = feedReadout('down', 'connected', { refused: reason })

    expect(readout.badge.label).toBe(label)
    // Being retried, so it pulses; not a fault on the Listener's side, so it
    // is not red — except for a reason this build cannot explain.
    expect(readout.badge.pulse).toBe(true)
    expect(readout.badge.color).toBe(label === 'REFUSED' ? 'red' : 'orange')
    expect(readout.empty.headline).toBe(headline)
    expect(readout.empty.detail).toContain(says)
  })

  /** A refusal is about the *feed*; the Listener's own choices still speak
   *  for themselves. */
  it('leaves the Listener\'s own silences their own words', () => {
    expect(feedReadout('off', 'connected', { refused: 'instance-full' }).badge.label).toBe('FEED OFF')
  })
})

describe('feedReadout', () => {
  it.each(READOUT_CASES)(
    'reads $badge.label for $status',
    ({ status, link, facts, badge, headline, says }) => {
      const readout = feedReadout(status, link, facts)

      expect(readout.badge).toEqual(badge)
      expect(readout.empty.headline).toBe(headline)
      expect(readout.empty.detail).toContain(says)
    },
  )

  /** A Listener glancing at the header must not have to read the word to know
   *  whether anything is wrong. */
  it('is green only when Calls are actually arriving', () => {
    const green = READOUT_CASES.filter(({ badge }) => badge.color === 'green')
    expect(green.length).toBeGreaterThan(0)
    expect(new Set(green.map(({ status }) => status))).toEqual(new Set(['live']))
  })

  /** Each state's phrase belongs to it alone, or the assertions above would pass
   *  on a readout that answered a different question. */
  it('says something different in every state', () => {
    const details = READOUT_CASES.map(
      ({ status, link, facts }) => feedReadout(status, link, facts).empty.detail,
    )
    expect(new Set(details).size).toBe(READOUT_CASES.length)
  })
})
