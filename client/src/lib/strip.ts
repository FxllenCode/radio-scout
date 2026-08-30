/**
 * What the global strip says — the app's one truthful readout of the transport
 * (#56, spec US 54).
 *
 * The Live screen is a whole player, and the moment a Listener leaves it for
 * Talkgroups, Search or Settings, everything it was saying goes with it: what
 * is playing, whose audio it is, and — worse — that the feed is deliberately
 * quiet. rdio-scanner has the same hole and answers it with nothing at all. So
 * one strip sits above the tab bar on every other tab, and this module decides
 * everything it says.
 *
 * **It is one component with two modes, not a player and a banner**, because a
 * Call playing and "the feed is quiet, and here is why" are mutually exclusive
 * by construction: in playback mode the source line *is* the explanation
 * ("3 of 421"), and with the feed off an archived Call is still the audio
 * (spec US 25). Two stacked bars would have to agree with each other; one
 * cannot disagree with itself.
 *
 * No store, no React, no dispatch — the same shape as [`feedReadout`] (#88),
 * [`runView`] (#89) and [`enqueue`] (#95): every rule is a value a test
 * constructs, and the component that draws it states no condition of its own.
 */
import type { Call } from '@/types'

import { feedReadout, type FeedBadge, type FeedStatus } from './feed'
import type { LiveStatus } from './liveFeed'

/** Whose audio is on the element (`@/store/transport`'s question, named). */
export type StripSource =
  /** The live feed (#11). */
  | 'live'
  /** A **Run** through the Archive, in playback mode (spec US 25). */
  | 'archive'
  /** One archived Call borrowing the audio, with the live feed and its
   *  listening queue waiting underneath (spec US 26). */
  | 'interrupting'

/** The one tap back to the live feed. Two silences, two different ways out —
 *  and naming the *intent* rather than carrying an action keeps this module
 *  free of the store. */
export type StripReturn =
  /** **Feed off** (#80): switch it back on. */
  | 'feed-on'
  /** **Playback mode**: leave it, and the feed picks up from now. */
  | 'leave-playback'

/** Where in a **Run** the Listener is — the "3 of 421" readout. `index` counts
 *  from the start of the filtered results, and is `-1` when nothing is playing
 *  (`@/store/playback`). */
export interface StripPosition {
  index: number
  total: number
}

/** Everything the strip's answer depends on. */
export interface StripFacts {
  /** Why the live feed is or is not delivering (#88). */
  status: FeedStatus
  /** What the socket is doing — read only for the badge's one refinement. */
  link: LiveStatus
  /** The Call on the element, from whichever source owns it. */
  playing: Call | null
  /** …and the Archive is that source. */
  fromArchive: boolean
  /** …with the live feed waiting underneath rather than switched off. */
  interrupting: boolean
  position: StripPosition
}

/** What the strip says, or `null` when it has nothing to say. */
export type Strip =
  | {
      mode: 'playing'
      call: Call
      source: StripSource
      /** The line under the Talkgroup: whose audio this is, and where in it. */
      detail: string
    }
  | {
      mode: 'quiet'
      /** The header's own dot and word ([`feedReadout`]), so a Listener
       *  glancing between the two never reads two different states. */
      badge: FeedBadge
      /** What the way back is called. */
      label: string
      does: StripReturn
    }

/** What the one tap back is called, and what it does. */
export interface WayBack {
  label: string
  does: StripReturn
}

/**
 * The way back out of each silence the Listener chose.
 *
 * **Its key set is the whole rule.** Absent for `live` and `down` — which are
 * exactly the two [`feedPlays`] accepts — so a second guard asking `feedPlays`
 * before this could never change an answer, and would be an assertion nothing
 * could ever fail. `strip.test.ts` pins the two key sets against each other
 * instead, which is the constraint that actually holds it.
 */
const WAY_BACK: Partial<Record<FeedStatus, WayBack>> = {
  off: { label: 'Turn on', does: 'feed-on' },
  playback: { label: 'Back to live', does: 'leave-playback' },
}

/**
 * How a Listener gets out of this silence, if they can.
 *
 * Exported because the Live screen offers the same way back from its own
 * controls (#56) and must call it the same thing: two spellings of one action
 * is two buttons as far as a screen reader — and as far as anybody reading the
 * code — is concerned.
 */
export const wayBack = (status: FeedStatus): WayBack | undefined => WAY_BACK[status]

/**
 * The line under the Talkgroup name, given whose audio it is.
 *
 * Shared with the Search screen's own now-playing bar, which asks the same
 * question of the same **Run** — two spellings of "3 of 421" would be two
 * screens quietly disagreeing about where a Listener is.
 */
export function playingDetail(
  source: StripSource,
  position: StripPosition,
): string {
  switch (source) {
    case 'live':
      return 'Live'
    case 'interrupting':
      return 'Interrupting live feed'
    case 'archive':
      return `${position.index + 1} of ${position.total}`
  }
}

/**
 * What the strip says right now.
 *
 * Three answers in order of what a Listener needs:
 *
 * - **A Call is playing** — show it, whatever the feed's standing is. Audio the
 *   Listener can hear outranks any explanation of silence, and with the feed
 *   off an archived Call is exactly that (spec US 25).
 * - **The feed is quiet because they asked** — say which silence it is, and
 *   offer the one tap out. Which silences those are is [`WAY_BACK`]'s key set,
 *   pinned against [`feedPlays`] so the strip cannot offer a way back out of a
 *   state the reducers do not think we are in.
 * - **Otherwise, nothing.** A lull on a healthy feed is not news, and a dropped
 *   socket is transient, self-healing and has no action to offer — a red bar on
 *   every tab for the first seconds of every visit would train a Listener to
 *   ignore the strip, which costs the two states above their audience.
 */
export function stripView(facts: StripFacts): Strip | null {
  const { status, link, playing, fromArchive, interrupting, position } = facts

  if (playing) {
    const source: StripSource = !fromArchive
      ? 'live'
      : interrupting
        ? 'interrupting'
        : 'archive'
    return {
      mode: 'playing',
      call: playing,
      source,
      detail: playingDetail(source, position),
    }
  }

  const back = wayBack(status)
  if (!back) return null
  return { mode: 'quiet', badge: feedReadout(status, link).badge, ...back }
}
