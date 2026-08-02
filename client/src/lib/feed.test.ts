import { describe, expect, it } from 'vitest'

import {
  controlsFor,
  feedReadout,
  feedStatus,
  type ControlFacts,
  type Controls,
  type FeedBadge,
  type FeedStatus,
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
    expected: only('holdSystem', 'holdTalkgroup', 'skip', 'replay', 'pause', 'avoid'),
  },
  {
    what: 'a lull with nothing behind it',
    status: 'live',
    facts: NOTHING,
    expected: only(),
  },
  {
    // The hold outlives the Call it was placed on, and releasing it is the only
    // way back — so the control that can release it stays reachable, and the
    // one that cannot does not.
    what: 'a System hold outliving the Call it was placed on',
    status: 'live',
    facts: { ...NOTHING, systemHold: true },
    expected: only('holdSystem'),
  },
  {
    what: 'a Talkgroup hold outliving the Call it was placed on',
    status: 'live',
    facts: { ...NOTHING, talkgroupHold: true },
    expected: only('holdTalkgroup'),
  },
  {
    // US 13: with nothing playing, Replay reaches back to the last Call.
    what: 'a lull with something in RECENT',
    status: 'live',
    facts: { ...NOTHING, hasRecent: true },
    expected: only('replay', 'recent'),
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
  badge: FeedBadge
  headline: string
  /** A phrase only this state's `detail` says. */
  says: string
}[] = [
  {
    status: 'live',
    link: 'connected',
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
    says: 'Leave playback mode from Search',
  },
  {
    status: 'down',
    link: 'offline',
    badge: { label: 'NO LINK', color: 'red', pulse: false },
    headline: 'No link to the server.',
    says: 'until the connection is back',
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
]

describe('feedReadout', () => {
  it.each(READOUT_CASES)(
    'reads $badge.label for $status',
    ({ status, link, badge, headline, says }) => {
      const readout = feedReadout(status, link)

      expect(readout.badge).toEqual(badge)
      expect(readout.empty.headline).toBe(headline)
      expect(readout.empty.detail).toContain(says)
    },
  )

  /** A Listener glancing at the header must not have to read the word to know
   *  whether anything is wrong. */
  it('is green only when Calls are actually arriving', () => {
    const green = READOUT_CASES.filter(({ badge }) => badge.color === 'green')
    expect(green.map(({ status }) => status)).toEqual(['live'])
  })

  /** Each state's phrase belongs to it alone, or the assertions above would pass
   *  on a readout that answered a different question. */
  it('says something different in every state', () => {
    const details = READOUT_CASES.map(
      ({ status, link }) => feedReadout(status, link).empty.detail,
    )
    expect(new Set(details).size).toBe(READOUT_CASES.length)
  })
})
