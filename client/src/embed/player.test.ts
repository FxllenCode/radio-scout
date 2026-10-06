import { describe, expect, it } from 'vitest'

import type { Call } from '@/types'

import {
  INITIAL,
  MAX_QUEUED,
  MAX_SHOWN,
  nowPlaying,
  step,
  type EmbedFeed,
  type PlayerState,
} from './player'

function call(id: number, extra: Partial<Call> = {}): Call {
  return {
    id,
    systemRef: 11,
    talkgroupRef: 100,
    timestamp: 1_700_000_000_000 + id * 1000,
    audioUrl: `/api/call/${id}/audio`,
    ...extra,
  }
}

function feed(...calls: Call[]): EmbedFeed {
  return {
    name: 'Fire dispatch',
    selection: { all: false, sel: { 11: { 100: true } } },
    calls,
  }
}

/** A page that has loaded `calls` (newest first, as the feed sends them). */
function loaded(...calls: Call[]): PlayerState {
  return step(INITIAL, { type: 'loaded', feed: feed(...calls) })
}

/** ...and whose reader pressed Listen live, and whose socket came up. */
function listening(...calls: Call[]): PlayerState {
  return step(step(loaded(...calls), { type: 'listen' }), {
    type: 'status',
    status: 'connected',
  })
}

const ids = (state: PlayerState) => state.calls.map((it) => it.id)

describe('loading a feed', () => {
  it('starts out loading, with nothing to show', () => {
    expect(INITIAL.phase).toBe('loading')
    expect(INITIAL.calls).toEqual([])
    expect(INITIAL.listening).toBe(false)
  })

  it('shows the name and the recent calls, newest first', () => {
    const state = loaded(call(3), call(2), call(1))

    expect(state.phase).toBe('ready')
    expect(state.name).toBe('Fire dispatch')
    expect(ids(state)).toEqual([3, 2, 1])
  })

  /** A deleted embed is the Operator taking it down: the page says so, and
   *  whatever was playing stops. */
  it('says the feed is gone, and stops, when the embed is deleted', () => {
    const playing = step(listening(call(1)), { type: 'play', id: 1 })

    const state = step(playing, { type: 'gone' })

    expect(state.phase).toBe('unavailable')
    expect(state.listening).toBe(false)
    expect(nowPlaying(state)).toBeUndefined()
  })

  it('says the scanner is unreachable when the first load fails', () => {
    const state = step(INITIAL, { type: 'unreachable' })

    expect(state.phase).toBe('unreachable')
  })

  /** **Degrades after load**: a refresh that fails leaves what the page already
   *  had where it was, rather than replacing a list with an error. */
  it('keeps what it has when a later load fails', () => {
    const state = step(loaded(call(2), call(1)), { type: 'unreachable' })

    expect(state.phase).toBe('ready')
    expect(ids(state)).toEqual([2, 1])
  })

  /** Loading again — which pressing Listen does — merges rather than replaces,
   *  so nothing heard live in between is dropped, and nothing is listed twice. */
  it('merges a fresh load into what is shown', () => {
    const heard = step(listening(call(2)), { type: 'arrived', call: call(5) })

    const state = step(heard, {
      type: 'loaded',
      feed: feed(call(4), call(3), call(2)),
    })

    expect(ids(state)).toEqual([5, 4, 3, 2])
  })

  /** A Call a Recorder stamped no time on still has a place: after the timed
   *  ones, newest id first. */
  it('lists a call with no time after the ones that have one', () => {
    const state = loaded(
      call(1, { timestamp: undefined }),
      call(3),
      call(2, { timestamp: undefined }),
    )

    expect(ids(state)).toEqual([3, 2, 1])
  })

  it('shows at most a page of calls', () => {
    const many = Array.from({ length: MAX_SHOWN + 5 }, (_, i) =>
      call(MAX_SHOWN + 5 - i),
    )

    const state = loaded(...many)

    expect(state.calls).toHaveLength(MAX_SHOWN)
    expect(state.calls[0].id).toBe(MAX_SHOWN + 5)
  })
})

describe('listening live', () => {
  it('connects when the reader presses listen, and not before', () => {
    expect(loaded(call(1)).listening).toBe(false)

    const state = step(loaded(call(1)), { type: 'listen' })

    expect(state.listening).toBe(true)
    expect(state.live).toBe('connecting')
  })

  /** Listening is from now: pressing Listen does not replay the backlog. */
  it('plays nothing until something arrives', () => {
    expect(nowPlaying(listening(call(2), call(1)))).toBeUndefined()
  })

  it('plays a call as it arrives, and lists it first', () => {
    const state = step(listening(call(1)), { type: 'arrived', call: call(2) })

    expect(nowPlaying(state)?.id).toBe(2)
    expect(ids(state)).toEqual([2, 1])
  })

  it('queues what arrives while something plays, and plays it in order', () => {
    let state = step(listening(), { type: 'arrived', call: call(1) })
    state = step(state, { type: 'arrived', call: call(2) })
    state = step(state, { type: 'arrived', call: call(3) })

    expect(nowPlaying(state)?.id).toBe(1)
    state = step(state, { type: 'ended' })
    expect(nowPlaying(state)?.id).toBe(2)
    state = step(state, { type: 'ended' })
    expect(nowPlaying(state)?.id).toBe(3)
    state = step(state, { type: 'ended' })
    expect(nowPlaying(state)).toBeUndefined()
  })

  /** Delivery is at-least-once (ADR-0004): a Backfill after a reconnect can
   *  hand back a Call the page already has, and it is neither listed nor played
   *  twice. */
  it('takes a call it already has as nothing', () => {
    let state = step(listening(call(1)), { type: 'arrived', call: call(2) })
    state = step(state, { type: 'arrived', call: call(3) })

    const again = step(state, { type: 'arrived', call: call(3) })

    expect(again).toBe(state)
  })

  /** An Encrypted Call is the activity and no audio (spec US 9): listed, never
   *  queued for a player that has nothing to play. */
  it('lists a call with no audio and plays nothing for it', () => {
    const state = step(listening(), {
      type: 'arrived',
      call: call(1, { audioUrl: undefined, encrypted: true }),
    })

    expect(ids(state)).toEqual([1])
    expect(nowPlaying(state)).toBeUndefined()
    expect(state.queue).toEqual([])
  })

  /** **Stay near live.** A busy county outruns the speaker, and a queue that
   *  only grows plays the afternoon at night — so the oldest waiting are passed
   *  over. */
  it('passes over the oldest waiting calls rather than falling behind', () => {
    let state = step(listening(), { type: 'arrived', call: call(1) })
    for (let id = 2; id <= MAX_QUEUED + 3; id += 1) {
      state = step(state, { type: 'arrived', call: call(id) })
    }

    expect(state.queue).toHaveLength(MAX_QUEUED)
    expect(state.queue[0]).toBe(4)
    expect(nowPlaying(state)?.id).toBe(1)
  })

  it('ignores a call that arrives after the reader stopped', () => {
    const stopped = step(listening(call(1)), { type: 'stop' })

    const state = step(stopped, { type: 'arrived', call: call(2) })

    expect(state).toBe(stopped)
  })

  it('stops everything when the reader presses stop', () => {
    let state = step(listening(), { type: 'arrived', call: call(1) })
    state = step(state, { type: 'arrived', call: call(2) })

    state = step(state, { type: 'stop' })

    expect(state.listening).toBe(false)
    expect(state.live).toBe('offline')
    expect(nowPlaying(state)).toBeUndefined()
    expect(state.queue).toEqual([])
  })

  it('follows the socket while listening', () => {
    const state = step(listening(), { type: 'status', status: 'offline' })

    expect(state.live).toBe('offline')
  })

  /** The socket says `connecting` the moment it is opened, which the page
   *  already knew — and an event that changes nothing must *be* nothing, or the
   *  adapter reacts to it as a change. */
  it('takes a status it already has as nothing', () => {
    const connecting = step(loaded(), { type: 'listen' })

    expect(step(connecting, { type: 'status', status: 'connecting' })).toBe(
      connecting,
    )
  })

  /** A late status from a socket the reader already closed must not light the
   *  page back up. */
  it('takes no status once the reader has stopped', () => {
    const stopped = step(listening(), { type: 'stop' })

    expect(step(stopped, { type: 'status', status: 'connected' })).toBe(stopped)
  })
})

describe('a call that will not play', () => {
  /** **Degrades when unreachable, listening or not**: a reader who taps a row on
   *  an Instance that has gone away is told, rather than pressing a button that
   *  does nothing. */
  it('moves on, and remembers which call would not play', () => {
    const tapped = step(loaded(call(2), call(1)), { type: 'play', id: 1 })

    const state = step(tapped, { type: 'failed' })

    expect(nowPlaying(state)).toBeUndefined()
    expect(state.failed).toBe(1)
  })

  it('plays what waits next while listening', () => {
    let state = step(listening(), { type: 'arrived', call: call(1) })
    state = step(state, { type: 'arrived', call: call(2) })

    state = step(state, { type: 'failed' })

    expect(nowPlaying(state)?.id).toBe(2)
    expect(state.failed).toBe(1)
  })

  /** The note is about the last thing tried, so trying anything again clears
   *  it — a press, Listen, a feed that loads, a live call that starts. */
  it('forgets the failure once anything is tried again', () => {
    const failed = step(step(loaded(call(2), call(1)), { type: 'play', id: 1 }), {
      type: 'failed',
    })

    expect(step(failed, { type: 'play', id: 2 }).failed).toBeUndefined()
    expect(step(failed, { type: 'listen' }).failed).toBeUndefined()
    expect(step(failed, { type: 'loaded', feed: feed(call(1)) }).failed).toBeUndefined()
    const live = step(step(failed, { type: 'listen' }), { type: 'status', status: 'connected' })
    expect(step(live, { type: 'arrived', call: call(3) }).failed).toBeUndefined()
  })
})

describe('playing one call', () => {
  /** A reader can play any listed Call, listening or not — and it interrupts,
   *  because they pressed it. */
  it('plays the call the reader pressed', () => {
    const state = step(loaded(call(2), call(1)), { type: 'play', id: 1 })

    expect(nowPlaying(state)?.id).toBe(1)
  })

  it('does nothing for a call it does not have, or one with no audio', () => {
    const page = loaded(call(1, { audioUrl: undefined }))

    expect(step(page, { type: 'play', id: 1 })).toBe(page)
    expect(step(page, { type: 'play', id: 9 })).toBe(page)
  })

  it('plays nothing next when the reader is not listening', () => {
    const played = step(loaded(call(2), call(1)), { type: 'play', id: 1 })

    expect(nowPlaying(step(played, { type: 'ended' }))).toBeUndefined()
  })

  /** Pressing the Call that is playing moves on: it stops, or — listening —
   *  the next waiting Call plays. The row's button is a toggle, not a restart. */
  it('moves on when the reader presses the call that is playing', () => {
    const alone = step(loaded(call(1)), { type: 'play', id: 1 })
    expect(nowPlaying(step(alone, { type: 'play', id: 1 }))).toBeUndefined()

    let live = step(listening(), { type: 'arrived', call: call(1) })
    live = step(live, { type: 'arrived', call: call(2) })
    expect(nowPlaying(step(live, { type: 'play', id: 1 }))?.id).toBe(2)
  })

  /** ...and while listening, the live queue picks up where the press left it. */
  it('goes back to the live queue when a pressed call ends', () => {
    let state = step(listening(call(1)), { type: 'arrived', call: call(2) })
    state = step(state, { type: 'arrived', call: call(3) })
    state = step(state, { type: 'play', id: 1 })

    state = step(state, { type: 'ended' })

    expect(nowPlaying(state)?.id).toBe(3)
  })
})
