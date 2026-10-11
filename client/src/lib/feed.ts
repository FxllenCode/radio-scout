/**
 * Why the live feed is, or is not, delivering — derived once (#88).
 *
 * The screen used to work this out four times in three modules from a boolean
 * and a socket status, and the commit that shipped **Feed off** (#80) listed
 * three review defects that were each a reader which did not know. So the
 * question is answered here, once, over plain facts: no store, no React, and
 * reachable from a reducer guard and a banner alike.
 */
import type { LedColor } from './led'
import type { LiveStatus } from './liveFeed'

/**
 * The four ways the live feed can stand, in CONTEXT.md's vocabulary.
 *
 * - `live` — Calls are arriving and playing.
 * - `off` — **Feed off**: the Listener switched it off. Persists.
 * - `playback` — **Playback mode** owns the audio, and the two are mutually
 *   exclusive.
 * - `down` — **Feed down**: the connection isn't there, through no choice of
 *   the Listener's. Clears itself when the socket returns.
 */
export type FeedStatus = 'live' | 'off' | 'playback' | 'down'

/** What the feed's standing is derived from: the Listener's two choices, and
 *  the socket's condition. */
export interface FeedFacts {
  /** The Listener switched the feed off (#80). */
  off: boolean
  /** The Listener is playing the Archive instead. */
  playback: boolean
  /** What the socket is doing. */
  link: LiveStatus
  /** Why the server last turned this connection away, until it next takes one
   *  in (#77) — the `refused` frame's slug. */
  refused?: string | null
}

/**
 * Which of the four applies, in the order a Listener would want to be told.
 *
 * The order is the point, because more than one can be true at once:
 *
 * - **Off outranks everything.** It is a decision that persists, it is what the
 *   toggle reads, and it survives leaving playback mode — so naming any other
 *   cause would promise a feed that is not coming back.
 * - **Playback outranks down.** Switching to the Archive drops the subscription
 *   but keeps the socket; if that socket also drops, a Listener working through
 *   an incident does not care and should not be alarmed.
 * - **Connecting is down.** CONTEXT.md's **Feed down** is "not delivering
 *   because the connection isn't there", and a reconnect in progress is not
 *   delivering. How far along it is belongs to the display
 *   ([`feedReadout`]), not to the cause.
 */
export function feedStatus({ off, playback, link, refused }: FeedFacts): FeedStatus {
  if (off) return 'off'
  if (playback) return 'playback'
  // Turned away is down, whatever the socket says: the server opens it before
  // saying no, and the retry opens another — so the link alone would flash
  // `live` at a Listener being refused, once per attempt (#77).
  if (refused) return 'down'
  return link === 'connected' ? 'live' : 'down'
}

/**
 * Is the live feed the Listener's audio right now?
 *
 * True while it is delivering, and true while it is merely **Feed down** — a
 * dropped socket stops Calls arriving, but the Call already on the air plays
 * on, the queue behind it drains, and the Listener asked for none of that to
 * stop. False for the two the Listener chose: **Feed off** and playback mode.
 *
 * This is the sentence "off means nothing plays" was standing in for, written
 * once. Every guard in the live slice and every control on the Live screen asks
 * it, so none of them can answer it differently.
 */
export const feedPlays = (status: FeedStatus): boolean =>
  status === 'live' || status === 'down'

/** What the Live screen's controls have to act *on*, beyond the feed's own
 *  standing. Booleans rather than the Calls and Holds themselves: this module
 *  decides what is reachable, and has no business knowing what a **Hold** is
 *  made of. */
export interface ControlFacts {
  /** The live feed has a Call on the air — playing or paused. */
  onAir: boolean
  /** A **Hold** on a whole System is in force. */
  systemHold: boolean
  /** A **Hold** on one Talkgroup is in force. */
  talkgroupHold: boolean
  /** The RECENT list has something in it. */
  hasRecent: boolean
}

/** Which Live-screen controls a Listener can actually use. Every one of them,
 *  named — a control missing from this record does not compile, which is the
 *  half of #88 that stops the next control being added ungated. */
export interface Controls {
  holdSystem: boolean
  holdTalkgroup: boolean
  skip: boolean
  replay: boolean
  pause: boolean
  avoid: boolean
  /** Mark the shown Call's Talkgroup **Priority** (#58, spec US 27) — one of
   *  the controls that act on the Call being *shown*. */
  priority: boolean
  /** The `Q` counter, which opens the listening queue (#58, spec US 24).
   *
   *  Gated on the feed rather than on the queue being non-empty: US 24 asks to
   *  "see... what's waiting", and "nothing is waiting" is an answer. What it
   *  must not do is offer a queue in a state that has none by construction —
   *  **Feed off** and **Playback mode** both empty it. */
  queue: boolean
  /** The RECENT rows, which replay the Call they name. */
  recent: boolean
}

/**
 * What is reachable, given the feed's standing and what there is to act on.
 *
 * Each control's own precondition is written once and only here, so the screen
 * states no condition at all. Two are worth naming:
 *
 * - **Each Hold is gated on its own kind.** They shared one condition before
 *   (#88), which left *Hold system* enabled while a Talkgroup hold outlived its
 *   Call — a button that pressed and did nothing, because the reducer needs a
 *   Call to place a System hold on. A hold can always release itself; it can
 *   only be *placed* on a Call.
 * - **Five controls act on the Call being *shown*, not the one on the air**
 *   (#56). The display keeps the last Call up between transmissions, dimmed, so
 *   Hold, Avoid, Replay and Priority (#58) go on meaning something after it
 *   ends — which is exactly when a Listener reaches for them, a chatty
 *   Talkgroup having just stopped keying. Skip and Pause do not join them:
 *   those act on audio, and there is none.
 */
export function controlsFor(status: FeedStatus, facts: ControlFacts): Controls {
  const plays = feedPlays(status)
  const { onAir, systemHold, talkgroupHold, hasRecent } = facts
  // The Call on the card. `hasRecent` is the whole of "there is a last Call",
  // because switching a Call off the air files it at the head of RECENT — so
  // this is not two sources agreeing, it is one fact asked from two angles.
  const showing = onAir || hasRecent

  return {
    holdSystem: plays && (showing || systemHold),
    holdTalkgroup: plays && (showing || talkgroupHold),
    skip: plays && onAir,
    replay: plays && showing,
    pause: plays && onAir,
    avoid: plays && showing,
    priority: plays && showing,
    queue: plays,
    recent: plays && hasRecent,
  }
}

/** The header's dot and the word beside it. */
export interface FeedBadge {
  label: string
  color: LedColor
  /** The pulse means something is happening. A state with nothing arriving does
   *  not pulse, except while a reconnect is in flight — there the pulse is the
   *  only thing saying something is being attempted. */
  pulse: boolean
}

/** What the display says in place of a Call. */
export interface FeedEmpty {
  headline: string
  /** The sentence under it, which says what to *do* about it. rdio shows nothing
   *  at all until a Call arrives, leaving "quiet" and "broken" looking
   *  identical. */
  detail: string
}

/**
 * How the feed's standing reads on screen.
 *
 * Two halves, because no component wants both — the header draws the badge and
 * the empty display draws the words, so each takes the half it uses and its
 * props say so. One *function* decides them, which is the part that matters:
 * they are answers to the same question and must never describe two different
 * situations.
 */
export interface FeedReadout {
  badge: FeedBadge
  empty: FeedEmpty
}

/**
 * What the readout knows beyond the feed's standing — each a refinement of
 * one state's words, never a different state.
 */
export interface ReadoutFacts {
  /** Why the server last turned this connection away, until it next takes one
   *  in (#77) — the `refused` frame's slug. */
  refused?: string | null
  /** The device itself has no network (`navigator.onLine`, #78). */
  offline?: boolean
  /** The Instance has no System yet — nothing has ever been ingested — so a
   *  quiet feed is a setup still to be done rather than a lull (#78). */
  firstRun?: boolean
}

/**
 * Everything the screen says about why it is quiet, from one answer.
 *
 * The dot, the word and the prose are decided together and in one place, so a
 * state cannot end up amber but pulsing, lit green under the words FEED OFF, or
 * telling a Listener the server is gone under a header reading `connected`.
 * Green means exactly one thing — Calls are arriving — which is what lets a
 * Listener glance rather than read.
 *
 * `link` is consulted for one refinement, and having it here is why that
 * refinement is made once rather than per reader: **Feed down** is one cause
 * but two situations, and the first seconds of every visit are the *other* one.
 * Saying the server is unreachable while we are in the act of reaching it would
 * make the message worthless the rest of the time.
 *
 * **Offline outranks everything else Feed down could say** (#78). With no
 * network a reconnect in flight is going nowhere, and a refusal the server sent
 * before the device dropped describes a connection that no longer exists — and
 * of all of them it is the one a Listener can do something about, so it must
 * not read like the server's fault. rdio shows the same spinner for both.
 */
export function feedReadout(
  status: FeedStatus,
  link: LiveStatus,
  { refused, offline, firstRun }: ReadoutFacts = {},
): FeedReadout {
  if (status === 'down' && offline) return OFFLINE
  if (status === 'down' && refused) return turnedAway(refused)
  switch (status) {
    case 'live':
      return {
        badge: { label: 'connected', color: 'green', pulse: true },
        // The setup sentence only where there is setup still to do. Every
        // other quiet is a Listener on a working Instance between Calls, and
        // telling them to point a recorder at the server is an instruction
        // for somebody else at the moment they wonder if something is wrong.
        empty: firstRun
          ? {
              headline: 'Waiting for the first call…',
              detail:
                'Point a Trunk Recorder or SDRTrunk instance at this server and calls for your selected talkgroups will play here automatically.',
            }
          : {
              headline: 'Listening — nothing on the air.',
              detail:
                'Calls on your selected talkgroups play here the moment they happen.',
            },
      }
    // Amber and still for both of the Listener's own silences: neither is a
    // fault, and the words are what say which of them it is.
    case 'off':
      return {
        badge: { label: 'FEED OFF', color: 'orange', pulse: false },
        empty: {
          headline: 'The live feed is switched off.',
          detail:
            'Nothing is being streamed or queued. Turn the feed back on to listen — it will start from whatever is happening then.',
        },
      }
    case 'playback':
      return {
        badge: { label: 'PLAYBACK', color: 'orange', pulse: false },
        empty: {
          headline: 'Playing from the archive.',
          detail:
            'The live feed and playback mode are mutually exclusive, so nothing is arriving here. Go back to the live feed and it picks up from whatever is happening then.',
        },
      }
    case 'down':
      return link === 'connecting'
        ? {
            badge: { label: 'linking…', color: 'red', pulse: true },
            empty: {
              headline: 'Connecting to the server…',
              detail:
                'Calls will start playing as soon as the connection is up. A brief drop is filled in for you when it returns.',
            },
          }
        : {
            // Not "calls are still being recorded": the device is online, so
            // the likeliest thing that is down is the server — and then that
            // is the one thing that is not true.
            badge: { label: 'NO LINK', color: 'red', pulse: false },
            empty: {
              headline: 'The server isn\'t answering.',
              detail:
                'Your connection is fine — the scanner itself is down or out of reach. This keeps retrying on its own.',
            },
          }
  }
}

/** **Feed down** because this device has no network (#78). Red, because
 *  nothing is reaching the Listener; still, because nothing can be attempted
 *  until it is back. */
const OFFLINE: FeedReadout = {
  badge: { label: 'OFFLINE', color: 'red', pulse: false },
  empty: {
    headline: 'This device is offline.',
    detail:
      'Nothing can reach you without a network. Calls are still being recorded, and the feed picks up by itself when you are back — a brief gap is filled in for you.',
  },
}

/**
 * **Feed down** because the server said no, rather than because it could not
 * be reached (#77) — and so, said as *that*, with what to do about it.
 *
 * Amber for the two a Listener can understand: neither is a fault, and both
 * clear by themselves as the client keeps trying. It pulses, because it is
 * being retried. A reason this build has no words for is red, and says only
 * that it was refused — the Operator's log has the slug.
 *
 * rdio closes the socket without a word (`client.go:61`), so its client sits in
 * a reconnect loop a Listener can only read as the network being broken.
 */
function turnedAway(reason: string): FeedReadout {
  switch (reason) {
    case 'instance-full':
      return {
        badge: { label: 'FULL', color: 'orange', pulse: true },
        empty: {
          headline: 'This scanner is full right now.',
          detail:
            'Every listening slot is taken. You will be let in as soon as somebody leaves — this keeps trying on its own.',
        },
      }
    case 'access-connection-limit':
      return {
        badge: { label: 'CODE IN USE', color: 'orange', pulse: true },
        empty: {
          headline: 'Your access code is in use elsewhere.',
          detail:
            'It allows only so many connections at once, and they are all taken. Close the app on another device, or wait — this keeps trying.',
        },
      }
    default:
      return {
        badge: { label: 'REFUSED', color: 'red', pulse: true },
        empty: {
          headline: 'The server turned this connection away.',
          detail: 'It keeps trying. If this does not clear, the operator can see why in their log.',
        },
      }
  }
}
